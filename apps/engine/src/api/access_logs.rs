use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::access_logs::{self, AccessLogFilters},
    error::{AppError, AppResult},
};

#[derive(Debug, Deserialize)]
pub struct AccessLogsQuery {
    pub project_id: Option<Uuid>,
    pub before_id: Option<i64>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub status: Option<u16>,
    pub client_ip: Option<String>,
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
    let filters = normalize_filters(query)?;
    let logs = access_logs::list(&state.pool, auth_user.user_id, filters).await?;
    let next_before_id = logs.last().map(|log| log.id);

    Ok(Json(AccessLogsResponse {
        logs: logs.into_iter().map(AccessLogResponse::from).collect(),
        next_before_id,
    }))
}

fn normalize_filters(query: AccessLogsQuery) -> Result<AccessLogFilters, AppError> {
    if let (Some(from), Some(to)) = (&query.from, &query.to) {
        if from > to {
            return Err(AppError::BadRequest("`from` must be <= `to`".into()));
        }
    }

    let host = query.host.map(|value| value.trim().to_lowercase()).filter(|value| !value.is_empty());
    let path_prefix = query
        .path_prefix
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let client_ip = query
        .client_ip
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());

    Ok(AccessLogFilters {
        project_id: query.project_id,
        before_id: query.before_id,
        from: query.from,
        to: query.to,
        host,
        path_prefix,
        status: query.status.map(i32::from),
        client_ip,
        limit: i64::from(query.limit.unwrap_or(100).clamp(1, 1000)),
    })
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
