pub mod health;
pub mod process;
pub mod scaler;

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    config::Config,
    db::{deployments, env_vars},
    error::AppError,
};

use self::{
    health::wait_for_port,
    process::{allocate_port, spawn_deno_next, spawn_deno_static, spawn_node_server},
};

#[derive(Clone, Debug)]
pub struct RuntimeManager {
    inner: Arc<Mutex<RuntimeState>>,
}

#[derive(Debug)]
struct RuntimeState {
    active: HashMap<Uuid, ActiveRuntime>,
    suspended: HashMap<Uuid, SuspendedRuntime>,
}

#[derive(Debug)]
struct ActiveRuntime {
    deployment_id: Uuid,
    port: u16,
    child: Arc<Mutex<tokio::process::Child>>,
    kind: RuntimeKind,
    env_vars: Vec<(String, String)>,
    last_request: Instant,
}

/// A runtime that was killed due to inactivity but can be re-launched.
#[derive(Debug, Clone)]
struct SuspendedRuntime {
    deployment_id: Uuid,
    kind: RuntimeKind,
    env_vars: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub enum RuntimeKind {
    /// Static site served by Deno with tight sandboxed permissions.
    StaticDeno { dir: PathBuf },
    /// Next.js app: Deno runs the standalone server.js via Node compat.
    NextDeno { dir: PathBuf },
    /// Node.js SSR server (Nuxt, Astro, SvelteKit, Remix).
    NodeServer { dir: PathBuf, entry: PathBuf },
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
            inner: Arc::new(Mutex::new(RuntimeState {
                active: HashMap::new(),
                suspended: HashMap::new(),
            })),
        }
    }

    /// Deploy a project and return `(internal_url, port)`.
    ///
    /// Zero-downtime: the new process is started and health-checked before the
    /// old one is touched. After the atomic swap the old process gets a 5-second
    /// drain period before being killed.
    pub async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<(String, u16), AppError> {
        let port = allocate_port()?;

        let child = match &spec.kind {
            RuntimeKind::StaticDeno { dir } => spawn_deno_static(dir, port, &spec.env_vars)?,
            RuntimeKind::NextDeno { dir } => spawn_deno_next(dir, port, &spec.env_vars)?,
            RuntimeKind::NodeServer { dir, entry } => {
                spawn_node_server(dir, entry, port, &spec.env_vars)?
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

        let mut state = self.inner.lock().await;

        // Remove from suspended if present (fresh deploy replaces any suspended state)
        state.suspended.remove(&spec.project_id);

        // Atomic swap — old runtime returned, new one inserted.
        let old = state.active.insert(
            spec.project_id,
            ActiveRuntime {
                deployment_id: spec.deployment_id,
                port,
                child: Arc::new(Mutex::new(child)),
                kind: spec.kind,
                env_vars: spec.env_vars,
                last_request: Instant::now(),
            },
        );

        drop(state);

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
        let mut state = self.inner.lock().await;
        state.suspended.remove(&project_id);
        if let Some(runtime) = state.active.remove(&project_id) {
            let mut child = runtime.child.lock().await;
            let _ = child.kill().await;
        }
        Ok(())
    }

    pub async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.inner
            .lock()
            .await
            .active
            .get(&project_id)
            .map(|runtime| format!("http://127.0.0.1:{}", runtime.port))
    }

    pub async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.inner
            .lock()
            .await
            .active
            .get(&project_id)
            .map(|runtime| runtime.deployment_id)
    }

    /// Record a request to a project, keeping it alive for scale-to-zero.
    pub async fn touch(&self, project_id: Uuid) {
        if let Some(runtime) = self.inner.lock().await.active.get_mut(&project_id) {
            runtime.last_request = Instant::now();
        }
    }

    /// Check if a project has a suspended runtime that can be woken.
    pub async fn is_suspended(&self, project_id: Uuid) -> bool {
        self.inner.lock().await.suspended.contains_key(&project_id)
    }

    /// Wake a suspended project: re-spawn the Deno process and return the URL.
    /// Returns `None` if the project isn't suspended.
    pub async fn wake(&self, project_id: Uuid) -> Result<Option<String>, AppError> {
        let suspended = { self.inner.lock().await.suspended.remove(&project_id) };

        let suspended = match suspended {
            Some(s) => s,
            None => return Ok(None),
        };

        tracing::info!(
            project_id = %project_id,
            deployment_id = %suspended.deployment_id,
            "waking suspended deployment"
        );

        let (url, _port) = self
            .deploy(RuntimeLaunchSpec {
                project_id,
                deployment_id: suspended.deployment_id,
                kind: suspended.kind,
                env_vars: suspended.env_vars,
            })
            .await?;

        Ok(Some(url))
    }

    /// Suspend idle projects. Returns the number of projects suspended.
    /// Called by the scaler background loop.
    pub async fn suspend_idle(&self, idle_threshold: std::time::Duration) -> usize {
        let now = Instant::now();
        let mut to_suspend = Vec::new();

        {
            let state = self.inner.lock().await;
            for (&project_id, runtime) in &state.active {
                if now.duration_since(runtime.last_request) > idle_threshold {
                    to_suspend.push(project_id);
                }
            }
        }

        let mut suspended = 0;
        for project_id in to_suspend {
            let mut state = self.inner.lock().await;
            if let Some(runtime) = state.active.remove(&project_id) {
                // Store the info needed to re-launch
                state.suspended.insert(
                    project_id,
                    SuspendedRuntime {
                        deployment_id: runtime.deployment_id,
                        kind: runtime.kind.clone(),
                        env_vars: runtime.env_vars.clone(),
                    },
                );
                drop(state);

                // Kill the process
                let _ = runtime.child.lock().await.kill().await;
                tracing::info!(
                    project_id = %project_id,
                    deployment_id = %runtime.deployment_id,
                    "suspended idle deployment (scale-to-zero)"
                );
                suspended += 1;
            }
        }

        suspended
    }

    /// Restore deployments that were running before the engine restarted.
    ///
    /// Queries the DB for all `status = 'ready'` deployments (latest per project),
    /// detects the runtime kind from the filesystem, decrypts env vars, and
    /// re-launches the Deno processes.
    pub async fn restore_deployments(&self, pool: &PgPool, config: &Config) -> usize {
        let ready = match deployments::list_latest_ready_per_project(pool).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "failed to query ready deployments for restore");
                return 0;
            }
        };

        if ready.is_empty() {
            return 0;
        }

        tracing::info!(
            count = ready.len(),
            "restoring deployments from previous run"
        );
        let mut restored = 0;

        for deployment in ready {
            let workspace_dir = PathBuf::from(&config.deploy_root).join(deployment.id.to_string());
            if !workspace_dir.exists() {
                tracing::warn!(
                    deployment_id = %deployment.id,
                    project_id = %deployment.project_id,
                    "workspace directory missing, skipping restore"
                );
                continue;
            }

            // Detect runtime kind from filesystem
            let kind = if workspace_dir.join(".next/standalone").exists() {
                RuntimeKind::NextDeno {
                    dir: workspace_dir.clone(),
                }
            } else if workspace_dir.join(".output/server/index.mjs").exists() {
                let entry = workspace_dir.join(".output/server/index.mjs");
                RuntimeKind::NodeServer {
                    dir: workspace_dir,
                    entry,
                }
            } else if workspace_dir.join("dist/server/entry.mjs").exists() {
                let entry = workspace_dir.join("dist/server/entry.mjs");
                RuntimeKind::NodeServer {
                    dir: workspace_dir,
                    entry,
                }
            } else if workspace_dir.join("build/index.js").exists()
                && workspace_dir.join("build/handler.js").exists()
            {
                let entry = workspace_dir.join("build/index.js");
                RuntimeKind::NodeServer {
                    dir: workspace_dir,
                    entry,
                }
            } else if workspace_dir.join("build/server/index.js").exists() {
                let entry = workspace_dir.join("build/server/index.js");
                RuntimeKind::NodeServer {
                    dir: workspace_dir,
                    entry,
                }
            } else if let Some(entry_dir) = find_entry_ts(&workspace_dir) {
                RuntimeKind::StaticDeno { dir: entry_dir }
            } else {
                tracing::warn!(
                    deployment_id = %deployment.id,
                    "cannot detect runtime kind, skipping restore"
                );
                continue;
            };

            // Decrypt env vars
            let env_vars =
                env_vars::get_decrypted_env_vars(pool, deployment.project_id, &config.master_key)
                    .await
                    .unwrap_or_default();

            match self
                .deploy(RuntimeLaunchSpec {
                    project_id: deployment.project_id,
                    deployment_id: deployment.id,
                    kind,
                    env_vars,
                })
                .await
            {
                Ok((url, port)) => {
                    tracing::info!(
                        deployment_id = %deployment.id,
                        project_id = %deployment.project_id,
                        url = %url,
                        port = port,
                        "restored deployment"
                    );
                    restored += 1;
                }
                Err(e) => {
                    tracing::error!(
                        deployment_id = %deployment.id,
                        project_id = %deployment.project_id,
                        error = %e,
                        "failed to restore deployment"
                    );
                }
            }
        }

        restored
    }
}

/// Find the directory containing `_entry.ts` for static site deployments.
fn find_entry_ts(workspace_dir: &PathBuf) -> Option<PathBuf> {
    // Check common build output locations
    for subdir in ["", "dist", "build", "out", "public", ".output/public"] {
        let dir = if subdir.is_empty() {
            workspace_dir.clone()
        } else {
            workspace_dir.join(subdir)
        };
        if dir.join("_entry.ts").exists() {
            return Some(dir);
        }
    }
    None
}

impl Default for RuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}
