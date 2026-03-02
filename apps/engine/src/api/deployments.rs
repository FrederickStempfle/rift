use std::{net::SocketAddr, time::Duration};

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{deployments, domains, edge, projects},
    error::{AppError, AppResult},
    lifecycle::operations::{self, BeginOutcome},
    services::abuse::{AbuseDecision, AbuseLimit},
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list_deployments).post(create_deployment))
}

#[derive(Debug, Deserialize)]
pub struct ListDeploymentsQuery {
    pub project_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreateDeploymentRequest {
    pub project_id: Uuid,
    /// Optional idempotency key. If provided, replaying the same op_id
    /// returns the prior result without re-executing the build.
    pub op_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct DeploymentResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub commit_sha: String,
    pub commit_message: Option<String>,
    pub branch: String,
    pub status: String,
    pub build_duration_ms: Option<i32>,
    pub url: Option<String>,
    pub public_url: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub suspended_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
pub struct PackageDeploymentResponse {
    pub deployment_id: Uuid,
    pub artifact_id: Uuid,
    pub release_id: Uuid,
    pub release_version: i64,
    pub digest: String,
}

pub async fn list_deployments(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ListDeploymentsQuery>,
) -> AppResult<Json<Vec<DeploymentResponse>>> {
    let project = projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let items =
        deployments::list_deployments_for_project(&state.pool, query.project_id, auth_user.user_id)
            .await?;
    let mut responses = Vec::with_capacity(items.len());
    for deployment in items {
        let public_url = public_url_for_deployment(&state, &project).await?;
        responses.push(DeploymentResponse::from_deployment(deployment, public_url));
    }
    Ok(Json(responses))
}

pub async fn create_deployment(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    auth_user: AuthUser,
    Json(payload): Json<CreateDeploymentRequest>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
    if !state.abuse_guard.is_trusted_request(addr.ip(), &headers) {
        let by_ip = state.abuse_guard.resolve_limit(
            "api.deploy.create.ip",
            Some(payload.project_id),
            45,
            Duration::from_secs(10 * 60),
            Some(30),
        );
        if by_ip.enabled {
            enforce_abuse_limit(
                &state,
                AbuseLimit::per_ip(
                    "api.deploy.create.ip",
                    addr.ip(),
                    "deploy",
                    by_ip.limit,
                    by_ip.window,
                    by_ip.challenge_after,
                ),
            )
            .await?;
        }

        let by_user = state.abuse_guard.resolve_limit(
            "api.deploy.create.user",
            Some(payload.project_id),
            20,
            Duration::from_secs(10 * 60),
            Some(12),
        );
        if by_user.enabled {
            enforce_abuse_limit(
                &state,
                AbuseLimit {
                    scope: "api.deploy.create.user",
                    actor_key: format!("user:{}", auth_user.user_id),
                    bucket_key: format!("scope:api.deploy.create.user:user:{}", auth_user.user_id),
                    limit: by_user.limit,
                    window: by_user.window,
                    challenge_after: by_user.challenge_after,
                },
            )
            .await?;
        }
    }

    let project =
        projects::get_project_for_user(&state.pool, payload.project_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let op_id = payload.op_id.unwrap_or_else(Uuid::new_v4);

    // Idempotency: check if this op_id was already executed.
    match operations::begin_operation(&state.pool, op_id, "deploy", project.id, None).await? {
        BeginOutcome::Completed(op) => {
            // Return the prior result without re-executing.
            if let Some(deployment_id) = op.deployment_id {
                if let Some(deployment) =
                    deployments::get_deployment_by_id(&state.pool, deployment_id).await?
                {
                    let public_url = public_url_for_deployment(&state, &project).await?;
                    return Ok((
                        StatusCode::CREATED,
                        Json(DeploymentResponse::from_deployment(deployment, public_url)),
                    ));
                }
            }
            // Fallback: op completed but deployment not found — return error from op.
            return Err(AppError::Internal(
                op.error
                    .unwrap_or_else(|| "prior operation result unavailable".into()),
            ));
        }
        BeginOutcome::Failed(op) => {
            return Err(AppError::Conflict(
                op.error
                    .unwrap_or_else(|| "prior operation failed".to_owned()),
            ));
        }
        BeginOutcome::InProgress => {
            return Err(AppError::Conflict("operation already in progress".into()));
        }
        BeginOutcome::Proceed => {}
    }

    let deployment = match state
        .build_manager
        .enqueue_project_build(project.clone())
        .await
    {
        Ok(deployment) => {
            operations::complete_operation(
                &state.pool,
                op_id,
                serde_json::json!({ "deployment_id": deployment.id }),
            )
            .await?;
            // Backfill the deployment_id on the operation row.
            sqlx::query("UPDATE lifecycle_operations SET deployment_id = $2 WHERE op_id = $1")
                .bind(op_id)
                .bind(deployment.id)
                .execute(&state.pool)
                .await
                .map_err(AppError::Db)?;
            deployment
        }
        Err(e) => {
            let _ = operations::fail_operation(&state.pool, op_id, &e.to_string()).await;
            return Err(e);
        }
    };

    let public_url = public_url_for_deployment(&state, &project).await?;

    Ok((
        StatusCode::CREATED,
        Json(DeploymentResponse::from_deployment(deployment, public_url)),
    ))
}

pub async fn package_deployment(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    auth_user: AuthUser,
    Path(deployment_id): Path<Uuid>,
) -> AppResult<(StatusCode, Json<PackageDeploymentResponse>)> {
    let deployment =
        deployments::get_deployment_for_user(&state.pool, deployment_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("deployment not found".into()))?;
    if !state.abuse_guard.is_trusted_request(addr.ip(), &headers) {
        let by_ip = state.abuse_guard.resolve_limit(
            "api.deploy.package.ip",
            Some(deployment.project_id),
            60,
            Duration::from_secs(60 * 60),
            Some(40),
        );
        if by_ip.enabled {
            enforce_abuse_limit(
                &state,
                AbuseLimit::per_ip(
                    "api.deploy.package.ip",
                    addr.ip(),
                    "package",
                    by_ip.limit,
                    by_ip.window,
                    by_ip.challenge_after,
                ),
            )
            .await?;
        }
    }
    if deployment.status != "ready" && deployment.status != "suspended" {
        return Err(AppError::Conflict(
            "only ready/suspended deployments can be packaged".into(),
        ));
    }

    let mut hasher = Sha256::new();
    hasher.update(deployment.id.as_bytes());
    hasher.update(deployment.project_id.as_bytes());
    hasher.update(deployment.branch.as_bytes());
    hasher.update(deployment.commit_sha.as_bytes());
    let digest = format!("{:x}", hasher.finalize());

    let manifest = serde_json::json!({
        "deployment_id": deployment.id,
        "project_id": deployment.project_id,
        "commit_sha": deployment.commit_sha,
        "branch": deployment.branch,
        "status": deployment.status,
        "url": deployment.url,
        "port": deployment.port,
    });

    let artifact = edge::create_or_update_artifact(
        &state.pool,
        deployment.id,
        &digest,
        manifest.to_string().len() as i64,
        &manifest,
    )
    .await?;

    let release = edge::create_release_for_deployment(
        &state.pool,
        deployment.project_id,
        deployment.id,
        artifact.id,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(PackageDeploymentResponse {
            deployment_id: deployment.id,
            artifact_id: artifact.id,
            release_id: release.id,
            release_version: release.version,
            digest,
        }),
    ))
}

async fn public_url_for_deployment(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<Option<String>, AppError> {
    if let Some(domain) = domains::get_primary_domain_for_project(&state.pool, project.id).await? {
        return Ok(Some(state.config.public_url_for_host(&domain.domain)));
    }
    // No custom domain — use subdomain-based URL if subdomain is set
    Ok(project
        .subdomain
        .as_deref()
        .map(|s| state.config.public_url_for_subdomain(s)))
}

impl DeploymentResponse {
    fn from_deployment(value: crate::db::models::Deployment, public_url: Option<String>) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            commit_sha: value.commit_sha,
            commit_message: value.commit_message,
            branch: value.branch,
            status: value.status,
            build_duration_ms: value.build_duration_ms,
            url: value.url,
            public_url,
            started_at: value.started_at,
            finished_at: value.finished_at,
            created_at: value.created_at,
            suspended_at: value.suspended_at,
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
