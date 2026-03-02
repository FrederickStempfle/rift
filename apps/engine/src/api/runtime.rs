use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{deployments, projects},
    error::{AppError, AppResult},
    lifecycle::operations::{self, BeginOutcome},
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

    let pool_stats = state
        .runtime_backend
        .pool_stats()
        .await
        .map(|stats| PoolStatsResponse {
            warm_workers: stats.warm_workers,
            active_workers: stats.active_workers,
            suspended_deployments: stats.suspended_deployments,
            max_active: stats.max_active,
            warm_target: stats.warm_target,
        });

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

    let (status, deployment_id, url) = if let Some(url) = backend.active_url(query.project_id).await
    {
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

// ── lifecycle action request/response ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LifecycleActionRequest {
    pub op_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct LifecycleActionResponse {
    pub ok: bool,
    pub op_id: Uuid,
}

/// `POST /api/projects/{project_id}/stop`
pub async fn stop_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<LifecycleActionRequest>,
) -> AppResult<Json<LifecycleActionResponse>> {
    let project = projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let op_id = payload.op_id.unwrap_or_else(Uuid::new_v4);

    match operations::begin_operation(&state.pool, op_id, "stop", project.id, None).await? {
        BeginOutcome::Completed(_) => {
            return Ok(Json(LifecycleActionResponse { ok: true, op_id }));
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

    match state.runtime_backend.stop(project_id).await {
        Ok(()) => {
            // Mark latest active/suspended deployment as cancelled in DB.
            if let Some(dep) = deployments::latest_ready_or_suspended_deployment_for_project(
                &state.pool,
                project_id,
            )
            .await?
            {
                let _ = deployments::update_status(&state.pool, dep.id, "cancelled").await;
            }
            let _ = operations::complete_operation(
                &state.pool,
                op_id,
                serde_json::json!({"stopped": true}),
            )
            .await;
        }
        Err(e) => {
            let _ = operations::fail_operation(&state.pool, op_id, &e.to_string()).await;
            return Err(e);
        }
    }

    Ok(Json(LifecycleActionResponse { ok: true, op_id }))
}

/// `POST /api/projects/{project_id}/suspend`
pub async fn suspend_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<LifecycleActionRequest>,
) -> AppResult<Json<LifecycleActionResponse>> {
    let project = projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let op_id = payload.op_id.unwrap_or_else(Uuid::new_v4);

    match operations::begin_operation(&state.pool, op_id, "suspend", project.id, None).await? {
        BeginOutcome::Completed(_) => {
            return Ok(Json(LifecycleActionResponse { ok: true, op_id }));
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

    match state.runtime_backend.suspend(project_id).await {
        Ok(suspended) => {
            let _ = operations::complete_operation(
                &state.pool,
                op_id,
                serde_json::json!({"suspended": suspended}),
            )
            .await;
        }
        Err(e) => {
            let _ = operations::fail_operation(&state.pool, op_id, &e.to_string()).await;
            return Err(e);
        }
    }

    Ok(Json(LifecycleActionResponse { ok: true, op_id }))
}

/// `POST /api/projects/{project_id}/wake`
pub async fn wake_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<LifecycleActionRequest>,
) -> AppResult<Json<LifecycleActionResponse>> {
    let project = projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let op_id = payload.op_id.unwrap_or_else(Uuid::new_v4);

    match operations::begin_operation(&state.pool, op_id, "wake", project.id, None).await? {
        BeginOutcome::Completed(_) => {
            return Ok(Json(LifecycleActionResponse { ok: true, op_id }));
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

    match state.runtime_backend.wake(project_id).await {
        Ok(url) => {
            let _ = operations::complete_operation(
                &state.pool,
                op_id,
                serde_json::json!({"woke": url.is_some(), "url": url}),
            )
            .await;
        }
        Err(e) => {
            let _ = operations::fail_operation(&state.pool, op_id, &e.to_string()).await;
            return Err(e);
        }
    }

    Ok(Json(LifecycleActionResponse { ok: true, op_id }))
}
