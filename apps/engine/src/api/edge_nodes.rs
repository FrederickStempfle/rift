use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;

use crate::{
    api::{auth::AuthUser, AppState},
    db::edge,
    error::AppResult,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list_edge_nodes))
}

#[derive(Debug, Serialize)]
pub struct EdgeNodeResponse {
    pub id: uuid::Uuid,
    pub region_id: uuid::Uuid,
    pub addr: String,
    pub status: String,
    pub capacity: i32,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn list_edge_nodes(
    State(state): State<AppState>,
    _auth_user: AuthUser,
) -> AppResult<Json<Vec<EdgeNodeResponse>>> {
    let nodes = edge::list_edge_nodes(&state.pool).await?;
    Ok(Json(
        nodes
            .into_iter()
            .map(|node| EdgeNodeResponse {
                id: node.id,
                region_id: node.region_id,
                addr: node.addr,
                status: node.status,
                capacity: node.capacity,
                last_heartbeat_at: node.last_heartbeat_at,
                created_at: node.created_at,
            })
            .collect(),
    ))
}
