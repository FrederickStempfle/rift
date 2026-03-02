use std::collections::HashMap;
use std::net::IpAddr;

use chrono::{DateTime, Timelike, Utc};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug)]
pub struct RequestEvent {
    pub project_id: Option<Uuid>,
    pub timestamp: DateTime<Utc>,
    pub client_ip: IpAddr,
    pub host: Option<String>,
    pub method: String,
    pub status: u16,
    pub duration_ms: u64,
    /// Whether this request triggered a cold start (worker specialization).
    pub cold_start: bool,
    /// Request path (e.g. "/api/users").
    pub path: String,
    /// Referrer domain (e.g. "google.com") or None for direct traffic.
    pub referer: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AnalyticsCollector {
    tx: mpsc::UnboundedSender<RequestEvent>,
}

impl AnalyticsCollector {
    pub fn new(pool: PgPool) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(flush_loop(rx, pool));
        Self { tx }
    }

    /// Non-blocking fire-and-forget. Silently drops if the channel is closed.
    pub fn record(&self, event: RequestEvent) {
        let _ = self.tx.send(event);
    }
}

type AnalyticsKey = (Uuid, chrono::DateTime<chrono::Utc>);
type AnalyticsCounters = (i64, i64, i64, i64);

// (project_id, bucket, referrer_domain) -> requests
type ReferrerKey = (Uuid, chrono::DateTime<chrono::Utc>, String);
// (project_id, bucket, path) -> (requests, errors, total_ms)
type PathKey = (Uuid, chrono::DateTime<chrono::Utc>, String);
type PathCounters = (i64, i64, i64);

async fn flush_loop(mut rx: mpsc::UnboundedReceiver<RequestEvent>, pool: PgPool) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    // (project_id, hour_bucket) -> (requests, errors, total_ms, cold_starts)
    let mut buffer: HashMap<AnalyticsKey, AnalyticsCounters> = HashMap::new();
    let mut referrer_buffer: HashMap<ReferrerKey, i64> = HashMap::new();
    let mut path_buffer: HashMap<PathKey, PathCounters> = HashMap::new();
    let mut access_buffer: Vec<crate::db::access_logs::NewAccessLog> = Vec::new();

    loop {
        interval.tick().await;

        // Drain all pending events
        while let Ok(event) = rx.try_recv() {
            // Truncate to hour — these unwraps are safe (0 is always valid)
            let bucket = event
                .timestamp
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();

            let is_error = event.status >= 400;
            if let Some(project_id) = event.project_id {
                // Hourly aggregate
                let entry = buffer.entry((project_id, bucket)).or_default();
                entry.0 += 1;
                if is_error {
                    entry.1 += 1;
                }
                entry.2 += event.duration_ms as i64;
                if event.cold_start {
                    entry.3 += 1;
                }

                // Referrer aggregate
                let referrer = event.referer.unwrap_or_else(|| "(direct)".to_owned());
                *referrer_buffer
                    .entry((project_id, bucket, referrer))
                    .or_default() += 1;

                // Path aggregate
                let path_entry = path_buffer
                    .entry((project_id, bucket, event.path.clone()))
                    .or_default();
                path_entry.0 += 1;
                if is_error {
                    path_entry.1 += 1;
                }
                path_entry.2 += event.duration_ms as i64;
            }

            access_buffer.push(crate::db::access_logs::NewAccessLog {
                project_id: event.project_id,
                timestamp: event.timestamp,
                client_ip: event.client_ip.to_string(),
                host: event.host.map(|value| truncate(value, 255)),
                method: truncate(event.method, 16),
                path: truncate(event.path, 2048),
                status: event.status as i32,
                duration_ms: event.duration_ms as i64,
            });
        }

        // Flush aggregated hourly buckets to DB
        let entries: Vec<_> = buffer.drain().collect();
        for ((project_id, bucket), (requests, errors, total_ms, cold_starts)) in entries {
            if let Err(e) = crate::db::analytics::upsert_hourly(
                &pool,
                project_id,
                bucket,
                requests,
                errors,
                total_ms,
                cold_starts,
            )
            .await
            {
                tracing::warn!(error = %e, "failed to flush analytics bucket");
            }
        }

        // Flush referrer buckets to DB
        let referrer_entries: Vec<_> = referrer_buffer.drain().collect();
        for ((project_id, bucket, referrer), requests) in referrer_entries {
            if let Err(e) =
                crate::db::analytics::upsert_referrer(&pool, project_id, bucket, &referrer, requests)
                    .await
            {
                tracing::warn!(error = %e, "failed to flush analytics referrer bucket");
            }
        }

        // Flush path buckets to DB
        let path_entries: Vec<_> = path_buffer.drain().collect();
        for ((project_id, bucket, path), (requests, errors, total_ms)) in path_entries {
            if let Err(e) = crate::db::analytics::upsert_path(
                &pool, project_id, bucket, &path, requests, errors, total_ms,
            )
            .await
            {
                tracing::warn!(error = %e, "failed to flush analytics path bucket");
            }
        }

        if !access_buffer.is_empty() {
            let to_insert = std::mem::take(&mut access_buffer);
            if let Err(e) = crate::db::access_logs::insert_batch(&pool, &to_insert).await {
                tracing::warn!(error = %e, "failed to flush access logs");
            }
        }
    }
}

fn truncate(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    value.chars().take(max_chars).collect()
}
