use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Debug, Serialize, Deserialize)]
pub struct CliConfig {
    pub api_url: String,
}

fn config_dir() -> Result<PathBuf, CliError> {
    let home = dirs::home_dir()
        .ok_or_else(|| CliError::UserMessage("cannot find home directory".into()))?;
    Ok(home.join(".rift"))
}

pub fn config_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("config.json"))
}

pub fn load() -> Result<CliConfig, CliError> {
    let path = config_path()?;
    if !path.exists() {
        return Err(CliError::UserMessage(
            "Not configured. Run `rift login` first.".into(),
        ));
    }
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save(config: &CliConfig) -> Result<(), CliError> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let data = serde_json::to_string_pretty(config)?;
    std::fs::write(config_path()?, data)?;
    Ok(())
}
