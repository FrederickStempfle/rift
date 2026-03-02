//! Edge WAF rule engine.
//!
//! Evaluates Layer-7 rules against incoming requests at the proxy edge.
//! Supports global (project_id = None) and per-project rules with
//! deterministic precedence: global first, then project, ordered by
//! priority (ascending) then creation time. The first terminal action
//! wins (`allow`, `challenge`, `block`); `log` is non-terminal.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use ipnet::IpNet;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Terminal or non-terminal action a WAF rule can prescribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WafAction {
    Allow,
    Challenge,
    Block,
    Log,
}

impl WafAction {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Allow | Self::Challenge | Self::Block)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Challenge => "challenge",
            Self::Block => "block",
            Self::Log => "log",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "challenge" => Some(Self::Challenge),
            "block" => Some(Self::Block),
            "log" => Some(Self::Log),
            _ => None,
        }
    }
}

/// Matcher field type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WafMatchField {
    Ip,
    Method,
    Host,
    Path,
    Query,
    UserAgent,
    Header,
}

impl WafMatchField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Method => "method",
            Self::Host => "host",
            Self::Path => "path",
            Self::Query => "query",
            Self::UserAgent => "user_agent",
            Self::Header => "header",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "ip" => Some(Self::Ip),
            "method" => Some(Self::Method),
            "host" => Some(Self::Host),
            "path" => Some(Self::Path),
            "query" => Some(Self::Query),
            "user_agent" => Some(Self::UserAgent),
            "header" => Some(Self::Header),
            _ => None,
        }
    }
}

/// Match operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WafMatchOp {
    Exact,
    Prefix,
    Contains,
    Regex,
    Cidr,
}

impl WafMatchOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Contains => "contains",
            Self::Regex => "regex",
            Self::Cidr => "cidr",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(Self::Exact),
            "prefix" => Some(Self::Prefix),
            "contains" => Some(Self::Contains),
            "regex" => Some(Self::Regex),
            "cidr" => Some(Self::Cidr),
            _ => None,
        }
    }
}

/// Request context fed into the WAF evaluator.
pub struct WafRequestContext {
    pub client_ip: IpAddr,
    /// Pre-formatted IP string so field_value can return a &str for non-CIDR operators.
    pub client_ip_str: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub query: String,
    pub user_agent: String,
    pub headers: Vec<(String, String)>,
}

impl WafRequestContext {
    fn field_value(&self, field: WafMatchField, header_name: Option<&str>) -> &str {
        match field {
            WafMatchField::Ip => &self.client_ip_str,
            WafMatchField::Method => &self.method,
            WafMatchField::Host => &self.host,
            WafMatchField::Path => &self.path,
            WafMatchField::Query => &self.query,
            WafMatchField::UserAgent => &self.user_agent,
            WafMatchField::Header => {
                if let Some(name) = header_name {
                    let lower = name.to_ascii_lowercase();
                    for (k, v) in &self.headers {
                        if k.to_ascii_lowercase() == lower {
                            return v.as_str();
                        }
                    }
                }
                ""
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compiled rule
// ---------------------------------------------------------------------------

/// A single WAF rule condition (one matcher).
#[derive(Debug, Clone)]
pub struct CompiledCondition {
    pub field: WafMatchField,
    pub op: WafMatchOp,
    /// For header matching, the header name to inspect.
    pub header_name: Option<String>,
    /// Pre-compiled matcher state.
    pub matcher: ConditionMatcher,
}

/// Pre-compiled match data so we avoid re-parsing on every request.
#[derive(Debug, Clone)]
pub enum ConditionMatcher {
    Exact(String),
    Prefix(String),
    Contains(String),
    Regex(Regex),
    Cidr(IpNet),
}

impl CompiledCondition {
    /// Compile a condition from raw strings. Returns None on invalid input.
    pub fn compile(
        field: WafMatchField,
        op: WafMatchOp,
        value: &str,
        header_name: Option<String>,
    ) -> Option<Self> {
        let matcher = match op {
            WafMatchOp::Exact => ConditionMatcher::Exact(value.to_owned()),
            WafMatchOp::Prefix => ConditionMatcher::Prefix(value.to_owned()),
            WafMatchOp::Contains => ConditionMatcher::Contains(value.to_owned()),
            WafMatchOp::Regex => {
                let re = Regex::new(value).ok()?;
                ConditionMatcher::Regex(re)
            }
            WafMatchOp::Cidr => {
                let net = value
                    .parse::<IpNet>()
                    .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
                    .ok()?;
                ConditionMatcher::Cidr(net)
            }
        };
        Some(Self {
            field,
            op,
            header_name,
            matcher,
        })
    }

    /// Evaluate against a request context.
    fn matches(&self, ctx: &WafRequestContext) -> bool {
        match &self.matcher {
            ConditionMatcher::Cidr(net) => net.contains(&ctx.client_ip),
            ConditionMatcher::Exact(val) => {
                ctx.field_value(self.field, self.header_name.as_deref()) == val.as_str()
            }
            ConditionMatcher::Prefix(val) => ctx
                .field_value(self.field, self.header_name.as_deref())
                .starts_with(val.as_str()),
            ConditionMatcher::Contains(val) => ctx
                .field_value(self.field, self.header_name.as_deref())
                .contains(val.as_str()),
            ConditionMatcher::Regex(re) => {
                re.is_match(ctx.field_value(self.field, self.header_name.as_deref()))
            }
        }
    }
}

/// A fully compiled WAF rule ready for evaluation.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub id: Uuid,
    pub name: String,
    pub action: WafAction,
    pub priority: i32,
    pub conditions: Vec<CompiledCondition>,
    pub is_managed: bool,
}

impl CompiledRule {
    /// A rule matches when ALL conditions match (AND logic).
    pub fn matches(&self, ctx: &WafRequestContext) -> bool {
        !self.conditions.is_empty() && self.conditions.iter().all(|c| c.matches(ctx))
    }
}

// ---------------------------------------------------------------------------
// WAF decision
// ---------------------------------------------------------------------------

/// The result of WAF evaluation.
#[derive(Debug, Clone)]
pub struct WafDecision {
    pub action: WafAction,
    pub matched_rule_id: Option<Uuid>,
    pub matched_rule_name: Option<String>,
    /// Rules that matched with `log` action (non-terminal).
    pub logged_rules: Vec<(Uuid, String)>,
}

impl WafDecision {
    pub fn allow() -> Self {
        Self {
            action: WafAction::Allow,
            matched_rule_id: None,
            matched_rule_name: None,
            logged_rules: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Rule evaluation
// ---------------------------------------------------------------------------

/// Evaluate an ordered list of compiled rules against a request context.
/// Rules must be pre-sorted by scope (global first), then priority ASC,
/// then created_at ASC. First terminal action wins.
pub fn evaluate_rules(rules: &[CompiledRule], ctx: &WafRequestContext) -> WafDecision {
    let mut logged = Vec::new();

    for rule in rules {
        if rule.matches(ctx) {
            if rule.action == WafAction::Log {
                logged.push((rule.id, rule.name.clone()));
                continue;
            }
            // Terminal action
            return WafDecision {
                action: rule.action,
                matched_rule_id: Some(rule.id),
                matched_rule_name: Some(rule.name.clone()),
                logged_rules: logged,
            };
        }
    }

    // No terminal rule matched — implicit allow
    WafDecision {
        action: WafAction::Allow,
        matched_rule_id: None,
        matched_rule_name: None,
        logged_rules: logged,
    }
}

// ---------------------------------------------------------------------------
// WAF cache
// ---------------------------------------------------------------------------

const WAF_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct WafCacheEntry {
    rules: Arc<Vec<CompiledRule>>,
    fetched_at: Instant,
}

/// Scope key: `None` for global, `Some(project_id)` for per-project.
type ScopeKey = Option<Uuid>;

/// Caches compiled WAF rules per scope with TTL and explicit invalidation.
/// Also owns the batched event writer channel.
#[derive(Clone)]
pub struct WafCache {
    cache: Arc<RwLock<HashMap<ScopeKey, WafCacheEntry>>>,
    event_tx: mpsc::Sender<crate::db::waf::NewWafEvent>,
    dropped_events: Arc<AtomicU64>,
}

impl WafCache {
    pub fn new(pool: PgPool) -> Self {
        let (tx, rx) = mpsc::channel(8_192);
        tokio::spawn(waf_event_flush_loop(rx, pool));
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            event_tx: tx,
            dropped_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Send a WAF event to the batched writer. Non-blocking; drops on backpressure.
    pub fn record_event(&self, event: crate::db::waf::NewWafEvent) {
        if self.event_tx.try_send(event).is_err() {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Get compiled rules for a scope, fetching from DB if expired/missing.
    pub async fn get_rules(
        &self,
        pool: &PgPool,
        scope: ScopeKey,
    ) -> Result<Arc<Vec<CompiledRule>>, AppError> {
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&scope) {
                if entry.fetched_at.elapsed() < WAF_CACHE_TTL {
                    return Ok(Arc::clone(&entry.rules));
                }
            }
        }
        self.fetch_and_cache(pool, scope).await
    }

    async fn fetch_and_cache(
        &self,
        pool: &PgPool,
        scope: ScopeKey,
    ) -> Result<Arc<Vec<CompiledRule>>, AppError> {
        let db_rules = crate::db::waf::list_enabled_rules(pool, scope).await?;
        let compiled: Vec<CompiledRule> =
            db_rules.into_iter().filter_map(compile_db_rule).collect();
        let rules = Arc::new(compiled);
        let entry = WafCacheEntry {
            rules: Arc::clone(&rules),
            fetched_at: Instant::now(),
        };
        self.cache.write().await.insert(scope, entry);
        Ok(rules)
    }

    /// Invalidate a specific scope (or global if None).
    pub async fn invalidate(&self, scope: ScopeKey) {
        self.cache.write().await.remove(&scope);
    }

    /// Invalidate all cached entries.
    pub async fn invalidate_all(&self) {
        self.cache.write().await.clear();
    }
}

/// Batched WAF event writer. Collects events from the channel and flushes
/// to the database in small batches, avoiding per-request spawn overhead.
async fn waf_event_flush_loop(mut rx: mpsc::Receiver<crate::db::waf::NewWafEvent>, pool: PgPool) {
    let mut buffer: Vec<crate::db::waf::NewWafEvent> = Vec::with_capacity(64);
    let mut interval = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        buffer.push(event);
                        // Drain any immediately available events without waiting
                        while buffer.len() < 256 {
                            match rx.try_recv() {
                                Ok(e) => buffer.push(e),
                                Err(_) => break,
                            }
                        }
                        if buffer.len() >= 64 {
                            flush_waf_events(&pool, &mut buffer).await;
                        }
                    }
                    None => {
                        flush_waf_events(&pool, &mut buffer).await;
                        break;
                    }
                }
            }
            _ = interval.tick() => {
                flush_waf_events(&pool, &mut buffer).await;
            }
        }
    }
}

async fn flush_waf_events(pool: &PgPool, buffer: &mut Vec<crate::db::waf::NewWafEvent>) {
    if buffer.is_empty() {
        return;
    }
    for event in buffer.drain(..) {
        if let Err(e) = crate::db::waf::insert_event(pool, event).await {
            tracing::warn!(error = %e, "failed to write WAF event");
        }
    }
}

/// Compile a DB rule row into a CompiledRule, skipping on parse errors.
fn compile_db_rule(row: crate::db::waf::WafRuleRow) -> Option<CompiledRule> {
    let action = WafAction::from_str_opt(&row.action)?;
    let field = WafMatchField::from_str_opt(&row.match_field)?;
    let op = WafMatchOp::from_str_opt(&row.match_op)?;
    let condition = CompiledCondition::compile(field, op, &row.match_value, row.header_name)?;
    Some(CompiledRule {
        id: row.id,
        name: row.name,
        action,
        priority: row.priority,
        conditions: vec![condition],
        is_managed: row.is_managed,
    })
}

// ---------------------------------------------------------------------------
// Top-level evaluate function used by the proxy handler
// ---------------------------------------------------------------------------

/// Evaluate WAF rules for a request. Checks global rules first, then
/// project rules. Returns the decision and any logged rules.
pub async fn evaluate(
    cache: &WafCache,
    pool: &PgPool,
    project_id: Option<Uuid>,
    ctx: &WafRequestContext,
) -> Result<WafDecision, AppError> {
    // Global rules
    let global_rules = cache.get_rules(pool, None).await?;
    let global_decision = evaluate_rules(&global_rules, ctx);
    if global_decision.action != WafAction::Allow {
        return Ok(global_decision);
    }

    // Project-scoped rules (if we have a project)
    if let Some(pid) = project_id {
        let project_rules = cache.get_rules(pool, Some(pid)).await?;
        let mut project_decision = evaluate_rules(&project_rules, ctx);
        // Merge logged rules from global pass
        let mut all_logged = global_decision.logged_rules;
        all_logged.append(&mut project_decision.logged_rules);
        project_decision.logged_rules = all_logged;
        return Ok(project_decision);
    }

    Ok(global_decision)
}

// ---------------------------------------------------------------------------
// Baseline managed rules
// ---------------------------------------------------------------------------

/// Built-in managed rule definitions for common scanner/exploit probes.
pub fn baseline_managed_rules() -> Vec<ManagedRuleDef> {
    vec![
        // Path traversal probes
        ManagedRuleDef {
            name: "path-traversal-encoded",
            description: "Blocks URL-encoded path traversal sequences",
            field: WafMatchField::Path,
            op: WafMatchOp::Regex,
            value: r"(?i)(\.\./|%2e%2e[/%]|%252e%252e|\.\.%2f|%00)",
            action: WafAction::Block,
            priority: 10,
        },
        // Sensitive file probes
        ManagedRuleDef {
            name: "sensitive-dotenv",
            description: "Blocks access to .env files",
            field: WafMatchField::Path,
            op: WafMatchOp::Regex,
            value: r"(?i)/\.env(\.[a-z]+)?$",
            action: WafAction::Block,
            priority: 20,
        },
        ManagedRuleDef {
            name: "sensitive-git-config",
            description: "Blocks access to .git directory",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/.git/",
            action: WafAction::Block,
            priority: 20,
        },
        ManagedRuleDef {
            name: "sensitive-aws-credentials",
            description: "Blocks access to .aws credentials",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/.aws/",
            action: WafAction::Block,
            priority: 20,
        },
        ManagedRuleDef {
            name: "sensitive-ssh-keys",
            description: "Blocks access to .ssh directory",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/.ssh/",
            action: WafAction::Block,
            priority: 20,
        },
        // CMS exploit probes
        ManagedRuleDef {
            name: "cms-wp-login",
            description: "Blocks WordPress login probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Exact,
            value: "/wp-login.php",
            action: WafAction::Block,
            priority: 30,
        },
        ManagedRuleDef {
            name: "cms-xmlrpc",
            description: "Blocks XML-RPC probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Exact,
            value: "/xmlrpc.php",
            action: WafAction::Block,
            priority: 30,
        },
        ManagedRuleDef {
            name: "cms-wp-admin",
            description: "Blocks WordPress admin probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/wp-admin/",
            action: WafAction::Block,
            priority: 30,
        },
        ManagedRuleDef {
            name: "cms-wp-includes",
            description: "Blocks WordPress includes probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/wp-includes/",
            action: WafAction::Block,
            priority: 30,
        },
        ManagedRuleDef {
            name: "cms-wp-content",
            description: "Blocks WordPress content probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/wp-content/",
            action: WafAction::Block,
            priority: 30,
        },
        // Admin and scanner probes
        ManagedRuleDef {
            name: "scanner-phpmyadmin",
            description: "Blocks phpMyAdmin probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Regex,
            value: r"(?i)/phpmyadmin",
            action: WafAction::Block,
            priority: 40,
        },
        ManagedRuleDef {
            name: "scanner-cgi-bin",
            description: "Blocks CGI-BIN probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/cgi-bin/",
            action: WafAction::Block,
            priority: 40,
        },
        ManagedRuleDef {
            name: "scanner-vendor-phpunit",
            description: "Blocks PHPUnit vendor probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Contains,
            value: "/vendor/phpunit/",
            action: WafAction::Block,
            priority: 40,
        },
        ManagedRuleDef {
            name: "scanner-boaform",
            description: "Blocks Boa router exploit probes",
            field: WafMatchField::Path,
            op: WafMatchOp::Prefix,
            value: "/boaform/",
            action: WafAction::Block,
            priority: 40,
        },
        // SQL injection in query string
        ManagedRuleDef {
            name: "sqli-query-basic",
            description: "Blocks common SQL injection patterns in query strings",
            field: WafMatchField::Query,
            op: WafMatchOp::Regex,
            value: r"(?i)(union\s+select|;\s*drop\s+|;\s*delete\s+|'\s*or\s+'1'\s*=\s*'1|--\s*$)",
            action: WafAction::Block,
            priority: 15,
        },
        // XSS in path/query
        ManagedRuleDef {
            name: "xss-script-tag",
            description: "Blocks script tag injection attempts",
            field: WafMatchField::Path,
            op: WafMatchOp::Regex,
            value: r"(?i)<script[\s>]",
            action: WafAction::Block,
            priority: 15,
        },
    ]
}

/// Definition of a managed baseline rule.
pub struct ManagedRuleDef {
    pub name: &'static str,
    pub description: &'static str,
    pub field: WafMatchField,
    pub op: WafMatchOp,
    pub value: &'static str,
    pub action: WafAction,
    pub priority: i32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_ctx() -> WafRequestContext {
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 42));
        WafRequestContext {
            client_ip: ip,
            client_ip_str: ip.to_string(),
            method: "GET".into(),
            host: "myapp.rift.dev".into(),
            path: "/api/data".into(),
            query: "page=1".into(),
            user_agent: "Mozilla/5.0".into(),
            headers: vec![("x-custom".into(), "test-value".into())],
        }
    }

    fn test_ctx_with(ip: IpAddr, path: &str, query: &str, ua: &str) -> WafRequestContext {
        WafRequestContext {
            client_ip: ip,
            client_ip_str: ip.to_string(),
            method: "GET".into(),
            host: "test.rift.dev".into(),
            path: path.into(),
            query: query.into(),
            user_agent: ua.into(),
            headers: Vec::new(),
        }
    }

    fn make_rule(
        name: &str,
        field: WafMatchField,
        op: WafMatchOp,
        value: &str,
        action: WafAction,
        priority: i32,
    ) -> CompiledRule {
        CompiledRule {
            id: Uuid::new_v4(),
            name: name.into(),
            action,
            priority,
            conditions: vec![CompiledCondition::compile(field, op, value, None).unwrap()],
            is_managed: false,
        }
    }

    #[test]
    fn exact_match_works() {
        let rule = make_rule(
            "test",
            WafMatchField::Path,
            WafMatchOp::Exact,
            "/api/data",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));

        let rule2 = make_rule(
            "test",
            WafMatchField::Path,
            WafMatchOp::Exact,
            "/other",
            WafAction::Block,
            1,
        );
        assert!(!rule2.matches(&test_ctx()));
    }

    #[test]
    fn prefix_match_works() {
        let rule = make_rule(
            "test",
            WafMatchField::Path,
            WafMatchOp::Prefix,
            "/api/",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));

        let rule2 = make_rule(
            "test",
            WafMatchField::Path,
            WafMatchOp::Prefix,
            "/other/",
            WafAction::Block,
            1,
        );
        assert!(!rule2.matches(&test_ctx()));
    }

    #[test]
    fn contains_match_works() {
        let rule = make_rule(
            "test",
            WafMatchField::UserAgent,
            WafMatchOp::Contains,
            "Mozilla",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));
    }

    #[test]
    fn regex_match_works() {
        let rule = make_rule(
            "test",
            WafMatchField::Path,
            WafMatchOp::Regex,
            r"^/api/.*$",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));

        let rule2 = make_rule(
            "test",
            WafMatchField::Path,
            WafMatchOp::Regex,
            r"^/admin/.*$",
            WafAction::Block,
            1,
        );
        assert!(!rule2.matches(&test_ctx()));
    }

    #[test]
    fn cidr_match_works() {
        let rule = make_rule(
            "test",
            WafMatchField::Ip,
            WafMatchOp::Cidr,
            "203.0.113.0/24",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));

        let rule2 = make_rule(
            "test",
            WafMatchField::Ip,
            WafMatchOp::Cidr,
            "10.0.0.0/8",
            WafAction::Block,
            1,
        );
        assert!(!rule2.matches(&test_ctx()));
    }

    #[test]
    fn single_ip_cidr_match() {
        let rule = make_rule(
            "test",
            WafMatchField::Ip,
            WafMatchOp::Cidr,
            "203.0.113.42",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));
    }

    #[test]
    fn header_match_works() {
        let condition = CompiledCondition::compile(
            WafMatchField::Header,
            WafMatchOp::Exact,
            "test-value",
            Some("x-custom".into()),
        )
        .unwrap();
        let rule = CompiledRule {
            id: Uuid::new_v4(),
            name: "header-test".into(),
            action: WafAction::Block,
            priority: 1,
            conditions: vec![condition],
            is_managed: false,
        };
        assert!(rule.matches(&test_ctx()));
    }

    #[test]
    fn first_terminal_wins() {
        let rules = vec![
            make_rule(
                "log1",
                WafMatchField::Path,
                WafMatchOp::Prefix,
                "/api/",
                WafAction::Log,
                1,
            ),
            make_rule(
                "challenge1",
                WafMatchField::Path,
                WafMatchOp::Prefix,
                "/api/",
                WafAction::Challenge,
                2,
            ),
            make_rule(
                "block1",
                WafMatchField::Path,
                WafMatchOp::Prefix,
                "/api/",
                WafAction::Block,
                3,
            ),
        ];
        let decision = evaluate_rules(&rules, &test_ctx());
        assert_eq!(decision.action, WafAction::Challenge);
        assert_eq!(decision.logged_rules.len(), 1);
    }

    #[test]
    fn no_match_allows() {
        let rules = vec![make_rule(
            "block1",
            WafMatchField::Path,
            WafMatchOp::Exact,
            "/admin",
            WafAction::Block,
            1,
        )];
        let decision = evaluate_rules(&rules, &test_ctx());
        assert_eq!(decision.action, WafAction::Allow);
    }

    #[test]
    fn log_only_allows() {
        let rules = vec![make_rule(
            "log1",
            WafMatchField::Path,
            WafMatchOp::Prefix,
            "/api/",
            WafAction::Log,
            1,
        )];
        let decision = evaluate_rules(&rules, &test_ctx());
        assert_eq!(decision.action, WafAction::Allow);
        assert_eq!(decision.logged_rules.len(), 1);
    }

    #[test]
    fn allow_rule_terminates() {
        let rules = vec![
            make_rule(
                "allow1",
                WafMatchField::Path,
                WafMatchOp::Prefix,
                "/api/",
                WafAction::Allow,
                1,
            ),
            make_rule(
                "block1",
                WafMatchField::Path,
                WafMatchOp::Prefix,
                "/api/",
                WafAction::Block,
                2,
            ),
        ];
        let decision = evaluate_rules(&rules, &test_ctx());
        assert_eq!(decision.action, WafAction::Allow);
        assert!(decision.matched_rule_id.is_some());
    }

    #[test]
    fn empty_rules_allow() {
        let decision = evaluate_rules(&[], &test_ctx());
        assert_eq!(decision.action, WafAction::Allow);
    }

    #[test]
    fn invalid_regex_returns_none() {
        let result =
            CompiledCondition::compile(WafMatchField::Path, WafMatchOp::Regex, "[invalid", None);
        assert!(result.is_none());
    }

    #[test]
    fn baseline_rules_compile() {
        for def in baseline_managed_rules() {
            let cond = CompiledCondition::compile(def.field, def.op, def.value, None);
            assert!(
                cond.is_some(),
                "baseline rule '{}' failed to compile",
                def.name
            );
        }
    }

    #[test]
    fn baseline_blocks_dotenv() {
        let defs = baseline_managed_rules();
        let rules: Vec<CompiledRule> = defs
            .into_iter()
            .filter_map(|def| {
                let cond = CompiledCondition::compile(def.field, def.op, def.value, None)?;
                Some(CompiledRule {
                    id: Uuid::new_v4(),
                    name: def.name.into(),
                    action: def.action,
                    priority: def.priority,
                    conditions: vec![cond],
                    is_managed: true,
                })
            })
            .collect();

        let ctx = test_ctx_with(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), "/.env", "", "");
        let decision = evaluate_rules(&rules, &ctx);
        assert_eq!(decision.action, WafAction::Block);
    }

    #[test]
    fn baseline_blocks_wp_login() {
        let defs = baseline_managed_rules();
        let rules: Vec<CompiledRule> = defs
            .into_iter()
            .filter_map(|def| {
                let cond = CompiledCondition::compile(def.field, def.op, def.value, None)?;
                Some(CompiledRule {
                    id: Uuid::new_v4(),
                    name: def.name.into(),
                    action: def.action,
                    priority: def.priority,
                    conditions: vec![cond],
                    is_managed: true,
                })
            })
            .collect();

        let ctx = test_ctx_with(
            IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)),
            "/wp-login.php",
            "",
            "",
        );
        let decision = evaluate_rules(&rules, &ctx);
        assert_eq!(decision.action, WafAction::Block);
    }

    #[test]
    fn baseline_blocks_path_traversal() {
        let defs = baseline_managed_rules();
        let rules: Vec<CompiledRule> = defs
            .into_iter()
            .filter_map(|def| {
                let cond = CompiledCondition::compile(def.field, def.op, def.value, None)?;
                Some(CompiledRule {
                    id: Uuid::new_v4(),
                    name: def.name.into(),
                    action: def.action,
                    priority: def.priority,
                    conditions: vec![cond],
                    is_managed: true,
                })
            })
            .collect();

        for path in &["/../etc/passwd", "/foo/%2e%2e/bar", "/foo/..%2fbar"] {
            let ctx = test_ctx_with(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), path, "", "");
            let decision = evaluate_rules(&rules, &ctx);
            assert_eq!(
                decision.action,
                WafAction::Block,
                "expected block for path: {path}"
            );
        }
    }

    #[test]
    fn baseline_allows_normal_request() {
        let defs = baseline_managed_rules();
        let rules: Vec<CompiledRule> = defs
            .into_iter()
            .filter_map(|def| {
                let cond = CompiledCondition::compile(def.field, def.op, def.value, None)?;
                Some(CompiledRule {
                    id: Uuid::new_v4(),
                    name: def.name.into(),
                    action: def.action,
                    priority: def.priority,
                    conditions: vec![cond],
                    is_managed: true,
                })
            })
            .collect();

        let ctx = test_ctx_with(
            IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)),
            "/api/users",
            "page=1",
            "Mozilla/5.0",
        );
        let decision = evaluate_rules(&rules, &ctx);
        assert_eq!(decision.action, WafAction::Allow);
    }

    #[test]
    fn ip_exact_match_works() {
        let rule = make_rule(
            "ip-exact",
            WafMatchField::Ip,
            WafMatchOp::Exact,
            "203.0.113.42",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));

        let rule2 = make_rule(
            "ip-exact-miss",
            WafMatchField::Ip,
            WafMatchOp::Exact,
            "10.0.0.1",
            WafAction::Block,
            1,
        );
        assert!(!rule2.matches(&test_ctx()));
    }

    #[test]
    fn ip_regex_match_works() {
        let rule = make_rule(
            "ip-regex",
            WafMatchField::Ip,
            WafMatchOp::Regex,
            r"^203\.0\.113\.\d+$",
            WafAction::Block,
            1,
        );
        assert!(rule.matches(&test_ctx()));
    }
}
