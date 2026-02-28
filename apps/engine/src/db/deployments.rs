use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::models::{DeployLog, Deployment},
    error::AppError,
};

#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub project_id: Uuid,
    pub branch: String,
    pub commit_sha: String,
    pub commit_message: Option<String>,
}

pub async fn create_deployment(
    pool: &PgPool,
    input: NewDeployment,
) -> Result<Deployment, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        INSERT INTO deployments (
            project_id,
            commit_sha,
            commit_message,
            branch,
            status,
            started_at
        )
        VALUES ($1, $2, $3, $4, 'queued', now())
        RETURNING
            id,
            project_id,
            commit_sha,
            commit_message,
            branch,
            status::text AS status,
            build_duration_ms,
            url,
            port,
            started_at,
            finished_at,
            created_at
        "#,
    )
    .bind(input.project_id)
    .bind(input.commit_sha)
    .bind(input.commit_message)
    .bind(input.branch)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn update_source_metadata(
    pool: &PgPool,
    deployment_id: Uuid,
    commit_sha: &str,
    commit_message: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE deployments
        SET commit_sha = $2,
            commit_message = $3
        WHERE id = $1
        "#,
    )
    .bind(deployment_id)
    .bind(commit_sha)
    .bind(commit_message)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn get_deployment_for_user(
    pool: &PgPool,
    deployment_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Deployment>, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT
            d.id,
            d.project_id,
            d.commit_sha,
            d.commit_message,
            d.branch,
            d.status::text AS status,
            d.build_duration_ms,
            d.url,
            d.port,
            d.started_at,
            d.finished_at,
            d.created_at
        FROM deployments d
        JOIN projects p ON p.id = d.project_id
        WHERE d.id = $1 AND p.user_id = $2
        "#,
    )
    .bind(deployment_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_deployments_for_project(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<Deployment>, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT
            d.id,
            d.project_id,
            d.commit_sha,
            d.commit_message,
            d.branch,
            d.status::text AS status,
            d.build_duration_ms,
            d.url,
            d.port,
            d.started_at,
            d.finished_at,
            d.created_at
        FROM deployments d
        JOIN projects p ON p.id = d.project_id
        WHERE d.project_id = $1 AND p.user_id = $2
        ORDER BY d.created_at DESC
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn update_status(
    pool: &PgPool,
    deployment_id: Uuid,
    status: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE deployments
        SET status = $2::deployment_status
        WHERE id = $1
        "#,
    )
    .bind(deployment_id)
    .bind(status)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn mark_ready(
    pool: &PgPool,
    deployment_id: Uuid,
    url: &str,
    port: u16,
    build_duration_ms: i32,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE deployments
        SET status = 'ready',
            url = $2,
            port = $3,
            build_duration_ms = $4,
            finished_at = now()
        WHERE id = $1
        "#,
    )
    .bind(deployment_id)
    .bind(url)
    .bind(port as i32)
    .bind(build_duration_ms)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn mark_failed(
    pool: &PgPool,
    deployment_id: Uuid,
    build_duration_ms: Option<i32>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE deployments
        SET status = 'failed',
            build_duration_ms = COALESCE($2, build_duration_ms),
            finished_at = now()
        WHERE id = $1
        "#,
    )
    .bind(deployment_id)
    .bind(build_duration_ms)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn insert_log(
    pool: &PgPool,
    deployment_id: Uuid,
    level: &str,
    message: &str,
    source: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO deploy_logs (deployment_id, level, message, source)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(deployment_id)
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
    deployment_id: Uuid,
    level: &str,
    message: &str,
    source: &str,
) -> Result<DeployLog, AppError> {
    sqlx::query_as::<_, DeployLog>(
        r#"
        INSERT INTO deploy_logs (deployment_id, level, message, source)
        VALUES ($1, $2, $3, $4)
        RETURNING id, deployment_id, timestamp, level, message, source
        "#,
    )
    .bind(deployment_id)
    .bind(level)
    .bind(message)
    .bind(source)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_logs_for_deployment(
    pool: &PgPool,
    deployment_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<DeployLog>, AppError> {
    sqlx::query_as::<_, DeployLog>(
        r#"
        SELECT
            l.id,
            l.deployment_id,
            l.timestamp,
            l.level,
            l.message,
            l.source
        FROM deploy_logs l
        JOIN deployments d ON d.id = l.deployment_id
        JOIN projects p ON p.id = d.project_id
        WHERE l.deployment_id = $1 AND p.user_id = $2
        ORDER BY l.id ASC
        "#,
    )
    .bind(deployment_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn latest_ready_deployment_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<Deployment>, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT
            id,
            project_id,
            commit_sha,
            commit_message,
            branch,
            status::text AS status,
            build_duration_ms,
            url,
            port,
            started_at,
            finished_at,
            created_at
        FROM deployments
        WHERE project_id = $1 AND status = 'ready'
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn latest_deployment_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<Deployment>, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT
            id,
            project_id,
            commit_sha,
            commit_message,
            branch,
            status::text AS status,
            build_duration_ms,
            url,
            port,
            started_at,
            finished_at,
            created_at
        FROM deployments
        WHERE project_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn set_started_at(
    pool: &PgPool,
    deployment_id: Uuid,
    started_at: DateTime<Utc>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE deployments
        SET started_at = $2
        WHERE id = $1
        "#,
    )
    .bind(deployment_id)
    .bind(started_at)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

/// Returns the latest `ready` deployment per project (one per project).
pub async fn list_latest_ready_per_project(pool: &PgPool) -> Result<Vec<Deployment>, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT DISTINCT ON (project_id)
            id, project_id, commit_sha, commit_message, branch,
            status::text AS status, build_duration_ms, url, port,
            started_at, finished_at, created_at
        FROM deployments
        WHERE status = 'ready'
        ORDER BY project_id, created_at DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_latest_for_projects(
    pool: &PgPool,
    project_ids: &[Uuid],
) -> Result<Vec<Deployment>, AppError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT DISTINCT ON (project_id)
            id,
            project_id,
            commit_sha,
            commit_message,
            branch,
            status::text AS status,
            build_duration_ms,
            url,
            port,
            started_at,
            finished_at,
            created_at
        FROM deployments
        WHERE project_id = ANY($1)
        ORDER BY project_id, created_at DESC
        "#,
    )
    .bind(project_ids)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

/// Returns all `ready` deployments for a project except the given one.
pub async fn list_old_ready_deployments(
    pool: &PgPool,
    project_id: Uuid,
    current_deployment_id: Uuid,
) -> Result<Vec<Deployment>, AppError> {
    sqlx::query_as::<_, Deployment>(
        r#"
        SELECT
            id, project_id, commit_sha, commit_message, branch,
            status::text AS status, build_duration_ms, url, port,
            started_at, finished_at, created_at
        FROM deployments
        WHERE project_id = $1
          AND status = 'ready'
          AND id != $2
        "#,
    )
    .bind(project_id)
    .bind(current_deployment_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}
