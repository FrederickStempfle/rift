pub mod backend;
pub mod function_registry;
pub mod health;
#[cfg(feature = "v8-isolate")]
pub mod isolate;
pub mod namespace;
pub mod pool;
pub mod process;
pub mod scaler;
pub mod seccomp;

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
    pool::sandbox::SeccompEnforcer,
    process::{allocate_port, spawn_deno_functions, spawn_deno_next, spawn_deno_static, spawn_node_server},
};

#[derive(Clone, Debug)]
pub struct RuntimeManager {
    inner: Arc<Mutex<RuntimeState>>,
    /// Global function dispatcher registry (None if not yet initialized).
    function_registry: Option<function_registry::FunctionRegistry>,
    /// Seccomp enforcer for process-level BPF filtering.
    seccomp: Option<SeccompEnforcer>,
    /// Whether to apply PID/mount namespace isolation to worker processes.
    namespace_isolate: bool,
    /// Milliseconds between health-check TCP probes.
    healthcheck_interval_ms: u64,
    /// Maximum number of health-check attempts.
    healthcheck_attempts: usize,
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
    /// Serverless functions: Deno dispatcher with per-function Web Worker isolates.
    Functions { dir: PathBuf },
    /// Hybrid framework + functions: combined Deno entry dispatches function
    /// requests to per-request Workers and falls through to the framework handler.
    Combined {
        /// Path to the generated `_rift_combined_entry.ts`.
        entry: PathBuf,
        /// Functions output directory (contains bundles, worker wrappers, routes).
        functions_dir: PathBuf,
    },
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
            function_registry: None,
            seccomp: None,
            namespace_isolate: false,
            healthcheck_interval_ms: 200,
            healthcheck_attempts: 50,
        }
    }

    /// Configure namespace isolation for spawned worker processes.
    pub fn set_namespace_isolate(&mut self, enabled: bool) {
        self.namespace_isolate = enabled;
    }

    /// Configure health-check parameters from engine config.
    pub fn set_healthcheck(&mut self, interval_ms: u64, attempts: usize) {
        self.healthcheck_interval_ms = interval_ms;
        self.healthcheck_attempts = attempts;
    }

    /// Get the current health-check interval in milliseconds.
    pub fn healthcheck_interval_ms(&self) -> u64 {
        self.healthcheck_interval_ms
    }

    /// Get the current health-check attempt count.
    pub fn healthcheck_attempts(&self) -> usize {
        self.healthcheck_attempts
    }

    /// Initialize seccomp enforcement for spawned worker processes.
    pub fn init_seccomp(&mut self, deploy_root: &std::path::Path, enforce: bool) {
        match SeccompEnforcer::init(deploy_root, enforce) {
            Ok(enforcer) => {
                tracing::info!(
                    enforce = enforcer.enforce,
                    has_profile = enforcer.profile_path.is_some(),
                    "process-level seccomp initialized"
                );
                self.seccomp = Some(enforcer);
            }
            Err(e) => {
                if enforce {
                    tracing::error!(error = %e, "seccomp enforcement required but initialization failed");
                } else {
                    tracing::warn!(error = %e, "seccomp initialization failed, continuing without enforcement");
                }
            }
        }
    }

    /// Get the seccomp profile path for process spawning.
    /// Returns None when enforcement is disabled (RIFT_SECCOMP_ENFORCE=false).
    fn seccomp_profile_path(&self) -> Option<&std::path::Path> {
        self.seccomp
            .as_ref()
            .filter(|s| s.enforce)
            .and_then(|s| s.profile_path.as_deref())
    }

    /// Set the function registry (called after global dispatcher starts).
    pub fn set_function_registry(&mut self, registry: function_registry::FunctionRegistry) {
        self.function_registry = Some(registry);
    }

    /// Get a reference to the function registry.
    pub fn function_registry(&self) -> Option<&function_registry::FunctionRegistry> {
        self.function_registry.as_ref()
    }

    /// Deploy a project and return `(internal_url, port)`.
    ///
    /// Zero-downtime: the new process is started and health-checked before the
    /// old one is touched. After the atomic swap the old process gets a 5-second
    /// drain period before being killed.
    pub async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<(String, u16), AppError> {
        // Function-only projects: register with the global dispatcher instead
        // of spawning a per-project Deno process.
        if let RuntimeKind::Functions { ref dir } = spec.kind {
            if let Some(registry) = &self.function_registry {
                // Read routes manifest
                let manifest_path = dir.join("_routes.json");
                let routes = if manifest_path.exists() {
                    let content = tokio::fs::read_to_string(&manifest_path)
                        .await
                        .map_err(|e| {
                            AppError::Internal(format!("failed to read _routes.json: {e}"))
                        })?;
                    serde_json::from_str(&content).map_err(|e| {
                        AppError::Internal(format!("failed to parse _routes.json: {e}"))
                    })?
                } else {
                    Vec::new()
                };

                let output_dir = dir.to_string_lossy().to_string();

                // Unregister old version (if any)
                let _ = registry.unregister(spec.project_id).await;

                // Register new routes with global dispatcher
                registry
                    .register(
                        spec.project_id,
                        spec.deployment_id,
                        &routes,
                        &spec.env_vars,
                        &output_dir,
                    )
                    .await?;

                let url = registry.dispatcher_url();
                let port = 0; // No port allocated for function-only projects
                return Ok((url, port));
            }
            // Fall through to legacy per-project process if no registry
        }

        let port = allocate_port()?;
        let seccomp_path = self.seccomp_profile_path();
        let ns = self.namespace_isolate;

        let child = match &spec.kind {
            RuntimeKind::StaticDeno { dir } => spawn_deno_static(dir, port, &spec.env_vars, seccomp_path, ns)?,
            RuntimeKind::NextDeno { dir } => spawn_deno_next(dir, port, &spec.env_vars, seccomp_path, ns)?,
            RuntimeKind::NodeServer { dir, entry } => {
                spawn_node_server(dir, entry, port, &spec.env_vars, seccomp_path, ns)?
            }
            RuntimeKind::Functions { dir } => spawn_deno_functions(dir, port, &spec.env_vars, seccomp_path, ns)?,
            RuntimeKind::Combined { entry, functions_dir } => {
                process::spawn_deno_combined(entry, functions_dir, port, &spec.env_vars, seccomp_path, ns)?
            }
        };

        if !wait_for_port("127.0.0.1", port, self.healthcheck_attempts, self.healthcheck_interval_ms).await {
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
        // Unregister from global dispatcher if function-only
        if let Some(registry) = &self.function_registry {
            if registry.is_function_project(project_id).await {
                return registry.unregister(project_id).await;
            }
        }

        let mut state = self.inner.lock().await;
        state.suspended.remove(&project_id);
        if let Some(runtime) = state.active.remove(&project_id) {
            let mut child = runtime.child.lock().await;
            let _ = child.kill().await;
        }
        Ok(())
    }

    pub async fn active_url(&self, project_id: Uuid) -> Option<String> {
        // Function-only projects are always active via the global dispatcher
        if let Some(registry) = &self.function_registry {
            if registry.is_function_project(project_id).await {
                return Some(registry.dispatcher_url());
            }
        }

        self.inner
            .lock()
            .await
            .active
            .get(&project_id)
            .map(|runtime| format!("http://127.0.0.1:{}", runtime.port))
    }

    pub async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        // Check function registry first
        if let Some(registry) = &self.function_registry {
            if let Some(dep_id) = registry.deployment_id(project_id).await {
                return Some(dep_id);
            }
        }

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
            let kind = if workspace_dir.join("_rift_functions_output/_rift_combined_entry.ts").exists() {
                let fn_dir = workspace_dir.join("_rift_functions_output");
                RuntimeKind::Combined {
                    entry: fn_dir.join("_rift_combined_entry.ts"),
                    functions_dir: fn_dir,
                }
            } else if workspace_dir.join(".next/standalone").exists() {
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
            } else if workspace_dir.join("_rift_functions_output/bundles").is_dir() {
                let fn_dir = workspace_dir.join("_rift_functions_output");
                RuntimeKind::Functions { dir: fn_dir }
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

            // Function-only projects: register with global dispatcher (no process needed)
            if let RuntimeKind::Functions { ref dir } = kind {
                if let Some(registry) = &self.function_registry {
                    let manifest_path = dir.join("_routes.json");
                    let routes: Vec<crate::build::functions::FunctionRoute> =
                        if manifest_path.exists() {
                            match tokio::fs::read_to_string(&manifest_path).await {
                                Ok(content) => {
                                    serde_json::from_str(&content).unwrap_or_default()
                                }
                                Err(_) => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };

                    if routes.is_empty() {
                        tracing::warn!(
                            deployment_id = %deployment.id,
                            "no routes manifest found, skipping function restore"
                        );
                        continue;
                    }

                    let output_dir = dir.to_string_lossy().to_string();
                    match registry
                        .register(
                            deployment.project_id,
                            deployment.id,
                            &routes,
                            &env_vars,
                            &output_dir,
                        )
                        .await
                    {
                        Ok(()) => {
                            tracing::info!(
                                deployment_id = %deployment.id,
                                project_id = %deployment.project_id,
                                routes = routes.len(),
                                "restored function deployment via global dispatcher"
                            );
                            restored += 1;
                        }
                        Err(e) => {
                            tracing::error!(
                                deployment_id = %deployment.id,
                                project_id = %deployment.project_id,
                                error = %e,
                                "failed to restore function deployment"
                            );
                        }
                    }
                    continue;
                }
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_manager_default_has_no_seccomp() {
        let rm = RuntimeManager::new();
        assert!(rm.seccomp.is_none());
        assert!(rm.seccomp_profile_path().is_none());
    }
}
