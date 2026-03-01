pub mod auth;
pub mod deploy;
pub mod domains;
pub mod env;
pub mod logs;
pub mod projects;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client::RiftClient;
use crate::error::CliError;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectLink {
    pub project_id: Uuid,
    pub project_name: String,
}

fn link_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".rift")
        .join("project.json")
}

pub fn load_link() -> Option<ProjectLink> {
    let path = link_path();
    if !path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save_link(link: &ProjectLink) -> Result<(), CliError> {
    let dir = std::env::current_dir()?.join(".rift");
    std::fs::create_dir_all(&dir)?;
    let data = serde_json::to_string_pretty(link)?;
    std::fs::write(dir.join("project.json"), data)?;
    Ok(())
}

pub fn remove_link() -> Result<bool, CliError> {
    let path = link_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Resolve a project ID from --project flag or .rift/project.json link.
pub async fn resolve_project_id(
    client: &mut RiftClient,
    project_override: &Option<String>,
) -> Result<Uuid, CliError> {
    // 1. --project flag
    if let Some(p) = project_override {
        // Try as UUID first
        if let Ok(id) = p.parse::<Uuid>() {
            return Ok(id);
        }
        // Try as project name
        let projects = client.list_projects().await?;
        let found = projects
            .iter()
            .find(|proj| proj.name == *p)
            .ok_or_else(|| CliError::UserMessage(format!("project '{p}' not found")))?;
        return Ok(found.id);
    }

    // 2. .rift/project.json
    if let Some(link) = load_link() {
        return Ok(link.project_id);
    }

    Err(CliError::NoProject)
}
