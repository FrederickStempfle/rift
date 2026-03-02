//! Background heartbeat task that periodically reports worker health
//! to the distributed state store.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::runtime::backend::RuntimeBackend;
use crate::state::{StateStore, WorkerHeartbeat};

/// Heartbeat interval.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Spawn a background task that sends periodic heartbeats for this worker.
pub fn spawn_heartbeat(
    state_store: Arc<dyn StateStore>,
    runtime_backend: Arc<dyn RuntimeBackend>,
    worker_id: String,
    max_runtimes: u32,
) {
    tokio::spawn(async move {
        tracing::info!(
            worker_id = %worker_id,
            interval_secs = HEARTBEAT_INTERVAL.as_secs(),
            "worker heartbeat started"
        );

        loop {
            let active_runtimes = runtime_backend
                .pool_stats()
                .await
                .map(|s| s.active_workers as u32)
                .unwrap_or(0);

            let heartbeat = WorkerHeartbeat {
                worker_id: worker_id.clone(),
                timestamp: Utc::now(),
                // CPU/memory metrics are stubs for Phase 1.
                // Real measurement will be added in Phase 5 (tenant isolation).
                cpu_free_pct: 100.0,
                mem_free_bytes: u64::MAX,
                active_runtimes,
                max_runtimes,
            };

            match state_store.send_heartbeat(&heartbeat).await {
                Ok(()) => {
                    crate::metrics::HEARTBEAT_SEND
                        .with_label_values(&["ok"])
                        .inc();
                }
                Err(e) => {
                    crate::metrics::HEARTBEAT_SEND
                        .with_label_values(&["error"])
                        .inc();
                    tracing::warn!(error = %e, "heartbeat send failed");
                }
            }

            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        }
    });
}
