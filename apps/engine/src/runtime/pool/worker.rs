use std::path::Path;
use std::time::Instant;

use tokio::process::{Child, Command};
use uuid::Uuid;

use crate::error::AppError;
use crate::runtime::process::allocate_port;

/// States a worker can be in.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerState {
    /// Process is running but has no user code loaded.
    Warm,
    /// Process has loaded a specific deployment's code.
    Specialized {
        project_id: Uuid,
        deployment_id: Uuid,
    },
}

/// A single Deno worker process managed by the pool.
pub struct Worker {
    pub id: Uuid,
    pub state: WorkerState,
    pub child: Child,
    /// Loopback port this worker listens on.
    pub port: u16,
    pub created_at: Instant,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("port", &self.port)
            .finish()
    }
}

impl Worker {
    /// Spawn a new pre-warmed Deno worker running the loader script.
    pub async fn spawn_warm(loader_script: &Path) -> Result<Self, AppError> {
        let port = allocate_port()?;
        let id = Uuid::new_v4();

        let child = Command::new("deno")
            .arg("run")
            .arg("--allow-net=127.0.0.1")
            .arg("--allow-read")
            .arg("--allow-env")
            .arg("--allow-write")
            .arg("--allow-sys")
            .arg("--unstable-detect-cjs")
            .arg("--no-prompt")
            .arg(loader_script)
            .env("RIFT_SERVE_PORT", port.to_string())
            .env("RIFT_WORKER_ID", id.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                AppError::Internal(format!("failed to spawn warm worker: {e}"))
            })?;

        Ok(Self {
            id,
            state: WorkerState::Warm,
            child,
            port,
            created_at: Instant::now(),
        })
    }

    /// Specialize this worker by loading a deployment's bundle.
    pub async fn specialize(
        &mut self,
        bundle_path: &Path,
        env_vars: &[(String, String)],
        project_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<(), AppError> {
        let env_map: serde_json::Map<String, serde_json::Value> = env_vars
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();

        let body = serde_json::json!({
            "bundle_path": bundle_path.to_string_lossy(),
            "env_vars": env_map,
            "deployment_id": deployment_id.to_string(),
            "project_id": project_id.to_string(),
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/__rift/specialize", self.port))
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                AppError::Internal(format!(
                    "failed to specialize worker {}: {e}",
                    self.id
                ))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Internal(format!(
                "worker specialization failed ({}): {text}",
                self.id
            )));
        }

        self.state = WorkerState::Specialized {
            project_id,
            deployment_id,
        };

        Ok(())
    }

    /// Check if this worker process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill this worker process.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    /// URL for forwarding requests to this worker.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}
