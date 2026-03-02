//! Scheduler and placement service for distributed runtime management.
//!
//! The scheduler selects the best worker for new deployments based on
//! worker heartbeat data (least-loaded-first scoring) and acquires
//! placement leases through the [`StateStore`].

pub mod heartbeat;

use std::sync::Arc;

use uuid::Uuid;

use crate::error::AppError;
use crate::state::{PlacementLease, StateStore, WorkerHeartbeat};

/// Scheduler responsible for worker placement decisions.
pub struct Scheduler {
    state_store: Arc<dyn StateStore>,
    worker_id: String,
}

impl Scheduler {
    pub fn new(state_store: Arc<dyn StateStore>, worker_id: String) -> Self {
        Self {
            state_store,
            worker_id,
        }
    }

    /// The ID of the local worker this scheduler runs on.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Determine the next lease version for a project by reading the
    /// current placement (if any) and incrementing.
    async fn next_version(&self, project_id: Uuid) -> Result<u64, AppError> {
        let current = self.state_store.get_placement(project_id).await?;
        Ok(current.map_or(1, |l| l.version + 1))
    }

    /// Choose the best worker for a new deployment and acquire its
    /// placement lease. Returns the worker ID on success.
    pub async fn place(&self, project_id: Uuid, deployment_id: Uuid) -> Result<String, AppError> {
        let workers = self.state_store.list_workers().await?;
        if workers.is_empty() {
            // Single-node mode: no heartbeats yet, place on self.
            return self.place_on_self(project_id, deployment_id).await;
        }

        let scored = Self::score_workers(&workers);
        let best = scored
            .first()
            .ok_or_else(|| AppError::Internal("no available workers with capacity".into()))?;

        let version = self.next_version(project_id).await?;
        let lease = PlacementLease {
            worker_id: best.worker_id.clone(),
            deployment_id,
            project_id,
            version,
            ttl_secs: 300,
        };

        let acquired = self
            .state_store
            .acquire_placement(project_id, &lease)
            .await?;
        if !acquired {
            return Err(AppError::Conflict(
                "placement lease already held for project".into(),
            ));
        }

        tracing::info!(
            project_id = %project_id,
            worker_id = %best.worker_id,
            active = best.active_runtimes,
            max = best.max_runtimes,
            "placed deployment on worker"
        );

        Ok(best.worker_id.clone())
    }

    /// Place directly on the local worker (bypass scoring).
    async fn place_on_self(
        &self,
        project_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<String, AppError> {
        let version = self.next_version(project_id).await?;
        let lease = PlacementLease {
            worker_id: self.worker_id.clone(),
            deployment_id,
            project_id,
            version,
            ttl_secs: 300,
        };

        let acquired = self
            .state_store
            .acquire_placement(project_id, &lease)
            .await?;
        if !acquired {
            return Err(AppError::Conflict(
                "placement lease already held for project".into(),
            ));
        }
        Ok(self.worker_id.clone())
    }

    /// Score workers by load factor (least-loaded first).
    /// Filters out workers that are at capacity.
    fn score_workers(workers: &[WorkerHeartbeat]) -> Vec<&WorkerHeartbeat> {
        let mut candidates: Vec<_> = workers
            .iter()
            .filter(|w| w.active_runtimes < w.max_runtimes)
            .collect();

        candidates.sort_by(|a, b| {
            let a_load = a.active_runtimes as f64 / a.max_runtimes.max(1) as f64;
            let b_load = b.active_runtimes as f64 / b.max_runtimes.max(1) as f64;
            a_load
                .partial_cmp(&b_load)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::local::LocalStateStore;
    use chrono::Utc;

    fn hb(id: &str, active: u32, max: u32) -> WorkerHeartbeat {
        WorkerHeartbeat {
            worker_id: id.to_owned(),
            timestamp: Utc::now(),
            cpu_free_pct: 80.0,
            mem_free_bytes: 1024 * 1024 * 512,
            active_runtimes: active,
            max_runtimes: max,
        }
    }

    #[test]
    fn score_workers_least_loaded_first() {
        let workers = vec![hb("w1", 8, 10), hb("w2", 2, 10), hb("w3", 5, 10)];
        let scored = Scheduler::score_workers(&workers);
        assert_eq!(scored[0].worker_id, "w2");
        assert_eq!(scored[1].worker_id, "w3");
        assert_eq!(scored[2].worker_id, "w1");
    }

    #[test]
    fn score_workers_filters_at_capacity() {
        let workers = vec![hb("w1", 10, 10), hb("w2", 3, 10)];
        let scored = Scheduler::score_workers(&workers);
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].worker_id, "w2");
    }

    #[test]
    fn score_workers_empty_when_all_full() {
        let workers = vec![hb("w1", 10, 10), hb("w2", 10, 10)];
        let scored = Scheduler::score_workers(&workers);
        assert!(scored.is_empty());
    }

    #[tokio::test]
    async fn place_on_empty_workers_uses_self() {
        let store = Arc::new(LocalStateStore::new());
        let scheduler = Scheduler::new(store.clone(), "local-1".to_owned());

        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();
        let worker = scheduler.place(pid, did).await.unwrap();
        assert_eq!(worker, "local-1");

        let lease = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(lease.deployment_id, did);
    }

    #[tokio::test]
    async fn place_increments_version_on_re_placement() {
        let store = Arc::new(LocalStateStore::new());
        let scheduler = Scheduler::new(store.clone(), "local-1".to_owned());

        let pid = Uuid::new_v4();
        let did1 = Uuid::new_v4();
        let did2 = Uuid::new_v4();

        // First placement: version should be 1 (fresh).
        scheduler.place(pid, did1).await.unwrap();
        let lease1 = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(lease1.version, 1);
        assert_eq!(lease1.deployment_id, did1);

        // Second placement: version should be 2 (increment).
        scheduler.place(pid, did2).await.unwrap();
        let lease2 = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(lease2.version, 2);
        assert_eq!(lease2.deployment_id, did2);
    }

    #[tokio::test]
    async fn place_on_self_returns_error_when_lease_held_at_same_version() {
        let store = Arc::new(LocalStateStore::new());
        let scheduler = Scheduler::new(store.clone(), "local-1".to_owned());

        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();

        // Pre-seed a lease at version=1.
        let preexisting = PlacementLease {
            worker_id: "other-worker".to_owned(),
            deployment_id: Uuid::new_v4(),
            project_id: pid,
            version: 1,
            ttl_secs: 300,
        };
        store.acquire_placement(pid, &preexisting).await.unwrap();

        // Scheduler reads version=1, creates version=2, which should succeed.
        let result = scheduler.place(pid, did).await;
        assert!(result.is_ok());
        let lease = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(lease.version, 2);
    }

    #[tokio::test]
    async fn place_picks_least_loaded() {
        let store = Arc::new(LocalStateStore::new());
        let scheduler = Scheduler::new(store.clone(), "local-1".to_owned());

        // Register workers
        store.send_heartbeat(&hb("w1", 8, 10)).await.unwrap();
        store.send_heartbeat(&hb("w2", 1, 10)).await.unwrap();
        store.send_heartbeat(&hb("w3", 5, 10)).await.unwrap();

        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();
        let worker = scheduler.place(pid, did).await.unwrap();
        assert_eq!(worker, "w2");
    }
}
