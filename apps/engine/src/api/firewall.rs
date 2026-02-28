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
    db::firewall,
    error::{AppError, AppResult},
    services::audit::AuditEvent,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/rules", post(create_rule).get(list_rules))
        .route("/rules/{rule_id}", axum::routing::delete(delete_rule))
        .route("/mode", get(get_mode).put(set_mode))
}

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub project_id: Uuid,
    pub cidr: String,
    pub action: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct ProjectIdQuery {
    pub project_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct SetModeRequest {
    pub project_id: Uuid,
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct FirewallRuleResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub cidr: String,
    pub action: String,
    pub description: String,
    pub created_at: String,
}

async fn create_rule(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<CreateRuleRequest>,
) -> AppResult<(StatusCode, Json<FirewallRuleResponse>)> {
    crate::db::projects::get_project_for_user(&state.pool, payload.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    if payload.action != "allow" && payload.action != "block" {
        return Err(AppError::BadRequest(
            "action must be 'allow' or 'block'".into(),
        ));
    }

    if payload.cidr.parse::<ipnet::IpNet>().is_err()
        && payload.cidr.parse::<std::net::IpAddr>().is_err()
    {
        return Err(AppError::BadRequest("invalid IP address or CIDR range".into()));
    }

    let rule = firewall::create_rule(
        &state.pool,
        firewall::NewFirewallRule {
            project_id: payload.project_id,
            cidr: payload.cidr,
            action: payload.action,
            description: payload.description,
        },
    )
    .await?;

    state.firewall_cache.invalidate(payload.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "firewall.rule.create",
            resource_id: Some(rule.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({
                "project_id": rule.project_id,
                "cidr": rule.cidr,
                "action": rule.action,
            }),
        })
        .await?;

    Ok((StatusCode::CREATED, Json(FirewallRuleResponse::from(rule))))
}

async fn list_rules(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ProjectIdQuery>,
) -> AppResult<Json<Vec<FirewallRuleResponse>>> {
    crate::db::projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let rules = firewall::list_rules(&state.pool, query.project_id).await?;
    Ok(Json(
        rules.into_iter().map(FirewallRuleResponse::from).collect(),
    ))
}

async fn delete_rule(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(rule_id): Path<Uuid>,
    Query(query): Query<ProjectIdQuery>,
) -> AppResult<StatusCode> {
    crate::db::projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let deleted = firewall::delete_rule(&state.pool, rule_id, query.project_id).await?;
    if !deleted {
        return Err(AppError::NotFound("rule not found".into()));
    }

    state.firewall_cache.invalidate(query.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "firewall.rule.delete",
            resource_id: Some(rule_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "project_id": query.project_id }),
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn get_mode(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ProjectIdQuery>,
) -> AppResult<Json<serde_json::Value>> {
    crate::db::projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let mode = firewall::get_firewall_mode(&state.pool, query.project_id).await?;
    Ok(Json(json!({ "mode": mode })))
}

async fn set_mode(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<SetModeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if payload.mode != "allow_all" && payload.mode != "block_all" {
        return Err(AppError::BadRequest(
            "mode must be 'allow_all' or 'block_all'".into(),
        ));
    }

    crate::db::projects::get_project_for_user(&state.pool, payload.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    firewall::set_firewall_mode(&state.pool, payload.project_id, &payload.mode).await?;
    state.firewall_cache.invalidate(payload.project_id).await;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "firewall.mode.update",
            resource_id: Some(payload.project_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "mode": payload.mode }),
        })
        .await?;

    Ok(Json(json!({ "mode": payload.mode })))
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

impl From<crate::db::models::FirewallRule> for FirewallRuleResponse {
    fn from(r: crate::db::models::FirewallRule) -> Self {
        Self {
            id: r.id,
            project_id: r.project_id,
            cidr: r.cidr,
            action: r.action,
            description: r.description,
            created_at: r.created_at.to_rfc3339(),
        }
    }
}
