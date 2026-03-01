use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CliError;

use super::RiftClient;

#[derive(Debug, Serialize, Deserialize)]
pub struct EnvVarResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub preview: String,
}

impl RiftClient {
    pub async fn list_env_vars(
        &mut self,
        project_id: Uuid,
    ) -> Result<Vec<EnvVarResponse>, CliError> {
        let resp = self
            .get(&format!("/api/env-vars?project_id={project_id}"))
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn create_env_var(
        &mut self,
        project_id: Uuid,
        key: &str,
        value: &str,
    ) -> Result<EnvVarResponse, CliError> {
        let resp = self
            .post(
                "/api/env-vars",
                &serde_json::json!({
                    "project_id": project_id,
                    "key": key,
                    "value": value,
                }),
            )
            .await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_env_var(
        &mut self,
        env_var_id: Uuid,
        project_id: Uuid,
    ) -> Result<(), CliError> {
        self.delete(&format!(
            "/api/env-vars/{env_var_id}?project_id={project_id}"
        ))
        .await?;
        Ok(())
    }
}
