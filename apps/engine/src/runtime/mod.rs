pub mod health;
pub mod process;
pub mod scaler;

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::AppError;

use self::{
    health::wait_for_port,
    process::{allocate_port, spawn_deno_next, spawn_deno_static},
};

#[derive(Clone, Debug)]
pub struct RuntimeManager {
    inner: Arc<Mutex<HashMap<Uuid, ActiveRuntime>>>,
}

#[derive(Debug)]
struct ActiveRuntime {
    deployment_id: Uuid,
    port: u16,
    child: Arc<Mutex<tokio::process::Child>>,
}

#[derive(Clone, Debug)]
pub enum RuntimeKind {
    /// Static site served by Deno with tight sandboxed permissions.
    StaticDeno { dir: PathBuf },
    /// Next.js app: Deno runs the standalone server.js via Node compat.
    NextDeno { dir: PathBuf },
}

#[derive(Clone, Debug)]
pub struct RuntimeLaunchSpec {
    pub project_id: Uuid,
    pub deployment_id: Uuid,
    pub kind: RuntimeKind,
    /// User-defined environment variables injected at runtime.
    pub env_vars: Vec<(String, String)>,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Deploy a project and return `(internal_url, port)`.
    ///
    /// Zero-downtime: the new process is started and health-checked before the
    /// old one is touched. After the atomic swap the old process gets a 5-second
    /// drain period before being killed.
    pub async fn deploy(
        &self,
        spec: RuntimeLaunchSpec,
    ) -> Result<(String, u16), AppError> {
        let port = allocate_port()?;

        let child = match &spec.kind {
            RuntimeKind::StaticDeno { dir } => {
                spawn_deno_static(dir, port, &spec.env_vars)?
            }
            RuntimeKind::NextDeno { dir } => {
                spawn_deno_next(dir, port, &spec.env_vars)?
            }
        };

        if !wait_for_port("127.0.0.1", port, 40).await {
            // New process failed health check — kill it, leave old running.
            let mut child = child;
            let _ = child.kill().await;
            return Err(AppError::Internal(
                "runtime failed to become healthy".into(),
            ));
        }

        // Atomic swap — old runtime returned, new one inserted.
        let old = self.inner.lock().await.insert(
            spec.project_id,
            ActiveRuntime {
                deployment_id: spec.deployment_id,
                port,
                child: Arc::new(Mutex::new(child)),
            },
        );

        // Graceful drain: give the old process 5 seconds to finish in-flight
        // requests before killing it.
        if let Some(old_runtime) = old {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let _ = old_runtime.child.lock().await.kill().await;
            });
        }

        Ok((format!("http://127.0.0.1:{port}"), port))
    }

    pub async fn stop_project(&self, project_id: Uuid) -> Result<(), AppError> {
        if let Some(runtime) = self.inner.lock().await.remove(&project_id) {
            let mut child = runtime.child.lock().await;
            let _ = child.kill().await;
        }
        Ok(())
    }

    pub async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.inner
            .lock()
            .await
            .get(&project_id)
            .map(|runtime| format!("http://127.0.0.1:{}", runtime.port))
    }

    pub async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.inner
            .lock()
            .await
            .get(&project_id)
            .map(|runtime| runtime.deployment_id)
    }
}

impl Default for RuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}
