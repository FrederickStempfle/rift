use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::waf,
    error::{AppError, AppResult},
    proxy::waf::{WafAction, WafMatchField, WafMatchOp},
    services::audit::AuditEvent,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/rules", post(create_rule).get(list_rules))
        .route(
            "/rules/{rule_id}",
            get(get_rule)
                .put(update_rule)
                .delete(delete_rule),
        )
        .route("/policy", get(get_policy).put(set_policy))
        .route("/events", get(list_events))
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub project_id: Option<Uuid>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub match_field: String,
    pub match_op: String,
    pub match_value: String,
    pub header_name: Option<String>,
    pub action: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_priority() -> i32 {
    100
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub match_field: String,
    pub match_op: String,
    pub match_value: String,
    pub header_name: Option<String>,
    pub action: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub project_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[derive(Debug, Deserialize)]
pub struct SetPolicyRequest {
    pub project_id: Option<Uuid>,
    pub mode: String,
    #[serde(default = "default_fail_open")]
    pub fail_open: bool,
}

fn default_fail_open() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct WafRuleResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub match_field: String,
    pub match_op: String,
    pub match_value: String,
    pub header_name: Option<String>,
    pub action: String,
    pub priority: i32,
    pub enabled: bool,
    pub is_managed: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<waf::WafRuleRow> for WafRuleResponse {
    fn from(r: waf::WafRuleRow) -> Self {
        Self {
            id: r.id,
            project_id: r.project_id,
            name: r.name,
            description: r.description,
            match_field: r.match_field,
            match_op: r.match_op,
            match_value: r.match_value,
            header_name: r.header_name,
            action: r.action,
            priority: r.priority,
            enabled: r.enabled,
            is_managed: r.is_managed,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WafEventResponse {
    pub id: i64,
    pub project_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub client_ip: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub rule_name: Option<String>,
    pub created_at: String,
}

impl From<waf::WafEvent> for WafEventResponse {
    fn from(e: waf::WafEvent) -> Self {
        Self {
            id: e.id,
            project_id: e.project_id,
            rule_id: e.rule_id,
            action: e.action,
            client_ip: e.client_ip,
            method: e.method,
            host: e.host,
            path: e.path,
            user_agent: e.user_agent,
            rule_name: e.rule_name,
            created_at: e.created_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_rule_fields(
    match_field: &str,
    match_op: &str,
    match_value: &str,
    header_name: &Option<String>,
    action: &str,
) -> Result<(), AppError> {
    WafMatchField::from_str_opt(match_field)
        .ok_or_else(|| AppError::BadRequest(format!("invalid match_field: {match_field}")))?;

    let op = WafMatchOp::from_str_opt(match_op)
        .ok_or_else(|| AppError::BadRequest(format!("invalid match_op: {match_op}")))?;

    WafAction::from_str_opt(action)
        .ok_or_else(|| AppError::BadRequest(format!("invalid action: {action}")))?;

    let field = WafMatchField::from_str_opt(match_field).unwrap();

    // CIDR op only valid for IP field
    if op == WafMatchOp::Cidr && field != WafMatchField::Ip {
        return Err(AppError::BadRequest(
            "cidr operator is only valid for ip field".into(),
        ));
    }

    // IP field requires cidr or exact operator (regex also useful for IP ranges)
    if field == WafMatchField::Ip
        && !matches!(op, WafMatchOp::Cidr | WafMatchOp::Exact | WafMatchOp::Regex)
    {
        return Err(AppError::BadRequest(
            "ip field only supports 'cidr', 'exact', or 'regex' operators".into(),
        ));
    }

    // Header field requires header_name
    if field == WafMatchField::Header
        && header_name.as_deref().is_none_or(str::is_empty)
    {
        return Err(AppError::BadRequest(
            "header_name is required when match_field is 'header'".into(),
        ));
    }

    // Validate regex compiles
    if op == WafMatchOp::Regex {
        regex::Regex::new(match_value).map_err(|e| {
            AppError::BadRequest(format!("invalid regex pattern: {e}"))
        })?;
    }

    // Validate CIDR parses
    if op == WafMatchOp::Cidr {
        match_value
            .parse::<ipnet::IpNet>()
            .or_else(|_| match_value.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
            .map_err(|_| AppError::BadRequest("invalid CIDR or IP address".into()))?;
    }

    if match_value.is_empty() {
        return Err(AppError::BadRequest("match_value cannot be empty".into()));
    }

    Ok(())
}

/// Verify the user owns the project (if project-scoped).
async fn authorize_project(
    state: &AppState,
    user_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<(), AppError> {
    if let Some(pid) = project_id {
        crate::db::projects::get_project_for_user(&state.pool, pid, user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn create_rule(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<CreateRuleRequest>,
) -> AppResult<(StatusCode, Json<WafRuleResponse>)> {
    authorize_project(&state, auth_user.user_id, payload.project_id).await?;

    validate_rule_fields(
        &payload.match_field,
        &payload.match_op,
        &payload.match_value,
        &payload.header_name,
        &payload.action,
    )?;

    let rule = waf::create_rule(
        &state.pool,
        waf::NewWafRule {
            project_id: payload.project_id,
            name: payload.name,
            description: payload.description,
            match_field: payload.match_field,
            match_op: payload.match_op,
            match_value: payload.match_value,
            header_name: payload.header_name,
            action: payload.action,
            priority: payload.priority,
            is_managed: false,
        },
    )
    .await?;

    // Invalidate cache for the affected scope
    state.waf_cache.invalidate(rule.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "waf.rule.create",
            resource_id: Some(rule.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({
                "project_id": rule.project_id,
                "name": rule.name,
                "action": rule.action,
                "match_field": rule.match_field,
                "match_op": rule.match_op,
            }),
        })
        .await?;

    Ok((StatusCode::CREATED, Json(WafRuleResponse::from(rule))))
}

async fn list_rules(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ScopeQuery>,
) -> AppResult<Json<Vec<WafRuleResponse>>> {
    authorize_project(&state, auth_user.user_id, query.project_id).await?;

    let rules = waf::list_rules(&state.pool, query.project_id).await?;
    Ok(Json(rules.into_iter().map(WafRuleResponse::from).collect()))
}

async fn get_rule(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(rule_id): Path<Uuid>,
) -> AppResult<Json<WafRuleResponse>> {
    let rule = waf::get_rule(&state.pool, rule_id)
        .await?
        .ok_or_else(|| AppError::NotFound("rule not found".into()))?;

    authorize_project(&state, auth_user.user_id, rule.project_id).await?;

    Ok(Json(WafRuleResponse::from(rule)))
}

async fn update_rule(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(rule_id): Path<Uuid>,
    Json(payload): Json<UpdateRuleRequest>,
) -> AppResult<Json<WafRuleResponse>> {
    let existing = waf::get_rule(&state.pool, rule_id)
        .await?
        .ok_or_else(|| AppError::NotFound("rule not found".into()))?;

    authorize_project(&state, auth_user.user_id, existing.project_id).await?;

    validate_rule_fields(
        &payload.match_field,
        &payload.match_op,
        &payload.match_value,
        &payload.header_name,
        &payload.action,
    )?;

    let updated = waf::update_rule(
        &state.pool,
        waf::UpdateWafRule {
            rule_id,
            name: &payload.name,
            description: &payload.description,
            match_field: &payload.match_field,
            match_op: &payload.match_op,
            match_value: &payload.match_value,
            header_name: payload.header_name.as_deref(),
            action: &payload.action,
            priority: payload.priority,
            enabled: payload.enabled,
        },
    )
    .await?
    .ok_or_else(|| AppError::NotFound("rule not found".into()))?;

    state.waf_cache.invalidate(updated.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "waf.rule.update",
            resource_id: Some(rule_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({
                "project_id": updated.project_id,
                "name": updated.name,
                "enabled": updated.enabled,
            }),
        })
        .await?;

    Ok(Json(WafRuleResponse::from(updated)))
}

async fn delete_rule(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(rule_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let existing = waf::get_rule(&state.pool, rule_id)
        .await?
        .ok_or_else(|| AppError::NotFound("rule not found".into()))?;

    authorize_project(&state, auth_user.user_id, existing.project_id).await?;

    let deleted = waf::delete_rule(&state.pool, rule_id).await?;
    if !deleted {
        return Err(AppError::NotFound("rule not found".into()));
    }

    state.waf_cache.invalidate(existing.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "waf.rule.delete",
            resource_id: Some(rule_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "project_id": existing.project_id }),
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn get_policy(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ScopeQuery>,
) -> AppResult<Json<serde_json::Value>> {
    authorize_project(&state, auth_user.user_id, query.project_id).await?;

    let policy = waf::get_policy(&state.pool, query.project_id).await?;
    match policy {
        Some(p) => Ok(Json(json!({
            "mode": p.mode,
            "fail_open": p.fail_open,
            "project_id": p.project_id,
        }))),
        None => Ok(Json(json!({
            "mode": "active",
            "fail_open": true,
            "project_id": query.project_id,
        }))),
    }
}

async fn set_policy(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<SetPolicyRequest>,
) -> AppResult<Json<serde_json::Value>> {
    authorize_project(&state, auth_user.user_id, payload.project_id).await?;

    if !matches!(payload.mode.as_str(), "active" | "log_only" | "disabled") {
        return Err(AppError::BadRequest(
            "mode must be 'active', 'log_only', or 'disabled'".into(),
        ));
    }

    let policy =
        waf::upsert_policy(&state.pool, payload.project_id, &payload.mode, payload.fail_open)
            .await?;

    state.waf_cache.invalidate(payload.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "waf.policy.update",
            resource_id: payload.project_id,
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({
                "mode": policy.mode,
                "fail_open": policy.fail_open,
            }),
        })
        .await?;

    Ok(Json(json!({
        "mode": policy.mode,
        "fail_open": policy.fail_open,
        "project_id": policy.project_id,
    })))
}

async fn list_events(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<EventsQuery>,
) -> AppResult<Json<Vec<WafEventResponse>>> {
    authorize_project(&state, auth_user.user_id, query.project_id).await?;

    let limit = query.limit.clamp(1, 1000);
    let events = waf::list_events(&state.pool, query.project_id, limit).await?;
    Ok(Json(events.into_iter().map(WafEventResponse::from).collect()))
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
