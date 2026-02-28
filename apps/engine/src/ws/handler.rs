use axum::{
    extract::{Query, State, WebSocketUpgrade},
    response::IntoResponse,
};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    api::AppState,
    db::deployments,
    error::AppError,
    ws::broadcast::DeployLogMessage,
};

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub token: String,
    pub deployment_id: Uuid,
}

pub async fn ws_logs_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let claims = state.token_service.verify_access_token(&query.token)?;
    let user_id = claims.sub;

    let deployment = deployments::get_deployment_for_user(
        &state.pool,
        query.deployment_id,
        user_id,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("deployment not found".into()))?;

    let deployment_id = deployment.id;

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, deployment_id, user_id)))
}

async fn handle_socket(socket: WebSocket, state: AppState, deployment_id: Uuid, user_id: Uuid) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe BEFORE querying DB to avoid missing any logs in the gap.
    let mut rx = state.log_broadcaster.subscribe(deployment_id).await;

    // Send existing logs from the database.
    let existing_logs = match deployments::list_logs_for_deployment(&state.pool, deployment_id, user_id).await {
        Ok(logs) => logs,
        Err(e) => {
            tracing::error!(error = %e, "failed to fetch existing logs for ws");
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
    };

    let mut last_id: i64 = 0;
    for log in existing_logs {
        last_id = log.id;
        let msg = DeployLogMessage {
            id: log.id,
            deployment_id: log.deployment_id,
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
                        tracing::warn!(deployment_id = %deployment_id, lagged = n, "ws client lagged behind");
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

    state.log_broadcaster.cleanup(deployment_id).await;
}
