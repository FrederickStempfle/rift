use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::{is_unique_violation, models::Project},
    error::AppError,
};

#[derive(Debug, Clone)]
pub struct NewProject {
    pub user_id: Uuid,
    pub name: String,
    pub repo_url: String,
    pub branch: String,
    pub framework: String,
    pub build_command: Option<String>,
    pub output_dir: Option<String>,
    pub install_command: Option<String>,
    pub subdomain: String,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateProject {
    pub name: Option<String>,
    pub repo_url: Option<String>,
    pub branch: Option<String>,
    pub framework: Option<String>,
    pub build_command: Option<String>,
    pub output_dir: Option<String>,
    pub install_command: Option<String>,
    pub subdomain: Option<String>,
}

pub async fn create_project(pool: &PgPool, input: NewProject) -> Result<Project, AppError> {
    sqlx::query_as::<_, Project>(
        r#"
        INSERT INTO projects (
            user_id,
            name,
            repo_url,
            branch,
            framework,
            build_command,
            output_dir,
            install_command,
            subdomain
        )
        VALUES ($1, $2, $3, $4, $5::framework_type, $6, $7, $8, $9)
        RETURNING
            id,
            user_id,
            name,
            repo_url,
            branch,
            framework::text AS framework,
            build_command,
            output_dir,
            install_command,
            subdomain,
            webhook_id,
            webhook_secret,
            created_at,
            updated_at
        "#,
    )
    .bind(input.user_id)
    .bind(input.name)
    .bind(input.repo_url)
    .bind(input.branch)
    .bind(input.framework)
    .bind(input.build_command)
    .bind(input.output_dir)
    .bind(input.install_command)
    .bind(input.subdomain)
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("project name or subdomain already exists".into())
        } else {
            AppError::Db(error)
        }
    })
}

pub async fn list_projects_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<Project>, AppError> {
    sqlx::query_as::<_, Project>(
        r#"
        SELECT
            id,
            user_id,
            name,
            repo_url,
            branch,
            framework::text AS framework,
            build_command,
            output_dir,
            install_command,
            subdomain,
            webhook_id,
            webhook_secret,
            created_at,
            updated_at
        FROM projects
        WHERE user_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_project_for_user(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Project>, AppError> {
    sqlx::query_as::<_, Project>(
        r#"
        SELECT
            id,
            user_id,
            name,
            repo_url,
            branch,
            framework::text AS framework,
            build_command,
            output_dir,
            install_command,
            subdomain,
            webhook_id,
            webhook_secret,
            created_at,
            updated_at
        FROM projects
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn update_project_for_user(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    input: UpdateProject,
) -> Result<Option<Project>, AppError> {
    sqlx::query_as::<_, Project>(
        r#"
        UPDATE projects
        SET
            name = COALESCE($3, name),
            repo_url = COALESCE($4, repo_url),
            branch = COALESCE($5, branch),
            framework = COALESCE($6::framework_type, framework),
            build_command = COALESCE($7, build_command),
            output_dir = COALESCE($8, output_dir),
            install_command = COALESCE($9, install_command),
            subdomain = COALESCE($10, subdomain),
            updated_at = now()
        WHERE id = $1 AND user_id = $2
        RETURNING
            id,
            user_id,
            name,
            repo_url,
            branch,
            framework::text AS framework,
            build_command,
            output_dir,
            install_command,
            subdomain,
            webhook_id,
            webhook_secret,
            created_at,
            updated_at
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .bind(input.name)
    .bind(input.repo_url)
    .bind(input.branch)
    .bind(input.framework)
    .bind(input.build_command)
    .bind(input.output_dir)
    .bind(input.install_command)
    .bind(input.subdomain)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("project name or subdomain already exists".into())
        } else {
            AppError::Db(error)
        }
    })
}

pub async fn delete_project_for_user(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        DELETE FROM projects
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

pub async fn get_project_by_subdomain(
    pool: &PgPool,
    subdomain: &str,
) -> Result<Option<Project>, AppError> {
    sqlx::query_as::<_, Project>(
        r#"
        SELECT
            id,
            user_id,
            name,
            repo_url,
            branch,
            framework::text AS framework,
            build_command,
            output_dir,
            install_command,
            subdomain,
            webhook_id,
            webhook_secret,
            created_at,
            updated_at
        FROM projects
        WHERE subdomain = $1
        "#,
    )
    .bind(subdomain)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}
