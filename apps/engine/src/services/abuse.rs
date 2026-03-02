use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use hickory_resolver::Resolver;
use hmac::{Hmac, Mac};
use http::HeaderMap;
use ipnet::IpNet;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{config::Config, error::AppError, metrics};

const STRIKE_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_BYPASS_HEADER: &str = "x-rift-abuse-bypass";
const CHALLENGE_COOKIE_NAME: &str = "rift_abuse_challenge";

#[derive(Debug, Clone)]
pub struct AbuseLimit {
    pub scope: &'static str,
    pub actor_key: String,
    pub bucket_key: String,
    pub limit: u64,
    pub window: Duration,
    pub challenge_after: Option<u64>,
}

impl AbuseLimit {
    pub fn per_ip(
        scope: &'static str,
        ip: IpAddr,
        bucket_suffix: impl AsRef<str>,
        limit: u64,
        window: Duration,
        challenge_after: Option<u64>,
    ) -> Self {
        let actor_key = format!("ip:{ip}");
        let bucket_key = format!("scope:{scope}:{}:{ip}", bucket_suffix.as_ref());
        Self {
            scope,
            actor_key,
            bucket_key,
            limit,
            window,
            challenge_after,
        }
    }
}

#[derive(Debug, Clone)]
pub enum AbuseDecision {
    Allow,
    Challenge {
        retry_after_secs: u64,
        reason: String,
    },
    Block {
        retry_after_secs: u64,
        reason: String,
        tier: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct AbuseScopeStats {
    pub scope: String,
    pub allowed: u64,
    pub challenged: u64,
    pub blocked: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbuseSnapshot {
    pub backend: String,
    pub checked_total: u64,
    pub allowed_total: u64,
    pub challenged_total: u64,
    pub blocked_total: u64,
    pub active_local_bans: usize,
    pub scopes: Vec<AbuseScopeStats>,
}

#[derive(Debug, Clone)]
pub struct ResolvedLimit {
    pub enabled: bool,
    pub limit: u64,
    pub window: Duration,
    pub challenge_after: Option<u64>,
}

#[derive(Clone)]
pub struct AbuseGuard {
    backend: Backend,
    telemetry: Arc<Telemetry>,
    settings: Arc<AbuseSettings>,
    crawler_verifier: Option<CrawlerVerifier>,
}

#[derive(Clone)]
enum Backend {
    Local {
        state: Arc<LocalState>,
    },
    Redis {
        client: redis::Client,
        fallback: Arc<LocalState>,
    },
}

#[derive(Default)]
struct Telemetry {
    checked_total: AtomicU64,
    allowed_total: AtomicU64,
    challenged_total: AtomicU64,
    blocked_total: AtomicU64,
    scope_counters: RwLock<HashMap<String, ScopeCounter>>,
}

#[derive(Default, Clone, Copy)]
struct ScopeCounter {
    allowed: u64,
    challenged: u64,
    blocked: u64,
}

#[derive(Default)]
struct LocalState {
    counters: RwLock<HashMap<String, LocalCounter>>,
    bans: RwLock<HashMap<String, LocalBan>>,
    strikes: RwLock<HashMap<String, LocalStrike>>,
}

#[derive(Clone, Copy)]
struct LocalCounter {
    started_at: Instant,
    count: u64,
    window: Duration,
}

#[derive(Clone, Copy)]
struct LocalBan {
    expires_at: Instant,
}

#[derive(Clone, Copy)]
struct LocalStrike {
    started_at: Instant,
    count: u64,
}

#[derive(Debug, Clone)]
struct AbuseSettings {
    allowlist_cidrs: Vec<IpNet>,
    bypass_header: String,
    bypass_token: Option<String>,
    challenge_ttl: Duration,
    bot_verify_enabled: bool,
    bot_verify_cache_ttl: Duration,
    limit_overrides: Vec<LimitOverride>,
    challenge_secret: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct LimitOverride {
    scope: String,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    window_secs: Option<u64>,
    #[serde(default)]
    challenge_after: Option<u64>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Clone)]
struct CrawlerVerifier {
    cache: Arc<RwLock<HashMap<String, BotCacheEntry>>>,
    ttl: Duration,
}

#[derive(Clone, Copy)]
struct BotCacheEntry {
    verified: bool,
    expires_at: Instant,
}

impl AbuseGuard {
    pub fn new(config: &Config) -> Self {
        let telemetry = Arc::new(Telemetry::default());
        let local = Arc::new(LocalState::default());
        let settings = Arc::new(AbuseSettings::from_config(config));

        let crawler_verifier = if settings.bot_verify_enabled {
            match Resolver::builder_tokio() {
                Ok(_) => Some(CrawlerVerifier {
                    cache: Arc::new(RwLock::new(HashMap::new())),
                    ttl: settings.bot_verify_cache_ttl,
                }),
                Err(error) => {
                    tracing::warn!(error = %error, "failed to initialize crawler DNS verifier");
                    None
                }
            }
        } else {
            None
        };

        if config.state_store == "redis" {
            match redis::Client::open(config.redis_url.clone()) {
                Ok(client) => {
                    return Self {
                        backend: Backend::Redis {
                            client,
                            fallback: local,
                        },
                        telemetry,
                        settings,
                        crawler_verifier,
                    };
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "failed to initialize redis for abuse guard; using local backend"
                    );
                }
            }
        }

        Self {
            backend: Backend::Local { state: local },
            telemetry,
            settings,
            crawler_verifier,
        }
    }

    pub fn local_for_tests() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"test-challenge-secret");
        let settings = AbuseSettings {
            allowlist_cidrs: Vec::new(),
            bypass_header: DEFAULT_BYPASS_HEADER.to_owned(),
            bypass_token: None,
            challenge_ttl: Duration::from_secs(900),
            bot_verify_enabled: false,
            bot_verify_cache_ttl: Duration::from_secs(600),
            limit_overrides: Vec::new(),
            challenge_secret: hasher.finalize().to_vec(),
        };

        Self {
            backend: Backend::Local {
                state: Arc::new(LocalState::default()),
            },
            telemetry: Arc::new(Telemetry::default()),
            settings: Arc::new(settings),
            crawler_verifier: None,
        }
    }

    pub fn redis_for_tests(redis_url: &str) -> Result<Self, AppError> {
        let mut guard = Self::local_for_tests();
        let client = redis::Client::open(redis_url)
            .map_err(|e| AppError::Internal(format!("redis init failed: {e}")))?;
        guard.backend = Backend::Redis {
            client,
            fallback: Arc::new(LocalState::default()),
        };
        Ok(guard)
    }

    pub fn resolve_limit(
        &self,
        scope: &str,
        project_id: Option<Uuid>,
        default_limit: u64,
        default_window: Duration,
        default_challenge_after: Option<u64>,
    ) -> ResolvedLimit {
        let mut resolved = ResolvedLimit {
            enabled: true,
            limit: default_limit,
            window: default_window,
            challenge_after: default_challenge_after,
        };

        for override_cfg in &self.settings.limit_overrides {
            if override_cfg.scope != scope {
                continue;
            }
            match (override_cfg.project_id, project_id) {
                (Some(override_project), Some(target_project)) if override_project == target_project => {}
                (Some(_), _) => continue,
                (None, _) => {}
            }

            if let Some(enabled) = override_cfg.enabled {
                resolved.enabled = enabled;
            }
            if let Some(limit) = override_cfg.limit {
                resolved.limit = limit;
            }
            if let Some(window_secs) = override_cfg.window_secs {
                resolved.window = Duration::from_secs(window_secs.max(1));
            }
            if let Some(challenge_after) = override_cfg.challenge_after {
                resolved.challenge_after = Some(challenge_after);
            }
        }

        resolved
    }

    pub fn is_trusted_request(&self, client_ip: IpAddr, headers: &HeaderMap) -> bool {
        self.is_allowlisted_ip(client_ip) || self.has_bypass_token(headers)
    }

    pub async fn should_bypass_proxy_limits(&self, client_ip: IpAddr, headers: &HeaderMap) -> bool {
        if self.is_trusted_request(client_ip, headers) {
            return true;
        }

        if self.has_valid_challenge_cookie(client_ip, headers) {
            return true;
        }

        self.is_verified_crawler(client_ip, headers).await
    }

    pub fn build_challenge_set_cookie(&self, client_ip: IpAddr, headers: &HeaderMap, secure: bool) -> String {
        let token = self.issue_challenge_token(client_ip, headers);
        let mut cookie = format!(
            "{}={}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
            CHALLENGE_COOKIE_NAME,
            token,
            self.settings.challenge_ttl.as_secs().max(1)
        );
        if secure {
            cookie.push_str("; Secure");
        }
        cookie
    }

    pub async fn enforce(&self, limit: AbuseLimit) -> Result<AbuseDecision, AppError> {
        self.telemetry.checked_total.fetch_add(1, Ordering::Relaxed);

        if let Some(retry_after_secs) = self.ban_ttl_secs(&limit.actor_key).await {
            let decision = AbuseDecision::Block {
                retry_after_secs,
                reason: "temporary network ban active".to_owned(),
                tier: "existing".to_owned(),
            };
            self.record(limit.scope, &decision).await;
            return Ok(decision);
        }

        let (count, window_remaining) = self
            .incr_counter(&limit.bucket_key, limit.window)
            .await
            .map_err(|e| AppError::Internal(format!("abuse counter failed: {e}")))?;

        if count > limit.limit {
            let strikes = self
                .incr_strike(&limit.actor_key)
                .await
                .map_err(|e| AppError::Internal(format!("abuse strike failed: {e}")))?;
            let (ban_for, tier) = ban_tier(strikes);

            self.set_ban(&limit.actor_key, ban_for)
                .await
                .map_err(|e| AppError::Internal(format!("abuse ban failed: {e}")))?;

            metrics::ABUSE_BAN_TIER.with_label_values(&[tier]).inc();
            let decision = AbuseDecision::Block {
                retry_after_secs: ban_for.as_secs(),
                reason: format!("rate limit exceeded for {}", limit.scope),
                tier: tier.to_owned(),
            };
            self.record(limit.scope, &decision).await;
            return Ok(decision);
        }

        if let Some(challenge_after) = limit.challenge_after {
            if count > challenge_after {
                let decision = AbuseDecision::Challenge {
                    retry_after_secs: window_remaining.max(1),
                    reason: format!("challenge required for {}", limit.scope),
                };
                self.record(limit.scope, &decision).await;
                return Ok(decision);
            }
        }

        let decision = AbuseDecision::Allow;
        self.record(limit.scope, &decision).await;
        Ok(decision)
    }

    pub async fn snapshot(&self) -> AbuseSnapshot {
        let scope_map = self.telemetry.scope_counters.read().await;
        let mut scopes: Vec<AbuseScopeStats> = scope_map
            .iter()
            .map(|(scope, counter)| AbuseScopeStats {
                scope: scope.clone(),
                allowed: counter.allowed,
                challenged: counter.challenged,
                blocked: counter.blocked,
            })
            .collect();
        scopes.sort_by(|a, b| a.scope.cmp(&b.scope));

        let active_local_bans = match &self.backend {
            Backend::Local { state } => state.active_bans().await,
            Backend::Redis { fallback, .. } => fallback.active_bans().await,
        };

        AbuseSnapshot {
            backend: match &self.backend {
                Backend::Local { .. } => "local".to_owned(),
                Backend::Redis { .. } => "redis".to_owned(),
            },
            checked_total: self.telemetry.checked_total.load(Ordering::Relaxed),
            allowed_total: self.telemetry.allowed_total.load(Ordering::Relaxed),
            challenged_total: self.telemetry.challenged_total.load(Ordering::Relaxed),
            blocked_total: self.telemetry.blocked_total.load(Ordering::Relaxed),
            active_local_bans,
            scopes,
        }
    }

    fn is_allowlisted_ip(&self, client_ip: IpAddr) -> bool {
        self.settings
            .allowlist_cidrs
            .iter()
            .any(|cidr| cidr.contains(&client_ip))
    }

    fn has_bypass_token(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = self.settings.bypass_token.as_deref() else {
            return false;
        };

        let Some(value) = headers
            .get(self.settings.bypass_header.as_str())
            .and_then(|v| v.to_str().ok())
        else {
            return false;
        };

        constant_time_eq(expected.as_bytes(), value.as_bytes())
    }

    async fn is_verified_crawler(&self, client_ip: IpAddr, headers: &HeaderMap) -> bool {
        let Some(verifier) = &self.crawler_verifier else {
            return false;
        };

        let Some(user_agent) = headers
            .get(http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
        else {
            return false;
        };

        verifier.verify(client_ip, user_agent).await
    }

    fn issue_challenge_token(&self, client_ip: IpAddr, headers: &HeaderMap) -> String {
        let window = current_window(self.settings.challenge_ttl);
        let ua_hash = user_agent_hash(headers);
        let sig = self.sign_challenge(client_ip, &ua_hash, window);
        format!("{window}:{sig}")
    }

    fn has_valid_challenge_cookie(&self, client_ip: IpAddr, headers: &HeaderMap) -> bool {
        let Some(cookie_value) = extract_cookie(headers, CHALLENGE_COOKIE_NAME) else {
            return false;
        };

        let mut parts = cookie_value.split(':');
        let Some(window_raw) = parts.next() else {
            return false;
        };
        let Some(sig) = parts.next() else {
            return false;
        };
        if parts.next().is_some() {
            return false;
        }

        let Ok(window) = window_raw.parse::<u64>() else {
            return false;
        };

        let current = current_window(self.settings.challenge_ttl);
        if window > current || current.saturating_sub(window) > 1 {
            return false;
        }

        let ua_hash = user_agent_hash(headers);
        let expected_sig = self.sign_challenge(client_ip, &ua_hash, window);
        constant_time_eq(expected_sig.as_bytes(), sig.as_bytes())
    }

    fn sign_challenge(&self, client_ip: IpAddr, ua_hash: &str, window: u64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.settings.challenge_secret)
            .expect("HMAC accepts any key length");
        mac.update(format!("{client_ip}|{ua_hash}|{window}").as_bytes());
        let bytes = mac.finalize().into_bytes();
        hex::encode(bytes)
    }

    async fn record(&self, scope: &str, decision: &AbuseDecision) {
        let action = match decision {
            AbuseDecision::Allow => {
                self.telemetry.allowed_total.fetch_add(1, Ordering::Relaxed);
                "allow"
            }
            AbuseDecision::Challenge { .. } => {
                self.telemetry
                    .challenged_total
                    .fetch_add(1, Ordering::Relaxed);
                "challenge"
            }
            AbuseDecision::Block { .. } => {
                self.telemetry.blocked_total.fetch_add(1, Ordering::Relaxed);
                "block"
            }
        };

        metrics::ABUSE_DECISION.with_label_values(&[scope, action]).inc();

        let mut lock = self.telemetry.scope_counters.write().await;
        let item = lock.entry(scope.to_owned()).or_default();
        match decision {
            AbuseDecision::Allow => item.allowed += 1,
            AbuseDecision::Challenge { .. } => item.challenged += 1,
            AbuseDecision::Block { .. } => item.blocked += 1,
        }
    }

    async fn incr_counter(
        &self,
        key: &str,
        window: Duration,
    ) -> Result<(u64, u64), anyhow::Error> {
        match &self.backend {
            Backend::Local { state } => Ok(state.incr_counter(key, window).await),
            Backend::Redis { client, fallback } => match redis_incr_counter(client, key, window).await {
                Ok(item) => Ok(item),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "redis counter update failed; falling back to local abuse state"
                    );
                    Ok(fallback.incr_counter(key, window).await)
                }
            },
        }
    }

    async fn incr_strike(&self, actor_key: &str) -> Result<u64, anyhow::Error> {
        match &self.backend {
            Backend::Local { state } => Ok(state.incr_strike(actor_key).await),
            Backend::Redis { client, fallback } => match redis_incr_strike(client, actor_key).await {
                Ok(strikes) => Ok(strikes),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "redis strike update failed; falling back to local abuse state"
                    );
                    Ok(fallback.incr_strike(actor_key).await)
                }
            },
        }
    }

    async fn set_ban(&self, actor_key: &str, duration: Duration) -> Result<(), anyhow::Error> {
        match &self.backend {
            Backend::Local { state } => {
                state.set_ban(actor_key, duration).await;
                Ok(())
            }
            Backend::Redis { client, fallback } => match redis_set_ban(client, actor_key, duration).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "redis set ban failed; falling back to local abuse state"
                    );
                    fallback.set_ban(actor_key, duration).await;
                    Ok(())
                }
            },
        }
    }

    async fn ban_ttl_secs(&self, actor_key: &str) -> Option<u64> {
        match &self.backend {
            Backend::Local { state } => state.ban_ttl_secs(actor_key).await,
            Backend::Redis { client, fallback } => match redis_ban_ttl_secs(client, actor_key).await {
                Ok(ttl) => ttl,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "redis ban lookup failed; falling back to local abuse state"
                    );
                    fallback.ban_ttl_secs(actor_key).await
                }
            },
        }
    }
}

impl AbuseSettings {
    fn from_config(config: &Config) -> Self {
        let allowlist_cidrs = parse_allowlist(&config.abuse_allowlist_cidrs);
        let bypass_header = config
            .abuse_bypass_header
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>();
        let bypass_header = if bypass_header.is_empty() {
            DEFAULT_BYPASS_HEADER.to_owned()
        } else {
            bypass_header
        };

        let limit_overrides = parse_limit_overrides(config.abuse_limit_overrides_json.as_deref());

        let mut hasher = Sha256::new();
        hasher.update(config.master_key.as_bytes());
        hasher.update(b"rift-abuse-challenge");

        Self {
            allowlist_cidrs,
            bypass_header,
            bypass_token: config.abuse_bypass_token.clone(),
            challenge_ttl: Duration::from_secs(config.abuse_challenge_ttl_secs.max(60)),
            bot_verify_enabled: config.abuse_bot_verify,
            bot_verify_cache_ttl: Duration::from_secs(config.abuse_bot_verify_cache_secs.max(60)),
            limit_overrides,
            challenge_secret: hasher.finalize().to_vec(),
        }
    }
}

impl LocalState {
    async fn incr_counter(&self, key: &str, window: Duration) -> (u64, u64) {
        let mut lock = self.counters.write().await;
        let now = Instant::now();

        let entry = lock.entry(key.to_owned()).or_insert(LocalCounter {
            started_at: now,
            count: 0,
            window,
        });

        if now.duration_since(entry.started_at) >= entry.window {
            *entry = LocalCounter {
                started_at: now,
                count: 0,
                window,
            };
        }

        entry.count += 1;
        let elapsed = now.duration_since(entry.started_at);
        let remaining = entry.window.saturating_sub(elapsed).as_secs().max(1);
        (entry.count, remaining)
    }

    async fn ban_ttl_secs(&self, actor_key: &str) -> Option<u64> {
        let mut lock = self.bans.write().await;
        let now = Instant::now();

        if let Some(item) = lock.get(actor_key).copied() {
            if item.expires_at > now {
                return Some(item.expires_at.duration_since(now).as_secs().max(1));
            }
            lock.remove(actor_key);
        }
        None
    }

    async fn set_ban(&self, actor_key: &str, duration: Duration) {
        let mut lock = self.bans.write().await;
        lock.insert(
            actor_key.to_owned(),
            LocalBan {
                expires_at: Instant::now() + duration,
            },
        );
    }

    async fn incr_strike(&self, actor_key: &str) -> u64 {
        let mut lock = self.strikes.write().await;
        let now = Instant::now();

        let entry = lock.entry(actor_key.to_owned()).or_insert(LocalStrike {
            started_at: now,
            count: 0,
        });

        if now.duration_since(entry.started_at) >= STRIKE_WINDOW {
            *entry = LocalStrike {
                started_at: now,
                count: 0,
            };
        }

        entry.count += 1;
        entry.count
    }

    async fn active_bans(&self) -> usize {
        let mut lock = self.bans.write().await;
        let now = Instant::now();
        lock.retain(|_, value| value.expires_at > now);
        lock.len()
    }
}

impl CrawlerVerifier {
    async fn verify(&self, client_ip: IpAddr, user_agent: &str) -> bool {
        let ua = user_agent.to_ascii_lowercase();
        let Some((cache_key, suffixes)) = crawler_rules(&ua, client_ip) else {
            return false;
        };

        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&cache_key) {
                if entry.expires_at > Instant::now() {
                    return entry.verified;
                }
            }
        }

        let verified = self.verify_ptr_forward(client_ip, suffixes).await;
        let mut cache = self.cache.write().await;
        cache.insert(
            cache_key,
            BotCacheEntry {
                verified,
                expires_at: Instant::now() + self.ttl,
            },
        );
        verified
    }

    async fn verify_ptr_forward(&self, client_ip: IpAddr, valid_suffixes: &[&str]) -> bool {
        let resolver = match Resolver::builder_tokio() {
            Ok(builder) => builder.build(),
            Err(_) => return false,
        };

        let reverse = match resolver.reverse_lookup(client_ip).await {
            Ok(result) => result,
            Err(_) => return false,
        };

        for name in reverse.iter() {
            let host = name.to_utf8().to_ascii_lowercase();
            if !valid_suffixes.iter().any(|suffix| host.ends_with(suffix)) {
                continue;
            }

            let forward = match resolver.lookup_ip(host.clone()).await {
                Ok(result) => result,
                Err(_) => continue,
            };

            if forward.iter().any(|ip| ip == client_ip) {
                return true;
            }
        }

        false
    }
}

fn parse_allowlist(raw: &str) -> Vec<IpNet> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter_map(|item| {
            item.parse::<IpNet>()
                .or_else(|_| item.parse::<IpAddr>().map(IpNet::from))
                .ok()
        })
        .collect()
}

fn parse_limit_overrides(raw: Option<&str>) -> Vec<LimitOverride> {
    let Some(raw_json) = raw else {
        return Vec::new();
    };

    if raw_json.trim().is_empty() {
        return Vec::new();
    }

    match serde_json::from_str::<Vec<LimitOverride>>(raw_json) {
        Ok(overrides) => overrides,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to parse RIFT_ABUSE_LIMIT_OVERRIDES_JSON; ignoring overrides"
            );
            Vec::new()
        }
    }
}

fn redis_counter_key(key: &str) -> String {
    format!("rift:abuse:counter:{key}")
}

fn redis_ban_key(actor_key: &str) -> String {
    format!("rift:abuse:ban:{actor_key}")
}

fn redis_strike_key(actor_key: &str) -> String {
    format!("rift:abuse:strike:{actor_key}")
}

async fn redis_incr_counter(
    client: &redis::Client,
    key: &str,
    window: Duration,
) -> Result<(u64, u64), anyhow::Error> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let redis_key = redis_counter_key(key);
    let count: u64 = conn.incr(&redis_key, 1u64).await?;
    if count == 1 {
        let _: bool = conn.expire(&redis_key, window.as_secs() as i64).await?;
    }
    let ttl: i64 = conn.ttl(&redis_key).await.unwrap_or(window.as_secs() as i64);
    Ok((count, ttl.max(1) as u64))
}

async fn redis_ban_ttl_secs(
    client: &redis::Client,
    actor_key: &str,
) -> Result<Option<u64>, anyhow::Error> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let key = redis_ban_key(actor_key);
    let exists: bool = conn.exists(&key).await?;
    if !exists {
        return Ok(None);
    }
    let ttl: i64 = conn.ttl(&key).await.unwrap_or(0);
    Ok(Some(ttl.max(1) as u64))
}

async fn redis_set_ban(
    client: &redis::Client,
    actor_key: &str,
    duration: Duration,
) -> Result<(), anyhow::Error> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let key = redis_ban_key(actor_key);
    conn.set_ex::<_, _, ()>(&key, "1", duration.as_secs())
        .await?;
    Ok(())
}

async fn redis_incr_strike(client: &redis::Client, actor_key: &str) -> Result<u64, anyhow::Error> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let key = redis_strike_key(actor_key);
    let count: u64 = conn.incr(&key, 1u64).await?;
    if count == 1 {
        let _: bool = conn.expire(&key, STRIKE_WINDOW.as_secs() as i64).await?;
    }
    Ok(count)
}

fn ban_tier(strikes: u64) -> (Duration, &'static str) {
    match strikes {
        0 | 1 => (Duration::from_secs(5 * 60), "5m"),
        2 => (Duration::from_secs(30 * 60), "30m"),
        _ => (Duration::from_secs(24 * 60 * 60), "24h"),
    }
}

fn user_agent_hash(headers: &HeaderMap) -> String {
    let ua = headers
        .get(http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut hasher = Sha256::new();
    hasher.update(ua.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

fn current_window(ttl: Duration) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now / ttl.as_secs().max(1)
}

fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(http::header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let mut kv = pair.trim().splitn(2, '=');
        let key = kv.next()?.trim();
        let value = kv.next()?.trim();
        if key == name {
            return Some(value.to_owned());
        }
    }
    None
}

fn crawler_rules(user_agent: &str, ip: IpAddr) -> Option<(String, &'static [&'static str])> {
    let rules: &[(&str, &[&str])] = &[
        ("googlebot", &[".googlebot.com", ".google.com"]),
        ("bingbot", &[".search.msn.com"]),
        ("duckduckbot", &[".duckduckgo.com"]),
        ("yandexbot", &[".yandex.com", ".yandex.net", ".yandex.ru"]),
    ];

    for (needle, suffixes) in rules {
        if user_agent.contains(needle) {
            return Some((format!("{needle}:{ip}"), suffixes));
        }
    }
    None
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[tokio::test]
    async fn challenge_then_block_flow() {
        let guard = AbuseGuard::local_for_tests();
        let actor = "ip:127.0.0.1".to_owned();

        let allow = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: actor.clone(),
                bucket_key: "scope:proxy.global_ip:test".to_owned(),
                limit: 3,
                window: Duration::from_secs(60),
                challenge_after: Some(1),
            })
            .await
            .unwrap();
        assert!(matches!(allow, AbuseDecision::Allow));

        let challenged = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: actor.clone(),
                bucket_key: "scope:proxy.global_ip:test".to_owned(),
                limit: 3,
                window: Duration::from_secs(60),
                challenge_after: Some(1),
            })
            .await
            .unwrap();
        assert!(matches!(challenged, AbuseDecision::Challenge { .. }));

        let _ = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: actor.clone(),
                bucket_key: "scope:proxy.global_ip:test".to_owned(),
                limit: 1,
                window: Duration::from_secs(60),
                challenge_after: None,
            })
            .await
            .unwrap();

        let blocked = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: actor,
                bucket_key: "scope:proxy.global_ip:test".to_owned(),
                limit: 1,
                window: Duration::from_secs(60),
                challenge_after: None,
            })
            .await
            .unwrap();

        assert!(matches!(blocked, AbuseDecision::Block { .. }));
    }

    #[test]
    fn parses_allowlist_and_overrides() {
        let allowlist = parse_allowlist("127.0.0.1,10.0.0.0/8,invalid");
        assert_eq!(allowlist.len(), 2);

        let overrides = parse_limit_overrides(Some(
            r#"[{"scope":"proxy.global_ip","limit":10,"window_secs":5,"enabled":true}]"#,
        ));
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].scope, "proxy.global_ip");
    }

    #[tokio::test]
    async fn redis_backend_persists_ban_when_configured() {
        let Ok(redis_url) = std::env::var("TEST_REDIS_URL") else {
            return;
        };

        let guard = AbuseGuard::redis_for_tests(&redis_url).unwrap();
        let actor = format!("ip:test-{}", Uuid::new_v4());
        let bucket = format!("scope:test:{}", Uuid::new_v4());

        let _ = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: actor.clone(),
                bucket_key: bucket.clone(),
                limit: 0,
                window: Duration::from_secs(30),
                challenge_after: None,
            })
            .await
            .unwrap();

        let decision = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: actor.clone(),
                bucket_key: bucket,
                limit: 100,
                window: Duration::from_secs(30),
                challenge_after: None,
            })
            .await
            .unwrap();

        assert!(matches!(decision, AbuseDecision::Block { .. }));
    }

    #[tokio::test]
    async fn redis_fallback_works_for_unreachable_backend() {
        let mut guard = AbuseGuard::local_for_tests();
        let bad_client = redis::Client::open("redis://127.0.0.1:1").unwrap();
        guard.backend = Backend::Redis {
            client: bad_client,
            fallback: Arc::new(LocalState::default()),
        };

        let decision = guard
            .enforce(AbuseLimit {
                scope: "proxy.global_ip",
                actor_key: "ip:127.0.0.1".to_owned(),
                bucket_key: "scope:proxy.global_ip:fallback".to_owned(),
                limit: 10,
                window: Duration::from_secs(30),
                challenge_after: None,
            })
            .await
            .unwrap();

        assert!(matches!(decision, AbuseDecision::Allow));
    }

    #[test]
    fn allowlist_and_bypass_token_are_trusted() {
        let mut guard = AbuseGuard::local_for_tests();
        let settings = AbuseSettings {
            allowlist_cidrs: vec!["10.0.0.0/8".parse().unwrap()],
            bypass_header: "x-rift-abuse-bypass".to_owned(),
            bypass_token: Some("secret-token".to_owned()),
            challenge_ttl: Duration::from_secs(900),
            bot_verify_enabled: false,
            bot_verify_cache_ttl: Duration::from_secs(600),
            limit_overrides: Vec::new(),
            challenge_secret: vec![1; 32],
        };
        guard.settings = Arc::new(settings);

        let headers = HeaderMap::new();
        assert!(guard.is_trusted_request("10.1.1.1".parse().unwrap(), &headers));

        let mut token_headers = HeaderMap::new();
        token_headers.insert(
            "x-rift-abuse-bypass",
            HeaderValue::from_static("secret-token"),
        );
        assert!(guard.is_trusted_request("192.0.2.3".parse().unwrap(), &token_headers));
    }

    #[test]
    fn challenge_cookie_round_trip_validates() {
        let guard = AbuseGuard::local_for_tests();
        let ip: IpAddr = "203.0.113.8".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0"),
        );

        let set_cookie = guard.build_challenge_set_cookie(ip, &headers, true);
        let value = set_cookie
            .split(';')
            .next()
            .unwrap()
            .trim_start_matches("rift_abuse_challenge=");
        let mut request_headers = HeaderMap::new();
        request_headers.insert(
            http::header::COOKIE,
            HeaderValue::from_str(&format!("rift_abuse_challenge={value}")).unwrap(),
        );
        request_headers.insert(
            http::header::USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0"),
        );

        assert!(guard.has_valid_challenge_cookie(ip, &request_headers));
    }

    #[test]
    fn project_override_wins_over_global_default() {
        let mut guard = AbuseGuard::local_for_tests();
        let project_id = Uuid::new_v4();
        let settings = AbuseSettings {
            allowlist_cidrs: Vec::new(),
            bypass_header: DEFAULT_BYPASS_HEADER.to_owned(),
            bypass_token: None,
            challenge_ttl: Duration::from_secs(900),
            bot_verify_enabled: false,
            bot_verify_cache_ttl: Duration::from_secs(600),
            limit_overrides: vec![LimitOverride {
                scope: "proxy.project_ip".to_owned(),
                project_id: Some(project_id),
                limit: Some(999),
                window_secs: Some(22),
                challenge_after: Some(777),
                enabled: Some(true),
            }],
            challenge_secret: vec![7; 32],
        };
        guard.settings = Arc::new(settings);

        let resolved = guard.resolve_limit(
            "proxy.project_ip",
            Some(project_id),
            10,
            Duration::from_secs(10),
            Some(5),
        );
        assert_eq!(resolved.limit, 999);
        assert_eq!(resolved.window, Duration::from_secs(22));
        assert_eq!(resolved.challenge_after, Some(777));
    }
}
