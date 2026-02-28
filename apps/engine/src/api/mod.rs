use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderValue, Method, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
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
    proxy::{analytics_collector::AnalyticsCollector, firewall_cache::FirewallCache},
    runtime::RuntimeManager,
    services::{
        audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
    ws::LogBroadcaster,
};

pub mod analytics;
pub mod auth;
pub mod deployments;
pub mod domains;
pub mod env_vars;
pub mod firewall;
pub mod logs;
pub mod projects;
pub mod users;
pub mod webhooks;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub config: Arc<Config>,
    pub token_service: TokenService,
    pub password_service: PasswordService,
    pub auth_rate_limiters: AuthRateLimiters,
    pub audit_logger: AuditLogger,
    pub runtime_manager: RuntimeManager,
    pub build_manager: BuildManager,
    /// Auto-detected or overridden via RIFT_PUBLIC_IP. Resolved once at startup.
    pub public_ip: Option<String>,
    pub firewall_cache: FirewallCache,
    pub analytics_collector: AnalyticsCollector,
    pub log_broadcaster: LogBroadcaster,
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
        .nest("/api/env-vars", env_vars::routes())
        .nest("/api/domains", domains::routes())
        .nest("/api/firewall", firewall::routes())
        .nest("/api/webhooks", webhooks::routes())
        .route("/api/analytics", get(analytics::get_analytics))
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

async fn fallback() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "route not found" })),
    )
}
