use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone, Debug, Serialize)]
pub struct TrafficEvent {
    pub timestamp: DateTime<Utc>,
    pub src_lat: f64,
    pub src_lng: f64,
    pub dst_lat: f64,
    pub dst_lng: f64,
    pub method: String,
    pub status: u16,
    pub host: Option<String>,
    pub country: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct TrafficBroadcaster {
    tx: broadcast::Sender<TrafficEvent>,
}

impl Default for TrafficBroadcaster {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { tx }
    }
}

impl TrafficBroadcaster {
    pub fn new() -> Self {
        Self::default()
    }

    /// Broadcast a traffic event to all connected WS clients. No-op if no listeners.
    pub fn broadcast(&self, event: TrafficEvent) {
        // send() fails only when there are no receivers — that's fine.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TrafficEvent> {
        self.tx.subscribe()
    }
}
