use axum::{http::StatusCode, routing::get, Json, Router};
use serde_json::json;

use crate::api::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(not_implemented))
}

async fn not_implemented() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "env vars API not implemented in phase 1" })),
    )
}
