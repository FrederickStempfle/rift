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
    db::{
        domains,
        projects::{self, NewProject, UpdateProject},
    },
    error::{AppError, AppResult},
    services::audit::AuditEvent,
    validation,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", post(create_project).get(list_projects))
        .route(
            "/{project_id}",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
        )
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub repo_url: String,
    pub branch: Option<String>,
    pub framework: Option<String>,
    pub build_command: Option<String>,
    pub output_dir: Option<String>,
    pub install_command: Option<String>,
    pub subdomain: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub repo_url: Option<String>,
    pub branch: Option<String>,
    pub framework: Option<String>,
    pub build_command: Option<String>,
    pub output_dir: Option<String>,
    pub install_command: Option<String>,
    pub subdomain: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub repo_url: String,
    pub branch: String,
    pub framework: String,
    pub build_command: Option<String>,
    pub output_dir: Option<String>,
    pub install_command: Option<String>,
    pub subdomain: String,
    pub public_url: String,
    pub webhook_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub async fn create_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<CreateProjectRequest>,
) -> AppResult<(StatusCode, Json<ProjectResponse>)> {
    validation::validate_project_name(&payload.name)?;
    validation::validate_repo_url(&payload.repo_url)?;
    validation::validate_subdomain(&payload.subdomain)?;

    if let Some(branch) = &payload.branch {
        validation::ensure_no_null_bytes(branch, "branch")?;
        validation::ensure_max_len(branch, 255, "branch")?;
    }

    if let Some(command) = &payload.build_command {
        validation::ensure_no_null_bytes(command, "build command")?;
        validation::ensure_max_len(command, 1024, "build command")?;
    }

    let framework = payload.framework.unwrap_or_else(|| "unknown".to_owned());
    ensure_framework(&framework)?;

    let project = projects::create_project(
        &state.pool,
        NewProject {
            user_id: auth_user.user_id,
            name: payload.name,
            repo_url: payload.repo_url,
            branch: payload.branch.unwrap_or_else(|| "main".to_owned()),
            framework,
            build_command: payload.build_command,
            output_dir: payload.output_dir,
            install_command: payload.install_command,
            subdomain: payload.subdomain,
        },
    )
    .await?;

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "project.create",
            resource_id: Some(project.id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({}),
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ProjectResponse::from_project(&state, project).await?),
    ))
}

pub async fn list_projects(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> AppResult<Json<Vec<ProjectResponse>>> {
    let projects = projects::list_projects_for_user(&state.pool, auth_user.user_id).await?;
    let mut items = Vec::with_capacity(projects.len());
    for project in projects {
        items.push(ProjectResponse::from_project(&state, project).await?);
    }
    Ok(Json(items))
}

pub async fn get_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(project_id): Path<Uuid>,
) -> AppResult<Json<ProjectResponse>> {
    let project = projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    Ok(Json(ProjectResponse::from_project(&state, project).await?))
}

pub async fn update_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<UpdateProjectRequest>,
) -> AppResult<Json<ProjectResponse>> {
    if let Some(name) = &payload.name {
        validation::validate_project_name(name)?;
    }
    if let Some(repo_url) = &payload.repo_url {
        validation::validate_repo_url(repo_url)?;
    }
    if let Some(subdomain) = &payload.subdomain {
        validation::validate_subdomain(subdomain)?;
    }
    if let Some(framework) = &payload.framework {
        ensure_framework(framework)?;
    }

    let updated = projects::update_project_for_user(
        &state.pool,
        project_id,
        auth_user.user_id,
        UpdateProject {
            name: payload.name,
            repo_url: payload.repo_url,
            branch: payload.branch,
            framework: payload.framework,
            build_command: payload.build_command,
            output_dir: payload.output_dir,
            install_command: payload.install_command,
            subdomain: payload.subdomain,
        },
    )
    .await?
    .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    Ok(Json(ProjectResponse::from_project(&state, updated).await?))
}

pub async fn delete_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(project_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let deleted =
        projects::delete_project_for_user(&state.pool, project_id, auth_user.user_id).await?;
    if !deleted {
        return Err(AppError::NotFound("project not found".into()));
    }

    state
        .audit_logger
        .log(AuditEvent {
            user_id: Some(auth_user.user_id),
            event: "project.delete",
            resource_id: Some(project_id),
            ip_address: Some(addr.ip()),
            user_agent: user_agent(&headers),
            metadata: json!({}),
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

fn ensure_framework(value: &str) -> AppResult<()> {
    const ALLOWED: &[&str] = &[
        "nextjs", "vite", "remix", "astro", "svelte", "static", "unknown",
    ];

    if ALLOWED.contains(&value) {
        Ok(())
    } else {
        Err(AppError::BadRequest("invalid framework".into()))
    }
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

impl ProjectResponse {
    async fn from_project(
        state: &AppState,
        value: crate::db::models::Project,
    ) -> Result<Self, AppError> {
        let public_url = match domains::get_primary_domain_for_project(&state.pool, value.id).await?
        {
            Some(domain) => state.config.public_url_for_host(&domain.domain),
            None => state.config.public_url_for_subdomain(&value.subdomain),
        };
        Ok(Self {
            id: value.id,
            user_id: value.user_id,
            name: value.name,
            repo_url: value.repo_url,
            branch: value.branch,
            framework: value.framework,
            build_command: value.build_command,
            output_dir: value.output_dir,
            install_command: value.install_command,
            subdomain: value.subdomain,
            public_url,
            webhook_id: value.webhook_id,
            created_at: value.created_at,
            updated_at: value.updated_at,
        })
    }
}
