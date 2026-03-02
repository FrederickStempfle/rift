//! In-memory mock implementation of [`RuntimeBackend`] for testing.
//!
//! This module is only compiled under `#[cfg(test)]` and allows exercising
//! deploy/wake/suspend/stop lifecycle flows without spawning real Deno
//! processes or connecting to Postgres.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::Config;
use crate::error::AppError;

use super::backend::{DeployResult, RuntimeBackend};
use super::RuntimeLaunchSpec;

static NEXT_PORT: AtomicU16 = AtomicU16::new(10_000);

struct ActiveEntry {
    deployment_id: Uuid,
    url: String,
    last_request: Instant,
}

struct SuspendedEntry {
    spec: RuntimeLaunchSpec,
}

/// A fully in-memory [`RuntimeBackend`] for unit/integration tests.
pub struct MockBackend {
    active: Mutex<HashMap<Uuid, ActiveEntry>>,
    suspended: Mutex<HashMap<Uuid, SuspendedEntry>>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            suspended: Mutex::new(HashMap::new()),
        }
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RuntimeBackend for MockBackend {
    async fn deploy(&self, spec: RuntimeLaunchSpec) -> Result<DeployResult, AppError> {
        let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        let url = format!("http://mock:{port}");

        self.suspended.lock().await.remove(&spec.project_id);

        self.active.lock().await.insert(
            spec.project_id,
            ActiveEntry {
                deployment_id: spec.deployment_id,
                url: url.clone(),
                last_request: Instant::now(),
            },
        );

        Ok(DeployResult { url, port })
    }

    async fn stop(&self, project_id: Uuid) -> Result<(), AppError> {
        self.active.lock().await.remove(&project_id);
        self.suspended.lock().await.remove(&project_id);
        Ok(())
    }

    async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.active
            .lock()
            .await
            .get(&project_id)
            .map(|e| e.url.clone())
    }

    async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.active
            .lock()
            .await
            .get(&project_id)
            .map(|e| e.deployment_id)
    }

    async fn touch(&self, project_id: Uuid) {
        if let Some(entry) = self.active.lock().await.get_mut(&project_id) {
            entry.last_request = Instant::now();
        }
    }

    async fn is_suspended(&self, project_id: Uuid) -> bool {
        self.suspended.lock().await.contains_key(&project_id)
    }

    async fn wake(&self, project_id: Uuid) -> Result<Option<String>, AppError> {
        let suspended = self.suspended.lock().await.remove(&project_id);
        let suspended = match suspended {
            Some(s) => s,
            None => return Ok(None),
        };

        let result = self.deploy(suspended.spec).await?;
        Ok(Some(result.url))
    }

    async fn suspend(&self, project_id: Uuid) -> Result<bool, AppError> {
        let mut active = self.active.lock().await;
        if let Some(entry) = active.remove(&project_id) {
            self.suspended.lock().await.insert(
                project_id,
                SuspendedEntry {
                    spec: RuntimeLaunchSpec {
                        project_id,
                        deployment_id: entry.deployment_id,
                        kind: super::RuntimeKind::StaticDeno {
                            dir: std::path::PathBuf::from("/mock"),
                        },
                        env_vars: Vec::new(),
                    },
                },
            );
            drop(active);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn suspend_idle(&self, threshold: Duration) -> usize {
        let now = Instant::now();
        let mut to_suspend = Vec::new();

        {
            let active = self.active.lock().await;
            for (&project_id, entry) in active.iter() {
                if now.duration_since(entry.last_request) > threshold {
                    to_suspend.push(project_id);
                }
            }
        }

        let mut count = 0;
        for project_id in to_suspend {
            let mut active = self.active.lock().await;
            if let Some(entry) = active.remove(&project_id) {
                self.suspended.lock().await.insert(
                    project_id,
                    SuspendedEntry {
                        spec: RuntimeLaunchSpec {
                            project_id,
                            deployment_id: entry.deployment_id,
                            kind: super::RuntimeKind::StaticDeno {
                                dir: std::path::PathBuf::from("/mock"),
                            },
                            env_vars: Vec::new(),
                        },
                    },
                );
                drop(active);
                count += 1;
            }
        }

        count
    }

    async fn restore(&self, _pool: &sqlx::PgPool, _config: &Config) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{RuntimeKind, RuntimeLaunchSpec};
    use std::path::PathBuf;

    fn make_spec(project_id: Uuid, deployment_id: Uuid) -> RuntimeLaunchSpec {
        RuntimeLaunchSpec {
            project_id,
            deployment_id,
            kind: RuntimeKind::StaticDeno {
                dir: PathBuf::from("/mock/static"),
            },
            env_vars: Vec::new(),
        }
    }

    #[tokio::test]
    async fn deploy_makes_project_active() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        let result = backend.deploy(make_spec(pid, did)).await.unwrap();
        assert!(!result.url.is_empty());
        assert!(backend.active_url(pid).await.is_some());
        assert_eq!(backend.active_deployment_id(pid).await, Some(did));
        assert!(!backend.is_suspended(pid).await);
    }

    #[tokio::test]
    async fn suspend_idle_moves_to_suspended() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        backend.deploy(make_spec(pid, did)).await.unwrap();

        let count = backend.suspend_idle(Duration::ZERO).await;
        assert_eq!(count, 1);
        assert!(backend.active_url(pid).await.is_none());
        assert!(backend.is_suspended(pid).await);
    }

    #[tokio::test]
    async fn wake_restores_suspended_project() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        backend.deploy(make_spec(pid, did)).await.unwrap();
        backend.suspend_idle(Duration::ZERO).await;

        let url = backend.wake(pid).await.unwrap();
        assert!(url.is_some());
        assert!(backend.active_url(pid).await.is_some());
        assert!(!backend.is_suspended(pid).await);
    }

    #[tokio::test]
    async fn wake_returns_none_for_non_suspended() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();

        let url = backend.wake(pid).await.unwrap();
        assert!(url.is_none());
    }

    #[tokio::test]
    async fn redeploy_replaces_active_deployment() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did1 = Uuid::new_v4();
        let did2 = Uuid::new_v4();

        backend.deploy(make_spec(pid, did1)).await.unwrap();
        assert_eq!(backend.active_deployment_id(pid).await, Some(did1));

        backend.deploy(make_spec(pid, did2)).await.unwrap();
        assert_eq!(backend.active_deployment_id(pid).await, Some(did2));
    }

    #[tokio::test]
    async fn stop_clears_active_and_suspended() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        // Stop active
        backend.deploy(make_spec(pid, did)).await.unwrap();
        backend.stop(pid).await.unwrap();
        assert!(backend.active_url(pid).await.is_none());
        assert!(!backend.is_suspended(pid).await);

        // Stop suspended
        backend.deploy(make_spec(pid, did)).await.unwrap();
        backend.suspend_idle(Duration::ZERO).await;
        backend.stop(pid).await.unwrap();
        assert!(backend.active_url(pid).await.is_none());
        assert!(!backend.is_suspended(pid).await);
    }

    #[tokio::test]
    async fn touch_prevents_suspension() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        backend.deploy(make_spec(pid, did)).await.unwrap();
        backend.touch(pid).await;

        let count = backend.suspend_idle(Duration::from_secs(3600)).await;
        assert_eq!(count, 0);
        assert!(backend.active_url(pid).await.is_some());
    }

    #[tokio::test]
    async fn multiple_projects_independent() {
        let backend = MockBackend::new();
        let pid1 = Uuid::new_v4();
        let pid2 = Uuid::new_v4();

        backend
            .deploy(make_spec(pid1, Uuid::new_v4()))
            .await
            .unwrap();
        backend
            .deploy(make_spec(pid2, Uuid::new_v4()))
            .await
            .unwrap();

        backend.stop(pid1).await.unwrap();
        assert!(backend.active_url(pid1).await.is_none());
        assert!(backend.active_url(pid2).await.is_some());
    }

    #[tokio::test]
    async fn suspend_active_project() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        backend.deploy(make_spec(pid, did)).await.unwrap();
        let suspended = backend.suspend(pid).await.unwrap();
        assert!(suspended);
        assert!(backend.active_url(pid).await.is_none());
        assert!(backend.is_suspended(pid).await);
    }

    #[tokio::test]
    async fn suspend_non_active_returns_false() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();

        let suspended = backend.suspend(pid).await.unwrap();
        assert!(!suspended);
        assert!(!backend.is_suspended(pid).await);
    }

    #[tokio::test]
    async fn deploy_clears_suspended_state() {
        let backend = MockBackend::new();
        let pid = Uuid::new_v4();
        let did1 = Uuid::new_v4();
        let did2 = Uuid::new_v4();

        backend.deploy(make_spec(pid, did1)).await.unwrap();
        backend.suspend_idle(Duration::ZERO).await;
        assert!(backend.is_suspended(pid).await);

        backend.deploy(make_spec(pid, did2)).await.unwrap();
        assert!(!backend.is_suspended(pid).await);
        assert_eq!(backend.active_deployment_id(pid).await, Some(did2));
    }
}
