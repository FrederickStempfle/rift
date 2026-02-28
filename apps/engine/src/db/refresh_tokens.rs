use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{db::models::RefreshToken, error::AppError};

pub async fn create_refresh_token(
    pool: &PgPool,
    user_id: Uuid,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<RefreshToken, AppError> {
    sqlx::query_as::<_, RefreshToken>(
        r#"
        INSERT INTO refresh_tokens (user_id, token_hash, expires_at)
        VALUES ($1, $2, $3)
        RETURNING id, user_id, token_hash, expires_at, created_at, revoked
        "#,
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn find_active_by_hash(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<RefreshToken>, AppError> {
    sqlx::query_as::<_, RefreshToken>(
        r#"
        SELECT id, user_id, token_hash, expires_at, created_at, revoked
        FROM refresh_tokens
        WHERE token_hash = $1
          AND revoked = false
          AND expires_at > now()
        "#,
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn revoke_by_id(pool: &PgPool, refresh_token_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE refresh_tokens
        SET revoked = true
        WHERE id = $1
        "#,
    )
    .bind(refresh_token_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}

pub async fn revoke_by_hash(pool: &PgPool, token_hash: &str) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE refresh_tokens
        SET revoked = true
        WHERE token_hash = $1
        "#,
    )
    .bind(token_hash)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}
