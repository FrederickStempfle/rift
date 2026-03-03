use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::services as db_services,
    error::{AppError, AppResult},
};

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub service_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ServiceResponse {
    pub id: Uuid,
    pub service_type: String,
    pub name: String,
    pub status: String,
    pub connection_info: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub stopped_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct ServiceLogResponse {
    pub id: i64,
    pub service_id: Uuid,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub message: String,
    pub source: String,
}

pub async fn list_services(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> AppResult<Json<Vec<ServiceResponse>>> {
    let services = db_services::list_services_for_user(&state.pool, auth_user.user_id).await?;
    let responses: Vec<ServiceResponse> = services
        .into_iter()
        .map(ServiceResponse::from_service)
        .collect();
    Ok(Json(responses))
}

pub async fn create_service(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(payload): Json<CreateServiceRequest>,
) -> AppResult<(StatusCode, Json<ServiceResponse>)> {
    if !["supabase", "posthog"].contains(&payload.service_type.as_str()) {
        return Err(AppError::BadRequest(format!(
            "unsupported service type: {}",
            payload.service_type
        )));
    }

    let name = payload
        .name
        .unwrap_or_else(|| payload.service_type.clone());

    let config = serde_json::json!({
        "version": "latest",
    });

    let service =
        db_services::create_service(&state.pool, auth_user.user_id, &payload.service_type, &name, &config)
            .await
            .map_err(|e| {
                if matches!(&e, AppError::Db(sqlx_err) if crate::db::is_unique_violation(sqlx_err)) {
                    AppError::Conflict(format!(
                        "a {} service already exists",
                        payload.service_type
                    ))
                } else {
                    e
                }
            })?;

    let service_id = service.id;
    let service_type = service.service_type.clone();
    let docker_manager = state.docker_compose_manager.clone();

    // Spawn deployment in background (like BuildManager)
    tokio::spawn(async move {
        let result = match service_type.as_str() {
            "supabase" => docker_manager.deploy_supabase(service_id).await,
            "posthog" => docker_manager.deploy_posthog(service_id).await,
            _ => unreachable!(),
        };
        if let Err(e) = result {
            tracing::error!(service_id = %service_id, error = %e, "service deployment failed");
        }
    });

    Ok((
        StatusCode::CREATED,
        Json(ServiceResponse::from_service(service)),
    ))
}

pub async fn get_service(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(service_id): Path<Uuid>,
) -> AppResult<Json<ServiceResponse>> {
    let service = db_services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("service not found".into()))?;

    Ok(Json(ServiceResponse::from_service(service)))
}

pub async fn stop_service(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(service_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let service = db_services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("service not found".into()))?;

    if service.status != "running" {
        return Err(AppError::Conflict("service is not running".into()));
    }

    let service_type = service.service_type.clone();
    let docker_manager = state.docker_compose_manager.clone();
    tokio::spawn(async move {
        let result = match service_type.as_str() {
            "supabase" => docker_manager.stop_supabase(service_id).await,
            "posthog" => docker_manager.stop_posthog(service_id).await,
            _ => unreachable!(),
        };
        if let Err(e) = result {
            tracing::error!(service_id = %service_id, error = %e, "failed to stop service");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

pub async fn start_service(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(service_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let service = db_services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("service not found".into()))?;

    if service.status != "stopped" {
        return Err(AppError::Conflict("service is not stopped".into()));
    }

    let service_type = service.service_type.clone();
    let docker_manager = state.docker_compose_manager.clone();
    tokio::spawn(async move {
        let result = match service_type.as_str() {
            "supabase" => docker_manager.start_supabase(service_id).await,
            "posthog" => docker_manager.start_posthog(service_id).await,
            _ => unreachable!(),
        };
        if let Err(e) = result {
            tracing::error!(service_id = %service_id, error = %e, "failed to start service");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

pub async fn restart_service(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(service_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let service = db_services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("service not found".into()))?;

    if service.status != "running" {
        return Err(AppError::Conflict("service is not running".into()));
    }

    let service_type = service.service_type.clone();
    let docker_manager = state.docker_compose_manager.clone();
    tokio::spawn(async move {
        let result = match service_type.as_str() {
            "supabase" => docker_manager.restart_supabase(service_id).await,
            "posthog" => docker_manager.restart_posthog(service_id).await,
            _ => unreachable!(),
        };
        if let Err(e) = result {
            tracing::error!(service_id = %service_id, error = %e, "failed to restart service");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

pub async fn delete_service(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(service_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let service = db_services::get_service_for_user(&state.pool, service_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("service not found".into()))?;

    let docker_manager = state.docker_compose_manager.clone();
    let pool = state.pool.clone();
    let sid = service.id;
    let service_type = service.service_type.clone();

    tokio::spawn(async move {
        let result = match service_type.as_str() {
            "supabase" => docker_manager.remove_supabase(sid).await,
            "posthog" => docker_manager.remove_posthog(sid).await,
            _ => unreachable!(),
        };
        if let Err(e) = result {
            tracing::error!(service_id = %sid, error = %e, "failed to remove service containers");
        }
        if let Err(e) = db_services::delete_service(&pool, sid).await {
            tracing::error!(service_id = %sid, error = %e, "failed to delete service record");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

pub async fn list_service_logs(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(service_id): Path<Uuid>,
) -> AppResult<Json<Vec<ServiceLogResponse>>> {
    let logs =
        db_services::list_logs_for_service(&state.pool, service_id, auth_user.user_id).await?;

    let responses: Vec<ServiceLogResponse> = logs
        .into_iter()
        .map(|log| ServiceLogResponse {
            id: log.id,
            service_id: log.service_id,
            timestamp: log.timestamp,
            level: log.level,
            message: log.message,
            source: log.source,
        })
        .collect();

    Ok(Json(responses))
}

impl ServiceResponse {
    fn from_service(value: crate::db::models::Service) -> Self {
        Self {
            id: value.id,
            service_type: value.service_type,
            name: value.name,
            status: value.status,
            connection_info: value.connection_info,
            error_message: value.error_message,
            started_at: value.started_at,
            stopped_at: value.stopped_at,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}
