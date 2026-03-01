use colored::Colorize;
use futures_util::StreamExt;

use crate::client::RiftClient;
use crate::error::CliError;

use super::resolve_project_id;

pub async fn logs(
    client: &mut RiftClient,
    project_override: &Option<String>,
    deployment_id: Option<String>,
    follow: bool,
    json: bool,
) -> Result<(), CliError> {
    let dep_id = match deployment_id {
        Some(id) => id
            .parse()
            .map_err(|_| CliError::UserMessage("invalid deployment ID".into()))?,
        None => {
            // Get latest deployment
            let project_id = resolve_project_id(client, project_override).await?;
            let deployments = client.list_deployments(project_id).await?;
            deployments
                .first()
                .ok_or_else(|| CliError::UserMessage("no deployments found".into()))?
                .id
        }
    };

    if follow {
        let ws_url = client.ws_logs_url(dep_id).await?;
        stream_follow(&ws_url).await
    } else {
        let logs = client.list_logs(dep_id).await?;

        if json {
            println!("{}", serde_json::to_string_pretty(&logs)?);
            return Ok(());
        }

        if logs.is_empty() {
            println!("No logs for this deployment.");
            return Ok(());
        }

        for log in &logs {
            print_log_line(log);
        }

        Ok(())
    }
}

fn print_log_line(log: &crate::client::logs::DeployLogResponse) {
    let level = match log.level.as_str() {
        "error" => format!("[{}]", log.level).red().to_string(),
        "warn" => format!("[{}]", log.level).yellow().to_string(),
        "debug" => format!("[{}]", log.level).dimmed().to_string(),
        _ => format!("[{}]", log.level).blue().to_string(),
    };
    let time = log.timestamp.format("%H:%M:%S");
    println!("{time} {level} {}", log.message);
}

async fn stream_follow(ws_url: &str) -> Result<(), CliError> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| CliError::UserMessage(format!("WebSocket connection failed: {e}")))?;

    let (_, mut read) = ws_stream.split();

    println!("{}", "Streaming logs (Ctrl+C to stop)...".dimmed());

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                if let Ok(log) =
                    serde_json::from_str::<crate::client::logs::DeployLogResponse>(&text)
                {
                    print_log_line(&log);
                }
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }

    Ok(())
}
