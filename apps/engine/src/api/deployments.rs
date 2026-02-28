use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{deployments, domains, projects},
    error::{AppError, AppResult},
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
}

pub async fn list_deployments(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ListDeploymentsQuery>,
) -> AppResult<Json<Vec<DeploymentResponse>>> {
    let project =
        projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let items =
        deployments::list_deployments_for_project(&state.pool, query.project_id, auth_user.user_id)
            .await?;
    let mut responses = Vec::with_capacity(items.len());
    for deployment in items {
        let public_url = public_url_for_deployment(&state, &project, &deployment).await?;
        responses.push(DeploymentResponse::from_deployment(deployment, public_url));
    }
    Ok(Json(responses))
}

pub async fn create_deployment(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(payload): Json<CreateDeploymentRequest>,
) -> AppResult<(StatusCode, Json<DeploymentResponse>)> {
    let project =
        projects::get_project_for_user(&state.pool, payload.project_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let deployment = state.build_manager.enqueue_project_build(project.clone()).await?;
    let public_url = public_url_for_deployment(&state, &project, &deployment).await?;

    Ok((
        StatusCode::CREATED,
        Json(DeploymentResponse::from_deployment(deployment, public_url)),
    ))
}

async fn public_url_for_deployment(
    state: &AppState,
    project: &crate::db::models::Project,
    deployment: &crate::db::models::Deployment,
) -> Result<Option<String>, AppError> {
    if let Some(domain) = domains::get_primary_domain_for_project(&state.pool, project.id).await? {
        return Ok(Some(state.config.public_url_for_host(&domain.domain)));
    }
    // No domain — use direct IP:port from the deployment
    match deployment.port {
        Some(port) => {
            let ip = state.public_ip.as_deref().unwrap_or("localhost");
            Ok(Some(format!("http://{}:{}", ip, port)))
        }
        None => Ok(None),
    }
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
        }
    }
}
