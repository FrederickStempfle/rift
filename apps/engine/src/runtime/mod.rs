pub mod health;
pub mod process;
pub mod scaler;

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::AppError;

use self::{
    health::wait_for_port,
    process::{allocate_port, spawn_shell},
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
    StaticDir { dir: PathBuf },
    NextApp { dir: PathBuf },
}

#[derive(Clone, Debug)]
pub struct RuntimeLaunchSpec {
    pub project_id: Uuid,
    pub deployment_id: Uuid,
    pub kind: RuntimeKind,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Deploy a project and return `(internal_url, port)`.
    pub async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<(String, u16), AppError> {
        self.stop_project(spec.project_id).await?;

        let port = allocate_port()?;
        let child = match &spec.kind {
            RuntimeKind::StaticDir { dir } => spawn_shell(
                &format!("serve -s '{}' -l {port} -n", dir.display()),
                dir,
                &[],
            )?,
            RuntimeKind::NextApp { dir } => spawn_shell(
                &format!("npx next start -H 0.0.0.0 -p {port}"),
                dir,
                &[
                    ("PORT", port.to_string()),
                    ("HOSTNAME", "0.0.0.0".to_owned()),
                    ("NODE_ENV", "production".to_owned()),
                ],
            )?,
        };

        if !wait_for_port("127.0.0.1", port, 40).await {
            return Err(AppError::Internal(
                "runtime failed to become healthy".into(),
            ));
        }

        let child = Arc::new(Mutex::new(child));
        self.inner.lock().await.insert(
            spec.project_id,
            ActiveRuntime {
                deployment_id: spec.deployment_id,
                port,
                child,
            },
        );

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
