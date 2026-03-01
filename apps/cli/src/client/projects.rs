use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CliError;

use super::RiftClient;

#[derive(Debug, Serialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub repo_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,
    pub subdomain: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub name: String,
    pub repo_url: String,
    pub branch: String,
    pub framework: String,
    pub build_command: Option<String>,
    pub output_dir: Option<String>,
    pub install_command: Option<String>,
    pub subdomain: String,
    pub public_url: String,
    pub primary_domain: Option<String>,
    pub latest_deployment: Option<ProjectDeploymentSummary>,
    pub runtime_status: String,
    pub webhook_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectDeploymentSummary {
    pub id: Uuid,
    pub status: String,
    pub commit_sha: String,
    pub commit_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl RiftClient {
    pub async fn list_projects(&mut self) -> Result<Vec<ProjectResponse>, CliError> {
        let resp = self.get("/api/projects").await?;
        Ok(resp.json().await?)
    }

    pub async fn get_project(&mut self, id: Uuid) -> Result<ProjectResponse, CliError> {
        let resp = self.get(&format!("/api/projects/{id}")).await?;
        Ok(resp.json().await?)
    }

    pub async fn create_project(
        &mut self,
        req: CreateProjectRequest,
    ) -> Result<ProjectResponse, CliError> {
        let resp = self.post("/api/projects", &req).await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_project(&mut self, id: Uuid) -> Result<(), CliError> {
        self.delete(&format!("/api/projects/{id}")).await?;
        Ok(())
    }
}
