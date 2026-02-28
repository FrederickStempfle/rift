use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, Aes256Gcm, Key,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{env_vars, projects},
    error::{AppError, AppResult},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", post(create_env_var).get(list_env_vars))
        .route("/{env_var_id}", axum::routing::delete(delete_env_var))
}

#[derive(Debug, Deserialize)]
pub struct CreateEnvVarRequest {
    pub project_id: Uuid,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub project_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct EnvVarResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub preview: String,
}

fn derive_key(master_key: &str) -> Key<Aes256Gcm> {
    let mut hasher = Sha256::new();
    hasher.update(master_key.as_bytes());
    hasher.update(b"rift-env-vars-v1");
    let hash = hasher.finalize();
    *Key::<Aes256Gcm>::from_slice(&hash)
}

fn encrypt_value(master_key: &str, plaintext: &str) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    let key = derive_key(master_key);
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| AppError::Internal(format!("encryption failed: {e}")))?;
    Ok((ciphertext, nonce.to_vec()))
}

fn decrypt_value(
    master_key: &str,
    ciphertext: &[u8],
    nonce_bytes: &[u8],
) -> Result<String, AppError> {
    let key = derive_key(master_key);
    let cipher = Aes256Gcm::new(&key);
    let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| AppError::Internal(format!("decryption failed: {e}")))?;
    String::from_utf8(plaintext)
        .map_err(|e| AppError::Internal(format!("invalid utf-8 after decryption: {e}")))
}

fn mask_value(value: &str) -> String {
    if value.len() <= 4 {
        "••••••••".to_owned()
    } else {
        let visible = &value[..4];
        format!("{visible}••••••••")
    }
}

async fn create_env_var(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(payload): Json<CreateEnvVarRequest>,
) -> AppResult<(StatusCode, Json<EnvVarResponse>)> {
    if payload.key.is_empty() || payload.key.len() > 256 {
        return Err(AppError::BadRequest("key must be 1-256 characters".into()));
    }
    if payload.value.len() > 65536 {
        return Err(AppError::BadRequest("value too large".into()));
    }

    projects::get_project_for_user(&state.pool, payload.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let (encrypted, nonce) = encrypt_value(&state.config.master_key, &payload.value)?;

    let env_var = env_vars::create_env_var(
        &state.pool,
        payload.project_id,
        &payload.key,
        &encrypted,
        &nonce,
    )
    .await?;

    let preview = mask_value(&payload.value);

    Ok((
        StatusCode::CREATED,
        Json(EnvVarResponse {
            id: env_var.id,
            project_id: env_var.project_id,
            key: env_var.key,
            preview,
        }),
    ))
}

async fn list_env_vars(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<ListQuery>,
) -> AppResult<Json<Vec<EnvVarResponse>>> {
    projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let vars = env_vars::list_env_vars(&state.pool, query.project_id).await?;

    let items = vars
        .into_iter()
        .map(|v| {
            let preview =
                match decrypt_value(&state.config.master_key, &v.encrypted_value, &v.nonce) {
                    Ok(plain) => mask_value(&plain),
                    Err(_) => "••••••••".to_owned(),
                };
            EnvVarResponse {
                id: v.id,
                project_id: v.project_id,
                key: v.key,
                preview,
            }
        })
        .collect();

    Ok(Json(items))
}

async fn delete_env_var(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(env_var_id): Path<Uuid>,
    Query(query): Query<ListQuery>,
) -> AppResult<StatusCode> {
    projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let deleted = env_vars::delete_env_var(&state.pool, env_var_id, query.project_id).await?;
    if !deleted {
        return Err(AppError::NotFound("env var not found".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}
