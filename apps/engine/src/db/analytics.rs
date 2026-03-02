use chrono::{DateTime, Utc};
use serde::Serialize;
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
    pub cold_starts: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ReferrerBucket {
    pub referrer: String,
    pub requests: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct PathBucket {
    pub path: String,
    pub requests: i64,
    pub errors: i64,
    pub total_ms: i64,
}

// ---------------------------------------------------------------------------
// Upserts
// ---------------------------------------------------------------------------

pub async fn upsert_hourly(
    pool: &PgPool,
    project_id: Uuid,
    bucket: DateTime<Utc>,
    requests: i64,
    errors: i64,
    total_ms: i64,
    cold_starts: i64,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO analytics_hourly (project_id, bucket, requests, errors, total_ms, cold_starts)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (project_id, bucket) DO UPDATE SET
            requests    = analytics_hourly.requests    + EXCLUDED.requests,
            errors      = analytics_hourly.errors      + EXCLUDED.errors,
            total_ms    = analytics_hourly.total_ms    + EXCLUDED.total_ms,
            cold_starts = analytics_hourly.cold_starts + EXCLUDED.cold_starts
        "#,
    )
    .bind(project_id)
    .bind(bucket)
    .bind(requests)
    .bind(errors)
    .bind(total_ms)
    .bind(cold_starts)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

pub async fn upsert_referrer(
    pool: &PgPool,
    project_id: Uuid,
    bucket: DateTime<Utc>,
    referrer: &str,
    requests: i64,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO analytics_referrers (project_id, bucket, referrer, requests)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (project_id, bucket, referrer) DO UPDATE SET
            requests = analytics_referrers.requests + EXCLUDED.requests
        "#,
    )
    .bind(project_id)
    .bind(bucket)
    .bind(referrer)
    .bind(requests)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

pub async fn upsert_path(
    pool: &PgPool,
    project_id: Uuid,
    bucket: DateTime<Utc>,
    path: &str,
    requests: i64,
    errors: i64,
    total_ms: i64,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO analytics_paths (project_id, bucket, path, requests, errors, total_ms)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (project_id, bucket, path) DO UPDATE SET
            requests = analytics_paths.requests + EXCLUDED.requests,
            errors   = analytics_paths.errors   + EXCLUDED.errors,
            total_ms = analytics_paths.total_ms + EXCLUDED.total_ms
        "#,
    )
    .bind(project_id)
    .bind(bucket)
    .bind(path)
    .bind(requests)
    .bind(errors)
    .bind(total_ms)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

pub async fn query_hourly(
    pool: &PgPool,
    project_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<AnalyticsBucket>, AppError> {
    sqlx::query_as::<_, AnalyticsBucket>(
        r#"
        SELECT project_id, bucket, requests, errors, total_ms, cold_starts
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

/// Aggregate hourly analytics across all projects for a user.
pub async fn query_hourly_for_user(
    pool: &PgPool,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<AnalyticsBucket>, AppError> {
    sqlx::query_as::<_, AnalyticsBucket>(
        r#"
        SELECT '00000000-0000-0000-0000-000000000000'::uuid AS project_id,
               ah.bucket,
               SUM(ah.requests)::bigint AS requests,
               SUM(ah.errors)::bigint AS errors,
               SUM(ah.total_ms)::bigint AS total_ms,
               SUM(ah.cold_starts)::bigint AS cold_starts
        FROM analytics_hourly ah
        JOIN projects p ON p.id = ah.project_id
        WHERE p.user_id = $1 AND ah.bucket >= $2
        GROUP BY ah.bucket
        ORDER BY ah.bucket ASC
        "#,
    )
    .bind(user_id)
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

/// Top referrers aggregated across all buckets in the period.
pub async fn query_top_referrers(
    pool: &PgPool,
    project_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<ReferrerBucket>, AppError> {
    sqlx::query_as::<_, ReferrerBucket>(
        r#"
        SELECT referrer, SUM(requests)::bigint AS requests
        FROM analytics_referrers
        WHERE project_id = $1 AND bucket >= $2
        GROUP BY referrer
        ORDER BY requests DESC
        LIMIT $3
        "#,
    )
    .bind(project_id)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

/// Top referrers aggregated across all projects for a user.
pub async fn query_top_referrers_for_user(
    pool: &PgPool,
    user_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<ReferrerBucket>, AppError> {
    sqlx::query_as::<_, ReferrerBucket>(
        r#"
        SELECT ar.referrer, SUM(ar.requests)::bigint AS requests
        FROM analytics_referrers ar
        JOIN projects p ON p.id = ar.project_id
        WHERE p.user_id = $1 AND ar.bucket >= $2
        GROUP BY ar.referrer
        ORDER BY requests DESC
        LIMIT $3
        "#,
    )
    .bind(user_id)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

/// Top paths aggregated across all buckets in the period.
pub async fn query_top_paths(
    pool: &PgPool,
    project_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<PathBucket>, AppError> {
    sqlx::query_as::<_, PathBucket>(
        r#"
        SELECT path, SUM(requests)::bigint AS requests, SUM(errors)::bigint AS errors, SUM(total_ms)::bigint AS total_ms
        FROM analytics_paths
        WHERE project_id = $1 AND bucket >= $2
        GROUP BY path
        ORDER BY requests DESC
        LIMIT $3
        "#,
    )
    .bind(project_id)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

/// Top paths aggregated across all projects for a user.
pub async fn query_top_paths_for_user(
    pool: &PgPool,
    user_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<PathBucket>, AppError> {
    sqlx::query_as::<_, PathBucket>(
        r#"
        SELECT ap.path, SUM(ap.requests)::bigint AS requests, SUM(ap.errors)::bigint AS errors, SUM(ap.total_ms)::bigint AS total_ms
        FROM analytics_paths ap
        JOIN projects p ON p.id = ap.project_id
        WHERE p.user_id = $1 AND ap.bucket >= $2
        GROUP BY ap.path
        ORDER BY requests DESC
        LIMIT $3
        "#,
    )
    .bind(user_id)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}
