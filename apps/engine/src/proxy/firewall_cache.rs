use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use ipnet::IpNet;
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::AppError;

const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct CacheEntry {
    mode: String,
    rules: Vec<(IpNet, String)>,
    fetched_at: Instant,
}

#[derive(Clone)]
pub struct FirewallCache {
    cache: Arc<RwLock<HashMap<Uuid, CacheEntry>>>,
}

impl FirewallCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn is_allowed(
        &self,
        pool: &PgPool,
        project_id: Uuid,
        client_ip: IpAddr,
    ) -> Result<bool, AppError> {
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&project_id) {
                if entry.fetched_at.elapsed() < CACHE_TTL {
                    return Ok(evaluate(&entry.mode, &entry.rules, client_ip));
                }
            }
        }

        let entry = self.fetch_and_cache(pool, project_id).await?;
        Ok(evaluate(&entry.mode, &entry.rules, client_ip))
    }

    async fn fetch_and_cache(
        &self,
        pool: &PgPool,
        project_id: Uuid,
    ) -> Result<CacheEntry, AppError> {
        let mode = crate::db::firewall::get_firewall_mode(pool, project_id).await?;
        let rules = crate::db::firewall::list_rules(pool, project_id).await?;

        let parsed_rules: Vec<(IpNet, String)> = rules
            .into_iter()
            .filter_map(|r| {
                let net = r
                    .cidr
                    .parse::<IpNet>()
                    .or_else(|_| {
                        r.cidr
                            .parse::<IpAddr>()
                            .map(|ip| IpNet::from(ip))
                    })
                    .ok()?;
                Some((net, r.action))
            })
            .collect();

        let entry = CacheEntry {
            mode,
            rules: parsed_rules,
            fetched_at: Instant::now(),
        };

        self.cache.write().await.insert(project_id, entry.clone());
        Ok(entry)
    }

    pub async fn invalidate(&self, project_id: Uuid) {
        self.cache.write().await.remove(&project_id);
    }
}

fn evaluate(mode: &str, rules: &[(IpNet, String)], ip: IpAddr) -> bool {
    match mode {
        "allow_all" => !rules
            .iter()
            .any(|(net, action)| action == "block" && net.contains(&ip)),
        "block_all" => rules
            .iter()
            .any(|(net, action)| action == "allow" && net.contains(&ip)),
        _ => true,
    }
}
