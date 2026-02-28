pub mod broadcast;
pub mod handler;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast as tokio_broadcast, Mutex};
use uuid::Uuid;

use self::broadcast::DeployLogMessage;

const CHANNEL_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct LogBroadcaster {
    channels: Arc<Mutex<HashMap<Uuid, tokio_broadcast::Sender<DeployLogMessage>>>>,
}

impl LogBroadcaster {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Send a log message to all subscribers of this deployment.
    /// No-op if nobody is listening.
    pub async fn send(&self, deployment_id: Uuid, msg: DeployLogMessage) {
        let mut channels = self.channels.lock().await;
        if let Some(tx) = channels.get(&deployment_id) {
            if tx.receiver_count() == 0 {
                channels.remove(&deployment_id);
                return;
            }
            let _ = tx.send(msg);
        }
    }

    /// Subscribe to a deployment's log stream. Creates the channel lazily.
    pub async fn subscribe(
        &self,
        deployment_id: Uuid,
    ) -> tokio_broadcast::Receiver<DeployLogMessage> {
        let mut channels = self.channels.lock().await;
        let tx = channels
            .entry(deployment_id)
            .or_insert_with(|| tokio_broadcast::channel(CHANNEL_CAPACITY).0);
        tx.subscribe()
    }

    /// Remove a deployment's channel if no subscribers remain.
    pub async fn cleanup(&self, deployment_id: Uuid) {
        let mut channels = self.channels.lock().await;
        if let Some(tx) = channels.get(&deployment_id) {
            if tx.receiver_count() == 0 {
                channels.remove(&deployment_id);
            }
        }
    }
}
