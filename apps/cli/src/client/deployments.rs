use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CliError;

use super::RiftClient;

#[derive(Debug, Serialize, Deserialize)]
pub struct DeploymentResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub commit_sha: String,
    pub commit_message: Option<String>,
    pub branch: String,
    pub status: String,
    pub build_duration_ms: Option<i32>,
    pub url: Option<String>,
    pub public_url: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl RiftClient {
    pub async fn list_deployments(
        &mut self,
        project_id: Uuid,
    ) -> Result<Vec<DeploymentResponse>, CliError> {
        let resp = self
            .get(&format!("/api/deployments?project_id={project_id}"))
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn create_deployment(
        &mut self,
        project_id: Uuid,
    ) -> Result<DeploymentResponse, CliError> {
        let resp = self
            .post("/api/deployments", &serde_json::json!({ "project_id": project_id }))
            .await?;
        Ok(resp.json().await?)
    }
}
