use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use redis::AsyncCommands;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::{config::Config, error::AppError, metrics};

const STRIKE_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

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

impl AbuseDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
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

#[derive(Clone)]
pub struct AbuseGuard {
    backend: Backend,
    telemetry: Arc<Telemetry>,
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

impl AbuseGuard {
    pub fn new(config: &Config) -> Self {
        let telemetry = Arc::new(Telemetry::default());
        let local = Arc::new(LocalState::default());

        if config.state_store == "redis" {
            match redis::Client::open(config.redis_url.clone()) {
                Ok(client) => {
                    return Self {
                        backend: Backend::Redis {
                            client,
                            fallback: local,
                        },
                        telemetry,
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
        }
    }

    pub fn local_for_tests() -> Self {
        Self {
            backend: Backend::Local {
                state: Arc::new(LocalState::default()),
            },
            telemetry: Arc::new(Telemetry::default()),
        }
    }

    pub async fn enforce(&self, limit: AbuseLimit) -> Result<AbuseDecision, AppError> {
        self.telemetry.checked_total.fetch_add(1, Ordering::Relaxed);

        if let Some(retry_after_secs) = self.ban_ttl_secs(&limit.actor_key).await {
            let decision = AbuseDecision::Block {
                retry_after_secs,
                reason: "temporary network ban active".to_owned(),
                tier: "existing".to_owned(),
            };
            self.record(&limit.scope, &decision).await;
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

    async fn record(&self, scope: &str, decision: &AbuseDecision) {
        let action = match decision {
            AbuseDecision::Allow => {
                self.telemetry.allowed_total.fetch_add(1, Ordering::Relaxed);
                "allow"
            }
            AbuseDecision::Challenge { .. } => {
                self.telemetry.challenged_total.fetch_add(1, Ordering::Relaxed);
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
            Backend::Redis { client, fallback } => {
                match redis_incr_counter(client, key, window).await {
                    Ok(item) => Ok(item),
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "redis counter update failed; falling back to local abuse state"
                        );
                        Ok(fallback.incr_counter(key, window).await)
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn escalates_to_ban_after_limit() {
        let guard = AbuseGuard::local_for_tests();
        let actor = "ip:127.0.0.1".to_owned();

        for _ in 0..2 {
            let decision = guard
                .enforce(AbuseLimit {
                    scope: "api.auth.login",
                    actor_key: actor.clone(),
                    bucket_key: "login:127.0.0.1".to_owned(),
                    limit: 1,
                    window: Duration::from_secs(60),
                    challenge_after: None,
                })
                .await
                .unwrap();
            if matches!(decision, AbuseDecision::Block { .. }) {
                return;
            }
        }
        panic!("expected block decision after limit breach");
    }
}
