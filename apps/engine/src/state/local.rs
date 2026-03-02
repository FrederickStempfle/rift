//! In-memory [`StateStore`] implementation for single-process deployments.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::AppError;

use super::{PlacementLease, RoutingEntry, StateStore, WorkerHeartbeat};

/// Single-process, in-memory state store.
///
/// Suitable for development and single-node production. All data lives in
/// process memory and is lost on restart.
pub struct LocalStateStore {
    placements: RwLock<HashMap<Uuid, PlacementLease>>,
    workers: RwLock<HashMap<String, WorkerHeartbeat>>,
    routing: RwLock<HashMap<String, RoutingEntry>>,
}

impl Default for LocalStateStore {
    fn default() -> Self {
        Self {
            placements: RwLock::new(HashMap::new()),
            workers: RwLock::new(HashMap::new()),
            routing: RwLock::new(HashMap::new()),
        }
    }
}

impl LocalStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StateStore for LocalStateStore {
    async fn acquire_placement(
        &self,
        project_id: Uuid,
        lease: &PlacementLease,
    ) -> Result<bool, AppError> {
        let mut map = self.placements.write().await;
        match map.get(&project_id) {
            Some(existing) if existing.version >= lease.version => Ok(false),
            _ => {
                map.insert(project_id, lease.clone());
                Ok(true)
            }
        }
    }

    async fn get_placement(&self, project_id: Uuid) -> Result<Option<PlacementLease>, AppError> {
        Ok(self.placements.read().await.get(&project_id).cloned())
    }

    async fn release_placement(&self, project_id: Uuid, version: u64) -> Result<bool, AppError> {
        let mut map = self.placements.write().await;
        if let Some(existing) = map.get(&project_id) {
            if existing.version == version {
                map.remove(&project_id);
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn renew_placement(&self, project_id: Uuid, ttl_secs: u64) -> Result<bool, AppError> {
        let mut map = self.placements.write().await;
        if let Some(lease) = map.get_mut(&project_id) {
            lease.ttl_secs = ttl_secs;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn send_heartbeat(&self, heartbeat: &WorkerHeartbeat) -> Result<(), AppError> {
        self.workers
            .write()
            .await
            .insert(heartbeat.worker_id.clone(), heartbeat.clone());
        Ok(())
    }

    async fn list_workers(&self) -> Result<Vec<WorkerHeartbeat>, AppError> {
        Ok(self.workers.read().await.values().cloned().collect())
    }

    async fn remove_worker(&self, worker_id: &str) -> Result<(), AppError> {
        self.workers.write().await.remove(worker_id);
        Ok(())
    }

    async fn set_routing(&self, entry: &RoutingEntry) -> Result<(), AppError> {
        self.routing
            .write()
            .await
            .insert(entry.host.clone(), entry.clone());
        Ok(())
    }

    async fn get_routing(&self, host: &str) -> Result<Option<RoutingEntry>, AppError> {
        Ok(self.routing.read().await.get(host).cloned())
    }

    async fn remove_routing(&self, host: &str) -> Result<(), AppError> {
        self.routing.write().await.remove(host);
        Ok(())
    }

    async fn publish_routing_update(&self, _entry: &RoutingEntry) -> Result<(), AppError> {
        // No-op for single-process mode — no remote subscribers to notify.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_lease(project_id: Uuid, version: u64) -> PlacementLease {
        PlacementLease {
            worker_id: "worker-1".to_owned(),
            deployment_id: Uuid::new_v4(),
            project_id,
            version,
            ttl_secs: 300,
        }
    }

    fn make_heartbeat(worker_id: &str, active: u32, max: u32) -> WorkerHeartbeat {
        WorkerHeartbeat {
            worker_id: worker_id.to_owned(),
            timestamp: Utc::now(),
            cpu_free_pct: 80.0,
            mem_free_bytes: 1024 * 1024 * 512,
            active_runtimes: active,
            max_runtimes: max,
        }
    }

    #[tokio::test]
    async fn placement_acquire_and_get() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();
        let lease = make_lease(pid, 1);

        assert!(store.acquire_placement(pid, &lease).await.unwrap());
        let got = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(got.version, 1);
        assert_eq!(got.worker_id, "worker-1");
    }

    #[tokio::test]
    async fn placement_cas_rejects_same_version() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        assert!(store
            .acquire_placement(pid, &make_lease(pid, 1))
            .await
            .unwrap());
        // Same version should be rejected
        assert!(!store
            .acquire_placement(pid, &make_lease(pid, 1))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn placement_cas_accepts_higher_version() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        assert!(store
            .acquire_placement(pid, &make_lease(pid, 1))
            .await
            .unwrap());
        assert!(store
            .acquire_placement(pid, &make_lease(pid, 2))
            .await
            .unwrap());
        let got = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(got.version, 2);
    }

    #[tokio::test]
    async fn placement_release_matching_version() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        store
            .acquire_placement(pid, &make_lease(pid, 1))
            .await
            .unwrap();
        assert!(store.release_placement(pid, 1).await.unwrap());
        assert!(store.get_placement(pid).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn placement_release_wrong_version() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        store
            .acquire_placement(pid, &make_lease(pid, 1))
            .await
            .unwrap();
        assert!(!store.release_placement(pid, 99).await.unwrap());
        assert!(store.get_placement(pid).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn placement_renew() {
        let store = LocalStateStore::new();
        let pid = Uuid::new_v4();

        store
            .acquire_placement(pid, &make_lease(pid, 1))
            .await
            .unwrap();
        assert!(store.renew_placement(pid, 600).await.unwrap());
        let got = store.get_placement(pid).await.unwrap().unwrap();
        assert_eq!(got.ttl_secs, 600);
    }

    #[tokio::test]
    async fn heartbeat_send_and_list() {
        let store = LocalStateStore::new();

        store
            .send_heartbeat(&make_heartbeat("w1", 2, 10))
            .await
            .unwrap();
        store
            .send_heartbeat(&make_heartbeat("w2", 5, 10))
            .await
            .unwrap();

        let workers = store.list_workers().await.unwrap();
        assert_eq!(workers.len(), 2);
    }

    #[tokio::test]
    async fn heartbeat_remove_worker() {
        let store = LocalStateStore::new();

        store
            .send_heartbeat(&make_heartbeat("w1", 0, 10))
            .await
            .unwrap();
        store.remove_worker("w1").await.unwrap();
        assert!(store.list_workers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn routing_set_get_remove() {
        let store = LocalStateStore::new();
        let entry = RoutingEntry {
            host: "app.rift.dev".to_owned(),
            project_id: Uuid::new_v4(),
            deployment_id: Uuid::new_v4(),
            worker_addr: "127.0.0.1:8080".to_owned(),
            version: 1,
        };

        store.set_routing(&entry).await.unwrap();
        let got = store.get_routing("app.rift.dev").await.unwrap().unwrap();
        assert_eq!(got.project_id, entry.project_id);

        store.remove_routing("app.rift.dev").await.unwrap();
        assert!(store.get_routing("app.rift.dev").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn routing_update_overwrites_previous() {
        let store = LocalStateStore::new();
        let pid1 = Uuid::new_v4();
        let pid2 = Uuid::new_v4();

        let entry1 = RoutingEntry {
            host: "app.rift.dev".to_owned(),
            project_id: pid1,
            deployment_id: Uuid::new_v4(),
            worker_addr: "127.0.0.1:8080".to_owned(),
            version: 1,
        };
        store.set_routing(&entry1).await.unwrap();

        let entry2 = RoutingEntry {
            host: "app.rift.dev".to_owned(),
            project_id: pid2,
            deployment_id: Uuid::new_v4(),
            worker_addr: "127.0.0.1:9090".to_owned(),
            version: 2,
        };
        store.set_routing(&entry2).await.unwrap();

        let got = store.get_routing("app.rift.dev").await.unwrap().unwrap();
        assert_eq!(got.project_id, pid2);
        assert_eq!(got.version, 2);
    }

    #[tokio::test]
    async fn publish_routing_update_is_noop() {
        let store = LocalStateStore::new();
        let entry = RoutingEntry {
            host: "app.rift.dev".to_owned(),
            project_id: Uuid::new_v4(),
            deployment_id: Uuid::new_v4(),
            worker_addr: "127.0.0.1:8080".to_owned(),
            version: 1,
        };
        // Should not error even though there are no subscribers.
        store.publish_routing_update(&entry).await.unwrap();
    }
}
