use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CliError;

use super::RiftClient;

#[derive(Debug, Serialize, Deserialize)]
pub struct DeployLogResponse {
    pub id: i64,
    pub deployment_id: Uuid,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub message: String,
    pub source: String,
}

impl RiftClient {
    pub async fn list_logs(
        &mut self,
        deployment_id: Uuid,
    ) -> Result<Vec<DeployLogResponse>, CliError> {
        let resp = self
            .get(&format!("/api/logs?deployment_id={deployment_id}"))
            .await?;
        Ok(resp.json().await?)
    }

    /// Build a WebSocket URL for streaming logs.
    pub async fn ws_logs_url(&mut self, deployment_id: Uuid) -> Result<String, CliError> {
        let token = self.access_token().await?;
        let base = self
            .api_url()
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        Ok(format!(
            "{base}/api/ws/logs?token={token}&deployment_id={deployment_id}"
        ))
    }
}
