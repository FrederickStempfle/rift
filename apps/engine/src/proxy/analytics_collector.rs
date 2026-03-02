use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

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
    tx: mpsc::Sender<RequestEvent>,
    dropped_events: Arc<AtomicU64>,
}

impl AnalyticsCollector {
    pub fn new(pool: PgPool, access_log_retention_days: u16, cleanup_interval_secs: u64) -> Self {
        let (tx, rx) = mpsc::channel(20_000);
        tokio::spawn(flush_loop(
            rx,
            pool,
            access_log_retention_days,
            std::time::Duration::from_secs(cleanup_interval_secs.max(30)),
        ));
        Self {
            tx,
            dropped_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Non-blocking fire-and-forget. Drops when the queue is full or closed.
    pub fn record(&self, event: RequestEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                let dropped = self.dropped_events.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 || dropped.is_multiple_of(100) {
                    tracing::warn!(
                        dropped_events = dropped,
                        "analytics queue full, dropping request events"
                    );
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!("analytics queue closed, dropping request event");
            }
        }
    }
}

type AnalyticsKey = (Uuid, chrono::DateTime<chrono::Utc>);
type AnalyticsCounters = (i64, i64, i64, i64);

// (project_id, bucket, referrer_domain) -> requests
type ReferrerKey = (Uuid, chrono::DateTime<chrono::Utc>, String);
// (project_id, bucket, path) -> (requests, errors, total_ms)
type PathKey = (Uuid, chrono::DateTime<chrono::Utc>, String);
type PathCounters = (i64, i64, i64);

async fn flush_loop(
    mut rx: mpsc::Receiver<RequestEvent>,
    pool: PgPool,
    access_log_retention_days: u16,
    cleanup_interval: std::time::Duration,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut last_cleanup_at = std::time::Instant::now();
    // (project_id, hour_bucket) -> (requests, errors, total_ms, cold_starts)
    let mut buffer: HashMap<AnalyticsKey, AnalyticsCounters> = HashMap::new();
    let mut referrer_buffer: HashMap<ReferrerKey, i64> = HashMap::new();
    let mut path_buffer: HashMap<PathKey, PathCounters> = HashMap::new();
    let mut access_buffer: Vec<crate::db::access_logs::NewAccessLog> = Vec::new();

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => ingest_event(
                        event,
                        &mut buffer,
                        &mut referrer_buffer,
                        &mut path_buffer,
                        &mut access_buffer,
                    ),
                    None => {
                        flush_buffers(&pool, &mut buffer, &mut referrer_buffer, &mut path_buffer, &mut access_buffer).await;
                        break;
                    }
                }
            }
            _ = interval.tick() => {
                flush_buffers(&pool, &mut buffer, &mut referrer_buffer, &mut path_buffer, &mut access_buffer).await;
                if access_log_retention_days > 0 && last_cleanup_at.elapsed() >= cleanup_interval {
                    let cutoff = Utc::now() - chrono::Duration::days(i64::from(access_log_retention_days));
                    match crate::db::access_logs::delete_older_than(&pool, cutoff).await {
                        Ok(rows) if rows > 0 => {
                            tracing::info!(deleted_rows = rows, "cleaned expired access logs");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to cleanup expired access logs");
                        }
                    }
                    last_cleanup_at = std::time::Instant::now();
                }
            }
        }
    }
}

fn ingest_event(
    event: RequestEvent,
    buffer: &mut HashMap<AnalyticsKey, AnalyticsCounters>,
    referrer_buffer: &mut HashMap<ReferrerKey, i64>,
    path_buffer: &mut HashMap<PathKey, PathCounters>,
    access_buffer: &mut Vec<crate::db::access_logs::NewAccessLog>,
) {
    let bucket = event
        .timestamp
        .with_minute(0)
        .and_then(|timestamp| timestamp.with_second(0))
        .and_then(|timestamp| timestamp.with_nanosecond(0))
        .unwrap_or(event.timestamp);

    let is_error = event.status >= 400;
    if let Some(project_id) = event.project_id {
        let entry = buffer.entry((project_id, bucket)).or_default();
        entry.0 += 1;
        if is_error {
            entry.1 += 1;
        }
        entry.2 += event.duration_ms as i64;
        if event.cold_start {
            entry.3 += 1;
        }

        let referrer = event
            .referer
            .clone()
            .unwrap_or_else(|| "(direct)".to_owned());
        *referrer_buffer
            .entry((project_id, bucket, referrer))
            .or_default() += 1;

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

async fn flush_buffers(
    pool: &PgPool,
    buffer: &mut HashMap<AnalyticsKey, AnalyticsCounters>,
    referrer_buffer: &mut HashMap<ReferrerKey, i64>,
    path_buffer: &mut HashMap<PathKey, PathCounters>,
    access_buffer: &mut Vec<crate::db::access_logs::NewAccessLog>,
) {
    let entries: Vec<_> = buffer
        .iter()
        .map(
            |((project_id, bucket), (requests, errors, total_ms, cold_starts))| {
                (
                    *project_id,
                    bucket.to_owned(),
                    *requests,
                    *errors,
                    *total_ms,
                    *cold_starts,
                )
            },
        )
        .collect();
    for (project_id, bucket, requests, errors, total_ms, cold_starts) in entries {
        match crate::db::analytics::upsert_hourly(
            pool,
            project_id,
            bucket,
            requests,
            errors,
            total_ms,
            cold_starts,
        )
        .await
        {
            Ok(()) => {
                buffer.remove(&(project_id, bucket));
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to flush analytics bucket");
            }
        }
    }

    let referrer_entries: Vec<_> = referrer_buffer
        .iter()
        .map(|((project_id, bucket, referrer), requests)| {
            (*project_id, bucket.to_owned(), referrer.clone(), *requests)
        })
        .collect();
    for (project_id, bucket, referrer, requests) in referrer_entries {
        match crate::db::analytics::upsert_referrer(pool, project_id, bucket, &referrer, requests)
            .await
        {
            Ok(()) => {
                referrer_buffer.remove(&(project_id, bucket, referrer));
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to flush analytics referrer bucket");
            }
        }
    }

    let path_entries: Vec<_> = path_buffer
        .iter()
        .map(
            |((project_id, bucket, path), (requests, errors, total_ms))| {
                (
                    *project_id,
                    bucket.to_owned(),
                    path.clone(),
                    *requests,
                    *errors,
                    *total_ms,
                )
            },
        )
        .collect();
    for (project_id, bucket, path, requests, errors, total_ms) in path_entries {
        match crate::db::analytics::upsert_path(
            pool, project_id, bucket, &path, requests, errors, total_ms,
        )
        .await
        {
            Ok(()) => {
                path_buffer.remove(&(project_id, bucket, path));
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to flush analytics path bucket");
            }
        }
    }

    if !access_buffer.is_empty() {
        match crate::db::access_logs::insert_batch(pool, access_buffer).await {
            Ok(()) => {
                access_buffer.clear();
            }
            Err(e) => {
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
