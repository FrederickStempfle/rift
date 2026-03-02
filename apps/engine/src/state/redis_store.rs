//! Redis-backed [`StateStore`] for multi-node deployments.
//!
//! Key schema:
//! - `rift:placement:{project_id}` → JSON [`PlacementLease`], with TTL
//! - `rift:worker:{worker_id}` → JSON [`WorkerHeartbeat`], TTL 30s
//! - `rift:route:{host}` → JSON [`RoutingEntry`]
//! - Pub/sub channel: `rift:routing_updates`

use async_trait::async_trait;
use redis::AsyncCommands;
use uuid::Uuid;

use crate::error::AppError;

use super::{PlacementLease, RoutingEntry, StateStore, WorkerHeartbeat};

const WORKER_TTL_SECS: u64 = 30;
const ROUTING_CHANNEL: &str = "rift:routing_updates";

fn placement_key(project_id: Uuid) -> String {
    format!("rift:placement:{project_id}")
}

fn worker_key(worker_id: &str) -> String {
    format!("rift:worker:{worker_id}")
}

fn route_key(host: &str) -> String {
    format!("rift:route:{host}")
}

/// Redis-backed distributed state store.
pub struct RedisStateStore {
    client: redis::Client,
}

impl RedisStateStore {
    /// Connect to Redis using the given URL (e.g. `redis://127.0.0.1:6379`).
    pub fn new(redis_url: &str) -> Result<Self, AppError> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| AppError::Internal(format!("redis connection failed: {e}")))?;
        Ok(Self { client })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection, AppError> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| AppError::Internal(format!("redis connection failed: {e}")))
    }
}

/// Lua script for atomic compare-and-set placement acquisition.
///
/// KEYS[1] = placement key
/// ARGV[1] = JSON lease payload
/// ARGV[2] = TTL in seconds
/// ARGV[3] = new version (number)
///
/// Returns 1 if acquired, 0 if rejected.
const CAS_ACQUIRE_SCRIPT: &str = r#"
local current = redis.call('GET', KEYS[1])
if current == false then
    redis.call('SET', KEYS[1], ARGV[1], 'EX', tonumber(ARGV[2]))
    return 1
end
local parsed = cjson.decode(current)
if parsed.version < tonumber(ARGV[3]) then
    redis.call('SET', KEYS[1], ARGV[1], 'EX', tonumber(ARGV[2]))
    return 1
end
return 0
"#;

#[async_trait]
impl StateStore for RedisStateStore {
    async fn acquire_placement(
        &self,
        project_id: Uuid,
        lease: &PlacementLease,
    ) -> Result<bool, AppError> {
        let mut conn = self.conn().await?;
        let key = placement_key(project_id);
        let payload = serde_json::to_string(lease)
            .map_err(|e| AppError::Internal(format!("serialize lease: {e}")))?;

        let result: i32 = redis::Script::new(CAS_ACQUIRE_SCRIPT)
            .key(&key)
            .arg(&payload)
            .arg(lease.ttl_secs)
            .arg(lease.version)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| AppError::Internal(format!("redis acquire_placement: {e}")))?;

        Ok(result == 1)
    }

    async fn get_placement(&self, project_id: Uuid) -> Result<Option<PlacementLease>, AppError> {
        let mut conn = self.conn().await?;
        let key = placement_key(project_id);
        let val: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| AppError::Internal(format!("redis get_placement: {e}")))?;

        match val {
            Some(json) => {
                let lease: PlacementLease = serde_json::from_str(&json)
                    .map_err(|e| AppError::Internal(format!("parse placement: {e}")))?;
                Ok(Some(lease))
            }
            None => Ok(None),
        }
    }

    async fn release_placement(&self, project_id: Uuid, version: u64) -> Result<bool, AppError> {
        let mut conn = self.conn().await?;
        let key = placement_key(project_id);

        // Check-and-delete atomically via Lua
        let script = r#"
            local current = redis.call('GET', KEYS[1])
            if current == false then return 0 end
            local parsed = cjson.decode(current)
            if parsed.version == tonumber(ARGV[1]) then
                redis.call('DEL', KEYS[1])
                return 1
            end
            return 0
        "#;

        let result: i32 = redis::Script::new(script)
            .key(&key)
            .arg(version)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| AppError::Internal(format!("redis release_placement: {e}")))?;

        Ok(result == 1)
    }

    async fn renew_placement(&self, project_id: Uuid, ttl_secs: u64) -> Result<bool, AppError> {
        let mut conn = self.conn().await?;
        let key = placement_key(project_id);
        let renewed: bool = conn
            .expire(&key, ttl_secs as i64)
            .await
            .map_err(|e| AppError::Internal(format!("redis renew_placement: {e}")))?;
        Ok(renewed)
    }

    async fn send_heartbeat(&self, heartbeat: &WorkerHeartbeat) -> Result<(), AppError> {
        let mut conn = self.conn().await?;
        let key = worker_key(&heartbeat.worker_id);
        let payload = serde_json::to_string(heartbeat)
            .map_err(|e| AppError::Internal(format!("serialize heartbeat: {e}")))?;
        conn.set_ex::<_, _, ()>(&key, &payload, WORKER_TTL_SECS)
            .await
            .map_err(|e| AppError::Internal(format!("redis send_heartbeat: {e}")))?;
        Ok(())
    }

    async fn list_workers(&self) -> Result<Vec<WorkerHeartbeat>, AppError> {
        let mut conn = self.conn().await?;

        // Use SCAN instead of KEYS to avoid O(N) blocking in production.
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("rift:worker:*")
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(|e| AppError::Internal(format!("redis list_workers scan: {e}")))?;
            keys.extend(batch);
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let values: Vec<Option<String>> = conn
            .mget(&keys)
            .await
            .map_err(|e| AppError::Internal(format!("redis list_workers mget: {e}")))?;

        let mut workers = Vec::new();
        for val in values.into_iter().flatten() {
            if let Ok(hb) = serde_json::from_str::<WorkerHeartbeat>(&val) {
                workers.push(hb);
            }
        }
        Ok(workers)
    }

    async fn remove_worker(&self, worker_id: &str) -> Result<(), AppError> {
        let mut conn = self.conn().await?;
        let key = worker_key(worker_id);
        conn.del::<_, ()>(&key)
            .await
            .map_err(|e| AppError::Internal(format!("redis remove_worker: {e}")))?;
        Ok(())
    }

    async fn set_routing(&self, entry: &RoutingEntry) -> Result<(), AppError> {
        let mut conn = self.conn().await?;
        let key = route_key(&entry.host);
        let payload = serde_json::to_string(entry)
            .map_err(|e| AppError::Internal(format!("serialize routing: {e}")))?;
        conn.set::<_, _, ()>(&key, &payload)
            .await
            .map_err(|e| AppError::Internal(format!("redis set_routing: {e}")))?;
        Ok(())
    }

    async fn get_routing(&self, host: &str) -> Result<Option<RoutingEntry>, AppError> {
        let mut conn = self.conn().await?;
        let key = route_key(host);
        let val: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| AppError::Internal(format!("redis get_routing: {e}")))?;

        match val {
            Some(json) => {
                let entry: RoutingEntry = serde_json::from_str(&json)
                    .map_err(|e| AppError::Internal(format!("parse routing: {e}")))?;
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    async fn remove_routing(&self, host: &str) -> Result<(), AppError> {
        let mut conn = self.conn().await?;
        let key = route_key(host);
        conn.del::<_, ()>(&key)
            .await
            .map_err(|e| AppError::Internal(format!("redis remove_routing: {e}")))?;
        Ok(())
    }

    async fn publish_routing_update(&self, entry: &RoutingEntry) -> Result<(), AppError> {
        let mut conn = self.conn().await?;
        let payload = serde_json::to_string(entry)
            .map_err(|e| AppError::Internal(format!("serialize routing update: {e}")))?;
        conn.publish::<_, _, ()>(ROUTING_CHANNEL, &payload)
            .await
            .map_err(|e| AppError::Internal(format!("redis publish: {e}")))?;
        Ok(())
    }
}
