use std::path::Path;

use tokio::fs;
use uuid::Uuid;

use crate::{error::AppError, ws::LogBroadcaster};

use super::pipeline::{insert_and_broadcast_log, run_command_and_log};

/// Check if a project has serverless functions.
pub fn has_functions(workspace_dir: &Path) -> bool {
    workspace_dir.join("rift/functions").is_dir()
}

/// Scan the `rift/functions/` directory, bundle each function with esbuild,
/// generate per-function worker wrappers, and write the dispatcher entry point.
///
/// File naming conventions:
///   `rift/functions/api/hello.ts`      → route `/api/hello`
///   `rift/functions/api/users/[id].ts` → route `/api/users/:id`
///   `rift/functions/index.ts`          → route `/`
pub async fn build_function_bundle(
    workspace_dir: &Path,
    output_dir: &Path,
    template_dir: &Path,
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
) -> Result<Vec<FunctionRoute>, AppError> {
    let functions_dir = workspace_dir.join("rift/functions");
    if !functions_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut routes = Vec::new();
    scan_functions_dir(&functions_dir, &functions_dir, &mut routes)?;

    if routes.is_empty() {
        return Ok(Vec::new());
    }

    // Sort routes: static routes before parameterized, then alphabetically
    routes.sort_by(|a, b| {
        let a_has_param = a.pattern.contains(':');
        let b_has_param = b.pattern.contains(':');
        a_has_param.cmp(&b_has_param).then(a.pattern.cmp(&b.pattern))
    });

    fs::create_dir_all(output_dir).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to create output dir {}: {e}",
            output_dir.display()
        ))
    })?;

    let bundles_dir = output_dir.join("bundles");
    fs::create_dir_all(&bundles_dir).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to create bundles dir {}: {e}",
            bundles_dir.display()
        ))
    })?;

    // Bundle all functions with esbuild in a single invocation
    bundle_functions_with_esbuild(
        &routes,
        workspace_dir,
        &bundles_dir,
        pool,
        broadcaster,
        deployment_id,
    )
    .await?;

    // Assign bundle_file paths now that esbuild has run
    for route in &mut routes {
        let sanitized = sanitize_route_name(&route.pattern);
        route.bundle_file = format!("bundles/{sanitized}.js");
    }

    // Generate per-function worker wrappers
    generate_worker_wrappers(&routes, output_dir, template_dir).await?;

    // Generate the dispatcher entry as _entry.ts
    generate_dispatcher_entry(&routes, output_dir, template_dir).await?;

    Ok(routes)
}

/// A detected function route.
#[derive(Debug, Clone)]
pub struct FunctionRoute {
    /// URL pattern (e.g., "/api/hello", "/api/users/:id").
    pub pattern: String,
    /// Relative path to the source file.
    pub file_path: String,
    /// Path to the pre-bundled .js file (relative to output_dir), set after esbuild.
    pub bundle_file: String,
}

/// Recursively scan a directory for function files.
fn scan_functions_dir(
    base_dir: &Path,
    current_dir: &Path,
    routes: &mut Vec<FunctionRoute>,
) -> Result<(), AppError> {
    let entries = std::fs::read_dir(current_dir).map_err(|e| {
        AppError::Internal(format!(
            "failed to read functions dir {}: {e}",
            current_dir.display()
        ))
    })?;

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| AppError::Internal(format!("failed to get file type: {e}")))?;

        if file_type.is_dir() {
            // Skip node_modules and hidden directories
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            scan_functions_dir(base_dir, &path, routes)?;
        } else if file_type.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();

            // Only process .ts, .js, .tsx, .jsx files
            let is_function_file = name.ends_with(".ts")
                || name.ends_with(".js")
                || name.ends_with(".tsx")
                || name.ends_with(".jsx");

            // Skip test files and type definitions
            let is_test = name.contains(".test.") || name.contains(".spec.");
            let is_type_def = name.ends_with(".d.ts");

            if is_function_file && !is_test && !is_type_def {
                let rel_path = path
                    .strip_prefix(base_dir)
                    .map_err(|e| AppError::Internal(format!("path strip failed: {e}")))?;

                let route_pattern = file_path_to_route_pattern(rel_path);
                let file_rel = path
                    .strip_prefix(base_dir.parent().unwrap_or(base_dir))
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                routes.push(FunctionRoute {
                    pattern: route_pattern,
                    file_path: file_rel,
                    bundle_file: String::new(), // filled after esbuild
                });
            }
        }
    }

    Ok(())
}

/// Convert a file path to a URL route pattern.
///
/// Examples:
///   `api/hello.ts`        → `/api/hello`
///   `api/users/[id].ts`   → `/api/users/:id`
///   `index.ts`            → `/`
fn file_path_to_route_pattern(rel_path: &Path) -> String {
    let path_str = rel_path.to_string_lossy().to_string();

    // Remove file extension
    let without_ext = path_str
        .strip_suffix(".tsx")
        .or_else(|| path_str.strip_suffix(".jsx"))
        .or_else(|| path_str.strip_suffix(".ts"))
        .or_else(|| path_str.strip_suffix(".js"))
        .unwrap_or(&path_str);

    // Handle index files
    let without_index = if without_ext == "index" {
        ""
    } else {
        without_ext
            .strip_suffix("/index")
            .unwrap_or(without_ext)
    };

    // Convert [param] to :param for URLPattern
    let pattern = without_index
        .replace('[', ":")
        .replace(']', "");

    if pattern.is_empty() {
        "/".to_string()
    } else {
        format!("/{pattern}")
    }
}

/// Sanitize a route pattern into a valid filename.
/// e.g. "/api/hello" → "api_hello", "/api/users/:id" → "api_users__id"
fn sanitize_route_name(pattern: &str) -> String {
    pattern
        .trim_start_matches('/')
        .replace('/', "_")
        .replace(':', "_")
        .replace('*', "_star")
}

/// Bundle all function source files with esbuild in a single invocation.
async fn bundle_functions_with_esbuild(
    routes: &[FunctionRoute],
    workspace_dir: &Path,
    bundles_dir: &Path,
    pool: &sqlx::PgPool,
    broadcaster: &LogBroadcaster,
    deployment_id: Uuid,
) -> Result<(), AppError> {
    let functions_dir = workspace_dir.join("rift/functions");

    // Collect absolute paths to all function source files
    let source_files: Vec<String> = routes
        .iter()
        .map(|r| {
            functions_dir
                .parent()
                .unwrap_or(&functions_dir)
                .join(&r.file_path)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    if source_files.is_empty() {
        return Ok(());
    }

    insert_and_broadcast_log(
        pool,
        broadcaster,
        deployment_id,
        "info",
        &format!("Bundling {} function(s) with esbuild", source_files.len()),
        "build",
    )
    .await?;

    // Build esbuild command: bundle all files in one invocation
    // esbuild produces <bundles_dir>/<original_filename>.js for each entry
    let mut cmd_parts = vec![
        "npx".to_string(),
        "esbuild".to_string(),
        "--bundle".to_string(),
        "--format=esm".to_string(),
        "--platform=neutral".to_string(),
        format!("--outdir={}", bundles_dir.display()),
    ];
    cmd_parts.extend(source_files.clone());

    let cmd = cmd_parts.join(" ");
    run_command_and_log(pool, broadcaster, deployment_id, "build", workspace_dir, &cmd).await?;

    // esbuild names outputs based on the source filename (e.g. hello.js).
    // We need to rename them to match our sanitized route names.
    for route in routes {
        let source_path = functions_dir
            .parent()
            .unwrap_or(&functions_dir)
            .join(&route.file_path);
        let source_stem = source_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();

        let esbuild_output = bundles_dir.join(format!("{source_stem}.js"));
        let sanitized = sanitize_route_name(&route.pattern);
        let target = bundles_dir.join(format!("{sanitized}.js"));

        if esbuild_output.exists() && esbuild_output != target {
            fs::rename(&esbuild_output, &target).await.map_err(|e| {
                AppError::Internal(format!(
                    "failed to rename bundle {} → {}: {e}",
                    esbuild_output.display(),
                    target.display()
                ))
            })?;
        }
    }

    Ok(())
}

/// Generate per-function Web Worker wrapper files.
///
/// Each wrapper imports the pre-bundled function and handles postMessage IPC
/// with the dispatcher process.
async fn generate_worker_wrappers(
    routes: &[FunctionRoute],
    output_dir: &Path,
    template_dir: &Path,
) -> Result<(), AppError> {
    let template_path = template_dir.join("function_worker.ts");
    let template = fs::read_to_string(&template_path).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to read function_worker.ts template: {e}"
        ))
    })?;

    for route in routes {
        let sanitized = sanitize_route_name(&route.pattern);
        let bundle_abs = output_dir
            .join(&route.bundle_file)
            .to_string_lossy()
            .to_string();

        let wrapper_code = template.replace("\"__BUNDLE_IMPORT__\"", &format!("\"file://{bundle_abs}\""));
        let wrapper_path = output_dir.join(format!("_worker_{sanitized}.ts"));

        fs::write(&wrapper_path, wrapper_code).await.map_err(|e| {
            AppError::Internal(format!(
                "failed to write worker wrapper {}: {e}",
                wrapper_path.display()
            ))
        })?;
    }

    Ok(())
}

/// Generate the dispatcher entry point that routes requests to per-function Workers.
///
/// Reads the function_dispatcher.ts template and injects the route table.
/// Written as `_entry.ts` so the existing RuntimeKind::Functions spawn works.
async fn generate_dispatcher_entry(
    routes: &[FunctionRoute],
    output_dir: &Path,
    template_dir: &Path,
) -> Result<(), AppError> {
    let template_path = template_dir.join("function_dispatcher.ts");
    let template = fs::read_to_string(&template_path).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to read function_dispatcher.ts template: {e}"
        ))
    })?;

    // Build the route table entries
    let mut route_entries = Vec::new();
    for route in routes {
        let sanitized = sanitize_route_name(&route.pattern);
        let worker_path = output_dir
            .join(format!("_worker_{sanitized}.ts"))
            .to_string_lossy()
            .to_string();

        route_entries.push(format!(
            r#"  {{ pattern: new URLPattern({{ pathname: "{pattern}" }}), workerPath: "file://{worker_path}", active: 0 }}"#,
            pattern = route.pattern,
            worker_path = worker_path,
        ));
    }

    let routes_array = format!("[\n{}\n]", route_entries.join(",\n"));
    let dispatcher_code = template
        .replace("__ROUTES__", &routes_array)
        .replace("__ROUTE_COUNT__", &routes.len().to_string());

    let entry_path = output_dir.join("_entry.ts");
    fs::write(&entry_path, dispatcher_code).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to write function dispatcher entry: {e}"
        ))
    })?;

    Ok(())
}

/// Generate a combined entry point that dispatches function requests to Workers
/// and falls through to the framework handler for non-matching requests.
pub async fn generate_combined_entry(
    routes: &[FunctionRoute],
    output_dir: &Path,
    framework_entry_path: &Path,
    template_dir: &Path,
) -> Result<String, AppError> {
    let template_path = template_dir.join("function_worker.ts");
    let worker_template = fs::read_to_string(&template_path).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to read function_worker.ts template: {e}"
        ))
    })?;

    // Generate worker wrappers for the combined entry
    for route in routes {
        let sanitized = sanitize_route_name(&route.pattern);
        let bundle_abs = output_dir
            .join(&route.bundle_file)
            .to_string_lossy()
            .to_string();

        let wrapper_code = worker_template.replace("\"__BUNDLE_IMPORT__\"", &format!("\"file://{bundle_abs}\""));
        let wrapper_path = output_dir.join(format!("_worker_{sanitized}.ts"));

        fs::write(&wrapper_path, wrapper_code).await.map_err(|e| {
            AppError::Internal(format!(
                "failed to write worker wrapper {}: {e}",
                wrapper_path.display()
            ))
        })?;
    }

    // Build route table
    let mut route_entries = Vec::new();
    for route in routes {
        let sanitized = sanitize_route_name(&route.pattern);
        let worker_path = output_dir
            .join(format!("_worker_{sanitized}.ts"))
            .to_string_lossy()
            .to_string();

        route_entries.push(format!(
            r#"  {{ pattern: new URLPattern({{ pathname: "{pattern}" }}), workerPath: "file://{worker_path}", active: 0 }}"#,
            pattern = route.pattern,
            worker_path = worker_path,
        ));
    }

    let routes_array = format!("[\n{}\n]", route_entries.join(",\n"));
    let framework_path = framework_entry_path.to_string_lossy();

    let combined = format!(
        r#"/**
 * Rift Combined Dispatcher (auto-generated)
 *
 * True serverless: each request gets a fresh V8 isolate via a new Web Worker.
 * No shared state between invocations. Non-matching requests fall through
 * to the framework handler.
 *
 * {count} function route(s) detected.
 */

import frameworkHandler from "file://{framework_path}";

const PORT = parseInt(Deno.env.get("PORT") ?? "3000");
const MAX_CONCURRENT = parseInt(Deno.env.get("RIFT_MAX_CONCURRENT") ?? "50");

interface Route {{
  pattern: URLPattern;
  workerPath: string;
  active: number;
}}

interface WorkerRequest {{
  url: string;
  method: string;
  headers: [string, string][];
  body: number[] | null;
}}

interface WorkerResponse {{
  status: number;
  headers: [string, string][];
  body: number[] | null;
  error?: string;
}}

const routes: Route[] = {routes_array};

function dispatch(route: Route, data: WorkerRequest): Promise<Response> {{
  return new Promise<Response>((resolve) => {{
    if (route.active >= MAX_CONCURRENT) {{
      resolve(new Response(
        JSON.stringify({{ error: "Too Many Requests" }}),
        {{ status: 429, headers: {{ "content-type": "application/json" }} }},
      ));
      return;
    }}

    route.active++;
    let settled = false;
    const worker = new Worker(route.workerPath, {{
      type: "module",
      deno: {{ permissions: "inherit" }},
    }} as WorkerOptions);

    const timeout = setTimeout(() => {{
      if (!settled) {{
        settled = true;
        route.active--;
        worker.terminate();
        resolve(new Response(
          JSON.stringify({{ error: "Function timed out" }}),
          {{ status: 504, headers: {{ "content-type": "application/json" }} }},
        ));
      }}
    }}, 30_000);

    worker.onmessage = (e: MessageEvent<WorkerResponse>) => {{
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      route.active--;

      const resp = e.data;
      const body = resp.body ? new Uint8Array(resp.body) : null;
      worker.terminate();

      if (resp.error) {{
        resolve(new Response(
          JSON.stringify({{ error: "Internal Server Error" }}),
          {{ status: resp.status || 500, headers: {{ "content-type": "application/json" }} }},
        ));
        return;
      }}

      resolve(new Response(body, {{
        status: resp.status,
        headers: new Headers(resp.headers),
      }}));
    }};

    worker.onerror = (e: ErrorEvent) => {{
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      route.active--;
      worker.terminate();

      console.error(`[rift-dispatcher] Worker error: ${{e.message}}`);
      resolve(new Response(
        JSON.stringify({{ error: "Worker Error" }}),
        {{ status: 500, headers: {{ "content-type": "application/json" }} }},
      ));
    }};

    try {{
      worker.postMessage(data);
    }} catch (e) {{
      if (!settled) {{
        settled = true;
        clearTimeout(timeout);
        route.active--;
        worker.terminate();
        resolve(new Response(
          JSON.stringify({{ error: "Failed to dispatch to worker" }}),
          {{ status: 500, headers: {{ "content-type": "application/json" }} }},
        ));
      }}
    }}
  }});
}}

Deno.serve({{ port: PORT, hostname: "0.0.0.0" }}, async (req) => {{
  const url = new URL(req.url);

  // Try function routes first (isolated per-request Workers)
  for (const route of routes) {{
    const match = route.pattern.exec(url);
    if (!match) continue;

    const groups = match.pathname.groups;
    const headers: [string, string][] = [...req.headers.entries()];
    for (const [k, v] of Object.entries(groups)) {{
      if (v !== undefined) headers.push([`x-rift-param-${{k}}`, v as string]);
    }}

    const body = req.body
      ? Array.from(new Uint8Array(await req.arrayBuffer()))
      : null;

    return dispatch(route, {{
      url: req.url,
      method: req.method,
      headers,
      body,
    }});
  }}

  // No function route matched — delegate to framework handler
  if (frameworkHandler && typeof (frameworkHandler as any).fetch === "function") {{
    return (frameworkHandler as any).fetch(req);
  }}
  if (typeof frameworkHandler === "function") {{
    return (frameworkHandler as any)(req);
  }}

  return new Response(
    JSON.stringify({{ error: "Not Found" }}),
    {{ status: 404, headers: {{ "content-type": "application/json" }} }},
  );
}});
"#,
        count = routes.len(),
        framework_path = framework_path,
        routes_array = routes_array,
    );

    Ok(combined)
}
