pub mod ipc;
pub mod limits;
pub mod sandbox;
pub mod worker;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::Config;
use crate::db::{deployments, env_vars};
use crate::error::AppError;
use crate::runtime::policy::{EnforcementMode, RuntimePolicy};
use crate::runtime::{RuntimeKind, RuntimeLaunchSpec};

use self::ipc::wait_for_worker;
use self::sandbox::SeccompEnforcer;
use self::worker::Worker;

/// Configuration for the worker pool.
#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// Number of pre-warmed workers to maintain.
    pub warm_pool_size: usize,
    /// Maximum number of specialized (active) workers.
    pub max_active_workers: usize,
    /// Worker idle timeout before returning to pool.
    pub idle_timeout: Duration,
    /// Maximum memory per worker (bytes).
    pub worker_memory_limit: u64,
    /// Path to the worker loader script.
    pub loader_script: PathBuf,
    /// Base directory for deployment bundles.
    pub deploy_root: PathBuf,
    /// Whether to enforce seccomp on worker processes.
    pub seccomp_enforce: bool,
}

/// Tracks a worker that has been specialized for a project.
struct ActiveAssignment {
    worker: Worker,
    deployment_id: Uuid,
    kind: RuntimeKind,
    env_vars: Vec<(String, String)>,
    last_request: Instant,
    bundle_path: PathBuf,
}

/// Stored info for a suspended deployment that can be re-specialized.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct SuspendedInfo {
    deployment_id: Uuid,
    kind: RuntimeKind,
    env_vars: Vec<(String, String)>,
    bundle_path: PathBuf,
}

/// Pre-warmed Deno worker pool with specialization.
pub struct WorkerPool {
    config: PoolConfig,
    /// Workers ready to be specialized.
    warm: Mutex<Vec<Worker>>,
    /// Workers specialized for a specific project (keyed by project_id).
    active: Mutex<HashMap<Uuid, ActiveAssignment>>,
    /// Deployments that were active but whose workers were reclaimed.
    suspended: Mutex<HashMap<Uuid, SuspendedInfo>>,
    /// Seccomp enforcement state.
    seccomp: SeccompEnforcer,
    /// Database pool for persisting suspend/wake state transitions.
    db_pool: Option<sqlx::PgPool>,
    /// Resource enforcement mode (strict vs best-effort).
    enforcement_mode: EnforcementMode,
    /// Default runtime policy for workers.
    default_policy: RuntimePolicy,
}

impl WorkerPool {
    /// Create a new pool and pre-warm workers.
    pub async fn new(
        config: PoolConfig,
        db_pool: Option<sqlx::PgPool>,
        enforcement_mode: EnforcementMode,
        default_policy: RuntimePolicy,
    ) -> Result<Arc<Self>, AppError> {
        // Initialize cgroup base directory if available
        if let Err(e) = limits::ensure_base_cgroup() {
            if enforcement_mode == EnforcementMode::Strict {
                return Err(AppError::Internal(format!(
                    "cgroup setup required but failed: {e}"
                )));
            }
            tracing::warn!(error = %e, "cgroup setup failed, resource limits disabled");
        }

        // Initialize seccomp enforcement
        let seccomp = SeccompEnforcer::init(&config.deploy_root, config.seccomp_enforce)?;

        let pool = Arc::new(Self {
            config: config.clone(),
            warm: Mutex::new(Vec::new()),
            active: Mutex::new(HashMap::new()),
            suspended: Mutex::new(HashMap::new()),
            seccomp,
            db_pool,
            enforcement_mode,
            default_policy,
        });

        // Pre-warm workers in background
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            pool_clone.replenish_warm_pool().await;
        });

        Ok(pool)
    }

    /// Ensure we have `warm_pool_size` warm workers ready.
    async fn replenish_warm_pool(&self) {
        let mut warm = self.warm.lock().await;
        let needed = self.config.warm_pool_size.saturating_sub(warm.len());
        let seccomp_path = self.seccomp.profile_path.as_deref();

        for _ in 0..needed {
            match Worker::spawn_warm(&self.config.loader_script, seccomp_path).await {
                Ok(mut worker) => {
                    // Wait for the worker to become ready
                    match wait_for_worker(worker.port, 30).await {
                        Ok(()) => {
                            // Apply cgroup resource limits via policy enforcement
                            if let Some(pid) = worker.child.id() {
                                let resource_limits = self.default_policy.to_resource_limits();
                                if let Err(e) = super::policy::enforce_cgroup_limits(
                                    &worker.id,
                                    pid,
                                    &resource_limits,
                                    self.enforcement_mode,
                                ) {
                                    tracing::error!(
                                        worker_id = %worker.id,
                                        error = %e,
                                        "resource enforcement failed, killing worker"
                                    );
                                    worker.kill().await;
                                    continue;
                                }
                            }

                            tracing::debug!(
                                worker_id = %worker.id,
                                port = worker.port,
                                "pre-warmed worker ready"
                            );
                            warm.push(worker);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "warm worker failed health check, killing");
                            worker.kill().await;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to spawn warm worker");
                }
            }
        }
    }

    /// Get a warm worker from the pool, spawning one if none available.
    async fn acquire_warm_worker(self: &Arc<Self>) -> Result<Worker, AppError> {
        // Try to get from warm pool
        {
            let mut warm = self.warm.lock().await;
            if let Some(worker) = warm.pop() {
                // Replenish in background
                let pool = Arc::clone(self);
                tokio::spawn(async move {
                    pool.replenish_warm_pool().await;
                });
                return Ok(worker);
            }
        }

        // No warm workers — spawn a new one on demand
        tracing::warn!("no warm workers available, spawning on demand");
        let seccomp_path = self.seccomp.profile_path.as_deref();
        let mut worker = Worker::spawn_warm(&self.config.loader_script, seccomp_path).await?;
        wait_for_worker(worker.port, 40).await.inspect_err(|_| {
            // Kill the failed worker — fire-and-forget since we're propagating the error.
            drop(worker.child.kill());
        })?;

        // Apply the same resource enforcement as pre-warmed workers so
        // on-demand paths can't bypass cgroup policy.
        if let Some(pid) = worker.child.id() {
            let resource_limits = self.default_policy.to_resource_limits();
            if let Err(e) = super::policy::enforce_cgroup_limits(
                &worker.id,
                pid,
                &resource_limits,
                self.enforcement_mode,
            ) {
                tracing::error!(
                    worker_id = %worker.id,
                    error = %e,
                    "resource enforcement failed for on-demand worker"
                );
                worker.kill().await;
                return Err(e.into());
            }
        }

        Ok(worker)
    }

    /// Resolve the bundle entry path for a given RuntimeKind.
    fn resolve_bundle_path(&self, deployment_id: Uuid, kind: &RuntimeKind) -> PathBuf {
        let deploy_dir = self.config.deploy_root.join(deployment_id.to_string());
        match kind {
            RuntimeKind::StaticDeno { dir } => dir.join("_entry.ts"),
            RuntimeKind::Functions { dir } => dir.join("_entry.ts"),
            RuntimeKind::Combined { entry, .. } => entry.clone(),
            RuntimeKind::NextDeno { .. } => {
                // For pool mode, we use a wrapper entry that starts Next.js internally
                deploy_dir.join("_rift_pool_entry.ts")
            }
            RuntimeKind::NodeServer { .. } => {
                // For pool mode, we use a wrapper that starts the Node server internally
                deploy_dir.join("_rift_pool_entry.ts")
            }
        }
    }

    /// Deploy a new runtime: specialize a warm worker with the deployment's code.
    pub async fn deploy(
        self: &Arc<Self>,
        spec: RuntimeLaunchSpec,
    ) -> Result<(String, u16), AppError> {
        // Enforce max_active_workers capacity limit.
        // An existing deployment for the same project is being replaced (not additive),
        // so only reject if the project is genuinely new and we're at capacity.
        {
            let active = self.active.lock().await;
            if !active.contains_key(&spec.project_id)
                && active.len() >= self.config.max_active_workers
            {
                return Err(super::policy::ResourceError::PoolCapacityExceeded {
                    active: active.len(),
                    max: self.config.max_active_workers,
                }
                .into());
            }
        }

        let bundle_path = self.resolve_bundle_path(spec.deployment_id, &spec.kind);

        // For static sites, the bundle is the existing _entry.ts
        // For SSR frameworks, we need to generate a pool wrapper (Phase 3)
        // For now, fall back to the existing entry if pool wrapper doesn't exist
        let effective_path = if bundle_path.exists() {
            bundle_path.clone()
        } else {
            // Fallback to the standard entry for the runtime kind
            match &spec.kind {
                RuntimeKind::StaticDeno { dir } => dir.join("_entry.ts"),
                RuntimeKind::NextDeno { dir } => {
                    let standalone = dir.join(".next/standalone");
                    if standalone.join("server.js").exists() {
                        standalone.join("server.js")
                    } else {
                        find_server_js_recursive(&standalone)
                            .unwrap_or_else(|| standalone.join("server.js"))
                    }
                }
                RuntimeKind::NodeServer { entry, .. } => entry.clone(),
                RuntimeKind::Functions { dir } => dir.join("_entry.ts"),
                RuntimeKind::Combined { entry, .. } => entry.clone(),
            }
        };

        let mut worker = self.acquire_warm_worker().await?;

        // Specialize the worker
        worker
            .specialize(
                &effective_path,
                &spec.env_vars,
                spec.project_id,
                spec.deployment_id,
            )
            .await?;

        let url = worker.url();
        let port = worker.port;

        // Remove from suspended if present
        self.suspended.lock().await.remove(&spec.project_id);

        // Swap out old active worker if any
        let mut active = self.active.lock().await;
        let old = active.insert(
            spec.project_id,
            ActiveAssignment {
                worker,
                deployment_id: spec.deployment_id,
                kind: spec.kind,
                env_vars: spec.env_vars,
                last_request: Instant::now(),
                bundle_path,
            },
        );
        drop(active);

        // Graceful drain: kill old worker after 5s
        if let Some(mut old_assignment) = old {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                old_assignment.worker.kill().await;
            });
        }

        Ok((url, port))
    }

    /// Stop a project's runtime.
    pub async fn stop_project(&self, project_id: Uuid) -> Result<(), AppError> {
        self.suspended.lock().await.remove(&project_id);
        if let Some(mut assignment) = self.active.lock().await.remove(&project_id) {
            let worker_id = assignment.worker.id;
            assignment.worker.kill().await;
            super::policy::release_cgroup(&worker_id);
        }
        Ok(())
    }

    /// Get the internal URL for an active runtime.
    pub async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.active
            .lock()
            .await
            .get(&project_id)
            .map(|a| a.worker.url())
    }

    /// Get the deployment ID for an active runtime.
    pub async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.active
            .lock()
            .await
            .get(&project_id)
            .map(|a| a.deployment_id)
    }

    /// Explicitly suspend a single project's runtime.
    /// Returns `true` if the project was active and is now suspended.
    pub async fn suspend_project(&self, project_id: Uuid) -> Result<bool, AppError> {
        let mut active = self.active.lock().await;
        if let Some(mut assignment) = active.remove(&project_id) {
            self.suspended.lock().await.insert(
                project_id,
                SuspendedInfo {
                    deployment_id: assignment.deployment_id,
                    kind: assignment.kind.clone(),
                    env_vars: assignment.env_vars.clone(),
                    bundle_path: assignment.bundle_path.clone(),
                },
            );
            drop(active);

            let worker_id = assignment.worker.id;
            assignment.worker.kill().await;
            super::policy::release_cgroup(&worker_id);

            // Persist to DB
            if let Some(ref db) = self.db_pool {
                if let Err(e) = deployments::mark_suspended(db, assignment.deployment_id).await {
                    tracing::warn!(
                        deployment_id = %assignment.deployment_id,
                        error = %e,
                        "failed to persist suspended state to DB"
                    );
                }
            }

            tracing::info!(
                project_id = %project_id,
                deployment_id = %assignment.deployment_id,
                "explicitly suspended deployment (pool)"
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Record a request timestamp for scale-to-zero tracking.
    pub async fn touch(&self, project_id: Uuid) {
        if let Some(assignment) = self.active.lock().await.get_mut(&project_id) {
            assignment.last_request = Instant::now();
        }
    }

    /// Check if a project has a suspended runtime.
    pub async fn is_suspended(&self, project_id: Uuid) -> bool {
        self.suspended.lock().await.contains_key(&project_id)
    }

    /// Wake a suspended project by re-specializing a warm worker.
    pub async fn wake(self: &Arc<Self>, project_id: Uuid) -> Result<Option<String>, AppError> {
        let suspended = { self.suspended.lock().await.remove(&project_id) };

        let suspended = match suspended {
            Some(s) => s,
            None => return Ok(None),
        };

        tracing::info!(
            project_id = %project_id,
            deployment_id = %suspended.deployment_id,
            "waking suspended deployment (pool)"
        );

        let deployment_id = suspended.deployment_id;
        let (url, _) = self
            .deploy(RuntimeLaunchSpec {
                project_id,
                deployment_id,
                kind: suspended.kind,
                env_vars: suspended.env_vars,
            })
            .await?;

        // Persist wake to DB
        if let Some(ref db) = self.db_pool {
            if let Err(e) = deployments::mark_ready_from_suspended(db, deployment_id).await {
                tracing::warn!(
                    deployment_id = %deployment_id,
                    error = %e,
                    "failed to persist wake state to DB"
                );
            }
        }

        Ok(Some(url))
    }

    /// Suspend idle projects. Returns count of suspended.
    pub async fn suspend_idle(&self, idle_threshold: Duration) -> usize {
        let now = Instant::now();
        let mut to_suspend = Vec::new();

        {
            let active = self.active.lock().await;
            for (&project_id, assignment) in active.iter() {
                if now.duration_since(assignment.last_request) > idle_threshold {
                    to_suspend.push(project_id);
                }
            }
        }

        let mut suspended_count = 0;
        for project_id in to_suspend {
            let mut active = self.active.lock().await;
            if let Some(mut assignment) = active.remove(&project_id) {
                self.suspended.lock().await.insert(
                    project_id,
                    SuspendedInfo {
                        deployment_id: assignment.deployment_id,
                        kind: assignment.kind.clone(),
                        env_vars: assignment.env_vars.clone(),
                        bundle_path: assignment.bundle_path.clone(),
                    },
                );
                drop(active);

                assignment.worker.kill().await;

                // Persist to DB
                if let Some(ref db) = self.db_pool {
                    if let Err(e) = deployments::mark_suspended(db, assignment.deployment_id).await
                    {
                        tracing::warn!(
                            deployment_id = %assignment.deployment_id,
                            error = %e,
                            "failed to persist suspended state to DB"
                        );
                    }
                }

                tracing::info!(
                    project_id = %project_id,
                    deployment_id = %assignment.deployment_id,
                    "suspended idle deployment (pool, scale-to-zero)"
                );
                suspended_count += 1;
            }
        }

        // Replenish warm pool after suspending
        if suspended_count > 0 {
            self.replenish_warm_pool().await;
        }

        suspended_count
    }

    /// Restore deployments from database after restart.
    pub async fn restore_deployments(&self, pool: &sqlx::PgPool, config: &Config) -> usize {
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
            "restoring deployments from previous run (pool mode)"
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

            let kind = detect_runtime_kind(&workspace_dir);
            let kind = match kind {
                Some(k) => k,
                None => {
                    tracing::warn!(
                        deployment_id = %deployment.id,
                        "cannot detect runtime kind, skipping restore"
                    );
                    continue;
                }
            };

            let user_env_vars =
                env_vars::get_decrypted_env_vars(pool, deployment.project_id, &config.master_key)
                    .await
                    .unwrap_or_default();

            // Store as suspended — will be woken on first request (lazy restore)
            self.suspended.lock().await.insert(
                deployment.project_id,
                SuspendedInfo {
                    deployment_id: deployment.id,
                    kind: kind.clone(),
                    env_vars: user_env_vars,
                    bundle_path: self.resolve_bundle_path(deployment.id, &kind),
                },
            );

            tracing::info!(
                deployment_id = %deployment.id,
                project_id = %deployment.project_id,
                "registered deployment for lazy restore (pool)"
            );
            restored += 1;
        }

        // Also restore suspended deployments
        let suspended_list = match deployments::list_latest_suspended_per_project(pool).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "failed to query suspended deployments for restore");
                Vec::new()
            }
        };

        if !suspended_list.is_empty() {
            tracing::info!(
                count = suspended_list.len(),
                "restoring suspended deployments (pool)"
            );
        }

        for deployment in suspended_list {
            let workspace_dir = PathBuf::from(&config.deploy_root).join(deployment.id.to_string());
            if !workspace_dir.exists() {
                continue;
            }

            let kind = match detect_runtime_kind(&workspace_dir) {
                Some(k) => k,
                None => continue,
            };

            let user_env_vars =
                env_vars::get_decrypted_env_vars(pool, deployment.project_id, &config.master_key)
                    .await
                    .unwrap_or_default();

            self.suspended.lock().await.insert(
                deployment.project_id,
                SuspendedInfo {
                    deployment_id: deployment.id,
                    kind: kind.clone(),
                    env_vars: user_env_vars,
                    bundle_path: self.resolve_bundle_path(deployment.id, &kind),
                },
            );

            tracing::info!(
                deployment_id = %deployment.id,
                project_id = %deployment.project_id,
                "restored suspended deployment (pool)"
            );
            restored += 1;
        }

        restored
    }

    /// Spawn a background health monitor that periodically checks active workers
    /// and replaces dead ones. Also prunes dead warm workers.
    pub fn spawn_health_monitor(self: &Arc<Self>) {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            // Let workers settle before first check
            tokio::time::sleep(Duration::from_secs(30)).await;

            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;

                // Check warm workers — remove dead ones
                {
                    let mut warm = pool.warm.lock().await;
                    let before = warm.len();
                    warm.retain_mut(|w| w.is_alive());
                    let removed = before - warm.len();
                    if removed > 0 {
                        tracing::warn!(count = removed, "removed dead warm workers");
                    }
                }

                // Check active workers — mark crashed ones as suspended for re-wake
                let mut crashed = Vec::new();
                {
                    let mut active = pool.active.lock().await;
                    let project_ids: Vec<Uuid> = active.keys().copied().collect();
                    for project_id in project_ids {
                        if let Some(assignment) = active.get_mut(&project_id) {
                            if !assignment.worker.is_alive() {
                                tracing::error!(
                                    project_id = %project_id,
                                    deployment_id = %assignment.deployment_id,
                                    worker_id = %assignment.worker.id,
                                    "active worker crashed, will suspend for re-wake"
                                );
                                crashed.push(project_id);
                            }
                        }
                    }
                }

                // Move crashed workers to suspended so they can be re-woken on next request
                for project_id in crashed {
                    let mut active = pool.active.lock().await;
                    if let Some(assignment) = active.remove(&project_id) {
                        let worker_id = assignment.worker.id;
                        super::policy::release_cgroup(&worker_id);

                        pool.suspended.lock().await.insert(
                            project_id,
                            SuspendedInfo {
                                deployment_id: assignment.deployment_id,
                                kind: assignment.kind,
                                env_vars: assignment.env_vars,
                                bundle_path: assignment.bundle_path,
                            },
                        );
                    }
                }

                // Replenish warm pool if needed
                pool.replenish_warm_pool().await;
            }
        });
    }

    /// Gracefully shut down all workers. Called on SIGTERM/SIGINT.
    pub async fn shutdown(&self) {
        tracing::info!("shutting down worker pool");

        // Kill all active workers with a grace period
        let mut active = self.active.lock().await;
        for (project_id, mut assignment) in active.drain() {
            tracing::debug!(
                project_id = %project_id,
                "shutting down active worker"
            );
            assignment.worker.kill().await;
            super::policy::release_cgroup(&assignment.worker.id);
        }
        drop(active);

        // Kill all warm workers
        let mut warm = self.warm.lock().await;
        for mut worker in warm.drain(..) {
            worker.kill().await;
            super::policy::release_cgroup(&worker.id);
        }

        tracing::info!("worker pool shutdown complete");
    }

    /// Get pool statistics for observability.
    pub async fn stats(&self) -> PoolStats {
        let warm = self.warm.lock().await.len();
        let active = self.active.lock().await.len();
        let suspended = self.suspended.lock().await.len();

        // Update Prometheus gauges
        crate::metrics::POOL_WARM_WORKERS.set(warm as f64);
        crate::metrics::POOL_ACTIVE_WORKERS.set(active as f64);
        crate::metrics::POOL_SUSPENDED.set(suspended as f64);

        PoolStats {
            warm_workers: warm,
            active_workers: active,
            suspended_deployments: suspended,
            max_active: self.config.max_active_workers,
            warm_target: self.config.warm_pool_size,
        }
    }
}

/// Pool utilization statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolStats {
    pub warm_workers: usize,
    pub active_workers: usize,
    pub suspended_deployments: usize,
    pub max_active: usize,
    pub warm_target: usize,
}

/// Detect runtime kind from the filesystem (same logic as RuntimeManager).
fn detect_runtime_kind(workspace_dir: &std::path::Path) -> Option<RuntimeKind> {
    // Combined entry takes priority — it means functions + framework are wired together
    if workspace_dir
        .join("_rift_functions_output/_rift_combined_entry.ts")
        .exists()
    {
        let fn_dir = workspace_dir.join("_rift_functions_output");
        return Some(RuntimeKind::Combined {
            entry: fn_dir.join("_rift_combined_entry.ts"),
            functions_dir: fn_dir,
        });
    }

    if workspace_dir.join(".next/standalone").exists() {
        Some(RuntimeKind::NextDeno {
            dir: workspace_dir.to_path_buf(),
        })
    } else if workspace_dir.join(".output/server/index.mjs").exists() {
        Some(RuntimeKind::NodeServer {
            dir: workspace_dir.to_path_buf(),
            entry: workspace_dir.join(".output/server/index.mjs"),
        })
    } else if workspace_dir.join("dist/server/entry.mjs").exists() {
        Some(RuntimeKind::NodeServer {
            dir: workspace_dir.to_path_buf(),
            entry: workspace_dir.join("dist/server/entry.mjs"),
        })
    } else if workspace_dir.join("build/index.js").exists()
        && workspace_dir.join("build/handler.js").exists()
    {
        Some(RuntimeKind::NodeServer {
            dir: workspace_dir.to_path_buf(),
            entry: workspace_dir.join("build/index.js"),
        })
    } else if workspace_dir.join("build/server/index.js").exists() {
        Some(RuntimeKind::NodeServer {
            dir: workspace_dir.to_path_buf(),
            entry: workspace_dir.join("build/server/index.js"),
        })
    } else if workspace_dir
        .join("_rift_functions_output/bundles")
        .is_dir()
    {
        Some(RuntimeKind::Functions {
            dir: workspace_dir.join("_rift_functions_output"),
        })
    } else if find_entry_ts(workspace_dir).is_some() {
        Some(RuntimeKind::StaticDeno {
            dir: find_entry_ts(workspace_dir).unwrap(),
        })
    } else {
        None
    }
}

/// Find `_entry.ts` in common output dirs.
fn find_entry_ts(workspace_dir: &std::path::Path) -> Option<PathBuf> {
    for subdir in ["", "dist", "build", "out", "public", ".output/public"] {
        let dir = if subdir.is_empty() {
            workspace_dir.to_path_buf()
        } else {
            workspace_dir.join(subdir)
        };
        if dir.join("_entry.ts").exists() {
            return Some(dir);
        }
    }
    None
}

/// Find server.js recursively in standalone dir.
fn find_server_js_recursive(dir: &std::path::Path) -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let d1 = entry.path();
        if d1.join("server.js").exists() {
            return Some(d1.join("server.js"));
        }
        let Ok(sub) = std::fs::read_dir(&d1) else {
            continue;
        };
        for sub_entry in sub.flatten() {
            if !sub_entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let d2 = sub_entry.path();
            if d2.join("server.js").exists() {
                return Some(d2.join("server.js"));
            }
        }
    }
    None
}
