use std::{net::SocketAddr, time::Duration};

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{domains, edge, projects},
    error::{AppError, AppResult},
    services::abuse::{AbuseDecision, AbuseLimit},
    state::RoutingEntry,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_releases))
        .route("/{release_id}/promote", post(promote_release))
        .route("/{release_id}/rollback", post(rollback_release))
}

#[derive(Debug, Deserialize)]
pub struct ListReleasesQuery {
    pub project_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct ReleaseResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub deployment_id: Uuid,
    pub artifact_id: Uuid,
    pub version: i64,
    pub state: String,
    pub promoted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn list_releases(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ListReleasesQuery>,
) -> AppResult<Json<Vec<ReleaseResponse>>> {
    let project = projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let releases =
        edge::list_releases_for_project_user(&state.pool, project.id, auth_user.user_id).await?;
    Ok(Json(
        releases
            .into_iter()
            .map(ReleaseResponse::from_release)
            .collect(),
    ))
}

pub async fn promote_release(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    auth_user: AuthUser,
    Path(release_id): Path<Uuid>,
) -> AppResult<Json<ReleaseResponse>> {
    let release = edge::get_release_for_user(&state.pool, release_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("release not found".into()))?;
    if !state.abuse_guard.is_trusted_request(addr.ip(), &headers) {
        let by_ip = state.abuse_guard.resolve_limit(
            "api.release.promote.ip",
            Some(release.project_id),
            40,
            Duration::from_secs(30 * 60),
            Some(25),
        );
        if by_ip.enabled {
            enforce_abuse_limit(
                &state,
                AbuseLimit::per_ip(
                    "api.release.promote.ip",
                    addr.ip(),
                    "promote",
                    by_ip.limit,
                    by_ip.window,
                    by_ip.challenge_after,
                ),
            )
            .await?;
        }
    }

    let promoted = edge::mark_release_promoted(&state.pool, release.id)
        .await?
        .ok_or_else(|| AppError::NotFound("release not found".into()))?;

    if let Some(host) =
        release_host_for_project(&state, promoted.project_id, auth_user.user_id).await?
    {
        let binding =
            edge::upsert_route_binding(&state.pool, &host, promoted.project_id, promoted.id)
                .await?;
        state.routing_cache.invalidate_host(&host).await;

        let entry = RoutingEntry {
            host: host.clone(),
            project_id: promoted.project_id,
            deployment_id: promoted.deployment_id,
            worker_addr: state.config.proxy_addr(),
            version: binding.version as u64,
        };
        if let Err(e) = state.state_store.set_routing(&entry).await {
            tracing::warn!(host = %host, error = %e, "failed to persist route binding");
        }
        if let Err(e) = state.state_store.publish_routing_update(&entry).await {
            tracing::warn!(host = %host, error = %e, "failed to publish route binding");
        }
    }

    Ok(Json(ReleaseResponse::from_release(promoted)))
}

pub async fn rollback_release(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    auth_user: AuthUser,
    Path(release_id): Path<Uuid>,
) -> AppResult<(StatusCode, Json<ReleaseResponse>)> {
    let release = edge::get_release_for_user(&state.pool, release_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("release not found".into()))?;
    if !state.abuse_guard.is_trusted_request(addr.ip(), &headers) {
        let by_ip = state.abuse_guard.resolve_limit(
            "api.release.rollback.ip",
            Some(release.project_id),
            30,
            Duration::from_secs(30 * 60),
            Some(20),
        );
        if by_ip.enabled {
            enforce_abuse_limit(
                &state,
                AbuseLimit::per_ip(
                    "api.release.rollback.ip",
                    addr.ip(),
                    "rollback",
                    by_ip.limit,
                    by_ip.window,
                    by_ip.challenge_after,
                ),
            )
            .await?;
        }
    }

    let rolled_back = edge::mark_release_rollback(&state.pool, release.id)
        .await?
        .ok_or_else(|| AppError::NotFound("release not found".into()))?;

    Ok((
        StatusCode::OK,
        Json(ReleaseResponse::from_release(rolled_back)),
    ))
}

async fn release_host_for_project(
    state: &AppState,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<Option<String>, AppError> {
    if let Some(domain) = domains::get_primary_domain_for_project(&state.pool, project_id).await? {
        return Ok(Some(domain.domain));
    }

    let project = projects::get_project_for_user(&state.pool, project_id, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;
    Ok(project
        .subdomain
        .as_deref()
        .map(|subdomain| format!("{subdomain}.{}", state.config.base_domain)))
}

impl ReleaseResponse {
    fn from_release(value: crate::db::models::DeployRelease) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            deployment_id: value.deployment_id,
            artifact_id: value.artifact_id,
            version: value.version,
            state: value.state,
            promoted_at: value.promoted_at,
            created_at: value.created_at,
        }
    }
}

async fn enforce_abuse_limit(state: &AppState, limit: AbuseLimit) -> AppResult<()> {
    match state.abuse_guard.enforce(limit).await? {
        AbuseDecision::Allow => Ok(()),
        AbuseDecision::Challenge {
            retry_after_secs,
            reason,
        } => Err(AppError::RateLimited(format!(
            "{reason}; retry in {retry_after_secs}s"
        ))),
        AbuseDecision::Block {
            retry_after_secs,
            reason,
            tier: _,
        } => Err(AppError::RateLimited(format!(
            "{reason}; retry in {retry_after_secs}s"
        ))),
    }
}
