use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

use crate::{api::AppState, error::AppError};

#[derive(Debug, Deserialize)]
pub struct TrafficWsQuery {
    pub token: String,
}

pub async fn ws_traffic_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<TrafficWsQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Verify JWT — any authenticated user may subscribe
    state.token_service.verify_access_token(&query.token)?;
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state)))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.traffic_broadcaster.subscribe();

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let json = match serde_json::to_string(&event) {
                            Ok(j) => j,
                            Err(_) => continue,
                        };
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(lagged = n, "traffic ws client lagged");
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
}
