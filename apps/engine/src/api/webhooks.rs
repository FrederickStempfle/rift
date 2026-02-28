use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::{
    api::AppState,
    db::projects,
    error::AppError,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/github", post(handle_github_webhook))
}

async fn handle_github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if event == "ping" {
        return Ok((StatusCode::OK, Json(json!({ "status": "pong" }))));
    }

    if event != "push" {
        return Ok((StatusCode::OK, Json(json!({ "status": "ignored", "event": event }))));
    }

    let payload: Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON: {e}")))?;

    let repo_url = payload
        .pointer("/repository/clone_url")
        .or_else(|| payload.pointer("/repository/html_url"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let push_ref = payload
        .get("ref")
        .and_then(Value::as_str)
        .unwrap_or("");

    // refs/heads/main -> main
    let branch = push_ref.strip_prefix("refs/heads/").unwrap_or(push_ref);

    if repo_url.is_empty() || branch.is_empty() {
        return Ok((StatusCode::OK, Json(json!({ "status": "ignored", "reason": "missing repo or branch" }))));
    }

    // Normalize URL for matching (strip .git suffix, lowercase)
    let normalized = normalize_repo_url(repo_url);

    let project = match projects::find_project_by_repo_and_branch(&state.pool, &normalized, branch).await? {
        Some(p) => p,
        None => {
            // Also try with .git suffix
            let with_git = format!("{normalized}.git");
            match projects::find_project_by_repo_and_branch(&state.pool, &with_git, branch).await? {
                Some(p) => p,
                None => {
                    return Ok((StatusCode::OK, Json(json!({
                        "status": "ignored",
                        "reason": "no matching project"
                    }))));
                }
            }
        }
    };

    // Verify webhook signature if project has a secret
    if let Some(secret) = &project.webhook_secret {
        let signature = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_signature(secret, &body, signature) {
            return Err(AppError::Forbidden("invalid webhook signature".into()));
        }
    }

    tracing::info!(
        project_id = %project.id,
        branch = %branch,
        "webhook triggered build"
    );

    let deployment = state
        .build_manager
        .enqueue_project_build(project.clone())
        .await?;

    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "build_queued",
            "deployment_id": deployment.id,
            "project": project.name,
        })),
    ))
}

fn normalize_repo_url(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase()
}

fn verify_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    let expected = match signature.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);

    let computed = hex::encode(mac.finalize().into_bytes());
    constant_time_eq(computed.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
