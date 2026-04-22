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
    state::RoutingEntry,
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
    pub service_id: Option<Uuid>,
    pub target_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListDomainsQuery {
    pub project_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct DomainResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
    pub ssl_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ssl_error: Option<String>,
    pub target_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DomainListResponse {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
    pub ssl_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ssl_error: Option<String>,
    pub target_url: Option<String>,
    pub project_name: Option<String>,
    pub service_name: Option<String>,
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
        let project =
            crate::db::projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
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

    // Invalidate routing cache for this domain.
    state.routing_cache.invalidate_host(&domain.domain).await;
    if let Some(project_id) = domain.project_id {
        upsert_distributed_route(&state, domain.domain.clone(), project_id).await;
    } else {
        remove_distributed_route(&state, &domain.domain).await;
    }

    Ok((StatusCode::CREATED, Json(DomainResponse::from(domain))))
}

pub async fn list_domains(
    State(state): State<AppState>,
    auth_user: AuthUser,
    axum::extract::Query(query): axum::extract::Query<ListDomainsQuery>,
) -> AppResult<Json<Vec<DomainListResponse>>> {
    let domains = if let Some(project_id) = query.project_id {
        crate::db::projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;
        domains::list_domains_for_project_with_name(&state.pool, project_id).await?
    } else if let Some(service_id) = query.service_id {
        // Verify user owns the service
        crate::db::services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("service not found".into()))?;
        let service_domains = domains::list_domains_for_service(&state.pool, service_id).await?;
        // Convert Domain to DomainWithProject for consistent response
        service_domains
            .into_iter()
            .map(|d| DomainWithProject {
                id: d.id,
                project_id: d.project_id,
                service_id: d.service_id,
                domain: d.domain,
                is_primary: d.is_primary,
                ssl_status: d.ssl_status,
                ssl_expires_at: d.ssl_expires_at,
                ssl_error: d.ssl_error,
                target_url: d.target_url,
                project_name: None,
                service_name: None,
            })
            .collect()
    } else {
        domains::list_domains_for_user(&state.pool, auth_user.user_id).await?
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
    // Fetch domain name before deletion for cache invalidation.
    let domain_record = domains::get_domain(&state.pool, domain_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    let deleted = domains::delete_domain(&state.pool, domain_id, auth_user.user_id).await?;
    if !deleted {
        return Err(AppError::NotFound("domain not found".into()));
    }

    // Invalidate routing cache for this domain.
    state
        .routing_cache
        .invalidate_host(&domain_record.domain)
        .await;
    remove_distributed_route(&state, &domain_record.domain).await;
    state.ssl_manager.remove_cert(&domain_record.domain).await;

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
    let updated = if let Some(service_id) = payload.service_id {
        // Assign to service with a target_url
        let target_url = payload.target_url.as_deref().ok_or_else(|| {
            AppError::BadRequest("target_url is required when assigning to a service".into())
        })?;
        // Verify user owns the service
        crate::db::services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("service not found".into()))?;

        domains::assign_domain_to_service(
            &state.pool,
            domain_id,
            service_id,
            target_url,
            auth_user.user_id,
        )
        .await?
    } else if let Some(project_id) = payload.project_id {
        // Verify user owns the project
        crate::db::projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;

        domains::assign_domain_to_project(&state.pool, domain_id, project_id, auth_user.user_id)
            .await?
    } else {
        // Unassign from project or service
        domains::unassign_domain_from_project(&state.pool, domain_id, auth_user.user_id).await?
    };

    let domain = updated.ok_or_else(|| AppError::NotFound("domain not found".into()))?;

    // Invalidate routing cache — domain's routing changed.
    state.routing_cache.invalidate_host(&domain.domain).await;
    if let Some(project_id) = domain.project_id {
        upsert_distributed_route(&state, domain.domain.clone(), project_id).await;
    } else {
        remove_distributed_route(&state, &domain.domain).await;
    }

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "domain.assign",
            resource_id: Some(domain_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({
                "project_id": payload.project_id,
                "service_id": payload.service_id,
                "target_url": payload.target_url,
            }),
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
        .ok_or_else(|| AppError::Internal("public IP not available — cannot verify DNS".into()))?;
    let expected_addr: std::net::Ipv4Addr = expected_ip
        .parse()
        .map_err(|_| AppError::Internal("RIFT_PUBLIC_IP is not a valid IPv4 address".into()))?;

    let resolver = hickory_resolver::Resolver::builder_tokio()
        .map_err(|_| AppError::Internal("failed to create DNS resolver".into()))?
        .build();
    let lookup = resolver.ipv4_lookup(&domain.domain).await.map_err(|_| {
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

    // Trigger SSL certificate provisioning in the background
    if state.config.acme_email.is_some() {
        let ssl_manager = state.ssl_manager.clone();
        let domain_name = updated.domain.clone();
        tokio::spawn(async move {
            if let Err(e) = ssl_manager.provision_cert(&domain_name).await {
                tracing::error!(domain = %domain_name, error = %e, "SSL provisioning failed");
            }
        });
    }

    Ok(Json(DomainResponse::from(updated)))
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn upsert_distributed_route(state: &AppState, host: String, project_id: Uuid) {
    let entry = RoutingEntry {
        host: host.clone(),
        project_id,
        deployment_id: state
            .runtime_backend
            .active_deployment_id(project_id)
            .await
            .unwrap_or_else(Uuid::nil),
        worker_addr: state.config.proxy_addr(),
        version: 1,
    };

    if let Err(e) = state.state_store.set_routing(&entry).await {
        tracing::warn!(host = %host, error = %e, "failed to set distributed route");
        return;
    }
    if let Err(e) = state.state_store.publish_routing_update(&entry).await {
        tracing::warn!(host = %host, error = %e, "failed to publish routing update");
    }
}

async fn remove_distributed_route(state: &AppState, host: &str) {
    if let Err(e) = state.state_store.remove_routing(host).await {
        tracing::warn!(host = %host, error = %e, "failed to remove distributed route");
    }
    let entry = RoutingEntry {
        host: host.to_owned(),
        project_id: Uuid::nil(),
        deployment_id: Uuid::nil(),
        worker_addr: String::new(),
        version: 1,
    };
    if let Err(e) = state.state_store.publish_routing_update(&entry).await {
        tracing::warn!(
            host = %host,
            error = %e,
            "failed to publish routing removal update"
        );
    }
}

impl From<crate::db::models::Domain> for DomainResponse {
    fn from(value: crate::db::models::Domain) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            service_id: value.service_id,
            domain: value.domain,
            is_primary: value.is_primary,
            ssl_status: value.ssl_status,
            ssl_expires_at: value.ssl_expires_at,
            ssl_error: value.ssl_error,
            target_url: value.target_url,
        }
    }
}

impl From<DomainWithProject> for DomainListResponse {
    fn from(value: DomainWithProject) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            service_id: value.service_id,
            domain: value.domain,
            is_primary: value.is_primary,
            ssl_status: value.ssl_status,
            ssl_expires_at: value.ssl_expires_at,
            ssl_error: value.ssl_error,
            target_url: value.target_url,
            project_name: value.project_name,
            service_name: value.service_name,
        }
    }
}
