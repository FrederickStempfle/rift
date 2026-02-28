use sqlx::{PgPool, Postgres, Transaction};

use crate::{
    db::{is_unique_violation, models::User},
    error::AppError,
};

#[derive(Debug, Clone)]
pub struct GitHubIdentity {
    pub github_id: String,
    pub email: Option<String>,
    pub github_login: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

pub async fn create_user(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
) -> Result<User, AppError> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (email, password_hash)
        VALUES ($1, $2)
        RETURNING id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        "#,
    )
    .bind(email)
    .bind(password_hash)
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("email already registered".into())
        } else {
            AppError::Db(error)
        }
    })
}

pub async fn find_user_by_email(pool: &PgPool, email: &str) -> Result<Option<User>, AppError> {
    sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        FROM users
        WHERE email = $1
        "#,
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn find_user_by_id(pool: &PgPool, user_id: uuid::Uuid) -> Result<Option<User>, AppError> {
    sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn upsert_github_user(
    pool: &PgPool,
    identity: &GitHubIdentity,
    fallback_password_hash: &str,
) -> Result<User, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    if let Some(existing) = find_user_by_github_id_tx(&mut tx, &identity.github_id).await? {
        let updated = update_github_user_tx(&mut tx, existing.id, identity).await?;
        tx.commit().await.map_err(AppError::Db)?;
        return Ok(updated);
    }

    if let Some(email) = identity.email.as_deref() {
        if let Some(existing) = find_user_by_email_tx(&mut tx, email).await? {
            let updated = update_github_user_tx(&mut tx, existing.id, identity).await?;
            tx.commit().await.map_err(AppError::Db)?;
            return Ok(updated);
        }
    }

    let email = identity
        .email
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("github-{}@users.rift.local", identity.github_id));

    let created = sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (
            email,
            password_hash,
            github_id,
            github_login,
            display_name,
            avatar_url
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        "#,
    )
    .bind(email)
    .bind(fallback_password_hash)
    .bind(&identity.github_id)
    .bind(&identity.github_login)
    .bind(&identity.display_name)
    .bind(&identity.avatar_url)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("github identity is already linked".into())
        } else {
            AppError::Db(error)
        }
    })?;

    tx.commit().await.map_err(AppError::Db)?;
    Ok(created)
}

async fn find_user_by_github_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    github_id: &str,
) -> Result<Option<User>, AppError> {
    sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        FROM users
        WHERE github_id = $1
        "#,
    )
    .bind(github_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::Db)
}

async fn find_user_by_email_tx(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
) -> Result<Option<User>, AppError> {
    sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        FROM users
        WHERE email = $1
        "#,
    )
    .bind(email)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::Db)
}

async fn update_github_user_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: uuid::Uuid,
    identity: &GitHubIdentity,
) -> Result<User, AppError> {
    let email = identity
        .email
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("github-{}@users.rift.local", identity.github_id));

    sqlx::query_as::<_, User>(
        r#"
        UPDATE users
        SET email = $2,
            github_id = $3,
            github_login = $4,
            display_name = $5,
            avatar_url = $6
        WHERE id = $1
        RETURNING id, email, password_hash, github_id, github_login, display_name, avatar_url, created_at
        "#,
    )
    .bind(user_id)
    .bind(email)
    .bind(&identity.github_id)
    .bind(&identity.github_login)
    .bind(&identity.display_name)
    .bind(&identity.avatar_url)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("github identity is already linked".into())
        } else {
            AppError::Db(error)
        }
    })
}
