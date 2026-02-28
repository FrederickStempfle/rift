use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AnalyticsBucket {
    pub project_id: Uuid,
    pub bucket: DateTime<Utc>,
    pub requests: i64,
    pub errors: i64,
    pub total_ms: i64,
}

pub async fn upsert_hourly(
    pool: &PgPool,
    project_id: Uuid,
    bucket: DateTime<Utc>,
    requests: i64,
    errors: i64,
    total_ms: i64,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO analytics_hourly (project_id, bucket, requests, errors, total_ms)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (project_id, bucket) DO UPDATE SET
            requests = analytics_hourly.requests + EXCLUDED.requests,
            errors   = analytics_hourly.errors   + EXCLUDED.errors,
            total_ms = analytics_hourly.total_ms  + EXCLUDED.total_ms
        "#,
    )
    .bind(project_id)
    .bind(bucket)
    .bind(requests)
    .bind(errors)
    .bind(total_ms)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

pub async fn query_hourly(
    pool: &PgPool,
    project_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<AnalyticsBucket>, AppError> {
    sqlx::query_as::<_, AnalyticsBucket>(
        r#"
        SELECT project_id, bucket, requests, errors, total_ms
        FROM analytics_hourly
        WHERE project_id = $1 AND bucket >= $2
        ORDER BY bucket ASC
        "#,
    )
    .bind(project_id)
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}
