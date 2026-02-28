use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub struct RateLimitBucket {
    inner: Arc<RwLock<HashMap<String, WindowEntry>>>,
}

#[derive(Clone, Copy)]
struct WindowEntry {
    started_at: Instant,
    count: u32,
}

impl RateLimitBucket {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn check_and_increment(&self, key: &str, limit: u32, window: Duration) -> bool {
        let mut lock = self.inner.write().await;
        let now = Instant::now();
        let entry = lock.entry(key.to_owned()).or_insert(WindowEntry {
            started_at: now,
            count: 0,
        });

        if now.duration_since(entry.started_at) >= window {
            *entry = WindowEntry {
                started_at: now,
                count: 0,
            };
        }

        if entry.count >= limit {
            return false;
        }

        entry.count += 1;
        true
    }
}

#[derive(Clone, Default)]
pub struct AuthRateLimiters {
    pub register_by_ip: RateLimitBucket,
    pub login_by_email: RateLimitBucket,
}

impl AuthRateLimiters {
    pub fn new() -> Self {
        Self::default()
    }
}
