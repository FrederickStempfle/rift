use colored::Colorize;
use dialoguer::{Confirm, Input, Select};

use crate::client::projects::CreateProjectRequest;
use crate::client::RiftClient;
use crate::error::CliError;
use crate::output::{print_table, relative_time, status_color};

use super::{resolve_project_id, save_link, ProjectLink};

pub async fn list(client: &mut RiftClient, json: bool) -> Result<(), CliError> {
    let projects = client.list_projects().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&projects)?);
        return Ok(());
    }

    if projects.is_empty() {
        println!("No projects. Create one with `rift projects create`.");
        return Ok(());
    }

    let headers = &["NAME", "FRAMEWORK", "STATUS", "URL", "LAST DEPLOY"];
    let rows: Vec<Vec<String>> = projects
        .iter()
        .map(|p| {
            let last_deploy = match &p.latest_deployment {
                Some(d) => format!(
                    "{} ({})",
                    relative_time(d.created_at),
                    status_color(&d.status)
                ),
                None => "--".dimmed().to_string(),
            };
            vec![
                p.name.clone(),
                p.framework.clone(),
                status_color(&p.runtime_status),
                p.public_url.clone(),
                last_deploy,
            ]
        })
        .collect();

    print_table(headers, &rows);
    Ok(())
}

pub async fn create(client: &mut RiftClient, json: bool) -> Result<(), CliError> {
    let name: String = Input::new()
        .with_prompt("Project name")
        .interact_text()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let repo_url: String = Input::new()
        .with_prompt("Repository URL")
        .interact_text()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let branch: String = Input::new()
        .with_prompt("Branch")
        .default("main".into())
        .interact_text()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let framework: String = Input::new()
        .with_prompt("Framework (nextjs/vite/remix/astro/svelte/static/unknown)")
        .default("unknown".into())
        .interact_text()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let subdomain: String = Input::new()
        .with_prompt("Subdomain")
        .default(name.to_lowercase().replace(' ', "-"))
        .interact_text()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let project = client
        .create_project(CreateProjectRequest {
            name,
            repo_url,
            branch: Some(branch),
            framework: Some(framework),
            build_command: None,
            output_dir: None,
            install_command: None,
            subdomain,
        })
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&project)?);
    } else {
        println!("Created project {}", project.name.green().bold());
        println!("  URL: {}", project.public_url);
    }

    Ok(())
}

pub async fn info(
    client: &mut RiftClient,
    project_override: &Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    let project = client.get_project(project_id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&project)?);
        return Ok(());
    }

    println!("  {}: {}", "Name".bold(), project.name);
    println!("  {}:   {}", "ID".bold(), project.id);
    println!("  {}:  {}", "URL".bold(), project.public_url);
    println!("  {}: {}", "Repo".bold(), project.repo_url);
    println!("  {}: {}", "Branch".bold(), project.branch);
    println!(
        "  {}: {}",
        "Framework".bold(),
        project.framework
    );
    println!(
        "  {}: {}",
        "Status".bold(),
        status_color(&project.runtime_status)
    );
    if let Some(domain) = &project.primary_domain {
        println!("  {}: {domain}", "Domain".bold());
    }
    if let Some(d) = &project.latest_deployment {
        println!(
            "  {}: {} {} ({})",
            "Last deploy".bold(),
            &d.commit_sha[..7.min(d.commit_sha.len())],
            status_color(&d.status),
            relative_time(d.created_at)
        );
    }

    Ok(())
}

pub async fn delete(
    client: &mut RiftClient,
    project_override: &Option<String>,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    let project = client.get_project(project_id).await?;

    let confirmed = Confirm::new()
        .with_prompt(format!(
            "Are you sure you want to delete '{}'? This cannot be undone",
            project.name
        ))
        .default(false)
        .interact()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    if !confirmed {
        println!("Cancelled");
        return Ok(());
    }

    client.delete_project(project_id).await?;
    println!("Deleted project {}", project.name.red());

    Ok(())
}

pub async fn link(client: &mut RiftClient) -> Result<(), CliError> {
    let projects = client.list_projects().await?;

    if projects.is_empty() {
        return Err(CliError::UserMessage(
            "No projects found. Create one with `rift projects create`.".into(),
        ));
    }

    let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
    let selection = Select::new()
        .with_prompt("Select a project")
        .items(&names)
        .interact()
        .map_err(|e| CliError::UserMessage(e.to_string()))?;

    let project = &projects[selection];
    save_link(&ProjectLink {
        project_id: project.id,
        project_name: project.name.clone(),
    })?;

    println!(
        "Linked to {} ({})",
        project.name.green().bold(),
        project.id
    );
    Ok(())
}

pub async fn unlink() -> Result<(), CliError> {
    if super::remove_link()? {
        println!("Unlinked");
    } else {
        println!("No project linked in this directory");
    }
    Ok(())
}
