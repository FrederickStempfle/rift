use std::path::Path;

use tokio::fs;

use crate::error::AppError;

const DENO_STATIC_ENTRY: &str = r#"import { serveDir } from "jsr:@std/http/file-server";

const port = parseInt(Deno.env.get("PORT") ?? "3000");

Deno.serve({ port, hostname: "0.0.0.0" }, async (req) => {
  const url = new URL(req.url);
  let resp = await serveDir(req, { fsRoot: ".", quiet: true });

  // Handle Vite base path: if /base/assets/foo.css 404s, try /assets/foo.css
  if (resp.status === 404) {
    const segments = url.pathname.split("/").filter(Boolean);
    if (segments.length > 1) {
      const stripped = "/" + segments.slice(1).join("/");
      const strippedReq = new Request(new URL(stripped, req.url), req);
      const strippedResp = await serveDir(strippedReq, { fsRoot: ".", quiet: true });
      if (strippedResp.status !== 404) {
        return strippedResp;
      }
    }
  }

  // SPA fallback: serve index.html for non-file routes
  if (resp.status === 404) {
    const lastSegment = url.pathname.split("/").pop() ?? "";
    if (!lastSegment.includes(".")) {
      return serveDir(new Request(new URL("/index.html", req.url)), {
        fsRoot: ".",
        quiet: true,
      });
    }
  }

  return resp;
});
"#;

/// Write a Deno static file server entry point (`_entry.ts`) into `output_dir`.
pub async fn generate_deno_entry(output_dir: &Path) -> Result<(), AppError> {
    let entry_path = output_dir.join("_entry.ts");
    fs::write(&entry_path, DENO_STATIC_ENTRY)
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "failed to write _entry.ts to {}: {e}",
                entry_path.display()
            ))
        })
}

/// Generate a pool-compatible entry wrapper for SSR frameworks.
///
/// For static sites, no wrapper is needed — the existing `_entry.ts` already
/// exports a Deno.serve handler compatible with the worker loader.
///
/// For SSR frameworks (Next.js, Nuxt, Astro, SvelteKit, Remix), we generate
/// a wrapper that starts the framework's server internally and proxies to it.
pub async fn generate_pool_entry(
    kind: &crate::runtime::RuntimeKind,
    deploy_dir: &Path,
    wrapper_dir: &Path,
) -> Result<std::path::PathBuf, AppError> {
    use crate::runtime::RuntimeKind;

    let entry_path = deploy_dir.join("_rift_pool_entry.ts");

    match kind {
        RuntimeKind::StaticDeno { dir } => {
            // Static sites use the existing _entry.ts directly — no wrapper needed.
            Ok(dir.join("_entry.ts"))
        }
        RuntimeKind::Functions { dir } => {
            // Functions use the dispatcher _entry.ts directly — no wrapper needed.
            Ok(dir.join("_entry.ts"))
        }
        RuntimeKind::Combined { entry, .. } => {
            // Combined entries are already self-contained Deno.serve() scripts.
            Ok(entry.clone())
        }
        RuntimeKind::NextDeno { dir } => {
            let standalone_dir = dir.join(".next/standalone");
            let (server_js, server_dir) = if standalone_dir.join("server.js").exists() {
                (
                    "server.js".to_string(),
                    standalone_dir.to_string_lossy().to_string(),
                )
            } else {
                // Find server.js in monorepo nested structure
                find_server_js_in_standalone(&standalone_dir).unwrap_or_else(|| {
                    (
                        "server.js".to_string(),
                        standalone_dir.to_string_lossy().to_string(),
                    )
                })
            };

            // Read the wrapper template and inject paths
            let wrapper_src = wrapper_dir.join("wrappers/next_wrapper.ts");
            let mut content = fs::read_to_string(&wrapper_src)
                .await
                .map_err(|e| AppError::Internal(format!("failed to read next_wrapper.ts: {e}")))?;

            // Replace the env var defaults with actual values
            content = content.replace(
                r#"Deno.env.get("RIFT_NEXT_SERVER_JS") ?? "./server.js""#,
                &format!(r#""{server_js}""#),
            );
            content = content.replace(
                r#"Deno.env.get("RIFT_NEXT_SERVER_DIR") ?? ".""#,
                &format!(r#""{server_dir}""#),
            );

            fs::write(&entry_path, content)
                .await
                .map_err(|e| AppError::Internal(format!("failed to write pool entry: {e}")))?;

            Ok(entry_path)
        }
        RuntimeKind::NodeServer { dir, entry } => {
            let is_remix = dir.join("node_modules/.bin/remix-serve").exists()
                && entry.to_string_lossy().contains("build/server");

            let wrapper_src = wrapper_dir.join("wrappers/node_wrapper.ts");
            let mut content = fs::read_to_string(&wrapper_src)
                .await
                .map_err(|e| AppError::Internal(format!("failed to read node_wrapper.ts: {e}")))?;

            let entry_str = entry.to_string_lossy().to_string();
            let dir_str = dir.to_string_lossy().to_string();

            content = content.replace(
                r#"Deno.env.get("RIFT_NODE_ENTRY") ?? "./index.js""#,
                &format!(r#""{entry_str}""#),
            );
            content = content.replace(
                r#"Deno.env.get("RIFT_NODE_SERVER_DIR") ?? ".""#,
                &format!(r#""{dir_str}""#),
            );
            content = content.replace(
                r#"Deno.env.get("RIFT_IS_REMIX") === "true""#,
                if is_remix { "true" } else { "false" },
            );

            fs::write(&entry_path, content)
                .await
                .map_err(|e| AppError::Internal(format!("failed to write pool entry: {e}")))?;

            Ok(entry_path)
        }
    }
}

/// Find server.js path and directory within a standalone dir.
fn find_server_js_in_standalone(standalone_dir: &Path) -> Option<(String, String)> {
    let Ok(entries) = std::fs::read_dir(standalone_dir) else {
        return None;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let d1 = entry.path();
        if d1.join("server.js").exists() {
            return Some(("server.js".to_string(), d1.to_string_lossy().to_string()));
        }
        let Ok(sub) = std::fs::read_dir(&d1) else {
            continue;
        };
        for sub_entry in sub.flatten() {
            if !sub_entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let d2 = sub_entry.path();
            if d2.join("server.js").exists() {
                return Some(("server.js".to_string(), d2.to_string_lossy().to_string()));
            }
        }
    }
    None
}
