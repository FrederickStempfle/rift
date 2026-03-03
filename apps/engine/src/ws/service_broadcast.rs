use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Message broadcast over the in-memory channel and sent over WebSocket for services.
#[derive(Clone, Debug, Serialize)]
pub struct ServiceLogMessage {
    pub id: i64,
    pub service_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub source: String,
}
