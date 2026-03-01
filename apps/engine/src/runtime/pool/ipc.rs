use std::time::Duration;

use crate::error::AppError;

/// Wait for a worker's HTTP server to become ready.
pub async fn wait_for_worker(port: u16, max_attempts: usize) -> Result<(), AppError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| AppError::Internal(format!("failed to create HTTP client: {e}")))?;

    for attempt in 0..max_attempts {
        match client
            .get(format!("http://127.0.0.1:{port}/__rift/health"))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            _ => {
                if attempt < max_attempts - 1 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    Err(AppError::Internal(format!(
        "worker on port {port} did not become ready after {max_attempts} attempts"
    )))
}

/// Get health info from a worker.
pub async fn worker_health(port: u16) -> Result<serde_json::Value, AppError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| AppError::Internal(format!("failed to create HTTP client: {e}")))?;

    let resp = client
        .get(format!("http://127.0.0.1:{port}/__rift/health"))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("health check failed: {e}")))?;

    resp.json()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse health response: {e}")))
}
