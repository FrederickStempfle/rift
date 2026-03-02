//! Redis pub/sub subscriber for cross-node routing cache invalidation.
//!
//! When running in Redis-backed state store mode, another node may update
//! a routing entry. This subscriber listens on the `rift:routing_updates`
//! channel and invalidates the local in-memory [`RoutingCache`] so stale
//! entries don't persist after a peer publishes an update.

use futures_util::StreamExt;

use super::routing_cache::RoutingCache;
use crate::state::RoutingEntry;

const ROUTING_CHANNEL: &str = "rift:routing_updates";

/// Spawn a background task that subscribes to Redis routing update events
/// and invalidates the local routing cache accordingly.
///
/// This is a no-op if `redis_url` is `None` (local state store mode).
pub fn spawn_routing_subscriber(redis_url: Option<String>, routing_cache: RoutingCache) {
    let Some(url) = redis_url else {
        return;
    };

    tokio::spawn(async move {
        loop {
            if let Err(e) = subscribe_loop(&url, &routing_cache).await {
                tracing::warn!(error = %e, "routing subscriber disconnected, reconnecting in 5s");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });
}

async fn subscribe_loop(redis_url: &str, routing_cache: &RoutingCache) -> Result<(), String> {
    let client = redis::Client::open(redis_url).map_err(|e| format!("redis connect: {e}"))?;
    let mut pubsub = client
        .get_async_pubsub()
        .await
        .map_err(|e| format!("redis pubsub: {e}"))?;

    pubsub
        .subscribe(ROUTING_CHANNEL)
        .await
        .map_err(|e| format!("redis subscribe: {e}"))?;

    tracing::info!(channel = ROUTING_CHANNEL, "routing subscriber connected");

    loop {
        let msg = pubsub
            .on_message()
            .next()
            .await
            .ok_or_else(|| "pubsub stream ended".to_owned())?;

        let payload: String = msg
            .get_payload()
            .map_err(|e| format!("payload decode: {e}"))?;

        match serde_json::from_str::<RoutingEntry>(&payload) {
            Ok(entry) => {
                routing_cache.invalidate_host(&entry.host).await;
                tracing::debug!(
                    host = %entry.host,
                    project_id = %entry.project_id,
                    "routing cache invalidated via pub/sub"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse routing update");
            }
        }
    }
}
