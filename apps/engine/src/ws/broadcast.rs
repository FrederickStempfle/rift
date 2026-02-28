use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Message broadcast over the in-memory channel and sent over WebSocket.
/// Matches the `DeployLogResponse` shape the frontend expects.
#[derive(Clone, Debug, Serialize)]
pub struct DeployLogMessage {
    pub id: i64,
    pub deployment_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub source: String,
}
