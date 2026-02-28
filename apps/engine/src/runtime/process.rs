use std::net::TcpListener;

use tokio::process::{Child, Command};

use crate::error::AppError;

pub fn allocate_port() -> Result<u16, AppError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| AppError::Internal(format!("failed to allocate port: {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| AppError::Internal(format!("failed to read local addr: {error}")))?
        .port();
    drop(listener);
    Ok(port)
}

pub fn spawn_shell(
    command: &str,
    cwd: &std::path::Path,
    envs: &[(&str, String)],
) -> Result<Child, AppError> {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    cmd.spawn().map_err(|error| {
        AppError::Internal(format!("failed to spawn process '{command}': {error}"))
    })
}
