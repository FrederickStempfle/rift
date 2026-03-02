use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::access_logs,
    error::AppResult,
};

#[derive(Debug, Deserialize)]
pub struct AccessLogsQuery {
    pub project_id: Option<Uuid>,
    pub before_id: Option<i64>,
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct AccessLogResponse {
    pub id: i64,
    pub project_id: Option<Uuid>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub client_ip: String,
    pub host: Option<String>,
    pub method: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct AccessLogsResponse {
    pub logs: Vec<AccessLogResponse>,
    pub next_before_id: Option<i64>,
}

pub async fn list_access_logs(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<AccessLogsQuery>,
) -> AppResult<Json<AccessLogsResponse>> {
    let limit = normalize_limit(query.limit);
    let logs = if let Some(project_id) = query.project_id {
        access_logs::list_for_project(
            &state.pool,
            auth_user.user_id,
            project_id,
            query.before_id,
            limit,
        )
        .await?
    } else {
        access_logs::list_for_user(&state.pool, auth_user.user_id, query.before_id, limit).await?
    };
    let next_before_id = logs.last().map(|log| log.id);

    Ok(Json(AccessLogsResponse {
        logs: logs.into_iter().map(AccessLogResponse::from).collect(),
        next_before_id,
    }))
}

fn normalize_limit(limit: Option<u16>) -> i64 {
    i64::from(limit.unwrap_or(100).clamp(1, 1000))
}

impl From<crate::db::models::AccessLog> for AccessLogResponse {
    fn from(value: crate::db::models::AccessLog) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            timestamp: value.timestamp,
            client_ip: value.client_ip,
            host: value.host,
            method: value.method,
            path: value.path,
            status: value.status,
            duration_ms: value.duration_ms,
        }
    }
}
