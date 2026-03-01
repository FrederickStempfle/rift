use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{config::Config, error::AppError};

use super::{
    pool::{PoolStats, WorkerPool},
    RuntimeLaunchSpec, RuntimeManager,
};

/// Result of deploying a runtime.
#[derive(Clone, Debug)]
pub struct DeployResult {
    pub url: String,
    pub port: u16,
}

/// Abstraction over the runtime execution model.
///
/// Both the legacy subprocess model (`ProcessBackend`) and the new
/// worker-pool model (`PoolBackend`, coming in Phase 2) implement this trait.
/// The proxy and build manager use this trait exclusively, allowing the
/// backend to be swapped via configuration.
#[async_trait]
pub trait RuntimeBackend: Send + Sync + 'static {
    /// Deploy a new runtime (or re-deploy with zero-downtime swap).
    async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<DeployResult, AppError>;

    /// Stop a project's runtime.
    async fn stop(&self, project_id: Uuid) -> Result<(), AppError>;

    /// Get the internal URL for an active runtime, if any.
    async fn active_url(&self, project_id: Uuid) -> Option<String>;

    /// Get the deployment ID for an active runtime, if any.
    async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid>;

    /// Record a request timestamp (for scale-to-zero tracking).
    async fn touch(&self, project_id: Uuid);

    /// Check if a project has a suspended runtime that can be woken.
    async fn is_suspended(&self, project_id: Uuid) -> bool;

    /// Wake a suspended project. Returns the URL if successful, None if not suspended.
    async fn wake(&self, project_id: Uuid) -> Result<Option<String>, AppError>;

    /// Suspend idle runtimes. Returns count of suspended.
    async fn suspend_idle(&self, threshold: Duration) -> usize;

    /// Restore deployments from database after restart.
    async fn restore(&self, pool: &PgPool, config: &Config) -> usize;

    /// Check if a project is a function-only project served by the global dispatcher.
    async fn is_function_only(&self, project_id: Uuid) -> bool {
        let _ = project_id;
        false
    }

    /// Get pool statistics (only meaningful for pool backend).
    async fn pool_stats(&self) -> Option<PoolStats> {
        None
    }
}

/// Legacy backend that wraps `RuntimeManager` (one OS subprocess per project).
pub struct ProcessBackend {
    manager: RuntimeManager,
}

impl ProcessBackend {
    pub fn new(manager: RuntimeManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl RuntimeBackend for ProcessBackend {
    async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<DeployResult, AppError> {
        let (url, port) = self.manager.deploy(spec).await?;
        Ok(DeployResult { url, port })
    }

    async fn stop(&self, project_id: Uuid) -> Result<(), AppError> {
        self.manager.stop_project(project_id).await
    }

    async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.manager.active_url(project_id).await
    }

    async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.manager.active_deployment_id(project_id).await
    }

    async fn touch(&self, project_id: Uuid) {
        self.manager.touch(project_id).await
    }

    async fn is_suspended(&self, project_id: Uuid) -> bool {
        self.manager.is_suspended(project_id).await
    }

    async fn wake(&self, project_id: Uuid) -> Result<Option<String>, AppError> {
        self.manager.wake(project_id).await
    }

    async fn suspend_idle(&self, threshold: Duration) -> usize {
        self.manager.suspend_idle(threshold).await
    }

    async fn is_function_only(&self, project_id: Uuid) -> bool {
        if let Some(registry) = self.manager.function_registry() {
            return registry.is_function_project(project_id).await;
        }
        false
    }

    async fn restore(&self, pool: &PgPool, config: &Config) -> usize {
        self.manager.restore_deployments(pool, config).await
    }
}

/// Pool-based backend using pre-warmed Deno workers with specialization.
pub struct PoolBackend {
    pool: Arc<WorkerPool>,
}

impl PoolBackend {
    pub fn new(pool: Arc<WorkerPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RuntimeBackend for PoolBackend {
    async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<DeployResult, AppError> {
        let (url, port) = self.pool.deploy(spec).await?;
        Ok(DeployResult { url, port })
    }

    async fn stop(&self, project_id: Uuid) -> Result<(), AppError> {
        self.pool.stop_project(project_id).await
    }

    async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.pool.active_url(project_id).await
    }

    async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.pool.active_deployment_id(project_id).await
    }

    async fn touch(&self, project_id: Uuid) {
        self.pool.touch(project_id).await
    }

    async fn is_suspended(&self, project_id: Uuid) -> bool {
        self.pool.is_suspended(project_id).await
    }

    async fn wake(&self, project_id: Uuid) -> Result<Option<String>, AppError> {
        self.pool.wake(project_id).await
    }

    async fn suspend_idle(&self, threshold: Duration) -> usize {
        self.pool.suspend_idle(threshold).await
    }

    async fn restore(&self, db_pool: &PgPool, config: &Config) -> usize {
        self.pool.restore_deployments(db_pool, config).await
    }

    async fn pool_stats(&self) -> Option<PoolStats> {
        Some(self.pool.stats().await)
    }
}
