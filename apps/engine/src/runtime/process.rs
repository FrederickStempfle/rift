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
    let standalone_dir = dir.join(".next/standalone");

    // In monorepos, Next.js nests server.js inside the standalone dir
    // preserving the workspace structure (e.g. .next/standalone/apps/web/server.js)
    let server_js = if standalone_dir.join("server.js").exists() {
        standalone_dir.join("server.js")
    } else {
        find_server_js_recursive(&standalone_dir).unwrap_or_else(|| standalone_dir.join("server.js"))
    };
    let server_dir = server_js.parent().unwrap_or(&standalone_dir);

    let mut cmd = Command::new("deno");
    cmd.arg("run")
        .arg("--allow-net")
        .arg("--allow-read")
        .arg(format!("--allow-write={}/.next", dir.display()))
        .arg("--allow-env")
        .arg("--allow-sys")
        .arg("--unstable-detect-cjs")
        .arg("--no-prompt")
        .arg(&server_js)
        .current_dir(server_dir)
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

/// Search up to 3 levels deep for server.js inside a directory.
fn find_server_js_recursive(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let d1 = entry.path();
        if d1.join("server.js").exists() {
            return Some(d1.join("server.js"));
        }
        let Ok(sub_entries) = std::fs::read_dir(&d1) else {
            continue;
        };
        for sub_entry in sub_entries.flatten() {
            if !sub_entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let d2 = sub_entry.path();
            if d2.join("server.js").exists() {
                return Some(d2.join("server.js"));
            }
        }
    }
    None
}

/// Spawn a Deno process to run the serverless function dispatcher.
///
/// Broader permissions than static sites:
/// - Network: full outbound (functions may call external APIs)
/// - Filesystem: read access to the output dir (bundles + worker wrappers)
/// - Environment: user env vars
pub fn spawn_deno_functions(
    dir: &std::path::Path,
    port: u16,
    envs: &[(String, String)],
) -> Result<Child, AppError> {
    let entry = dir.join("_entry.ts");
    let mut cmd = Command::new("deno");
    cmd.arg("run")
        .arg("--allow-net")
        .arg("--allow-read")
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
            "failed to spawn Deno function dispatcher in {}: {e}",
            dir.display()
        ))
    })
}

/// Spawn the global function dispatcher — a single always-running Deno process
/// that handles ALL projects' function invocations via dynamic route registration.
pub fn spawn_global_dispatcher(
    template_dir: &std::path::Path,
    port: u16,
) -> Result<Child, AppError> {
    let entry = template_dir.join("global_function_dispatcher.ts");
    let mut cmd = Command::new("deno");
    cmd.arg("run")
        .arg("--allow-net")
        .arg("--allow-read")
        .arg("--allow-env")
        .arg("--no-prompt")
        .arg(&entry)
        .env("PORT", port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    cmd.spawn().map_err(|e| {
        AppError::Internal(format!(
            "failed to spawn global function dispatcher: {e}"
        ))
    })
}

/// Spawn a Node.js process to run an SSR server (Nuxt, Astro, SvelteKit, Remix).
///
/// For Remix, auto-detects `remix-serve` in node_modules and uses it instead of `node`.
pub fn spawn_node_server(
    dir: &std::path::Path,
    entry: &std::path::Path,
    port: u16,
    envs: &[(String, String)],
) -> Result<Child, AppError> {
    // Remix: entry exports handlers, needs remix-serve to wrap them
    let is_remix = dir.join("node_modules/.bin/remix-serve").exists()
        && entry
            .to_string_lossy()
            .contains("build/server");

    let mut cmd = if is_remix {
        let mut c = Command::new("npx");
        c.arg("remix-serve").arg(entry);
        c
    } else {
        let mut c = Command::new("node");
        c.arg(entry);
        c
    };

    cmd.current_dir(dir)
        .env("PORT", port.to_string())
        .env("HOST", "0.0.0.0")
        .env("NODE_ENV", "production")
        .env("NITRO_PORT", port.to_string())
        .env("NITRO_HOST", "0.0.0.0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    cmd.spawn().map_err(|e| {
        AppError::Internal(format!(
            "failed to spawn Node server in {}: {e}",
            dir.display()
        ))
    })
}
