use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CliError;

use super::RiftClient;

#[derive(Debug, Serialize, Deserialize)]
pub struct DomainResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
    pub ssl_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ssl_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DomainListResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
    pub project_name: Option<String>,
}

impl RiftClient {
    pub async fn list_domains(
        &mut self,
        project_id: Uuid,
    ) -> Result<Vec<DomainListResponse>, CliError> {
        let resp = self
            .get(&format!("/api/domains?project_id={project_id}"))
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn create_domain(
        &mut self,
        domain: &str,
        project_id: Uuid,
    ) -> Result<DomainResponse, CliError> {
        let resp = self
            .post(
                "/api/domains",
                &serde_json::json!({
                    "domain": domain,
                    "project_id": project_id,
                }),
            )
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_domain_by_id(&mut self, domain_id: Uuid) -> Result<(), CliError> {
        self.delete(&format!("/api/domains/{domain_id}")).await?;
        Ok(())
    }

    pub async fn verify_domain(&mut self, domain_id: Uuid) -> Result<DomainResponse, CliError> {
        let resp = self
            .post(&format!("/api/domains/{domain_id}/verify"), &serde_json::json!({}))
            .await?;
        Ok(resp.json().await?)
    }
}
