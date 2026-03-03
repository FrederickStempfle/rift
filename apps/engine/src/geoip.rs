use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lru::LruCache;
use tokio::sync::Mutex;

const CACHE_SIZE: usize = 2048;
const CACHE_TTL: Duration = Duration::from_secs(86_400); // 24 hours

#[derive(Clone, Debug)]
struct CachedGeo {
    lat: f64,
    lng: f64,
    country: Option<String>,
    fetched_at: Instant,
}

#[derive(Clone, Debug)]
pub struct GeoIpResolver {
    cache: Arc<Mutex<LruCache<String, CachedGeo>>>,
    client: reqwest::Client,
}

impl GeoIpResolver {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(CACHE_SIZE).unwrap(),
            ))),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .expect("failed to build GeoIP HTTP client"),
        }
    }

    /// Resolve an IP to (lat, lng, country). Returns None for private/loopback IPs
    /// or if the lookup fails. Results are cached per /24 subnet for 24 hours.
    pub async fn lookup(&self, ip: IpAddr) -> Option<(f64, f64, Option<String>)> {
        if is_private(ip) {
            return None;
        }

        let key = subnet_key(ip);

        // Check cache first
        {
            let mut cache = self.cache.lock().await;
            if let Some(cached) = cache.get(&key) {
                if cached.fetched_at.elapsed() < CACHE_TTL {
                    return Some((cached.lat, cached.lng, cached.country.clone()));
                }
                // Expired — remove and re-fetch
                cache.pop(&key);
            }
        }

        // Fetch from ip-api.com (free, no key needed, 45 req/min)
        let url = format!("http://ip-api.com/json/{}?fields=lat,lon,country", ip);
        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, %ip, "GeoIP lookup failed");
                return None;
            }
        };

        #[derive(serde::Deserialize)]
        struct IpApiResponse {
            lat: Option<f64>,
            lon: Option<f64>,
            country: Option<String>,
        }

        let body: IpApiResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(error = %e, %ip, "GeoIP response parse failed");
                return None;
            }
        };

        let (lat, lng) = match (body.lat, body.lon) {
            (Some(la), Some(lo)) => (la, lo),
            _ => return None,
        };

        let entry = CachedGeo {
            lat,
            lng,
            country: body.country.clone(),
            fetched_at: Instant::now(),
        };

        self.cache.lock().await.put(key, entry);

        Some((lat, lng, body.country))
    }
}

impl Default for GeoIpResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key: /24 subnet (or full address for IPv6) for privacy + cache hit rate.
fn subnet_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            format!("{}.{}.{}", octets[0], octets[1], octets[2])
        }
        IpAddr::V6(_) => ip.to_string(), // IPv6: cache full address
    }
}

/// Returns true for loopback, private (RFC1918), link-local, and unspecified addresses.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}
