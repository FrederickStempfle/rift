use std::collections::HashMap;

use chrono::{Timelike, Utc};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug)]
pub struct RequestEvent {
    pub project_id: Uuid,
    pub status: u16,
    pub duration_ms: u64,
    /// Whether this request triggered a cold start (worker specialization).
    pub cold_start: bool,
    /// Request path (e.g. "/api/users").
    pub path: Option<String>,
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

    loop {
        interval.tick().await;

        // Drain all pending events
        while let Ok(event) = rx.try_recv() {
            let now = Utc::now();
            // Truncate to hour — these unwraps are safe (0 is always valid)
            let bucket = now
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();

            let is_error = event.status >= 400;

            // Hourly aggregate
            let entry = buffer.entry((event.project_id, bucket)).or_default();
            entry.0 += 1;
            if is_error {
                entry.1 += 1;
            }
            entry.2 += event.duration_ms as i64;
            if event.cold_start {
                entry.3 += 1;
            }

            // Referrer aggregate
            let referrer = event
                .referer
                .unwrap_or_else(|| "(direct)".to_owned());
            *referrer_buffer
                .entry((event.project_id, bucket, referrer))
                .or_default() += 1;

            // Path aggregate
            if let Some(path) = event.path {
                let path_entry = path_buffer
                    .entry((event.project_id, bucket, path))
                    .or_default();
                path_entry.0 += 1;
                if is_error {
                    path_entry.1 += 1;
                }
                path_entry.2 += event.duration_ms as i64;
            }
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
    }
}
