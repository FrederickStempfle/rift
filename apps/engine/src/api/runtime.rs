use axum::{extract::State, Json};
use serde::Serialize;

use crate::{
    api::{auth::AuthUser, AppState},
    error::AppResult,
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
