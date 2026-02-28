use std::{path::Path, process::Stdio, time::Instant};

use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
};
use uuid::Uuid;

use crate::{
    db::deployments,
    error::AppError,
    ws::{broadcast::DeployLogMessage, LogBroadcaster},
};

/// Insert a log into the database AND broadcast it to WebSocket subscribers.
pub async fn insert_and_broadcast_log(
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
    level: &str,
    message: &str,
    source: &str,
) -> Result<(), AppError> {
    let log =
        deployments::insert_log_returning(pool, deployment_id, level, message, source).await?;
    broadcaster
        .send(
            deployment_id,
            DeployLogMessage {
                id: log.id,
                deployment_id: log.deployment_id,
                timestamp: log.timestamp,
                level: log.level,
                message: log.message,
                source: log.source,
            },
        )
        .await;
    Ok(())
}

pub async fn run_command_and_log(
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
    source: &str,
    cwd: &Path,
    command: &str,
) -> Result<(), AppError> {
    run_command_and_log_with_env(pool, broadcaster, deployment_id, source, cwd, command, &[]).await
}

pub async fn run_command_and_log_with_env(
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
    source: &str,
    cwd: &Path,
    command: &str,
    envs: &[(String, String)],
) -> Result<(), AppError> {
    insert_and_broadcast_log(
        pool,
        broadcaster,
        deployment_id,
        "info",
        &format!("$ {command}"),
        source,
    )
    .await?;

    let mut cmd = Command::new("sh");
    cmd.arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    let mut child = cmd
        .spawn()
        .map_err(|error| AppError::Internal(format!("failed to spawn '{command}': {error}")))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_task = tokio::spawn(read_stream(
        stdout,
        pool.clone(),
        broadcaster.clone(),
        deployment_id,
        source.to_owned(),
        "info".to_owned(),
    ));
    let stderr_task = tokio::spawn(read_stream(
        stderr,
        pool.clone(),
        broadcaster.clone(),
        deployment_id,
        source.to_owned(),
        "error".to_owned(),
    ));

    let status = child
        .wait()
        .await
        .map_err(|error| AppError::Internal(format!("failed waiting for '{command}': {error}")))?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if status.success() {
        Ok(())
    } else {
        Err(AppError::Internal(format!(
            "command failed with status {:?}: {command}",
            status.code()
        )))
    }
}

async fn read_stream(
    stream: Option<impl AsyncRead + Unpin + Send + 'static>,
    pool: sqlx::PgPool,
    broadcaster: LogBroadcaster,
    deployment_id: Uuid,
    source: String,
    level: String,
) {
    if let Some(stream) = stream {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = insert_and_broadcast_log(
                &pool,
                &broadcaster,
                deployment_id,
                &level,
                &line,
                &source,
            )
            .await;
        }
    }
}

pub async fn read_git_metadata(workspace_dir: &Path) -> Result<(String, Option<String>), AppError> {
    let sha = tokio::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(workspace_dir)
        .output()
        .await
        .map_err(|error| AppError::Internal(format!("git rev-parse failed: {error}")))?;
    if !sha.status.success() {
        return Err(AppError::Internal("git rev-parse HEAD failed".into()));
    }

    let message = tokio::process::Command::new("git")
        .arg("log")
        .arg("-1")
        .arg("--pretty=%s")
        .current_dir(workspace_dir)
        .output()
        .await
        .map_err(|error| AppError::Internal(format!("git log failed: {error}")))?;

    let sha = String::from_utf8_lossy(&sha.stdout).trim().to_owned();
    let message = if message.status.success() {
        Some(String::from_utf8_lossy(&message.stdout).trim().to_owned())
    } else {
        None
    };

    Ok((sha, message))
}

pub fn elapsed_ms(started_at: Instant) -> i32 {
    started_at.elapsed().as_millis().min(i32::MAX as u128) as i32
}
