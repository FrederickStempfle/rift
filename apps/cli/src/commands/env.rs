use colored::Colorize;

use crate::client::RiftClient;
use crate::error::CliError;
use crate::output::print_table;

use super::resolve_project_id;

pub async fn list(
    client: &mut RiftClient,
    project_override: &Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    let vars = client.list_env_vars(project_id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&vars)?);
        return Ok(());
    }

    if vars.is_empty() {
        println!("No environment variables set.");
        return Ok(());
    }

    let headers = &["KEY", "VALUE"];
    let rows: Vec<Vec<String>> = vars
        .iter()
        .map(|v| vec![v.key.clone(), v.preview.dimmed().to_string()])
        .collect();

    print_table(headers, &rows);
    Ok(())
}

pub async fn set(
    client: &mut RiftClient,
    project_override: &Option<String>,
    key: &str,
    value: &str,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    client.create_env_var(project_id, key, value).await?;
    println!("Set {}", key.green().bold());
    Ok(())
}

pub async fn unset(
    client: &mut RiftClient,
    project_override: &Option<String>,
    key: &str,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;

    // Find the env var ID by key
    let vars = client.list_env_vars(project_id).await?;
    let var = vars
        .iter()
        .find(|v| v.key == key)
        .ok_or_else(|| CliError::UserMessage(format!("env var '{key}' not found")))?;

    client.delete_env_var(var.id, project_id).await?;
    println!("Removed {}", key.red());
    Ok(())
}
