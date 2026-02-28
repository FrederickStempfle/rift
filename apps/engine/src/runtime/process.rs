use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, Ordering};

use tokio::process::{Child, Command};

use crate::error::AppError;

/// Port range for deployed apps. Must NOT be exposed outside Docker — all
/// traffic arrives through the hyper reverse proxy on port 8080.
const PORT_RANGE_START: u16 = 10000;
const PORT_RANGE_END: u16 = 10100;

static NEXT_PORT: AtomicU16 = AtomicU16::new(PORT_RANGE_START);

pub fn allocate_port() -> Result<u16, AppError> {
    for _ in 0..=(PORT_RANGE_END - PORT_RANGE_START) {
        let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        if port > PORT_RANGE_END {
            NEXT_PORT.store(PORT_RANGE_START, Ordering::Relaxed);
            continue;
        }
        if TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(AppError::Internal(
        "no available ports in deployment range".into(),
    ))
}

/// Spawn a Deno process to serve static files via the generated `_entry.ts`.
///
/// Permissions are tightly sandboxed:
/// - Network: only listen on the assigned port
/// - Filesystem: read-only access to the bundle directory
/// - Environment: access injected env vars
pub fn spawn_deno_static(
    dir: &std::path::Path,
    port: u16,
    envs: &[(String, String)],
) -> Result<Child, AppError> {
    let entry = dir.join("_entry.ts");
    let mut cmd = Command::new("deno");
    cmd.arg("run")
        .arg(format!("--allow-net=0.0.0.0:{port}"))
        .arg(format!("--allow-read={}", dir.display()))
        .arg("--allow-env")
        .arg("--no-prompt")
        .arg(&entry)
        .current_dir(dir)
        .env("PORT", port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    cmd.spawn().map_err(|e| {
        AppError::Internal(format!(
            "failed to spawn Deno static server in {}: {e}",
            dir.display()
        ))
    })
}

/// Spawn a Deno process to run a Next.js standalone server via Node compat.
///
/// Broader permissions than static sites because Next.js needs:
/// - Network: outbound for API calls, listen on port
/// - Filesystem: read access everywhere (node_modules resolution), write for cache
/// - Environment: PORT, HOSTNAME, NODE_ENV, plus user env vars
pub fn spawn_deno_next(
    dir: &std::path::Path,
    port: u16,
    envs: &[(String, String)],
) -> Result<Child, AppError> {
    let server_js = dir.join(".next/standalone/server.js");
    let mut cmd = Command::new("deno");
    cmd.arg("run")
        .arg("--allow-net")
        .arg("--allow-read")
        .arg(format!("--allow-write={}/.next", dir.display()))
        .arg("--allow-env")
        .arg("--unstable-detect-cjs")
        .arg("--no-prompt")
        .arg(&server_js)
        .current_dir(dir.join(".next/standalone"))
        .env("PORT", port.to_string())
        .env("HOSTNAME", "0.0.0.0")
        .env("NODE_ENV", "production")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    cmd.spawn().map_err(|e| {
        AppError::Internal(format!(
            "failed to spawn Deno Next.js server in {}: {e}",
            dir.display()
        ))
    })
}
