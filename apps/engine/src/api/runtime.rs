use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::projects,
    error::{AppError, AppResult},
};

#[derive(Debug, Serialize)]
pub struct RuntimeStatsResponse {
    pub mode: String,
    pub pool: Option<PoolStatsResponse>,
}

#[derive(Debug, Serialize)]
pub struct PoolStatsResponse {
    pub warm_workers: usize,
    pub active_workers: usize,
    pub suspended_deployments: usize,
    pub max_active: usize,
    pub warm_target: usize,
}

pub async fn get_runtime_stats(
    State(state): State<AppState>,
    _auth_user: AuthUser,
) -> AppResult<Json<RuntimeStatsResponse>> {
    let mode = state.config.runtime_mode.clone();

    let pool_stats = if let Some(stats) = state.runtime_backend.pool_stats().await {
        Some(PoolStatsResponse {
            warm_workers: stats.warm_workers,
            active_workers: stats.active_workers,
            suspended_deployments: stats.suspended_deployments,
            max_active: stats.max_active,
            warm_target: stats.warm_target,
        })
    } else {
        None
    };

    Ok(Json(RuntimeStatsResponse {
        mode,
        pool: pool_stats,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ProjectRuntimeQuery {
    pub project_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct ProjectRuntimeResponse {
    pub status: String,
    pub deployment_id: Option<String>,
    pub url: Option<String>,
    pub runtime_mode: String,
}

pub async fn get_project_runtime(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ProjectRuntimeQuery>,
) -> AppResult<Json<ProjectRuntimeResponse>> {
    projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let backend = &state.runtime_backend;

    let (status, deployment_id, url) =
        if let Some(url) = backend.active_url(query.project_id).await {
            let dep_id = backend.active_deployment_id(query.project_id).await;
            ("active".to_owned(), dep_id, Some(url))
        } else if backend.is_suspended(query.project_id).await {
            ("suspended".to_owned(), None, None)
        } else {
            ("stopped".to_owned(), None, None)
        };

    Ok(Json(ProjectRuntimeResponse {
        status,
        deployment_id: deployment_id.map(|id| id.to_string()),
        url,
        runtime_mode: state.config.runtime_mode.clone(),
    }))
}
