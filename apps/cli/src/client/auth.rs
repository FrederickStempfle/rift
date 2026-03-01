use serde::{Deserialize, Serialize};

use crate::credentials::{self, Credentials, UserInfo};
use crate::error::CliError;

use super::RiftClient;

#[derive(Debug, Serialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct BackendSessionResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub user: UserResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserResponse {
    pub id: uuid::Uuid,
    pub email: String,
    pub github_login: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl RiftClient {
    pub async fn login(&mut self, email: &str, password: &str) -> Result<Credentials, CliError> {
        let body = LoginRequest {
            email: email.to_string(),
            password: password.to_string(),
        };

        let resp = self
            .http()
            .post(format!("{}/api/users/login/cli", self.api_url()))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let message = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("login failed")
                .to_string();
            return Err(CliError::Api { status, message });
        }

        let session: BackendSessionResponse = resp.json().await?;

        let creds = Credentials {
            access_token: session.access_token,
            refresh_token: session.refresh_token,
            expires_at: session.expires_at,
            user: UserInfo {
                id: session.user.id,
                email: session.user.email,
                github_login: session.user.github_login,
                display_name: session.user.display_name,
                avatar_url: session.user.avatar_url,
                created_at: session.user.created_at,
            },
        };

        credentials::save(&creds)?;
        self.set_credentials(creds.clone());

        Ok(creds)
    }

    pub async fn logout(&mut self) -> Result<(), CliError> {
        if let Some(creds) = self.credentials() {
            let refresh_token = creds.refresh_token.clone();
            let _ = self
                .http()
                .post(format!("{}/api/users/logout/cli", self.api_url()))
                .json(&serde_json::json!({ "refresh_token": refresh_token }))
                .send()
                .await;
        }

        credentials::clear()?;
        Ok(())
    }

    pub async fn whoami(&mut self) -> Result<UserResponse, CliError> {
        let resp = self.get("/api/users/me").await?;
        Ok(resp.json().await?)
    }
}
