use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};
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

#[derive(Debug, Clone, Default)]
pub struct AccessLogFilters {
    pub project_id: Option<Uuid>,
    pub before_id: Option<i64>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub status: Option<i32>,
    pub client_ip: Option<String>,
    pub limit: i64,
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

pub async fn list(
    pool: &PgPool,
    user_id: Uuid,
    mut filters: AccessLogFilters,
) -> Result<Vec<AccessLog>, AppError> {
    filters.limit = filters.limit.clamp(1, 1000);

    let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new(
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
        WHERE p.user_id = "#,
    );
    query_builder.push_bind(user_id);

    if let Some(project_id) = filters.project_id {
        query_builder.push(" AND l.project_id = ");
        query_builder.push_bind(project_id);
    }
    if let Some(before_id) = filters.before_id {
        query_builder.push(" AND l.id < ");
        query_builder.push_bind(before_id);
    }
    if let Some(from) = filters.from {
        query_builder.push(" AND l.timestamp >= ");
        query_builder.push_bind(from);
    }
    if let Some(to) = filters.to {
        query_builder.push(" AND l.timestamp <= ");
        query_builder.push_bind(to);
    }
    if let Some(host) = filters.host {
        query_builder.push(" AND lower(l.host) = lower(");
        query_builder.push_bind(host);
        query_builder.push(")");
    }
    if let Some(path_prefix) = filters.path_prefix {
        query_builder.push(" AND l.path LIKE ");
        query_builder.push_bind(format!("{path_prefix}%"));
    }
    if let Some(status) = filters.status {
        query_builder.push(" AND l.status = ");
        query_builder.push_bind(status);
    }
    if let Some(client_ip) = filters.client_ip {
        query_builder.push(" AND l.client_ip = ");
        query_builder.push_bind(client_ip);
    }

    query_builder.push(" ORDER BY l.id DESC LIMIT ");
    query_builder.push_bind(filters.limit);

    query_builder
        .build_query_as::<AccessLog>()
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)
}

pub async fn list_for_project(
    pool: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<AccessLog>, AppError> {
    list(
        pool,
        user_id,
        AccessLogFilters {
            project_id: Some(project_id),
            before_id,
            limit,
            ..AccessLogFilters::default()
        },
    )
    .await
}

pub async fn list_for_user(
    pool: &PgPool,
    user_id: Uuid,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<AccessLog>, AppError> {
    list(
        pool,
        user_id,
        AccessLogFilters {
            before_id,
            limit,
            ..AccessLogFilters::default()
        },
    )
    .await
}

pub async fn delete_older_than(pool: &PgPool, cutoff: DateTime<Utc>) -> Result<u64, AppError> {
    let result = sqlx::query("DELETE FROM access_logs WHERE timestamp < $1")
        .bind(cutoff)
        .execute(pool)
        .await
        .map_err(AppError::Db)?;
    Ok(result.rows_affected())
}
