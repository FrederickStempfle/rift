use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::{is_unique_violation, models::Domain},
    error::AppError,
};

#[derive(Debug, Clone)]
pub struct NewDomain {
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub created_by: Uuid,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct DomainWithProject {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
    pub project_name: Option<String>,
}

pub async fn create_domain(pool: &PgPool, input: NewDomain) -> Result<Domain, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    if input.is_primary {
        if let Some(pid) = input.project_id {
            sqlx::query(
                r#"
                UPDATE domains
                SET is_primary = false
                WHERE project_id = $1 AND is_primary = true
                "#,
            )
            .bind(pid)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        }
    }

    let domain = sqlx::query_as::<_, Domain>(
        r#"
        INSERT INTO domains (project_id, domain, is_primary, created_by)
        VALUES ($1, $2, $3, $4)
        RETURNING
            id,
            project_id,
            domain,
            is_primary,
            ssl_status::text AS ssl_status
        "#,
    )
    .bind(input.project_id)
    .bind(input.domain)
    .bind(input.is_primary)
    .bind(input.created_by)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("domain already exists".into())
        } else {
            AppError::Db(error)
        }
    })?;

    tx.commit().await.map_err(AppError::Db)?;
    Ok(domain)
}

/// List all domains owned by the user (through projects) + unassigned domains created by the user.
/// Since unassigned domains have no project link, we track the creator via a `created_by` column
/// added in migration 0008. For now, we only return project-linked domains for the global list
/// and provide a separate function for unassigned domains.
pub async fn list_domains_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<DomainWithProject>, AppError> {
    sqlx::query_as::<_, DomainWithProject>(
        r#"
        SELECT
            d.id,
            d.project_id,
            d.domain,
            d.is_primary,
            d.ssl_status::text AS ssl_status,
            p.name AS project_name
        FROM domains d
        LEFT JOIN projects p ON p.id = d.project_id
        WHERE p.user_id = $1
           OR d.created_by = $1
        ORDER BY d.domain ASC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_domain(
    pool: &PgPool,
    domain_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Domain>, AppError> {
    sqlx::query_as::<_, Domain>(
        r#"
        SELECT
            d.id,
            d.project_id,
            d.domain,
            d.is_primary,
            d.ssl_status::text AS ssl_status
        FROM domains d
        LEFT JOIN projects p ON p.id = d.project_id
        WHERE d.id = $1
          AND (p.user_id = $2 OR d.created_by = $2)
        "#,
    )
    .bind(domain_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn delete_domain(
    pool: &PgPool,
    domain_id: Uuid,
    user_id: Uuid,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        DELETE FROM domains
        WHERE id = $1
          AND (
            created_by = $2
            OR project_id IN (SELECT id FROM projects WHERE user_id = $2)
          )
        "#,
    )
    .bind(domain_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

pub async fn list_domains_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<Domain>, AppError> {
    sqlx::query_as::<_, Domain>(
        r#"
        SELECT
            id,
            project_id,
            domain,
            is_primary,
            ssl_status::text AS ssl_status
        FROM domains
        WHERE project_id = $1
        ORDER BY is_primary DESC, domain ASC
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_domains_for_project_with_name(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<DomainWithProject>, AppError> {
    sqlx::query_as::<_, DomainWithProject>(
        r#"
        SELECT
            d.id,
            d.project_id,
            d.domain,
            d.is_primary,
            d.ssl_status::text AS ssl_status,
            p.name AS project_name
        FROM domains d
        LEFT JOIN projects p ON p.id = d.project_id
        WHERE d.project_id = $1
        ORDER BY d.is_primary DESC, d.domain ASC
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_project_id_by_domain(
    pool: &PgPool,
    domain: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT project_id
        FROM domains
        WHERE domain = $1
          AND ssl_status = 'active'
          AND project_id IS NOT NULL
        "#,
    )
    .bind(domain)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_primary_domain_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<Domain>, AppError> {
    sqlx::query_as::<_, Domain>(
        r#"
        SELECT
            id,
            project_id,
            domain,
            is_primary,
            ssl_status::text AS ssl_status
        FROM domains
        WHERE project_id = $1 AND is_primary = true AND ssl_status = 'active'
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn set_primary_domain(
    pool: &PgPool,
    project_id: Option<Uuid>,
    domain_id: Uuid,
) -> Result<(), AppError> {
    let project_id = project_id.ok_or_else(|| {
        AppError::BadRequest("cannot set primary on a domain not assigned to a project".into())
    })?;

    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    sqlx::query("UPDATE domains SET is_primary = false WHERE project_id = $1 AND is_primary = true")
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;

    sqlx::query("UPDATE domains SET is_primary = true WHERE id = $1 AND project_id = $2")
        .bind(domain_id)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;

    tx.commit().await.map_err(AppError::Db)?;
    Ok(())
}

pub async fn mark_domain_active(
    pool: &PgPool,
    domain_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Domain>, AppError> {
    sqlx::query_as::<_, Domain>(
        r#"
        UPDATE domains
        SET ssl_status = 'active'
        WHERE id = $1
          AND ssl_status IN ('pending', 'failed')
          AND (
            created_by = $2
            OR project_id IN (SELECT id FROM projects WHERE user_id = $2)
          )
        RETURNING
            id,
            project_id,
            domain,
            is_primary,
            ssl_status::text AS ssl_status
        "#,
    )
    .bind(domain_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

/// Assign (or reassign) a domain to a project. Auto-sets is_primary if the project has no other domains.
pub async fn assign_domain_to_project(
    pool: &PgPool,
    domain_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Domain>, AppError> {
    // Check domain ownership
    let domain = get_domain(pool, domain_id, user_id).await?;
    if domain.is_none() {
        return Ok(None);
    }

    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    // Auto-set primary if project has no existing domains
    let existing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM domains WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;

    let is_primary = existing_count == 0;

    let updated = sqlx::query_as::<_, Domain>(
        r#"
        UPDATE domains
        SET project_id = $1, is_primary = $2
        WHERE id = $3
        RETURNING
            id,
            project_id,
            domain,
            is_primary,
            ssl_status::text AS ssl_status
        "#,
    )
    .bind(project_id)
    .bind(is_primary)
    .bind(domain_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Db)?;

    tx.commit().await.map_err(AppError::Db)?;
    Ok(updated)
}

/// Unassign a domain from its project.
pub async fn unassign_domain_from_project(
    pool: &PgPool,
    domain_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Domain>, AppError> {
    sqlx::query_as::<_, Domain>(
        r#"
        UPDATE domains
        SET project_id = NULL, is_primary = false
        WHERE id = $1
          AND (
            created_by = $2
            OR project_id IN (SELECT id FROM projects WHERE user_id = $2)
          )
        RETURNING
            id,
            project_id,
            domain,
            is_primary,
            ssl_status::text AS ssl_status
        "#,
    )
    .bind(domain_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}
