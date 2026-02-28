use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::deployments,
    error::AppResult,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list_logs))
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub deployment_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct DeployLogResponse {
    pub id: i64,
    pub deployment_id: Uuid,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub message: String,
    pub source: String,
}

pub async fn list_logs(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<LogsQuery>,
) -> AppResult<Json<Vec<DeployLogResponse>>> {
    let logs =
        deployments::list_logs_for_deployment(&state.pool, query.deployment_id, auth_user.user_id)
            .await?;
    Ok(Json(
        logs.into_iter().map(DeployLogResponse::from).collect(),
    ))
}

impl From<crate::db::models::DeployLog> for DeployLogResponse {
    fn from(value: crate::db::models::DeployLog) -> Self {
        Self {
            id: value.id,
            deployment_id: value.deployment_id,
            timestamp: value.timestamp,
            level: value.level,
            message: value.message,
            source: value.source,
        }
    }
}
