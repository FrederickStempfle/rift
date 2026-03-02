use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderValue, Method, StatusCode},
    routing::{get, post},
    Json, Router,
};
use ipnet::IpNet;
use serde_json::json;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{
    build::BuildManager,
    config::Config,
    db::DbPool,
    proxy::{
        acme::AcmeChallengeStore, analytics_collector::AnalyticsCollector,
        firewall_cache::FirewallCache, routing_cache::RoutingCache, tls::CertResolver,
        waf::WafCache,
    },
    runtime::backend::RuntimeBackend,
    scheduler::Scheduler,
    services::{
        abuse::AbuseGuard, audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
    ssl::SslManager,
    state::StateStore,
    ws::LogBroadcaster,
};

pub mod access_logs;
pub mod analytics;
pub mod auth;
pub mod deployments;
pub mod domains;
pub mod edge_nodes;
pub mod env_vars;
pub mod firewall;
pub mod logs;
pub mod projects;
pub mod regions;
pub mod releases;
pub mod routing;
pub mod runtime;
pub mod users;
pub mod waf;
pub mod webhooks;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub config: Arc<Config>,
    pub token_service: TokenService,
    pub password_service: PasswordService,
    pub auth_rate_limiters: AuthRateLimiters,
    pub abuse_guard: AbuseGuard,
    pub audit_logger: AuditLogger,
    /// The runtime backend (process-based or pool-based).
    pub runtime_backend: Arc<dyn RuntimeBackend>,
    pub build_manager: BuildManager,
    /// Auto-detected or overridden via RIFT_PUBLIC_IP. Resolved once at startup.
    pub public_ip: Option<String>,
    pub firewall_cache: FirewallCache,
    /// WAF rule cache (compiled rules per scope with TTL).
    pub waf_cache: WafCache,
    pub analytics_collector: AnalyticsCollector,
    pub log_broadcaster: LogBroadcaster,
    pub ssl_manager: SslManager,
    pub challenge_store: AcmeChallengeStore,
    pub cert_resolver: CertResolver,
    /// Hot-path routing cache (host → project_id).
    pub routing_cache: RoutingCache,
    /// Distributed state store (local or Redis-backed).
    pub state_store: Arc<dyn StateStore>,
    /// Scheduler for placement decisions.
    pub scheduler: Arc<Scheduler>,
    /// Trusted proxy CIDRs for forwarded client-IP extraction.
    pub trusted_proxy_cidrs: Arc<Vec<IpNet>>,
    /// Access-log-driven anti-bot detector and mitigation state.
    pub access_bot_guard: crate::proxy::access_bot_guard::AccessBotGuard,
    /// Global in-flight proxy limiter used for overload shedding.
    pub proxy_inflight: Arc<Semaphore>,
    #[cfg(feature = "v8-isolate")]
    pub isolate_pool: Option<crate::runtime::isolate::IsolatePool>,
}

pub fn router(state: AppState) -> Router {
    let mut cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            header::COOKIE,
        ])
        .allow_credentials(true);

    cors = if let Some(origin) = state.config.cors_origin.as_deref() {
        match HeaderValue::from_str(origin) {
            Ok(value) => cors.allow_origin(value),
            Err(_) => cors.allow_origin(Any),
        }
    } else {
        cors.allow_origin(Any)
    };

    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .route("/api/server-info", get(server_info))
        .nest("/api/users", users::routes())
        .route(
            "/api/projects",
            post(projects::create_project).get(projects::list_projects),
        )
        .route(
            "/api/projects/{project_id}",
            get(projects::get_project)
                .patch(projects::update_project)
                .delete(projects::delete_project),
        )
        .route(
            "/api/deployments",
            get(deployments::list_deployments).post(deployments::create_deployment),
        )
        .route(
            "/api/deployments/{deployment_id}/package",
            post(deployments::package_deployment),
        )
        .nest("/api/releases", releases::routes())
        .nest("/api/regions", regions::routes())
        .nest("/api/edge-nodes", edge_nodes::routes())
        .nest("/api/routing", routing::routes())
        .nest("/api/env-vars", env_vars::routes())
        .nest("/api/domains", domains::routes())
        .nest("/api/firewall", firewall::routes())
        .nest("/api/waf", waf::routes())
        .nest("/api/webhooks", webhooks::routes())
        .route("/api/analytics", get(analytics::get_analytics))
        .route("/api/access-logs", get(access_logs::list_access_logs))
        .route("/api/runtime/stats", get(runtime::get_runtime_stats))
        .route("/api/runtime/abuse", get(runtime::get_abuse_stats))
        .route("/api/runtime/project", get(runtime::get_project_runtime))
        .route(
            "/api/projects/{project_id}/stop",
            post(runtime::stop_project),
        )
        .route(
            "/api/projects/{project_id}/suspend",
            post(runtime::suspend_project),
        )
        .route(
            "/api/projects/{project_id}/wake",
            post(runtime::wake_project),
        )
        .route("/api/logs", get(logs::list_logs))
        .route("/api/ws/logs", get(crate::ws::handler::ws_logs_handler))
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(PropagateRequestIdLayer::x_request_id())
                .layer(RequestBodyLimitLayer::new(1_048_576))
                .layer(TraceLayer::new_for_http())
                .layer(cors),
        )
        .fallback(fallback)
        .with_state(state)
}

async fn healthz() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn server_info(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "public_ip": state.public_ip.as_deref().unwrap_or(""),
    }))
}

async fn metrics_handler() -> (
    StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    // Update pool gauges before rendering
    let body = crate::metrics::encode_metrics();
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn fallback() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "route not found" })),
    )
}
