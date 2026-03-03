use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::models::{Service, ServiceLog},
    error::AppError,
};

pub async fn create_service(
    pool: &PgPool,
    user_id: Uuid,
    service_type: &str,
    name: &str,
    config: &serde_json::Value,
) -> Result<Service, AppError> {
    sqlx::query_as::<_, Service>(
        r#"
        INSERT INTO services (user_id, service_type, name, config)
        VALUES ($1, $2, $3, $4)
        RETURNING id, user_id, service_type, name, status, config,
                  connection_info, error_message, started_at, stopped_at,
                  created_at, updated_at
        "#,
    )
    .bind(user_id)
    .bind(service_type)
    .bind(name)
    .bind(config)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_service_for_user(
    pool: &PgPool,
    service_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Service>, AppError> {
    sqlx::query_as::<_, Service>(
        r#"
        SELECT id, user_id, service_type, name, status, config,
               connection_info, error_message, started_at, stopped_at,
               created_at, updated_at
        FROM services
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(service_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_service_by_id(
    pool: &PgPool,
    service_id: Uuid,
) -> Result<Option<Service>, AppError> {
    sqlx::query_as::<_, Service>(
        r#"
        SELECT id, user_id, service_type, name, status, config,
               connection_info, error_message, started_at, stopped_at,
               created_at, updated_at
        FROM services
        WHERE id = $1
        "#,
    )
    .bind(service_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_services_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<Service>, AppError> {
    sqlx::query_as::<_, Service>(
        r#"
        SELECT id, user_id, service_type, name, status, config,
               connection_info, error_message, started_at, stopped_at,
               created_at, updated_at
        FROM services
        WHERE user_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_running_service_ids(pool: &PgPool) -> Result<Vec<Uuid>, AppError> {
    let rows = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM services WHERE status = 'running'",
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(rows)
}

pub async fn update_status(
    pool: &PgPool,
    service_id: Uuid,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE services
        SET status = $2,
            error_message = $3,
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(service_id)
    .bind(status)
    .bind(error_message)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn update_status_started(
    pool: &PgPool,
    service_id: Uuid,
    status: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE services
        SET status = $2,
            started_at = now(),
            stopped_at = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(service_id)
    .bind(status)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn update_status_stopped(
    pool: &PgPool,
    service_id: Uuid,
    status: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE services
        SET status = $2,
            stopped_at = now(),
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(service_id)
    .bind(status)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn update_connection_info(
    pool: &PgPool,
    service_id: Uuid,
    connection_info: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE services
        SET connection_info = $2,
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(service_id)
    .bind(connection_info)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn delete_service(pool: &PgPool, service_id: Uuid) -> Result<(), AppError> {
    sqlx::query("DELETE FROM services WHERE id = $1")
        .bind(service_id)
        .execute(pool)
        .await
        .map_err(AppError::Db)?;

    Ok(())
}

pub async fn insert_log(
    pool: &PgPool,
    service_id: Uuid,
    level: &str,
    message: &str,
    source: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO service_logs (service_id, level, message, source)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(service_id)
    .bind(level)
    .bind(message)
    .bind(source)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn insert_log_returning(
    pool: &PgPool,
    service_id: Uuid,
    level: &str,
    message: &str,
    source: &str,
) -> Result<ServiceLog, AppError> {
    sqlx::query_as::<_, ServiceLog>(
        r#"
        INSERT INTO service_logs (service_id, level, message, source)
        VALUES ($1, $2, $3, $4)
        RETURNING id, service_id, timestamp, level, message, source
        "#,
    )
    .bind(service_id)
    .bind(level)
    .bind(message)
    .bind(source)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_logs_for_service(
    pool: &PgPool,
    service_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<ServiceLog>, AppError> {
    sqlx::query_as::<_, ServiceLog>(
        r#"
        SELECT
            l.id,
            l.service_id,
            l.timestamp,
            l.level,
            l.message,
            l.source
        FROM service_logs l
        JOIN services s ON s.id = l.service_id
        WHERE l.service_id = $1 AND s.user_id = $2
        ORDER BY l.id ASC
        "#,
    )
    .bind(service_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}
