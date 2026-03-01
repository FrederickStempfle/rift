pub mod bundler;
pub mod detect;
pub mod pipeline;

use std::{path::PathBuf, sync::Arc, time::Instant};

use chrono::Utc;
use tokio::{fs, sync::Semaphore};
use uuid::Uuid;

use crate::{
    config::Config,
    db::{deployments, env_vars, models::Project, users},
    error::AppError,
    runtime::{RuntimeKind, RuntimeLaunchSpec, RuntimeManager},
    ws::LogBroadcaster,
};

use self::{
    bundler::generate_deno_entry,
    detect::{detect_build_plan, detect_output_dir, BuildOutput, PackageManager},
    pipeline::{
        elapsed_ms, insert_and_broadcast_log, read_git_metadata, run_command_and_log,
        run_command_and_log_with_env,
    },
};

#[derive(Clone, Debug)]
pub struct BuildManager {
    pool: sqlx::PgPool,
    config: Arc<Config>,
    runtime_manager: RuntimeManager,
    build_root: PathBuf,
    deploy_root: PathBuf,
    concurrency: Arc<Semaphore>,
    log_broadcaster: LogBroadcaster,
}

impl BuildManager {
    pub fn new(
        pool: sqlx::PgPool,
        config: Arc<Config>,
        runtime_manager: RuntimeManager,
        build_root: PathBuf,
        deploy_root: PathBuf,
        log_broadcaster: LogBroadcaster,
    ) -> Self {
        Self {
            pool,
            config,
            runtime_manager,
            build_root,
            deploy_root,
            concurrency: Arc::new(Semaphore::new(1)),
            log_broadcaster,
        }
    }

    pub async fn enqueue_project_build(
        &self,
        project: Project,
    ) -> Result<crate::db::models::Deployment, AppError> {
        let deployment = deployments::create_deployment(
            &self.pool,
            deployments::NewDeployment {
                project_id: project.id,
                branch: project.branch.clone(),
                commit_sha: "pending".to_owned(),
                commit_message: Some("Queued build".to_owned()),
            },
        )
        .await?;

        let manager = self.clone();
        tokio::spawn(async move {
            if let Err(error) = manager.run_build(project, deployment.id).await {
                tracing::error!(deployment_id = %deployment.id, error = %error, "build failed");
            }
        });

        Ok(deployment)
    }

    async fn run_build(&self, project: Project, deployment_id: Uuid) -> Result<(), AppError> {
        let _permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| AppError::Internal(format!("build queue failed: {error}")))?;

        fs::create_dir_all(&self.build_root)
            .await
            .map_err(|error| AppError::Internal(format!("failed to create build root: {error}")))?;
        fs::create_dir_all(&self.deploy_root)
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to create deploy root: {error}"))
            })?;

        let started_at = Instant::now();
        deployments::set_started_at(&self.pool, deployment_id, Utc::now()).await?;
        deployments::update_status(&self.pool, deployment_id, "cloning").await?;

        let workspace_dir = self.deploy_root.join(deployment_id.to_string());
        if workspace_dir.exists() {
            fs::remove_dir_all(&workspace_dir).await.map_err(|error| {
                AppError::Internal(format!("failed to clear workspace: {error}"))
            })?;
        }

        // Inject GitHub token into clone URL for private repos
        let clone_url = match users::find_user_by_id(&self.pool, project.user_id).await? {
            Some(user) if user.github_token.is_some() => {
                let token = user.github_token.unwrap();
                project.repo_url.replace(
                    "https://github.com/",
                    &format!("https://x-access-token:{token}@github.com/"),
                )
            }
            _ => project.repo_url.clone(),
        };

        run_command_and_log(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "build",
            &self.build_root,
            &format!(
                "git clone --depth 1 --branch '{}' '{}' '{}'",
                project.branch,
                clone_url,
                workspace_dir.display()
            ),
        )
        .await
        .inspect_err(|_| {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ =
                    deployments::mark_failed(&pool, deployment_id, Some(elapsed_ms(started_at)))
                        .await;
            });
        })?;

        let (sha, message) = read_git_metadata(&workspace_dir).await?;
        deployments::update_source_metadata(&self.pool, deployment_id, &sha, message.as_deref())
            .await?;

        let plan = match detect_build_plan(&project, &workspace_dir) {
            Ok(plan) => plan,
            Err(error) => {
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "error",
                    &error.to_string(),
                    "build",
                )
                .await?;
                deployments::mark_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                    .await?;
                return Err(error);
            }
        };
        insert_and_broadcast_log(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "info",
            &format!("Detected framework: {}", plan.framework),
            "build",
        )
        .await?;

        // Fetch env vars early so they're available during install & build
        let user_env_vars =
            env_vars::get_decrypted_env_vars(&self.pool, project.id, &self.config.master_key)
                .await
                .unwrap_or_default();

        if !user_env_vars.is_empty() {
            insert_and_broadcast_log(
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
                "info",
                &format!("Injecting {} environment variable(s)", user_env_vars.len()),
                "build",
            )
            .await?;
        }

        deployments::update_status(&self.pool, deployment_id, "building").await?;
        if let Err(error) = run_command_and_log_with_env(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "build",
            &workspace_dir,
            &plan.install_command,
            &user_env_vars,
        )
        .await
        {
            insert_and_broadcast_log(
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
                "error",
                &error.to_string(),
                "build",
            )
            .await?;
            deployments::mark_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                .await?;
            return Err(error);
        }

        // Clean package manager cache to free tmpfs space before build
        let cache_clean = match plan.package_manager {
            PackageManager::Yarn => Some("yarn cache clean"),
            PackageManager::Npm => Some("npm cache clean --force"),
            PackageManager::Pnpm => Some("pnpm store prune"),
            PackageManager::Bun => None,
        };
        if let Some(cmd) = cache_clean {
            let _ = run_command_and_log(
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
                "build",
                &workspace_dir,
                cmd,
            )
            .await;
        }

        // For Next.js: inject standalone config before building
        if matches!(plan.output, BuildOutput::Next) {
            self.inject_next_standalone_config(&workspace_dir, deployment_id)
                .await?;
            // Also inject in workspace packages for monorepos
            for container in ["apps", "packages", "sites"] {
                let container_dir = workspace_dir.join(container);
                if container_dir.is_dir() {
                    if let Ok(entries) = fs::read_dir(&container_dir).await {
                        let mut entries = entries;
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            if entry
                                .file_type()
                                .await
                                .map(|ft| ft.is_dir())
                                .unwrap_or(false)
                            {
                                let _ = self
                                    .inject_next_standalone_config(&entry.path(), deployment_id)
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        if let Err(error) = run_command_and_log_with_env(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "build",
            &workspace_dir,
            &plan.build_command,
            &user_env_vars,
        )
        .await
        {
            insert_and_broadcast_log(
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
                "error",
                &error.to_string(),
                "build",
            )
            .await?;
            deployments::mark_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                .await?;
            return Err(error);
        }

        deployments::update_status(&self.pool, deployment_id, "deploying").await?;
        let runtime_kind = match plan.output {
            BuildOutput::Nuxt => {
                // Find the Nuxt output directory (.output/server/index.mjs)
                let nuxt_app_dir = find_nuxt_output(&workspace_dir).await;

                let nuxt_app_dir = match nuxt_app_dir {
                    Some(dir) => dir,
                    None => {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "error",
                            "Nuxt server output not found (.output/server/index.mjs). Build may have failed.",
                            "build",
                        )
                        .await?;
                        deployments::mark_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal(
                            "Nuxt server output not found".into(),
                        ));
                    }
                };

                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    "Detected Nuxt server output (.output/server/index.mjs)",
                    "build",
                )
                .await?;

                let entry = nuxt_app_dir.join(".output/server/index.mjs");
                RuntimeKind::NodeServer {
                    dir: nuxt_app_dir,
                    entry,
                }
            }
            BuildOutput::Next => {
                // Find the directory containing .next/standalone/server.js
                // Could be at root or inside a workspace package (monorepo)
                let next_app_dir = find_next_standalone(&workspace_dir).await;

                let next_app_dir = match next_app_dir {
                    Some(dir) => dir,
                    None => {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "error",
                            "Next.js standalone output not found (.next/standalone/server.js). Ensure `output: \"standalone\"` is in next.config.",
                            "build",
                        )
                        .await?;
                        deployments::mark_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal(
                            "Next.js standalone output not found".into(),
                        ));
                    }
                };

                // Copy static assets into the directory where server.js lives.
                // In monorepos, Next.js nests server.js inside standalone/
                // (e.g. .next/standalone/apps/web/server.js), so assets must
                // go next to it, not in the standalone root.
                let standalone_dir = next_app_dir.join(".next/standalone");
                let server_dir = if standalone_dir.join("server.js").exists() {
                    standalone_dir.clone()
                } else {
                    // Search for server.js in subdirectories
                    find_server_js_dir(&standalone_dir)
                        .unwrap_or_else(|| standalone_dir.clone())
                };

                let static_src = next_app_dir.join(".next/static");
                let static_dst = server_dir.join(".next/static");
                if static_src.exists() {
                    copy_dir_recursive(&static_src, &static_dst).await?;
                }
                let public_src = next_app_dir.join("public");
                let public_dst = server_dir.join("public");
                if public_src.exists() {
                    copy_dir_recursive(&public_src, &public_dst).await?;
                }

                RuntimeKind::NextDeno { dir: next_app_dir }
            }
            BuildOutput::AstroSSR => {
                let app_dir =
                    find_ssr_entry(&workspace_dir, "dist/server/entry.mjs").await;
                let app_dir = match app_dir {
                    Some(dir) => dir,
                    None => {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "error",
                            "Astro SSR output not found (dist/server/entry.mjs). Ensure @astrojs/node adapter is configured in standalone mode.",
                            "build",
                        )
                        .await?;
                        deployments::mark_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal(
                            "Astro SSR output not found".into(),
                        ));
                    }
                };

                let entry = app_dir.join("dist/server/entry.mjs");
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    "Detected Astro SSR output (dist/server/entry.mjs)",
                    "build",
                )
                .await?;

                RuntimeKind::NodeServer { dir: app_dir, entry }
            }
            BuildOutput::SvelteKitSSR => {
                let app_dir =
                    find_ssr_entry(&workspace_dir, "build/index.js").await;
                let app_dir = match app_dir {
                    Some(dir) => dir,
                    None => {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "error",
                            "SvelteKit SSR output not found (build/index.js). Ensure @sveltejs/adapter-node is configured.",
                            "build",
                        )
                        .await?;
                        deployments::mark_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal(
                            "SvelteKit SSR output not found".into(),
                        ));
                    }
                };

                let entry = app_dir.join("build/index.js");
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    "Detected SvelteKit SSR output (build/index.js)",
                    "build",
                )
                .await?;

                RuntimeKind::NodeServer { dir: app_dir, entry }
            }
            BuildOutput::RemixSSR => {
                let app_dir =
                    find_ssr_entry(&workspace_dir, "build/server/index.js").await;
                let app_dir = match app_dir {
                    Some(dir) => dir,
                    None => {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "error",
                            "Remix server output not found (build/server/index.js). Build may have failed.",
                            "build",
                        )
                        .await?;
                        deployments::mark_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal(
                            "Remix server output not found".into(),
                        ));
                    }
                };

                let entry = app_dir.join("build/server/index.js");
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    "Detected Remix server output (build/server/index.js)",
                    "build",
                )
                .await?;

                RuntimeKind::NodeServer { dir: app_dir, entry }
            }
            BuildOutput::Static { .. } => {
                let detected_dir = detect_output_dir(&project, &workspace_dir);
                let output_dir = workspace_dir.join(&detected_dir);
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    &format!("Detected output directory: {detected_dir}"),
                    "build",
                )
                .await?;
                if !output_dir.exists() {
                    insert_and_broadcast_log(
                        &self.pool,
                        &self.log_broadcaster,
                        deployment_id,
                        "error",
                        "Build output directory not found",
                        "build",
                    )
                    .await?;
                    deployments::mark_failed(
                        &self.pool,
                        deployment_id,
                        Some(elapsed_ms(started_at)),
                    )
                    .await?;
                    return Err(AppError::Internal(
                        "build output directory not found".into(),
                    ));
                }

                // Generate Deno static file server entry point
                generate_deno_entry(&output_dir).await.map_err(|e| {
                    AppError::Internal(format!("failed to generate Deno entry: {e}"))
                })?;

                RuntimeKind::StaticDeno { dir: output_dir }
            }
        };

        let (url, port) = match self
            .runtime_manager
            .deploy(RuntimeLaunchSpec {
                project_id: project.id,
                deployment_id,
                kind: runtime_kind,
                env_vars: user_env_vars,
            })
            .await
        {
            Ok(result) => result,
            Err(error) => {
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "error",
                    &error.to_string(),
                    "runtime",
                )
                .await?;
                deployments::mark_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                    .await?;
                return Err(error);
            }
        };

        deployments::mark_ready(
            &self.pool,
            deployment_id,
            &url,
            port,
            elapsed_ms(started_at),
        )
        .await?;
        insert_and_broadcast_log(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "info",
            &format!("Deployment ready on port {port}"),
            "runtime",
        )
        .await?;

        // Clean up old deployments for this project
        let deploy_root = self.deploy_root.clone();
        let pool = self.pool.clone();
        tokio::spawn(async move {
            if let Ok(old) =
                deployments::list_old_ready_deployments(&pool, project.id, deployment_id).await
            {
                for old_deployment in old {
                    // Mark as superseded
                    let _ = deployments::update_status(&pool, old_deployment.id, "cancelled").await;
                    // Remove workspace directory
                    let old_dir = deploy_root.join(old_deployment.id.to_string());
                    if old_dir.exists() {
                        let _ = fs::remove_dir_all(&old_dir).await;
                        tracing::debug!(
                            deployment_id = %old_deployment.id,
                            "cleaned up old deployment workspace"
                        );
                    }
                }
            }
        });

        Ok(())
    }

    /// Inject `output: "standalone"` into next.config.{js,mjs,ts} if not
    /// already present. This is required for Deno to run the Next.js app.
    async fn inject_next_standalone_config(
        &self,
        workspace_dir: &std::path::Path,
        deployment_id: Uuid,
    ) -> Result<(), AppError> {
        let config_files = ["next.config.ts", "next.config.mjs", "next.config.js"];
        let mut found = None;
        for name in &config_files {
            let path = workspace_dir.join(name);
            if path.exists() {
                found = Some(path);
                break;
            }
        }

        let config_path = match found {
            Some(p) => p,
            None => {
                // No config file — create a minimal one with standalone output
                let new_config = workspace_dir.join("next.config.mjs");
                fs::write(&new_config, "export default { output: \"standalone\" };\n")
                    .await
                    .map_err(|e| {
                        AppError::Internal(format!(
                            "failed to create {}: {e}",
                            new_config.display()
                        ))
                    })?;
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    "Created next.config.mjs with output: \"standalone\"",
                    "build",
                )
                .await?;
                return Ok(());
            }
        };

        let content = fs::read_to_string(&config_path).await.map_err(|e| {
            AppError::Internal(format!("failed to read {}: {e}", config_path.display()))
        })?;

        // Check if standalone output is already configured (be specific to avoid
        // matching outputFileTracingRoot, outputFileTracing, comments, etc.)
        if content.contains("output:") || content.contains("output =") {
            return Ok(());
        }

        // Find the config object and inject `output: "standalone"`.
        // Handles common patterns:
        //   const nextConfig = { ... }
        //   module.exports = { ... }
        //   export default { ... }
        let injected = if let Some(eq_pos) = content.find("= {") {
            let brace_pos = eq_pos + 2;
            let (before, after) = content.split_at(brace_pos + 1);
            format!("{before}\n  output: \"standalone\",{after}")
        } else if let Some(def_pos) = content.find("default {") {
            let brace_pos = def_pos + 7; // position of `{`
            let (before, after) = content.split_at(brace_pos + 1);
            format!("{before}\n  output: \"standalone\",{after}")
        } else {
            // Couldn't parse config format, skip injection
            return Ok(());
        };

        fs::write(&config_path, &injected).await.map_err(|e| {
            AppError::Internal(format!("failed to write {}: {e}", config_path.display()))
        })?;

        insert_and_broadcast_log(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "info",
            "Injected output: \"standalone\" into next.config",
            "build",
        )
        .await?;

        Ok(())
    }
}

/// Find the app directory that has a `.next/standalone/` output.
/// Checks the workspace root first, then scans workspace packages (apps/*, packages/*).
/// The actual `server.js` location inside standalone/ is resolved at spawn time
/// by `process::find_server_js_recursive`.
async fn find_next_standalone(workspace_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    // Check root
    if workspace_dir.join(".next/standalone").exists() {
        return Some(workspace_dir.to_path_buf());
    }

    // Scan monorepo package directories
    for container in ["apps", "packages", "sites"] {
        let container_dir = workspace_dir.join(container);
        if !container_dir.exists() {
            continue;
        }
        let Ok(mut entries) = fs::read_dir(&container_dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                let candidate = entry.path();
                if candidate.join(".next/standalone").exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

/// Find the directory containing `server.js` inside a `.next/standalone/` dir.
/// In monorepos this is a nested path like `standalone/apps/web/`.
fn find_server_js_dir(standalone_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(standalone_dir) else {
        return None;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let d1 = entry.path();
        if d1.join("server.js").exists() {
            return Some(d1);
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
                return Some(d2);
            }
        }
    }
    None
}

/// Find an SSR entry file relative to workspace root or monorepo packages.
/// Returns the app directory (not the entry file itself).
///
/// Checks workspace root first, then scans `apps/*/` and `packages/*/`.
async fn find_ssr_entry(
    workspace_dir: &std::path::Path,
    relative_entry: &str,
) -> Option<std::path::PathBuf> {
    // Check root
    if workspace_dir.join(relative_entry).exists() {
        return Some(workspace_dir.to_path_buf());
    }

    // Scan monorepo package directories
    for container in ["apps", "packages", "sites"] {
        let container_dir = workspace_dir.join(container);
        if !container_dir.exists() {
            continue;
        }
        let Ok(mut entries) = fs::read_dir(&container_dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                let candidate = entry.path();
                if candidate.join(relative_entry).exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

/// Find the directory containing `.output/server/index.mjs` (Nuxt output).
async fn find_nuxt_output(workspace_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    find_ssr_entry(workspace_dir, ".output/server/index.mjs").await
}

/// Recursively copy a directory tree.
async fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), AppError> {
    fs::create_dir_all(dst)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create dir {}: {e}", dst.display())))?;

    let mut entries = fs::read_dir(src)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read dir {}: {e}", src.display())))?;

    while let Some(entry) = entries.next_entry().await.map_err(|e| {
        AppError::Internal(format!("failed to read entry in {}: {e}", src.display()))
    })? {
        let file_type = entry
            .file_type()
            .await
            .map_err(|e| AppError::Internal(format!("failed to get file type: {e}")))?;
        let dest_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &dest_path)).await?;
        } else {
            fs::copy(entry.path(), &dest_path).await.map_err(|e| {
                AppError::Internal(format!(
                    "failed to copy {} to {}: {e}",
                    entry.path().display(),
                    dest_path.display()
                ))
            })?;
        }
    }

    Ok(())
}
