use std::path::Path;

use tokio::fs;

use crate::error::AppError;

/// Check if a project has serverless functions.
pub fn has_functions(workspace_dir: &Path) -> bool {
    workspace_dir.join("rift/functions").is_dir()
}

/// Scan the `rift/functions/` directory and generate a bundled router entry point.
///
/// File naming conventions:
///   `rift/functions/api/hello.ts`      → route `/api/hello`
///   `rift/functions/api/users/[id].ts` → route `/api/users/:id`
///   `rift/functions/index.ts`          → route `/`
pub async fn build_function_bundle(
    workspace_dir: &Path,
    output_dir: &Path,
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

    // Generate the router entry point
    let router_code = generate_router_code(&routes, workspace_dir);

    fs::create_dir_all(output_dir).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to create output dir {}: {e}",
            output_dir.display()
        ))
    })?;

    let entry_path = output_dir.join("_rift_functions_entry.ts");
    fs::write(&entry_path, router_code).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to write function router: {e}"
        ))
    })?;

    Ok(routes)
}

/// A detected function route.
#[derive(Debug, Clone)]
pub struct FunctionRoute {
    /// URL pattern (e.g., "/api/hello", "/api/users/:id").
    pub pattern: String,
    /// Relative path to the source file.
    pub file_path: String,
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

/// Generate a combined entry point that routes to functions first, then
/// falls through to the framework handler for non-matching requests.
///
/// This is used when a project has both a framework (Next.js, Nuxt, etc.)
/// and serverless functions in `rift/functions/`.
pub fn generate_combined_entry(
    routes: &[FunctionRoute],
    workspace_dir: &Path,
    framework_entry_path: &Path,
) -> String {
    let functions_dir = workspace_dir.join("rift/functions");
    let mut route_entries = Vec::new();

    for route in routes {
        let abs_path = functions_dir
            .parent()
            .unwrap_or(&functions_dir)
            .join(&route.file_path);
        let abs_str = abs_path.to_string_lossy();
        let url_pattern = route.pattern.replace(":", ":");

        route_entries.push(format!(
            r#"  {{
    pattern: new URLPattern({{ pathname: "{url_pattern}" }}),
    path: "{file_path}",
    module: () => import("file://{abs_str}"),
  }}"#,
            url_pattern = url_pattern,
            file_path = route.file_path,
            abs_str = abs_str,
        ));
    }

    let routes_array = route_entries.join(",\n");
    let framework_path = framework_entry_path.to_string_lossy();

    format!(
        r#"/**
 * Rift Combined Router (auto-generated)
 *
 * Routes function requests to rift/functions/ handlers,
 * all other requests fall through to the framework handler.
 *
 * {count} function route(s) detected.
 */

import frameworkHandler from "file://{framework_path}";

const functionRoutes = [
{routes_array}
];

function resolveHandler(mod) {{
  const d = mod.default;
  if (d && typeof d.fetch === "function") return d.fetch.bind(d);
  if (typeof d === "function") return d;
  if (typeof mod.fetch === "function") return mod.fetch;
  if (typeof mod.handler === "function") return mod.handler;
  return null;
}}

export default {{
  async fetch(req) {{
    const url = new URL(req.url);

    // Try function routes first
    for (const route of functionRoutes) {{
      const match = route.pattern.exec(url);
      if (match) {{
        const groups = match.pathname.groups;
        const headers = new Headers(req.headers);
        for (const [k, v] of Object.entries(groups)) {{
          if (v !== undefined) headers.set(`x-rift-param-${{k}}`, v);
        }}

        try {{
          const mod = await route.module();
          const handler = resolveHandler(mod);
          if (!handler) {{
            return new Response(
              JSON.stringify({{ error: `No handler in ${{route.path}}` }}),
              {{ status: 500, headers: {{ "content-type": "application/json" }} }},
            );
          }}
          return await handler(new Request(req.url, {{
            method: req.method,
            headers,
            body: req.body,
          }}));
        }} catch (e) {{
          console.error(`[rift-functions] Error in ${{route.path}}: ${{e}}`);
          return new Response(
            JSON.stringify({{ error: "Internal Server Error" }}),
            {{ status: 500, headers: {{ "content-type": "application/json" }} }},
          );
        }}
      }}
    }}

    // No function route matched — delegate to framework handler
    if (frameworkHandler && typeof frameworkHandler.fetch === "function") {{
      return frameworkHandler.fetch(req);
    }}
    if (typeof frameworkHandler === "function") {{
      return frameworkHandler(req);
    }}

    return new Response(
      JSON.stringify({{ error: "Not Found" }}),
      {{ status: 404, headers: {{ "content-type": "application/json" }} }},
    );
  }},
}};
"#,
        count = routes.len(),
        routes_array = routes_array,
        framework_path = framework_path,
    )
}

/// Generate the router TypeScript code with the actual route definitions.
fn generate_router_code(routes: &[FunctionRoute], workspace_dir: &Path) -> String {
    let functions_dir = workspace_dir.join("rift/functions");
    let mut route_entries = Vec::new();

    for route in routes {
        let abs_path = functions_dir
            .parent()
            .unwrap_or(&functions_dir)
            .join(&route.file_path);
        let abs_str = abs_path.to_string_lossy();

        // Convert :param to URLPattern syntax (:param)
        let url_pattern = route.pattern.replace(":",":");

        route_entries.push(format!(
            r#"  {{
    pattern: new URLPattern({{ pathname: "{url_pattern}" }}),
    path: "{file_path}",
    module: () => import("file://{abs_str}"),
  }}"#,
            url_pattern = url_pattern,
            file_path = route.file_path,
            abs_str = abs_str,
        ));
    }

    let routes_array = route_entries.join(",\n");

    format!(
        r#"/**
 * Rift Function Router (auto-generated)
 *
 * {count} route(s) detected.
 */

const routes = [
{routes_array}
];

function resolveHandler(mod) {{
  const d = mod.default;
  if (d && typeof d.fetch === "function") return d.fetch.bind(d);
  if (typeof d === "function") return d;
  if (typeof mod.fetch === "function") return mod.fetch;
  if (typeof mod.handler === "function") return mod.handler;
  return null;
}}

export default {{
  async fetch(req) {{
    const url = new URL(req.url);

    for (const route of routes) {{
      const match = route.pattern.exec(url);
      if (match) {{
        const groups = match.pathname.groups;
        const headers = new Headers(req.headers);
        for (const [k, v] of Object.entries(groups)) {{
          if (v !== undefined) headers.set(`x-rift-param-${{k}}`, v);
        }}

        try {{
          const mod = await route.module();
          const handler = resolveHandler(mod);
          if (!handler) {{
            return new Response(
              JSON.stringify({{ error: `No handler in ${{route.path}}` }}),
              {{ status: 500, headers: {{ "content-type": "application/json" }} }},
            );
          }}
          return await handler(new Request(req.url, {{
            method: req.method,
            headers,
            body: req.body,
          }}));
        }} catch (e) {{
          console.error(`[rift-functions] Error in ${{route.path}}: ${{e}}`);
          return new Response(
            JSON.stringify({{ error: "Internal Server Error" }}),
            {{ status: 500, headers: {{ "content-type": "application/json" }} }},
          );
        }}
      }}
    }}

    return new Response(
      JSON.stringify({{ error: "Not Found" }}),
      {{ status: 404, headers: {{ "content-type": "application/json" }} }},
    );
  }},
}};
"#,
        count = routes.len(),
        routes_array = routes_array,
    )
}
