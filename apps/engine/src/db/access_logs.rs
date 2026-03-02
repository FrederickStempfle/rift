use chrono::{DateTime, Utc};
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

use crate::{
    db::models::AccessLog,
    error::AppError,
};

#[derive(Debug, Clone)]
pub struct NewAccessLog {
    pub project_id: Option<Uuid>,
    pub timestamp: DateTime<Utc>,
    pub client_ip: String,
    pub host: Option<String>,
    pub method: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i64,
}

pub async fn insert_batch(pool: &PgPool, logs: &[NewAccessLog]) -> Result<(), AppError> {
    if logs.is_empty() {
        return Ok(());
    }

    let mut query_builder = QueryBuilder::new(
        "INSERT INTO access_logs (project_id, timestamp, client_ip, host, method, path, status, duration_ms) ",
    );

    query_builder.push_values(logs, |mut builder, log| {
        builder
            .push_bind(log.project_id)
            .push_bind(&log.timestamp)
            .push_bind(&log.client_ip)
            .push_bind(log.host.as_deref())
            .push_bind(&log.method)
            .push_bind(&log.path)
            .push_bind(log.status)
            .push_bind(log.duration_ms);
    });

    query_builder
        .build()
        .execute(pool)
        .await
        .map_err(AppError::Db)?;
    Ok(())
}

pub async fn list_for_project(
    pool: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<AccessLog>, AppError> {
    sqlx::query_as::<_, AccessLog>(
        r#"
        SELECT
            l.id,
            l.project_id,
            l.timestamp,
            l.client_ip,
            l.host,
            l.method,
            l.path,
            l.status,
            l.duration_ms
        FROM access_logs l
        JOIN projects p ON p.id = l.project_id
        WHERE p.user_id = $1
          AND l.project_id = $2
          AND ($3::bigint IS NULL OR l.id < $3)
        ORDER BY l.id DESC
        LIMIT $4
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(before_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_for_user(
    pool: &PgPool,
    user_id: Uuid,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<AccessLog>, AppError> {
    sqlx::query_as::<_, AccessLog>(
        r#"
        SELECT
            l.id,
            l.project_id,
            l.timestamp,
            l.client_ip,
            l.host,
            l.method,
            l.path,
            l.status,
            l.duration_ms
        FROM access_logs l
        JOIN projects p ON p.id = l.project_id
        WHERE p.user_id = $1
          AND ($2::bigint IS NULL OR l.id < $2)
        ORDER BY l.id DESC
        LIMIT $3
        "#,
    )
    .bind(user_id)
    .bind(before_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}
