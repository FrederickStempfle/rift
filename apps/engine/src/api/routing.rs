use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::{
    api::{auth::AuthUser, AppState},
    db::edge,
    error::{AppError, AppResult},
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/host/{host}", get(get_route_binding))
}

#[derive(Debug, Serialize)]
pub struct RouteBindingResponse {
    pub host: String,
    pub project_id: uuid::Uuid,
    pub release_id: uuid::Uuid,
    pub version: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub async fn get_route_binding(
    State(state): State<AppState>,
    _auth_user: AuthUser,
    Path(host): Path<String>,
) -> AppResult<Json<RouteBindingResponse>> {
    let binding = edge::get_route_binding(&state.pool, &host)
        .await?
        .ok_or_else(|| AppError::NotFound("route binding not found".into()))?;
    Ok(Json(RouteBindingResponse {
        host: binding.host,
        project_id: binding.project_id,
        release_id: binding.release_id,
        version: binding.version,
        updated_at: binding.updated_at,
    }))
}
