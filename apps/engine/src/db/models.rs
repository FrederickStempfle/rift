use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub github_id: Option<String>,
    pub github_login: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Project {
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
    pub webhook_id: Option<i64>,
    pub webhook_secret: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Domain {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub domain: String,
    pub is_primary: bool,
    pub ssl_status: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Deployment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub commit_sha: String,
    pub commit_message: Option<String>,
    pub branch: String,
    pub status: String,
    pub build_duration_ms: Option<i32>,
    pub url: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DeployLog {
    pub id: i64,
    pub deployment_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct FirewallRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub cidr: String,
    pub action: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AuditLog {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub user_id: Option<Uuid>,
    pub event: String,
    pub resource_id: Option<Uuid>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub metadata: serde_json::Value,
}
