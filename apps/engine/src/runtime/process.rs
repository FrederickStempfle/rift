use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, Ordering};

use tokio::process::{Child, Command};

use crate::error::AppError;

/// Port range for deployed apps. Must match the range exposed in docker-compose.yml.
const PORT_RANGE_START: u16 = 10000;
const PORT_RANGE_END: u16 = 10100;

static NEXT_PORT: AtomicU16 = AtomicU16::new(PORT_RANGE_START);

pub fn allocate_port() -> Result<u16, AppError> {
    // Try each port in the range until we find one that's free
    for _ in 0..=(PORT_RANGE_END - PORT_RANGE_START) {
        let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        // Wrap around if we exceed the range
        if port > PORT_RANGE_END {
            NEXT_PORT.store(PORT_RANGE_START, Ordering::Relaxed);
            continue;
        }
        // Check if the port is actually available
        if TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(AppError::Internal(
        "no available ports in deployment range".into(),
    ))
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
