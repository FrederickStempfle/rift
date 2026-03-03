use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::{api::AppState, db::services, error::AppError, ws::service_broadcast::ServiceLogMessage};

#[derive(Debug, Deserialize)]
pub struct WsServiceQuery {
    pub token: String,
    pub service_id: Uuid,
}

pub async fn ws_service_logs_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsServiceQuery>,
) -> Result<impl IntoResponse, AppError> {
    let claims = state.token_service.verify_access_token(&query.token)?;
    let user_id = claims.sub;

    let service = services::get_service_for_user(&state.pool, query.service_id, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("service not found".into()))?;

    let service_id = service.id;

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, service_id, user_id)))
}

async fn handle_socket(socket: WebSocket, state: AppState, service_id: Uuid, user_id: Uuid) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe BEFORE querying DB to avoid missing any logs in the gap.
    let mut rx = state.service_log_broadcaster.subscribe(service_id).await;

    // Send existing logs from the database.
    let existing_logs =
        match services::list_logs_for_service(&state.pool, service_id, user_id).await {
            Ok(logs) => logs,
            Err(e) => {
                tracing::error!(error = %e, "failed to fetch existing service logs for ws");
                let _ = sender.send(Message::Close(None)).await;
                return;
            }
        };

    let mut last_id: i64 = 0;
    for log in existing_logs {
        last_id = log.id;
        let msg = ServiceLogMessage {
            id: log.id,
            service_id: log.service_id,
            timestamp: log.timestamp,
            level: log.level,
            message: log.message,
            source: log.source,
        };
        let json = match serde_json::to_string(&msg) {
            Ok(j) => j,
            Err(_) => continue,
        };
        if sender.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }

    // Stream new logs from broadcast channel, deduplicating by id.
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        if msg.id <= last_id {
                            continue;
                        }
                        last_id = msg.id;
                        let json = match serde_json::to_string(&msg) {
                            Ok(j) => j,
                            Err(_) => continue,
                        };
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(service_id = %service_id, lagged = n, "ws service client lagged behind");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    state.service_log_broadcaster.cleanup(service_id).await;
}
