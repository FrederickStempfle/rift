//! Distributed state abstractions for runtime placement, worker health,
//! and routing metadata.
//!
//! Two implementations are provided:
//! - [`local::LocalStateStore`] — in-memory, single-process (default).
//! - [`redis_store::RedisStateStore`] — Redis-backed, multi-node.

pub mod local;
pub mod redis_store;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::AppError;

/// Placement lease for a deployment on a worker node.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlacementLease {
    pub worker_id: String,
    pub deployment_id: Uuid,
    pub project_id: Uuid,
    pub version: u64,
    pub ttl_secs: u64,
}

/// Worker capacity and health information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerHeartbeat {
    pub worker_id: String,
    pub timestamp: DateTime<Utc>,
    pub cpu_free_pct: f32,
    pub mem_free_bytes: u64,
    pub active_runtimes: u32,
    pub max_runtimes: u32,
}

/// Routing entry cached for hot-path host resolution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingEntry {
    pub host: String,
    pub project_id: Uuid,
    pub deployment_id: Uuid,
    pub worker_addr: String,
    pub version: u64,
}

/// Abstraction over the distributed state backend.
///
/// Implementations must be safe for concurrent use from multiple async tasks.
#[async_trait]
pub trait StateStore: Send + Sync + 'static {
    // --- Placement leases ---

    /// Acquire a placement lease for a project using compare-and-set.
    ///
    /// Returns `true` if the lease was acquired (either vacant or the
    /// existing version was lower).
    async fn acquire_placement(
        &self,
        project_id: Uuid,
        lease: &PlacementLease,
    ) -> Result<bool, AppError>;

    /// Get the current placement lease for a project.
    async fn get_placement(&self, project_id: Uuid) -> Result<Option<PlacementLease>, AppError>;

    /// Release a placement lease if the current version matches.
    async fn release_placement(&self, project_id: Uuid, version: u64) -> Result<bool, AppError>;

    /// Renew (extend TTL of) a placement lease.
    async fn renew_placement(&self, project_id: Uuid, ttl_secs: u64) -> Result<bool, AppError>;

    // --- Worker heartbeats ---

    /// Record a worker heartbeat.
    async fn send_heartbeat(&self, heartbeat: &WorkerHeartbeat) -> Result<(), AppError>;

    /// List all known workers (that have sent a recent heartbeat).
    async fn list_workers(&self) -> Result<Vec<WorkerHeartbeat>, AppError>;

    /// Remove a worker from the registry.
    async fn remove_worker(&self, worker_id: &str) -> Result<(), AppError>;

    // --- Routing entries ---

    /// Set a routing entry for a host.
    async fn set_routing(&self, entry: &RoutingEntry) -> Result<(), AppError>;

    /// Get the routing entry for a host.
    async fn get_routing(&self, host: &str) -> Result<Option<RoutingEntry>, AppError>;

    /// Remove a routing entry.
    async fn remove_routing(&self, host: &str) -> Result<(), AppError>;

    // --- Pub/Sub ---

    /// Publish a routing update event for cache invalidation.
    /// No-op for local (single-process) stores.
    async fn publish_routing_update(&self, entry: &RoutingEntry) -> Result<(), AppError>;
}
