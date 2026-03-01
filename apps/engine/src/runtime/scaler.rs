use std::{sync::Arc, time::Duration};

use super::backend::RuntimeBackend;

/// Default idle threshold before a deployment is suspended (5 minutes).
const IDLE_THRESHOLD: Duration = Duration::from_secs(5 * 60);

/// How often the scaler checks for idle deployments.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Spawn a background task that periodically suspends idle deployments.
pub fn spawn_scaler(runtime_backend: Arc<dyn RuntimeBackend>) {
    tokio::spawn(async move {
        // Give deployments time to settle after engine start before scanning.
        tokio::time::sleep(Duration::from_secs(60)).await;

        tracing::info!(
            interval_secs = CHECK_INTERVAL.as_secs(),
            idle_threshold_secs = IDLE_THRESHOLD.as_secs(),
            "scale-to-zero scaler started"
        );

        loop {
            tokio::time::sleep(CHECK_INTERVAL).await;

            let suspended = runtime_backend.suspend_idle(IDLE_THRESHOLD).await;
            if suspended > 0 {
                tracing::info!(count = suspended, "suspended idle deployments");
            }
        }
    });
}
