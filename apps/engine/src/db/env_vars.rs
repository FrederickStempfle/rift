use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{db::is_unique_violation, error::AppError};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnvVar {
    pub id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub encrypted_value: Vec<u8>,
    pub nonce: Vec<u8>,
}

pub async fn create_env_var(
    pool: &PgPool,
    project_id: Uuid,
    key: &str,
    encrypted_value: &[u8],
    nonce: &[u8],
) -> Result<EnvVar, AppError> {
    sqlx::query_as::<_, EnvVar>(
        r#"
        INSERT INTO env_vars (project_id, key, encrypted_value, nonce)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (project_id, key) DO UPDATE SET
            encrypted_value = EXCLUDED.encrypted_value,
            nonce = EXCLUDED.nonce
        RETURNING id, project_id, key, encrypted_value, nonce
        "#,
    )
    .bind(project_id)
    .bind(key)
    .bind(encrypted_value)
    .bind(nonce)
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            AppError::Conflict("env var already exists".into())
        } else {
            AppError::Db(error)
        }
    })
}

pub async fn list_env_vars(pool: &PgPool, project_id: Uuid) -> Result<Vec<EnvVar>, AppError> {
    sqlx::query_as::<_, EnvVar>(
        r#"
        SELECT id, project_id, key, encrypted_value, nonce
        FROM env_vars
        WHERE project_id = $1
        ORDER BY key ASC
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn delete_env_var(pool: &PgPool, id: Uuid, project_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        DELETE FROM env_vars
        WHERE id = $1 AND project_id = $2
        "#,
    )
    .bind(id)
    .bind(project_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

pub async fn get_env_vars_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<EnvVar>, AppError> {
    list_env_vars(pool, project_id).await
}

fn derive_key(master_key: &str) -> Key<Aes256Gcm> {
    let mut hasher = Sha256::new();
    hasher.update(master_key.as_bytes());
    hasher.update(b"rift-env-vars-v1");
    let hash = hasher.finalize();
    *Key::<Aes256Gcm>::from_slice(&hash)
}

/// Decrypt all env vars for a project and return them as (key, value) pairs.
pub async fn get_decrypted_env_vars(
    pool: &PgPool,
    project_id: Uuid,
    master_key: &str,
) -> Result<Vec<(String, String)>, AppError> {
    let vars = list_env_vars(pool, project_id).await?;
    let key = derive_key(master_key);
    let cipher = Aes256Gcm::new(&key);

    let mut result = Vec::with_capacity(vars.len());
    for v in vars {
        let nonce = aes_gcm::Nonce::from_slice(&v.nonce);
        let plaintext = cipher
            .decrypt(nonce, v.encrypted_value.as_ref())
            .map_err(|e| AppError::Internal(format!("env var decryption failed: {e}")))?;
        let value = String::from_utf8(plaintext)
            .map_err(|e| AppError::Internal(format!("invalid utf-8 after decryption: {e}")))?;
        result.push((v.key, value));
    }
    Ok(result)
}
