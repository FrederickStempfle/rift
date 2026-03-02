use std::{net::SocketAddr, time::Duration};

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::Duration as CookieDuration;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{refresh_tokens, users},
    error::{AppError, AppResult},
    services::abuse::{AbuseDecision, AbuseLimit},
    services::audit::AuditEvent,
    validation,
};

const REFRESH_COOKIE_NAME: &str = "rift_refresh_token";
const INTERNAL_TOKEN_HEADER: &str = "x-rift-internal-token";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/refresh", post(refresh))
        .route("/refresh/internal", post(refresh_internal))
        .route("/logout", post(logout))
        .route("/logout/internal", post(logout_internal))
        .route("/me", get(me))
        .route("/exchange/github", post(exchange_github_session))
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHubExchangeRequest {
    pub github_id: String,
    pub email: Option<String>,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub github_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshInternalRequest {
    pub refresh_token: String,
}

#[derive(Debug, Deserialize)]
pub struct LogoutInternalRequest {
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserResponse {
    pub id: uuid::Uuid,
    pub email: String,
    pub github_login: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_at: i64,
    pub user: UserResponse,
}

#[derive(Debug, Serialize)]
pub struct BackendSessionResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_at: i64,
    pub refresh_expires_at: i64,
    pub user: UserResponse,
}

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(payload): Json<RegisterRequest>,
) -> AppResult<(StatusCode, CookieJar, Json<AuthResponse>)> {
    enforce_abuse_limit(
        &state,
        AbuseLimit::per_ip(
            "api.auth.register",
            addr.ip(),
            "register",
            12,
            Duration::from_secs(60 * 60),
            Some(6),
        ),
    )
    .await?;

    let email = payload.email.trim().to_ascii_lowercase();
    validation::validate_email(&email)?;
    validation::validate_password(&payload.password)?;

    let password_hash = state.password_service.hash_password(&payload.password)?;
    let user = users::create_user(&state.pool, &email, &password_hash).await?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(user.id),
            event: "user.register",
            resource_id: Some(user.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({}),
        })
        .await?;

    let session = create_session(state.clone(), &user).await?;
    let auth_response = session.to_auth_response();

    Ok((
        StatusCode::CREATED,
        jar.add(build_refresh_cookie(&state, &session.refresh_token)),
        Json(auth_response),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(payload): Json<LoginRequest>,
) -> AppResult<(StatusCode, CookieJar, Json<AuthResponse>)> {
    let email = payload.email.trim().to_ascii_lowercase();

    validation::validate_email(&email)?;
    validation::validate_password(&payload.password)?;

    enforce_abuse_limit(
        &state,
        AbuseLimit::per_ip(
            "api.auth.login.ip",
            addr.ip(),
            "login",
            30,
            Duration::from_secs(15 * 60),
            Some(18),
        ),
    )
    .await?;
    enforce_abuse_limit(
        &state,
        AbuseLimit {
            scope: "api.auth.login.email",
            actor_key: format!("ip:{}", addr.ip()),
            bucket_key: format!("scope:api.auth.login.email:email:{email}"),
            limit: 8,
            window: Duration::from_secs(15 * 60),
            challenge_after: Some(5),
        },
    )
    .await?;

    let maybe_user = users::find_user_by_email(&state.pool, &email).await?;
    let verified = state.password_service.verify_or_dummy(
        maybe_user.as_ref().map(|user| user.password_hash.as_str()),
        &payload.password,
    );

    if !verified {
        let email_hash = hash_email(&email);
        state
            .audit_logger
            .log(AuditEvent {
                user_id: None,
                event: "user.login_failed",
                resource_id: None,
                ip_address: Some(addr.ip()),
                user_agent: user_agent(&headers),
                metadata: json!({ "email_hash": email_hash }),
            })
            .await?;

        return Err(AppError::Unauthorized("invalid email or password".into()));
    }

    let user =
        maybe_user.ok_or_else(|| AppError::Unauthorized("invalid email or password".into()))?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(user.id),
            event: "user.login",
            resource_id: Some(user.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "success": true, "provider": "password" }),
        })
        .await?;

    let session = create_session(state.clone(), &user).await?;
    let auth_response = session.to_auth_response();

    Ok((
        StatusCode::OK,
        jar.add(build_refresh_cookie(&state, &session.refresh_token)),
        Json(auth_response),
    ))
}

pub async fn refresh(
    State(state): State<AppState>,
    jar: CookieJar,
) -> AppResult<(StatusCode, CookieJar, Json<AuthResponse>)> {
    let token = jar
        .get(REFRESH_COOKIE_NAME)
        .map(|cookie| cookie.value().to_owned())
        .ok_or_else(|| AppError::Unauthorized("missing refresh token".into()))?;

    let session = refresh_session_from_token(state.clone(), &token).await?;
    let auth_response = session.to_auth_response();

    Ok((
        StatusCode::OK,
        jar.add(build_refresh_cookie(&state, &session.refresh_token)),
        Json(auth_response),
    ))
}

pub async fn refresh_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RefreshInternalRequest>,
) -> AppResult<Json<BackendSessionResponse>> {
    require_internal_api_token(&state, &headers)?;

    let session = refresh_session_from_token(state, &payload.refresh_token).await?;
    Ok(Json(session.into_backend_response()))
}

pub async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> AppResult<(StatusCode, CookieJar)> {
    if let Some(existing) = jar.get(REFRESH_COOKIE_NAME) {
        let token_hash = state.token_service.hash_refresh_token(existing.value());
        refresh_tokens::revoke_by_hash(&state.pool, &token_hash).await?;
    }

    Ok((
        StatusCode::NO_CONTENT,
        jar.add(clear_refresh_cookie(&state)),
    ))
}

pub async fn logout_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<LogoutInternalRequest>,
) -> AppResult<StatusCode> {
    require_internal_api_token(&state, &headers)?;

    let token_hash = state
        .token_service
        .hash_refresh_token(&payload.refresh_token);
    refresh_tokens::revoke_by_hash(&state.pool, &token_hash).await?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> AppResult<Json<UserResponse>> {
    let user = users::find_user_by_id(&state.pool, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("user not found".into()))?;

    Ok(Json(UserResponse::from(user)))
}

pub async fn exchange_github_session(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<GitHubExchangeRequest>,
) -> AppResult<Json<BackendSessionResponse>> {
    require_internal_api_token(&state, &headers)?;

    validation::ensure_no_null_bytes(&payload.github_id, "github_id")?;
    validation::ensure_max_len(&payload.github_id, 255, "github_id")?;
    validation::ensure_no_null_bytes(&payload.login, "github login")?;
    validation::ensure_max_len(&payload.login, 255, "github login")?;

    let normalized_email = payload
        .email
        .as_ref()
        .map(|email| email.trim().to_ascii_lowercase());
    if let Some(email) = normalized_email.as_deref() {
        validation::validate_email(email)?;
    }

    let random_password = state.token_service.generate_refresh_token();
    let fallback_password_hash = state.password_service.hash_password(&random_password)?;
    let user = users::upsert_github_user(
        &state.pool,
        &users::GitHubIdentity {
            github_id: payload.github_id,
            email: normalized_email,
            github_login: payload.login,
            display_name: payload.name,
            avatar_url: payload.avatar_url,
            github_token: payload.github_token,
        },
        &fallback_password_hash,
    )
    .await?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(user.id),
            event: "user.login",
            resource_id: Some(user.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "success": true, "provider": "github" }),
        })
        .await?;

    let session = create_session(state, &user).await?;
    Ok(Json(session.into_backend_response()))
}

async fn create_session(
    state: AppState,
    user: &crate::db::models::User,
) -> AppResult<SessionTokens> {
    let access = state.token_service.issue_access_token(user.id)?;
    let refresh_token = state.token_service.generate_refresh_token();
    let refresh_hash = state.token_service.hash_refresh_token(&refresh_token);
    let refresh_expires_at = Utc::now() + state.token_service.refresh_ttl();

    refresh_tokens::create_refresh_token(&state.pool, user.id, &refresh_hash, refresh_expires_at)
        .await?;

    Ok(SessionTokens {
        access_token: access.token,
        refresh_token,
        expires_at: access.expires_at,
        refresh_expires_at: refresh_expires_at.timestamp(),
        user: UserResponse::from(user.clone()),
    })
}

async fn refresh_session_from_token(
    state: AppState,
    refresh_token: &str,
) -> AppResult<SessionTokens> {
    let token_hash = state.token_service.hash_refresh_token(refresh_token);
    let stored = refresh_tokens::find_active_by_hash(&state.pool, &token_hash)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid refresh token".into()))?;

    refresh_tokens::revoke_by_id(&state.pool, stored.id).await?;

    let user = users::find_user_by_id(&state.pool, stored.user_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid refresh token".into()))?;

    create_session(state, &user).await
}

fn require_internal_api_token(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let provided = headers
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());

    match provided {
        Some(token) if token == state.config.internal_api_token => Ok(()),
        _ => Err(AppError::Forbidden("invalid internal token".into())),
    }
}

fn build_refresh_cookie(state: &AppState, refresh_token: &str) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE_NAME, refresh_token.to_owned()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.config.cookie_secure)
        .max_age(CookieDuration::days(state.config.refresh_token_ttl_days))
        .build()
}

fn clear_refresh_cookie(state: &AppState) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE_NAME, String::new()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.config.cookie_secure)
        .max_age(CookieDuration::seconds(0))
        .build()
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn hash_email(email: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(email.as_bytes());
    hex::encode(hasher.finalize())
}

async fn enforce_abuse_limit(state: &AppState, limit: AbuseLimit) -> AppResult<()> {
    match state.abuse_guard.enforce(limit).await? {
        AbuseDecision::Allow => Ok(()),
        AbuseDecision::Challenge {
            retry_after_secs,
            reason,
        } => Err(AppError::RateLimited(format!(
            "{reason}; retry in {retry_after_secs}s"
        ))),
        AbuseDecision::Block {
            retry_after_secs,
            reason,
            tier: _,
        } => Err(AppError::RateLimited(format!(
            "{reason}; retry in {retry_after_secs}s"
        ))),
    }
}

#[derive(Debug)]
struct SessionTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    refresh_expires_at: i64,
    user: UserResponse,
}

impl SessionTokens {
    fn to_auth_response(&self) -> AuthResponse {
        AuthResponse {
            access_token: self.access_token.clone(),
            token_type: "Bearer",
            expires_at: self.expires_at,
            user: self.user.clone(),
        }
    }

    fn into_backend_response(self) -> BackendSessionResponse {
        BackendSessionResponse {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            token_type: "Bearer",
            expires_at: self.expires_at,
            refresh_expires_at: self.refresh_expires_at,
            user: self.user,
        }
    }
}

impl From<crate::db::models::User> for UserResponse {
    fn from(value: crate::db::models::User) -> Self {
        Self {
            id: value.id,
            email: value.email,
            github_login: value.github_login,
            display_name: value.display_name,
            avatar_url: value.avatar_url,
            created_at: value.created_at,
        }
    }
}
