use colored::Colorize;
use dialoguer::{Input, Password};

use crate::client::RiftClient;
use crate::config::{self, CliConfig};
use crate::error::CliError;

pub async fn login(client: &mut RiftClient, json: bool) -> Result<(), CliError> {
    let email: String = Input::new()
        .with_prompt("Email")
        .interact_text()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let password: String = Password::new()
        .with_prompt("Password")
        .interact()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let creds = client.login(&email, &password).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&creds)?);
    } else {
        println!("Logged in as {}", creds.user.email.green().bold());
    }

    Ok(())
}

pub async fn logout(client: &mut RiftClient) -> Result<(), CliError> {
    client.logout().await?;
    println!("Logged out");
    Ok(())
}

pub async fn whoami(client: &mut RiftClient, json: bool) -> Result<(), CliError> {
    let user = client.whoami().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&user)?);
    } else {
        println!("  {}: {}", "Email".bold(), user.email);
        println!("  {}:    {}", "ID".bold(), user.id);
        if let Some(name) = &user.display_name {
            println!("  {}:  {name}", "Name".bold());
        }
        if let Some(gh) = &user.github_login {
            println!("  {}: {gh}", "GitHub".bold());
        }
    }

    Ok(())
}

/// Ensure config exists. If not, prompt for API URL.
pub fn ensure_config() -> Result<CliConfig, CliError> {
    match config::load() {
        Ok(cfg) => Ok(cfg),
        Err(_) => {
            println!("{}", "First-time setup".bold());

            let api_url: String = Input::new()
                .with_prompt("Rift API URL")
                .default("http://localhost:3001".into())
                .interact_text()
                .map_err(|e| CliError::UserMessage(e.to_string()))?;

            let api_url = api_url.trim_end_matches('/').to_string();

            let cfg = CliConfig { api_url };
            config::save(&cfg)?;
            Ok(cfg)
        }
    }
}
