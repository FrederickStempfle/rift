use serde::Deserialize;

use crate::error::AppError;

#[derive(Debug, Deserialize)]
struct WebhookResponse {
    id: i64,
}

/// Register a push webhook on a GitHub repo. Returns the webhook ID.
pub async fn register_webhook(
    token: &str,
    owner: &str,
    repo: &str,
    webhook_url: &str,
    secret: &str,
) -> Result<i64, AppError> {
    let client = reqwest::Client::new();
    let url = format!("https://api.github.com/repos/{owner}/{repo}/hooks");

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "rift-engine")
        .json(&serde_json::json!({
            "name": "web",
            "active": true,
            "events": ["push"],
            "config": {
                "url": webhook_url,
                "content_type": "json",
                "secret": secret,
                "insecure_ssl": "0"
            }
        }))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to register webhook: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(%status, %body, "GitHub webhook registration failed");
        return Err(AppError::Internal(format!(
            "GitHub webhook registration failed ({status}): {body}"
        )));
    }

    let hook: WebhookResponse = response
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse webhook response: {e}")))?;

    Ok(hook.id)
}

/// Delete a webhook from a GitHub repo.
pub async fn delete_webhook(
    token: &str,
    owner: &str,
    repo: &str,
    webhook_id: i64,
) -> Result<(), AppError> {
    let client = reqwest::Client::new();
    let url = format!("https://api.github.com/repos/{owner}/{repo}/hooks/{webhook_id}");

    let response = client
        .delete(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "rift-engine")
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete webhook: {e}")))?;

    if !response.status().is_success() && response.status().as_u16() != 404 {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(%status, %body, "GitHub webhook deletion failed");
    }

    Ok(())
}

/// Parse "owner/repo" from a GitHub URL like "https://github.com/owner/repo"
pub fn parse_owner_repo(repo_url: &str) -> Option<(String, String)> {
    let url = repo_url.trim_end_matches('/').trim_end_matches(".git");

    let path = url.strip_prefix("https://github.com/")?;
    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?.to_owned();
    let repo = parts.next()?.to_owned();

    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some((owner, repo))
}
