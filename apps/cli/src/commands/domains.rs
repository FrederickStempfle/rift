use colored::Colorize;

use crate::client::RiftClient;
use crate::error::CliError;
use crate::output::{print_table, status_color};

use super::resolve_project_id;

pub async fn list(
    client: &mut RiftClient,
    project_override: &Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    let domains = client.list_domains(project_id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&domains)?);
        return Ok(());
    }

    if domains.is_empty() {
        println!("No custom domains. Add one with `rift domains add <domain>`.");
        return Ok(());
    }

    let headers = &["DOMAIN", "PRIMARY", "SSL"];
    let rows: Vec<Vec<String>> = domains
        .iter()
        .map(|d| {
            vec![
                d.domain.clone(),
                if d.is_primary {
                    "yes".green().to_string()
                } else {
                    "no".dimmed().to_string()
                },
                status_color(&d.ssl_status),
            ]
        })
        .collect();

    print_table(headers, &rows);
    Ok(())
}

pub async fn add(
    client: &mut RiftClient,
    project_override: &Option<String>,
    domain: &str,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    let resp = client.create_domain(domain, project_id).await?;

    println!("Added domain {}", resp.domain.green().bold());
    println!();
    println!(
        "  Next: add a DNS A record for {} pointing to your server IP,",
        resp.domain.bold()
    );
    println!(
        "  then run `rift domains verify {}`",
        resp.domain
    );

    Ok(())
}

pub async fn remove(
    client: &mut RiftClient,
    project_override: &Option<String>,
    domain: &str,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;

    // Find domain ID by name
    let domains = client.list_domains(project_id).await?;
    let found = domains
        .iter()
        .find(|d| d.domain == domain)
        .ok_or_else(|| CliError::UserMessage(format!("domain '{domain}' not found")))?;

    client.delete_domain_by_id(found.id).await?;
    println!("Removed domain {}", domain.red());
    Ok(())
}

pub async fn verify(
    client: &mut RiftClient,
    project_override: &Option<String>,
    domain: &str,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;

    // Find domain ID by name
    let domains = client.list_domains(project_id).await?;
    let found = domains
        .iter()
        .find(|d| d.domain == domain)
        .ok_or_else(|| CliError::UserMessage(format!("domain '{domain}' not found")))?;

    let result = client.verify_domain(found.id).await?;

    println!(
        "Domain {} — SSL: {}",
        result.domain.green().bold(),
        status_color(&result.ssl_status)
    );

    Ok(())
}
