use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;

use crate::{
    api::{auth::AuthUser, AppState},
    db::edge,
    error::AppResult,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list_regions))
}

#[derive(Debug, Serialize)]
pub struct RegionResponse {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn list_regions(
    State(state): State<AppState>,
    _auth_user: AuthUser,
) -> AppResult<Json<Vec<RegionResponse>>> {
    let regions = edge::list_regions(&state.pool).await?;
    Ok(Json(
        regions
            .into_iter()
            .map(|region| RegionResponse {
                id: region.id,
                name: region.name,
                status: region.status,
                created_at: region.created_at,
            })
            .collect(),
    ))
}
