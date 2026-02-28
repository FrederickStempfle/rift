use axum::{routing::any, Router};

use crate::api::AppState;

use super::handler;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", any(handler::proxy_request))
        .route("/{*path}", any(handler::proxy_request))
        .with_state(state)
}
