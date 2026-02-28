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
};

use self::{
    bundler::generate_deno_entry,
    detect::{detect_build_plan, detect_output_dir, BuildOutput, PackageManager},
    pipeline::{elapsed_ms, read_git_metadata, run_command_and_log, run_command_and_log_with_env},
};

#[derive(Clone, Debug)]
pub struct BuildManager {
    pool: sqlx::PgPool,
    config: Arc<Config>,
    runtime_manager: RuntimeManager,
    build_root: PathBuf,
    deploy_root: PathBuf,
    concurrency: Arc<Semaphore>,
}

impl BuildManager {
    pub fn new(
        pool: sqlx::PgPool,
        config: Arc<Config>,
        runtime_manager: RuntimeManager,
        build_root: PathBuf,
        deploy_root: PathBuf,
    ) -> Self {
        Self {
            pool,
            config,
            runtime_manager,
            build_root,
            deploy_root,
            concurrency: Arc::new(Semaphore::new(1)),
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
                project
                    .repo_url
                    .replace("https://github.com/", &format!("https://x-access-token:{token}@github.com/"))
            }
            _ => project.repo_url.clone(),
        };

        run_command_and_log(
            &self.pool,
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

        let plan = detect_build_plan(&project, &workspace_dir)?;
        deployments::insert_log(
            &self.pool,
            deployment_id,
            "info",
            &format!("Detected framework: {}", plan.framework),
            "build",
        )
        .await?;

        // Fetch env vars early so they're available during install & build
        let user_env_vars = env_vars::get_decrypted_env_vars(
            &self.pool,
            project.id,
            &self.config.master_key,
        )
        .await
        .unwrap_or_default();

        if !user_env_vars.is_empty() {
            deployments::insert_log(
                &self.pool,
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
            deployment_id,
            "build",
            &workspace_dir,
            &plan.install_command,
            &user_env_vars,
        )
        .await
        {
            deployments::insert_log(
                &self.pool,
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
            let _ = run_command_and_log(&self.pool, deployment_id, "build", &workspace_dir, cmd)
                .await;
        }

        // For Next.js: inject standalone config before building
        if matches!(plan.output, BuildOutput::Next) {
            self.inject_next_standalone_config(&workspace_dir, deployment_id)
                .await?;
        }

        if let Err(error) = run_command_and_log_with_env(
            &self.pool,
            deployment_id,
            "build",
            &workspace_dir,
            &plan.build_command,
            &user_env_vars,
        )
        .await
        {
            deployments::insert_log(
                &self.pool,
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
            BuildOutput::Next => {
                // Verify standalone output exists
                let standalone_server = workspace_dir.join(".next/standalone/server.js");
                if !standalone_server.exists() {
                    // The build may not have produced standalone output if
                    // next.config was already set but the build failed silently.
                    deployments::insert_log(
                        &self.pool,
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

                // Copy static assets into standalone dir (Next.js requires this)
                let standalone_dir = workspace_dir.join(".next/standalone");
                let static_src = workspace_dir.join(".next/static");
                let static_dst = standalone_dir.join(".next/static");
                if static_src.exists() {
                    copy_dir_recursive(&static_src, &static_dst).await?;
                }
                let public_src = workspace_dir.join("public");
                let public_dst = standalone_dir.join("public");
                if public_src.exists() {
                    copy_dir_recursive(&public_src, &public_dst).await?;
                }

                RuntimeKind::NextDeno {
                    dir: workspace_dir.clone(),
                }
            }
            BuildOutput::Static { .. } => {
                let detected_dir = detect_output_dir(&project, &workspace_dir);
                let output_dir = workspace_dir.join(&detected_dir);
                deployments::insert_log(
                    &self.pool,
                    deployment_id,
                    "info",
                    &format!("Detected output directory: {detected_dir}"),
                    "build",
                )
                .await?;
                if !output_dir.exists() {
                    deployments::insert_log(
                        &self.pool,
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
                deployments::insert_log(
                    &self.pool,
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

        deployments::mark_ready(&self.pool, deployment_id, &url, port, elapsed_ms(started_at))
            .await?;
        deployments::insert_log(
            &self.pool,
            deployment_id,
            "info",
            &format!("Deployment ready on port {port}"),
            "runtime",
        )
        .await?;
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
            None => return Ok(()), // No config file — Next.js will use defaults
        };

        let content = fs::read_to_string(&config_path).await.map_err(|e| {
            AppError::Internal(format!("failed to read {}: {e}", config_path.display()))
        })?;

        if content.contains("output") {
            // Already has an output field — don't overwrite user's config
            return Ok(());
        }

        // Find the config object: look for `= {` pattern (e.g. `const nextConfig = {`)
        // which indicates an object literal, not an import `{ ... }`.
        let injected = if let Some(eq_pos) = content.find("= {") {
            let brace_pos = eq_pos + 2; // position of `{`
            let (before, after) = content.split_at(brace_pos + 1);
            format!("{before}\n  output: \"standalone\",{after}")
        } else {
            // Fallback: couldn't find config object, skip injection
            return Ok(());
        };

        fs::write(&config_path, &injected).await.map_err(|e| {
            AppError::Internal(format!("failed to write {}: {e}", config_path.display()))
        })?;

        deployments::insert_log(
            &self.pool,
            deployment_id,
            "info",
            "Injected output: \"standalone\" into next.config",
            "build",
        )
        .await?;

        Ok(())
    }
}

/// Recursively copy a directory tree.
async fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), AppError> {
    fs::create_dir_all(dst).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to create dir {}: {e}",
            dst.display()
        ))
    })?;

    let mut entries = fs::read_dir(src).await.map_err(|e| {
        AppError::Internal(format!("failed to read dir {}: {e}", src.display()))
    })?;

    while let Some(entry) = entries.next_entry().await.map_err(|e| {
        AppError::Internal(format!("failed to read entry in {}: {e}", src.display()))
    })? {
        let file_type = entry.file_type().await.map_err(|e| {
            AppError::Internal(format!("failed to get file type: {e}"))
        })?;
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
