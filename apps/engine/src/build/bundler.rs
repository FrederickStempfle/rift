use std::path::Path;

use tokio::fs;

use crate::error::AppError;

const DENO_STATIC_ENTRY: &str = r#"import { serveDir } from "jsr:@std/http/file-server";

const port = parseInt(Deno.env.get("PORT") ?? "3000");

Deno.serve({ port, hostname: "0.0.0.0" }, async (req) => {
  const resp = await serveDir(req, { fsRoot: ".", quiet: true });
  if (resp.status === 404) {
    const url = new URL(req.url);
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
