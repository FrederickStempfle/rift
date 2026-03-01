use std::{collections::HashMap, net::SocketAddr};

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
        deployments, domains,
        projects::{self, NewProject, UpdateProject},
        users,
    },
    error::{AppError, AppResult},
    services::{audit::AuditEvent, github},
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
    pub primary_domain: Option<String>,
    pub latest_deployment: Option<ProjectDeploymentSummary>,
    pub runtime_status: String,
    pub webhook_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct ProjectDeploymentSummary {
    pub id: Uuid,
    pub status: String,
    pub commit_sha: String,
    pub commit_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
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

    // Auto-register GitHub webhook for push events
    if let Some((owner, repo)) = github::parse_owner_repo(&project.repo_url) {
        if let Ok(Some(user)) = users::find_user_by_id(&state.pool, auth_user.user_id).await {
            if let Some(token) = &user.github_token {
                let secret: String = (0..32)
                    .map(|_| format!("{:02x}", rand::random::<u8>()))
                    .collect();
                let webhook_url = format!(
                    "http://{}:{}/api/webhooks/github",
                    state.public_ip.as_deref().unwrap_or("localhost"),
                    state.config.api_port,
                );
                match github::register_webhook(token, &owner, &repo, &webhook_url, &secret).await {
                    Ok(webhook_id) => {
                        let _ = projects::set_webhook(&state.pool, project.id, webhook_id, &secret)
                            .await;
                        tracing::info!(project_id = %project.id, webhook_id, "registered GitHub webhook");
                    }
                    Err(e) => {
                        tracing::warn!(project_id = %project.id, error = %e, "failed to register webhook, auto-deploy disabled");
                    }
                }
            }
        }
    }

    // Re-fetch project to include webhook fields
    let project = projects::get_project_for_user(&state.pool, project.id, auth_user.user_id)
        .await?
        .unwrap_or(project);

    Ok((
        StatusCode::CREATED,
        Json(build_project_response(&state, project).await?),
    ))
}

pub async fn list_projects(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> AppResult<Json<Vec<ProjectResponse>>> {
    let projects = projects::list_projects_for_user(&state.pool, auth_user.user_id).await?;
    let project_ids: Vec<_> = projects.iter().map(|project| project.id).collect();
    let primary_domains =
        domains::list_configured_primary_domains_for_projects(&state.pool, &project_ids)
            .await?
            .into_iter()
            .filter_map(|domain| domain.project_id.map(|project_id| (project_id, domain)))
            .collect::<HashMap<_, _>>();
    let latest_deployments = deployments::list_latest_for_projects(&state.pool, &project_ids)
        .await?
        .into_iter()
        .map(|deployment| (deployment.project_id, deployment))
        .collect::<HashMap<_, _>>();
    let mut items = Vec::with_capacity(projects.len());
    for project in projects {
        let project_id = project.id;
        items.push(
            project_response_from_parts(
                &state,
                project,
                primary_domains.get(&project_id).cloned(),
                latest_deployments.get(&project_id).cloned(),
            )
            .await?,
        );
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

    Ok(Json(build_project_response(&state, project).await?))
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

    Ok(Json(build_project_response(&state, updated).await?))
}

pub async fn delete_project(
    State(state): State<AppState>,
    auth_user: AuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(project_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    // Clean up GitHub webhook before deleting
    if let Ok(Some(project)) =
        projects::get_project_for_user(&state.pool, project_id, auth_user.user_id).await
    {
        if let (Some(webhook_id), Some((owner, repo))) = (
            project.webhook_id,
            github::parse_owner_repo(&project.repo_url),
        ) {
            if let Ok(Some(user)) = users::find_user_by_id(&state.pool, auth_user.user_id).await {
                if let Some(token) = &user.github_token {
                    let _ = github::delete_webhook(token, &owner, &repo, webhook_id).await;
                }
            }
        }
    }

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

async fn build_project_response(
    state: &AppState,
    project: crate::db::models::Project,
) -> Result<ProjectResponse, AppError> {
    let primary_domain =
        domains::get_configured_primary_domain_for_project(&state.pool, project.id).await?;
    let latest_deployment =
        deployments::latest_deployment_for_project(&state.pool, project.id).await?;

    project_response_from_parts(state, project, primary_domain, latest_deployment).await
}

async fn project_response_from_parts(
    state: &AppState,
    value: crate::db::models::Project,
    primary_domain: Option<crate::db::models::Domain>,
    latest_deployment: Option<crate::db::models::Deployment>,
) -> Result<ProjectResponse, AppError> {
    let configured_primary_domain = primary_domain.as_ref().map(|domain| domain.domain.clone());
    let public_url = match primary_domain
        .as_ref()
        .filter(|domain| domain.ssl_status == "active")
        .map(|domain| domain.domain.as_str())
    {
        Some(domain) => state.config.public_url_for_host(domain),
        None => state.config.public_url_for_subdomain(&value.subdomain),
    };
    let runtime_status = runtime_status_for_project(state, value.id).await.to_owned();

    Ok(ProjectResponse {
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
        primary_domain: configured_primary_domain,
        latest_deployment: latest_deployment.map(ProjectDeploymentSummary::from),
        runtime_status,
        webhook_id: value.webhook_id,
        created_at: value.created_at,
        updated_at: value.updated_at,
    })
}

async fn runtime_status_for_project(state: &AppState, project_id: Uuid) -> &'static str {
    if state.runtime_backend.active_url(project_id).await.is_some() {
        "active"
    } else if state.runtime_backend.is_suspended(project_id).await {
        "suspended"
    } else {
        "inactive"
    }
}

impl From<crate::db::models::Deployment> for ProjectDeploymentSummary {
    fn from(value: crate::db::models::Deployment) -> Self {
        Self {
            id: value.id,
            status: value.status,
            commit_sha: value.commit_sha,
            commit_message: value.commit_message,
            created_at: value.created_at,
            finished_at: value.finished_at,
        }
    }
}
