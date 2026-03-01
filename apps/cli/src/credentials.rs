use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub user: UserInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: uuid::Uuid,
    pub email: String,
    pub github_login: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

fn creds_path() -> Result<PathBuf, CliError> {
    let home = dirs::home_dir()
        .ok_or_else(|| CliError::UserMessage("cannot find home directory".into()))?;
    Ok(home.join(".rift").join("credentials.json"))
}

pub fn load() -> Result<Credentials, CliError> {
    let path = creds_path()?;
    if !path.exists() {
        return Err(CliError::NotAuthenticated);
    }
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save(creds: &Credentials) -> Result<(), CliError> {
    let home = dirs::home_dir()
        .ok_or_else(|| CliError::UserMessage("cannot find home directory".into()))?;
    let dir = home.join(".rift");
    std::fs::create_dir_all(&dir)?;

    let path = dir.join("credentials.json");
    let data = serde_json::to_string_pretty(creds)?;
    std::fs::write(&path, data)?;

    // Set file permissions to 0600 on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

pub fn clear() -> Result<(), CliError> {
    let path = creds_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}
