use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::domains::{self, DomainWithProject, NewDomain},
    error::{AppError, AppResult},
    services::audit::AuditEvent,
    validation,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", post(create_domain).get(list_domains))
        .route(
            "/{domain_id}",
            get(get_domain).patch(update_domain).delete(delete_domain),
        )
        .route("/{domain_id}/verify", post(verify_domain))
        .route("/{domain_id}/assign", post(assign_domain))
}

#[derive(Debug, Deserialize)]
pub struct CreateDomainRequest {
    pub domain: String,
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub is_primary: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDomainRequest {
    pub is_primary: Option<bool>,
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct AssignDomainRequest {
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListDomainsQuery {
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct DomainResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
}

#[derive(Debug, Serialize)]
pub struct DomainListResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
    pub project_name: Option<String>,
}

pub async fn create_domain(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<CreateDomainRequest>,
) -> AppResult<(StatusCode, Json<DomainResponse>)> {
    validation::validate_domain(&payload.domain)?;

    let mut is_primary = payload.is_primary;

    // If a project_id is provided, verify the user owns it
    if let Some(project_id) = payload.project_id {
        let project = crate::db::projects::get_project_for_user(
            &state.pool,
            project_id,
            auth_user.user_id,
        )
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

        // Auto-set is_primary if this is the first domain for the project
        let existing = domains::list_domains_for_project(&state.pool, project.id).await?;
        if existing.is_empty() {
            is_primary = true;
        }
    }

    let domain = domains::create_domain(
        &state.pool,
        NewDomain {
            project_id: payload.project_id,
            domain: payload.domain,
            is_primary,
            created_by: auth_user.user_id,
        },
    )
    .await?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "domain.create",
            resource_id: Some(domain.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({}),
        })
        .await?;

    Ok((StatusCode::CREATED, Json(DomainResponse::from(domain))))
}

pub async fn list_domains(
    State(state): State<AppState>,
    auth_user: AuthUser,
    axum::extract::Query(query): axum::extract::Query<ListDomainsQuery>,
) -> AppResult<Json<Vec<DomainListResponse>>> {
    let domains = match query.project_id {
        Some(project_id) => {
            crate::db::projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
                .await?
                .ok_or_else(|| AppError::NotFound("project not found".into()))?;
            domains::list_domains_for_project_with_name(&state.pool, project_id).await?
        }
        None => domains::list_domains_for_user(&state.pool, auth_user.user_id).await?,
    };
    Ok(Json(
        domains
            .into_iter()
            .map(DomainListResponse::from)
            .collect::<Vec<_>>(),
    ))
}

pub async fn get_domain(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(domain_id): Path<Uuid>,
) -> AppResult<Json<DomainResponse>> {
    let domain = domains::get_domain(&state.pool, domain_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    Ok(Json(DomainResponse::from(domain)))
}

pub async fn delete_domain(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(domain_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let deleted = domains::delete_domain(&state.pool, domain_id, auth_user.user_id).await?;
    if !deleted {
        return Err(AppError::NotFound("domain not found".into()));
    }

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "domain.delete",
            resource_id: Some(domain_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({}),
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_domain(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(domain_id): Path<Uuid>,
    Json(payload): Json<UpdateDomainRequest>,
) -> AppResult<Json<DomainResponse>> {
    let domain = domains::get_domain(&state.pool, domain_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    if let Some(true) = payload.is_primary {
        domains::set_primary_domain(&state.pool, domain.project_id, domain_id).await?;
    }

    let updated = domains::get_domain(&state.pool, domain_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "domain.update",
            resource_id: Some(domain_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "is_primary": payload.is_primary }),
        })
        .await?;

    Ok(Json(DomainResponse::from(updated)))
}

pub async fn assign_domain(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(domain_id): Path<Uuid>,
    Json(payload): Json<AssignDomainRequest>,
) -> AppResult<Json<DomainResponse>> {
    let updated = match payload.project_id {
        Some(project_id) => {
            // Verify user owns the project
            crate::db::projects::get_project_for_user(
                &state.pool,
                project_id,
                auth_user.user_id,
            )
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;

            domains::assign_domain_to_project(
                &state.pool,
                domain_id,
                project_id,
                auth_user.user_id,
            )
            .await?
        }
        None => {
            domains::unassign_domain_from_project(&state.pool, domain_id, auth_user.user_id)
                .await?
        }
    };

    let domain = updated.ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "domain.assign",
            resource_id: Some(domain_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({ "project_id": payload.project_id }),
        })
        .await?;

    Ok(Json(DomainResponse::from(domain)))
}

pub async fn verify_domain(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(domain_id): Path<Uuid>,
) -> AppResult<Json<DomainResponse>> {
    let domain = domains::get_domain(&state.pool, domain_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    let expected_ip = state
        .public_ip
        .as_deref()
        .ok_or_else(|| {
            AppError::Internal("public IP not available — cannot verify DNS".into())
        })?;
    let expected_addr: std::net::Ipv4Addr = expected_ip
        .parse()
        .map_err(|_| AppError::Internal("RIFT_PUBLIC_IP is not a valid IPv4 address".into()))?;

    let resolver = hickory_resolver::Resolver::builder_tokio()
        .map_err(|_| AppError::Internal("failed to create DNS resolver".into()))?
        .build();
    let lookup = resolver
        .ipv4_lookup(&domain.domain)
        .await
        .map_err(|_| {
            AppError::BadRequest(
                "DNS lookup failed — A records not found. Check your DNS configuration.".into(),
            )
        })?;

    let matched = lookup
        .iter()
        .any(|a| std::net::Ipv4Addr::from(*a) == expected_addr);
    if !matched {
        let found: Vec<String> = lookup
            .iter()
            .map(|a| std::net::Ipv4Addr::from(*a).to_string())
            .collect();
        return Err(AppError::BadRequest(format!(
            "DNS A record for {} does not point to {}. Found: {}",
            domain.domain,
            expected_ip,
            found.join(", ")
        )));
    }

    let updated = domains::mark_domain_active(&state.pool, domain_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    Ok(Json(DomainResponse::from(updated)))
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

impl From<crate::db::models::Domain> for DomainResponse {
    fn from(value: crate::db::models::Domain) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            domain: value.domain,
            is_primary: value.is_primary,
            ssl_status: value.ssl_status,
        }
    }
}

impl From<DomainWithProject> for DomainListResponse {
    fn from(value: DomainWithProject) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            domain: value.domain,
            is_primary: value.is_primary,
            ssl_status: value.ssl_status,
            project_name: value.project_name,
        }
    }
}
