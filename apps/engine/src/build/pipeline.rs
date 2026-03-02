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

pub async fn run_argv_and_log(
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
    source: &str,
    cwd: &Path,
    program: &str,
    args: &[String],
) -> Result<(), AppError> {
    run_argv_and_log_with_env(
        pool,
        broadcaster,
        deployment_id,
        source,
        cwd,
        program,
        args,
        &[],
    )
    .await
}

pub async fn run_argv_and_log_with_env(
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
    source: &str,
    cwd: &Path,
    program: &str,
    args: &[String],
    envs: &[(String, String)],
) -> Result<(), AppError> {
    let display = format!("{} {}", program, args.join(" "));
    let redacted_display = redact_command_for_log(&display, envs);
    insert_and_broadcast_log(
        pool,
        broadcaster,
        deployment_id,
        "info",
        &format!("$ {redacted_display}"),
        source,
    )
    .await?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    run_child_and_capture(
        cmd,
        pool.clone(),
        broadcaster.clone(),
        deployment_id,
        source.to_owned(),
        &redacted_display,
    )
    .await
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
    let redacted_command = redact_command_for_log(command, envs);
    insert_and_broadcast_log(
        pool,
        broadcaster,
        deployment_id,
        "info",
        &format!("$ {redacted_command}"),
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

    run_child_and_capture(
        cmd,
        pool.clone(),
        broadcaster.clone(),
        deployment_id,
        source.to_owned(),
        &redacted_command,
    )
    .await
}

pub fn split_command_argv(command: &str) -> Result<(String, Vec<String>), AppError> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| AppError::BadRequest("command must not be empty".into()))?;
    let args = parts.map(ToOwned::to_owned).collect::<Vec<_>>();
    Ok((program.to_owned(), args))
}

async fn run_child_and_capture(
    mut cmd: Command,
    pool: sqlx::PgPool,
    broadcaster: LogBroadcaster,
    deployment_id: Uuid,
    source: String,
    display_command: &str,
) -> Result<(), AppError> {
    let mut child = cmd.spawn().map_err(|error| {
        AppError::Internal(format!(
            "failed to spawn '{display_command}': {error}"
        ))
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_task = tokio::spawn(read_stream(
        stdout,
        pool.clone(),
        broadcaster.clone(),
        deployment_id,
        source.clone(),
        "info".to_owned(),
    ));
    let stderr_task = tokio::spawn(read_stream(
        stderr,
        pool.clone(),
        broadcaster.clone(),
        deployment_id,
        source,
        "error".to_owned(),
    ));

    let status = child
        .wait()
        .await
        .map_err(|error| {
            AppError::Internal(format!(
                "failed waiting for '{display_command}': {error}"
            ))
        })?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if status.success() {
        Ok(())
    } else {
        Err(AppError::Internal(format!(
            "command failed with status {:?}: {display_command}",
            status.code()
        )))
    }
}

fn redact_command_for_log(command: &str, envs: &[(String, String)]) -> String {
    let mut redacted = command.to_owned();

    for (_key, value) in envs {
        if value.len() >= 6 && redacted.contains(value) {
            redacted = redacted.replace(value, "***");
        }
    }

    // Defensive redaction for accidental GitHub token URL embeddings.
    while let Some(token_start) = redacted.find("x-access-token:") {
        let from = token_start + "x-access-token:".len();
        let Some(rel_end) = redacted[from..].find('@') else {
            break;
        };
        let to = from + rel_end;
        redacted.replace_range(from..to, "***");
    }

    redacted
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_command_argv_splits_simple_command() {
        let (program, args) = split_command_argv("npm run build -- --prod").expect("must parse");
        assert_eq!(program, "npm");
        assert_eq!(args, vec!["run", "build", "--", "--prod"]);
    }

    #[test]
    fn redact_command_hides_env_secret() {
        let cmd = "git -c credential.helper=!f() { echo password=$RIFT_GITHUB_TOKEN; } clone";
        let envs = vec![("RIFT_GITHUB_TOKEN".to_owned(), "secret-token-value".to_owned())];
        let redacted = redact_command_for_log(cmd, &envs);
        assert!(!redacted.contains("secret-token-value"));
    }

    #[test]
    fn redact_command_hides_tokenized_urls() {
        let cmd = "git clone https://x-access-token:abc123@github.com/acme/repo";
        let redacted = redact_command_for_log(cmd, &[]);
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("x-access-token:***@"));
    }
}
