pub mod auth;
pub mod deployments;
pub mod domains;
pub mod env_vars;
pub mod logs;
pub mod projects;

use crate::config::CliConfig;
use crate::credentials::{self, Credentials};
use crate::error::CliError;

pub struct RiftClient {
    http: reqwest::Client,
    config: CliConfig,
    creds: Option<Credentials>,
}

impl RiftClient {
    pub fn new(config: CliConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to create HTTP client");

        let creds = credentials::load().ok();

        Self { http, config, creds }
    }

    pub fn api_url(&self) -> &str {
        &self.config.api_url
    }

    /// Get a valid access token, refreshing if needed.
    pub async fn access_token(&mut self) -> Result<String, CliError> {
        let creds = self.creds.as_ref().ok_or(CliError::NotAuthenticated)?;

        let now = chrono::Utc::now().timestamp();
        if creds.expires_at - now < 60 {
            self.refresh_token().await?;
        }

        Ok(self
            .creds
            .as_ref()
            .ok_or(CliError::NotAuthenticated)?
            .access_token
            .clone())
    }

    async fn refresh_token(&mut self) -> Result<(), CliError> {
        let creds = self.creds.as_ref().ok_or(CliError::SessionExpired)?;
        let refresh_token = creds.refresh_token.clone();

        let resp = self
            .http
            .post(format!("{}/api/users/refresh/cli", self.config.api_url))
            .json(&serde_json::json!({ "refresh_token": refresh_token }))
            .send()
            .await?;

        if !resp.status().is_success() {
            credentials::clear()?;
            self.creds = None;
            return Err(CliError::SessionExpired);
        }

        let body: auth::BackendSessionResponse = resp.json().await?;
        let new_creds = Credentials {
            access_token: body.access_token,
            refresh_token: body.refresh_token,
            expires_at: body.expires_at,
            user: credentials::UserInfo {
                id: body.user.id,
                email: body.user.email,
                github_login: body.user.github_login,
                display_name: body.user.display_name,
                avatar_url: body.user.avatar_url,
                created_at: body.user.created_at,
            },
        };
        credentials::save(&new_creds)?;
        self.creds = Some(new_creds);

        Ok(())
    }

    /// Make an authenticated GET request.
    pub async fn get(&mut self, path: &str) -> Result<reqwest::Response, CliError> {
        let token = self.access_token().await?;
        let resp = self
            .http
            .get(format!("{}{path}", self.config.api_url))
            .bearer_auth(&token)
            .send()
            .await?;
        check_response(resp).await
    }

    /// Make an authenticated POST request with JSON body.
    pub async fn post(
        &mut self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<reqwest::Response, CliError> {
        let token = self.access_token().await?;
        let resp = self
            .http
            .post(format!("{}{path}", self.config.api_url))
            .bearer_auth(&token)
            .json(body)
            .send()
            .await?;
        check_response(resp).await
    }

    /// Make an authenticated DELETE request.
    pub async fn delete(&mut self, path: &str) -> Result<reqwest::Response, CliError> {
        let token = self.access_token().await?;
        let resp = self
            .http
            .delete(format!("{}{path}", self.config.api_url))
            .bearer_auth(&token)
            .send()
            .await?;
        check_response(resp).await
    }

    pub fn set_credentials(&mut self, creds: Credentials) {
        self.creds = Some(creds);
    }

    pub fn credentials(&self) -> Option<&Credentials> {
        self.creds.as_ref()
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

async fn check_response(resp: reqwest::Response) -> Result<reqwest::Response, CliError> {
    if resp.status().is_success() {
        return Ok(resp);
    }

    let status = resp.status().as_u16();

    if status == 401 {
        return Err(CliError::SessionExpired);
    }

    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let message = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error")
        .to_string();

    Err(CliError::Api { status, message })
}
