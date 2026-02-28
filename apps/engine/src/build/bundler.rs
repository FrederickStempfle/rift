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
