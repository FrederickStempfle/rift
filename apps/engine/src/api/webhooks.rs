use axum::{http::StatusCode, routing::post, Json, Router};
use serde_json::json;

use crate::api::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", post(not_implemented))
}

async fn not_implemented() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": "webhooks API not implemented in phase 1" })),
    )
}
