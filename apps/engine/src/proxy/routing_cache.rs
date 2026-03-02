//! In-memory routing cache for the proxy hot path.
//!
//! Eliminates per-request Postgres queries for host → project_id resolution.
//! Entries are populated on cache miss and proactively invalidated when
//! domains/subdomains change via the API.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use uuid::Uuid;

/// How long a positive cache entry lives before expiry.
const POSITIVE_TTL: Duration = Duration::from_secs(60);

/// How long a negative (not-found) cache entry lives.
const NEGATIVE_TTL: Duration = Duration::from_secs(5);

/// How often the background evictor runs.
const EVICT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct PositiveEntry {
    project_id: Uuid,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct NegativeEntry {
    expires_at: Instant,
}

/// Result of a cache lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    /// Host maps to this project.
    Hit(Uuid),
    /// Host was recently looked up and found to not exist.
    NegativeHit,
    /// No cache entry — caller must query the database.
    Miss,
}

/// Thread-safe in-memory routing cache.
#[derive(Clone)]
pub struct RoutingCache {
    inner: Arc<Inner>,
}

struct Inner {
    positive: RwLock<HashMap<String, PositiveEntry>>,
    negative: RwLock<HashMap<String, NegativeEntry>>,
}

impl RoutingCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                positive: RwLock::new(HashMap::new()),
                negative: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Look up a host in the cache.
    pub async fn lookup(&self, host: &str) -> CacheLookup {
        // Check positive cache first.
        {
            let pos = self.inner.positive.read().await;
            if let Some(entry) = pos.get(host) {
                if entry.expires_at > Instant::now() {
                    return CacheLookup::Hit(entry.project_id);
                }
                // Expired — fall through to miss (evictor will clean up).
            }
        }

        // Check negative cache.
        {
            let neg = self.inner.negative.read().await;
            if let Some(entry) = neg.get(host) {
                if entry.expires_at > Instant::now() {
                    return CacheLookup::NegativeHit;
                }
            }
        }

        CacheLookup::Miss
    }

    /// Insert a positive host → project_id mapping.
    pub async fn insert(&self, host: String, project_id: Uuid) {
        // Remove from negative cache if present.
        {
            let mut neg = self.inner.negative.write().await;
            neg.remove(&host);
        }

        let mut pos = self.inner.positive.write().await;
        pos.insert(
            host,
            PositiveEntry {
                project_id,
                expires_at: Instant::now() + POSITIVE_TTL,
            },
        );
    }

    /// Insert a negative (not-found) entry for a host.
    pub async fn insert_negative(&self, host: String) {
        let mut neg = self.inner.negative.write().await;
        neg.insert(
            host,
            NegativeEntry {
                expires_at: Instant::now() + NEGATIVE_TTL,
            },
        );
    }

    /// Invalidate all cache entries for a specific host.
    pub async fn invalidate_host(&self, host: &str) {
        {
            let mut pos = self.inner.positive.write().await;
            pos.remove(host);
        }
        {
            let mut neg = self.inner.negative.write().await;
            neg.remove(host);
        }
    }

    /// Invalidate all cache entries that map to a given project_id.
    pub async fn invalidate_project(&self, project_id: Uuid) {
        {
            let mut pos = self.inner.positive.write().await;
            pos.retain(|_, entry| entry.project_id != project_id);
        }
        // Negative entries don't carry a project_id, so nothing to do there.
    }

    /// Remove expired entries from both caches.
    async fn evict_expired(&self) {
        let now = Instant::now();
        {
            let mut pos = self.inner.positive.write().await;
            pos.retain(|_, entry| entry.expires_at > now);
        }
        {
            let mut neg = self.inner.negative.write().await;
            neg.retain(|_, entry| entry.expires_at > now);
        }
    }

    /// Spawn a background task that periodically evicts expired entries.
    pub fn spawn_evictor(&self) {
        let cache = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(EVICT_INTERVAL).await;
                cache.evict_expired().await;
            }
        });
    }
}

impl Default for RoutingCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn miss_on_empty_cache() {
        let cache = RoutingCache::new();
        assert_eq!(cache.lookup("example.com").await, CacheLookup::Miss);
    }

    #[tokio::test]
    async fn insert_and_hit() {
        let cache = RoutingCache::new();
        let id = Uuid::new_v4();
        cache.insert("app.rift.dev".into(), id).await;
        assert_eq!(cache.lookup("app.rift.dev").await, CacheLookup::Hit(id));
    }

    #[tokio::test]
    async fn negative_hit() {
        let cache = RoutingCache::new();
        cache.insert_negative("gone.rift.dev".into()).await;
        assert_eq!(
            cache.lookup("gone.rift.dev").await,
            CacheLookup::NegativeHit
        );
    }

    #[tokio::test]
    async fn invalidate_host_clears_positive() {
        let cache = RoutingCache::new();
        let id = Uuid::new_v4();
        cache.insert("app.rift.dev".into(), id).await;
        cache.invalidate_host("app.rift.dev").await;
        assert_eq!(cache.lookup("app.rift.dev").await, CacheLookup::Miss);
    }

    #[tokio::test]
    async fn invalidate_host_clears_negative() {
        let cache = RoutingCache::new();
        cache.insert_negative("gone.rift.dev".into()).await;
        cache.invalidate_host("gone.rift.dev").await;
        assert_eq!(cache.lookup("gone.rift.dev").await, CacheLookup::Miss);
    }

    #[tokio::test]
    async fn invalidate_project_clears_matching_hosts() {
        let cache = RoutingCache::new();
        let project_a = Uuid::new_v4();
        let project_b = Uuid::new_v4();

        cache.insert("a1.rift.dev".into(), project_a).await;
        cache.insert("a2.rift.dev".into(), project_a).await;
        cache.insert("b1.rift.dev".into(), project_b).await;

        cache.invalidate_project(project_a).await;

        assert_eq!(cache.lookup("a1.rift.dev").await, CacheLookup::Miss);
        assert_eq!(cache.lookup("a2.rift.dev").await, CacheLookup::Miss);
        assert_eq!(
            cache.lookup("b1.rift.dev").await,
            CacheLookup::Hit(project_b)
        );
    }

    #[tokio::test]
    async fn evict_removes_expired_entries() {
        let cache = RoutingCache::new();
        let id = Uuid::new_v4();

        // Manually insert with already-expired TTL via the inner maps.
        {
            let mut pos = cache.inner.positive.write().await;
            pos.insert(
                "expired.rift.dev".into(),
                PositiveEntry {
                    project_id: id,
                    expires_at: Instant::now() - Duration::from_secs(1),
                },
            );
        }
        {
            let mut neg = cache.inner.negative.write().await;
            neg.insert(
                "neg-expired.rift.dev".into(),
                NegativeEntry {
                    expires_at: Instant::now() - Duration::from_secs(1),
                },
            );
        }

        // Before eviction, entries are present but expired (lookup returns Miss for expired).
        assert_eq!(cache.lookup("expired.rift.dev").await, CacheLookup::Miss);
        assert_eq!(
            cache.lookup("neg-expired.rift.dev").await,
            CacheLookup::Miss
        );

        cache.evict_expired().await;

        // After eviction, entries are actually removed from the maps.
        assert!(cache.inner.positive.read().await.is_empty());
        assert!(cache.inner.negative.read().await.is_empty());
    }

    #[tokio::test]
    async fn insert_positive_removes_negative() {
        let cache = RoutingCache::new();
        cache.insert_negative("app.rift.dev".into()).await;
        assert_eq!(cache.lookup("app.rift.dev").await, CacheLookup::NegativeHit);

        let id = Uuid::new_v4();
        cache.insert("app.rift.dev".into(), id).await;
        assert_eq!(cache.lookup("app.rift.dev").await, CacheLookup::Hit(id));
    }

    #[tokio::test]
    async fn concurrent_reads_and_writes() {
        let cache = RoutingCache::new();
        let id = Uuid::new_v4();
        cache.insert("shared.rift.dev".into(), id).await;

        let mut handles = Vec::new();
        for _ in 0..100 {
            let c = cache.clone();
            let expected_id = id;
            handles.push(tokio::spawn(async move {
                match c.lookup("shared.rift.dev").await {
                    CacheLookup::Hit(pid) => assert_eq!(pid, expected_id),
                    other => panic!("expected Hit, got {:?}", other),
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    }
}
