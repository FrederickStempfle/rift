use colored::Colorize;
use futures_util::StreamExt;

use crate::client::RiftClient;
use crate::error::CliError;
use crate::output::{print_table, relative_time, status_color};

use super::resolve_project_id;

pub async fn deploy(
    client: &mut RiftClient,
    project_override: &Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;

    let deployment = client.create_deployment(project_id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&deployment)?);
        return Ok(());
    }

    println!(
        "Deployment {} created ({})",
        &deployment.id.to_string()[..8],
        status_color(&deployment.status)
    );
    println!("Streaming build logs...\n");

    // Stream logs via WebSocket
    let ws_url = client.ws_logs_url(deployment.id).await?;
    match stream_logs(&ws_url).await {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Log streaming ended: {e}");
        }
    }

    // Fetch final deployment status
    let deployments = client.list_deployments(project_id).await?;
    if let Some(d) = deployments.iter().find(|d| d.id == deployment.id) {
        println!();
        let duration = d
            .build_duration_ms
            .map(|ms| format!("{}s", ms / 1000))
            .unwrap_or_else(|| "--".into());
        println!(
            "Deployment {} is {} (build: {duration})",
            &d.id.to_string()[..8],
            status_color(&d.status)
        );
        if let Some(url) = &d.public_url {
            println!("  {url}");
        }
    }

    Ok(())
}

pub async fn list(
    client: &mut RiftClient,
    project_override: &Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let project_id = resolve_project_id(client, project_override).await?;
    let deployments = client.list_deployments(project_id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&deployments)?);
        return Ok(());
    }

    if deployments.is_empty() {
        println!("No deployments yet. Run `rift deploy` to trigger one.");
        return Ok(());
    }

    let headers = &["ID", "STATUS", "COMMIT", "BRANCH", "DURATION", "CREATED"];
    let rows: Vec<Vec<String>> = deployments
        .iter()
        .map(|d| {
            let duration = d
                .build_duration_ms
                .map(|ms| format!("{}s", ms / 1000))
                .unwrap_or_else(|| "--".into());
            vec![
                d.id.to_string()[..8].to_string(),
                status_color(&d.status),
                d.commit_sha[..7.min(d.commit_sha.len())].to_string(),
                d.branch.clone(),
                duration,
                relative_time(d.created_at),
            ]
        })
        .collect();

    print_table(headers, &rows);
    Ok(())
}

async fn stream_logs(ws_url: &str) -> Result<(), CliError> {
    let (ws_stream, _) =
        tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CliError::UserMessage(format!("WebSocket connection failed: {e}")))?;

    let (_, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                if let Ok(log) =
                    serde_json::from_str::<crate::client::logs::DeployLogResponse>(&text)
                {
                    let level_colored = match log.level.as_str() {
                        "error" => format!("[{}]", log.level).red().to_string(),
                        "warn" => format!("[{}]", log.level).yellow().to_string(),
                        "debug" => format!("[{}]", log.level).dimmed().to_string(),
                        _ => format!("[{}]", log.level).blue().to_string(),
                    };
                    let time = log.timestamp.format("%H:%M:%S");
                    println!("{time} {level_colored} {}", log.message);
                }
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }

    Ok(())
}
