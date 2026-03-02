pub mod bundler;
pub mod detect;
pub mod functions;
pub mod pipeline;

use std::{path::PathBuf, sync::Arc, time::Instant};

use chrono::Utc;
use tokio::{fs, sync::Semaphore};
use uuid::Uuid;

use crate::{
    config::Config,
    db::{deployments, env_vars, models::Project, users},
    error::AppError,
    lifecycle::{
        state_machine::DeploymentState,
        transition::{transition, transition_to_failed, transition_to_ready},
    },
    runtime::{
        backend::RuntimeBackend,
        policy::{self, BuildPolicy},
        RuntimeKind, RuntimeLaunchSpec,
    },
    validation,
    ws::LogBroadcaster,
};

use self::{
    bundler::{generate_deno_entry, generate_pool_entry},
    detect::{detect_build_plan, detect_output_dir, BuildOutput, PackageManager},
    functions::build_function_bundle,
    pipeline::{
        elapsed_ms, insert_and_broadcast_log, read_git_metadata, run_argv_and_log,
        run_argv_and_log_with_env, run_command_and_log_with_env, split_command_argv,
    },
};

#[derive(Clone)]
pub struct BuildManager {
    pool: sqlx::PgPool,
    config: Arc<Config>,
    runtime_backend: Arc<dyn RuntimeBackend>,
    build_root: PathBuf,
    deploy_root: PathBuf,
    concurrency: Arc<Semaphore>,
    log_broadcaster: LogBroadcaster,
    /// Build-time resource policy (distinct from runtime policy).
    build_policy: BuildPolicy,
    #[cfg(feature = "v8-isolate")]
    isolate_pool: Option<crate::runtime::isolate::IsolatePool>,
}

impl std::fmt::Debug for BuildManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildManager")
            .field("build_root", &self.build_root)
            .field("deploy_root", &self.deploy_root)
            .finish()
    }
}

impl BuildManager {
    pub fn new(
        pool: sqlx::PgPool,
        config: Arc<Config>,
        runtime_backend: Arc<dyn RuntimeBackend>,
        build_root: PathBuf,
        deploy_root: PathBuf,
        log_broadcaster: LogBroadcaster,
        #[cfg(feature = "v8-isolate")] isolate_pool: Option<crate::runtime::isolate::IsolatePool>,
    ) -> Self {
        let max_concurrent = config.build_concurrency.max(1);
        let build_policy = policy::resolve_build_policy(&config, None);
        tracing::info!(
            concurrency = max_concurrent,
            cache_dir = %config.build_cache_dir,
            build_timeout_secs = build_policy.build_timeout_secs,
            build_memory_mb = build_policy.memory_max_bytes / (1024 * 1024),
            "build manager initialized"
        );
        Self {
            pool,
            config,
            runtime_backend,
            build_root,
            deploy_root,
            concurrency: Arc::new(Semaphore::new(max_concurrent)),
            log_broadcaster,
            build_policy,
            #[cfg(feature = "v8-isolate")]
            isolate_pool,
        }
    }

    #[tracing::instrument(skip(self, project), fields(project_id = %project.id))]
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
        let build_timeout = std::time::Duration::from_secs(manager.build_policy.build_timeout_secs);
        tokio::spawn(async move {
            let result =
                tokio::time::timeout(build_timeout, manager.run_build(project, deployment.id))
                    .await;
            match result {
                Ok(Err(error)) => {
                    tracing::error!(deployment_id = %deployment.id, error = %error, "build failed");
                    crate::metrics::DEPLOY_OUTCOME
                        .with_label_values(&["failed"])
                        .inc();
                }
                Err(_) => {
                    tracing::error!(
                        deployment_id = %deployment.id,
                        timeout_secs = build_timeout.as_secs(),
                        "build timed out"
                    );
                    crate::metrics::DEPLOY_OUTCOME
                        .with_label_values(&["timeout"])
                        .inc();
                    let _ = transition_to_failed(&manager.pool, deployment.id, None).await;
                }
                Ok(Ok(())) => {}
            }
        });

        Ok(deployment)
    }

    #[tracing::instrument(skip(self, project), fields(project_id = %project.id, %deployment_id))]
    async fn run_build(&self, project: Project, deployment_id: Uuid) -> Result<(), AppError> {
        // Track build queue depth
        crate::metrics::BUILD_QUEUE_DEPTH.inc();
        // Check if we need to wait for a build slot (backpressure logging)
        if self.concurrency.available_permits() == 0 {
            insert_and_broadcast_log(
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
                "info",
                "Build queued — waiting for available build slot",
                "build",
            )
            .await?;
            // Deployment is already in queued state from creation.
        }

        let _permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| AppError::Internal(format!("build queue failed: {error}")))?;
        crate::metrics::BUILD_QUEUE_DEPTH.dec();

        fs::create_dir_all(&self.build_root)
            .await
            .map_err(|error| AppError::Internal(format!("failed to create build root: {error}")))?;
        fs::create_dir_all(&self.deploy_root)
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to create deploy root: {error}"))
            })?;

        let started_at = Instant::now();
        let mut stage_timings: Vec<(&str, u128)> = Vec::new();
        deployments::set_started_at(&self.pool, deployment_id, Utc::now()).await?;
        if !transition(
            &self.pool,
            deployment_id,
            DeploymentState::Queued,
            DeploymentState::Cloning,
        )
        .await?
        {
            tracing::warn!(deployment_id = %deployment_id, "CAS transition queued→cloning failed — aborting build");
            return Ok(());
        }

        let workspace_dir = self.deploy_root.join(deployment_id.to_string());
        if workspace_dir.exists() {
            fs::remove_dir_all(&workspace_dir).await.map_err(|error| {
                AppError::Internal(format!("failed to clear workspace: {error}"))
            })?;
        }

        // Clone with explicit argv to avoid shell injection. If a GitHub token
        // exists, provide it via env-backed credential helper (not in args/logs).
        let mut clone_env: Vec<(String, String)> = Vec::new();
        let mut clone_args = vec![
            "clone".to_owned(),
            "--depth".to_owned(),
            "1".to_owned(),
            "--branch".to_owned(),
            project.branch.clone(),
        ];
        if let Some(user) = users::find_user_by_id(&self.pool, project.user_id).await? {
            if let Some(token) = user.github_token {
                clone_args.push("-c".to_owned());
                clone_args.push("credential.helper=!f() { echo username=x-access-token; echo password=$RIFT_GITHUB_TOKEN; }; f".to_owned());
                clone_env.push(("RIFT_GITHUB_TOKEN".to_owned(), token));
            }
        }
        clone_args.push(project.repo_url.clone());
        clone_args.push(workspace_dir.to_string_lossy().to_string());

        run_argv_and_log_with_env(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "build",
            &self.build_root,
            "git",
            &clone_args,
            &clone_env,
        )
        .await
        .inspect_err(|_| {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ =
                    transition_to_failed(&pool, deployment_id, Some(elapsed_ms(started_at))).await;
            });
        })?;
        stage_timings.push(("clone", started_at.elapsed().as_millis()));

        let (sha, message) = read_git_metadata(&workspace_dir).await?;
        deployments::update_source_metadata(&self.pool, deployment_id, &sha, message.as_deref())
            .await?;

        let install_is_custom = project.install_command.is_some();
        let build_is_custom = project.build_command.is_some();

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
                transition_to_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                    .await?;
                return Err(error);
            }
        };
        stage_timings.push(("detect", started_at.elapsed().as_millis()));
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

        if !transition(
            &self.pool,
            deployment_id,
            DeploymentState::Cloning,
            DeploymentState::Building,
        )
        .await?
        {
            tracing::warn!(deployment_id = %deployment_id, "CAS transition cloning→building failed — aborting build");
            return Ok(());
        }

        // Dependency caching strategy:
        //   1. Point the package manager's native cache at a persistent directory
        //   2. Restore cached node_modules if lockfile hash matches
        //   3. Skip install entirely when cache hit + skip flag enabled
        //   4. Otherwise run optimized install (frozen lockfile, --prefer-offline)
        //   5. Save node_modules back to cache after successful install
        let cache_enabled = !self.config.build_cache_dir.is_empty();
        let cache_dir = PathBuf::from(&self.config.build_cache_dir);
        let cache_key = compute_cache_key(&workspace_dir, &plan.package_manager).await;

        // Merge native cache env vars so the package manager stores its index
        // in our persistent cache directory (survives across builds).
        let mut install_env = user_env_vars.clone();
        if cache_enabled {
            install_env.extend(native_cache_env(&cache_dir, &plan.package_manager));
        }

        // Restore cached node_modules
        let cache_restored = if let Some(ref key) = cache_key {
            if cache_enabled {
                match restore_dependency_cache(&cache_dir, key, &workspace_dir).await {
                    Ok(true) => {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "info",
                            "Restored cached dependencies",
                            "build",
                        )
                        .await?;
                        true
                    }
                    _ => false,
                }
            } else {
                false
            }
        } else {
            false
        };

        // Decide whether to run install — only skip if the restored cache
        // actually contains binaries (node_modules/.bin must exist and be non-empty).
        let cache_has_binaries = if cache_restored {
            let bin_dir = workspace_dir.join("node_modules").join(".bin");
            match tokio::fs::read_dir(&bin_dir).await {
                Ok(mut rd) => rd.next_entry().await.ok().flatten().is_some(),
                Err(_) => false,
            }
        } else {
            false
        };
        let skip_install = cache_restored
            && cache_has_binaries
            && self.config.install_skip_on_cache_hit
            && cache_key.is_some();

        if skip_install {
            insert_and_broadcast_log(
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
                "info",
                "Skipping install (lockfile unchanged, cached node_modules restored)",
                "build",
            )
            .await?;
        } else {
            // Use optimized install command (frozen lockfile + offline-first)
            // only for auto-detected commands when a lockfile exists.
            let install_cmd = select_install_command(
                &plan.package_manager,
                &plan.install_command,
                cache_enabled,
                install_is_custom,
                cache_key.is_some(),
            );

            let install_result = if install_is_custom {
                if let Err(error) =
                    validation::validate_custom_command(&install_cmd, "install command")
                {
                    Err(error)
                } else {
                    run_command_and_log_with_env(
                        &self.pool,
                        &self.log_broadcaster,
                        deployment_id,
                        "build",
                        &workspace_dir,
                        &install_cmd,
                        &install_env,
                    )
                    .await
                }
            } else {
                match split_command_argv(&install_cmd) {
                    Ok((program, args)) => {
                        run_argv_and_log_with_env(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "build",
                            &workspace_dir,
                            &program,
                            &args,
                            &install_env,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            };

            if let Err(error) = install_result
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
                transition_to_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                    .await?;
                return Err(error);
            }
        }

        // Save dependency cache after successful install
        if let Some(ref key) = cache_key {
            if cache_enabled {
                if let Err(e) = save_dependency_cache(&cache_dir, key, &workspace_dir).await {
                    tracing::debug!(error = %e, "failed to save dependency cache (non-fatal)");
                }
            }
        }

        stage_timings.push(("install", started_at.elapsed().as_millis()));

        // Clean package manager cache (disabled by default — destroys warm-cache benefit).
        // Enable via RIFT_BUILD_CLEAN_CACHE=true only when tmpfs space is tight.
        if self.config.build_clean_cache {
            let cache_clean = match plan.package_manager {
                PackageManager::Yarn => Some("yarn cache clean"),
                PackageManager::Npm => Some("npm cache clean --force"),
                PackageManager::Pnpm => Some("pnpm store prune"),
                PackageManager::Bun => None,
            };
            if let Some(cmd) = cache_clean {
                if let Ok((program, args)) = split_command_argv(cmd) {
                    let _ = run_argv_and_log(
                        &self.pool,
                        &self.log_broadcaster,
                        deployment_id,
                        "build",
                        &workspace_dir,
                        &program,
                        &args,
                    )
                    .await;
                }
            }
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

        // Restore Next.js build cache for incremental compilation.
        // Keyed by project ID so each project has its own persistent cache.
        if cache_enabled && matches!(plan.output, BuildOutput::Next) {
            let restored =
                restore_next_build_caches(&cache_dir, project.id, &workspace_dir).await;
            if restored {
                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    "Restored Next.js build cache",
                    "build",
                )
                .await?;
            }
        }

        let build_result = if build_is_custom {
            if let Err(error) = validation::validate_custom_command(&plan.build_command, "build command")
            {
                Err(error)
            } else {
                run_command_and_log_with_env(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "build",
                    &workspace_dir,
                    &plan.build_command,
                    &user_env_vars,
                )
                .await
            }
        } else {
            match split_command_argv(&plan.build_command) {
                Ok((program, args)) => {
                    run_argv_and_log_with_env(
                        &self.pool,
                        &self.log_broadcaster,
                        deployment_id,
                        "build",
                        &workspace_dir,
                        &program,
                        &args,
                        &user_env_vars,
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        };

        if let Err(error) = build_result
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
            transition_to_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at))).await?;
            return Err(error);
        }

        stage_timings.push(("build", started_at.elapsed().as_millis()));

        // Save Next.js build cache after a successful build so the next deploy
        // can skip recompiling unchanged modules. Cost is counted in artifact time.
        if cache_enabled && matches!(plan.output, BuildOutput::Next) {
            save_next_build_caches(&cache_dir, project.id, &workspace_dir).await;
        }

        if !transition(
            &self.pool,
            deployment_id,
            DeploymentState::Building,
            DeploymentState::Deploying,
        )
        .await?
        {
            tracing::warn!(deployment_id = %deployment_id, "CAS transition building→deploying failed — aborting build");
            return Ok(());
        }
        let mut runtime_kind = match plan.output {
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
                        transition_to_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal("Nuxt server output not found".into()));
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
                        transition_to_failed(
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
                    find_server_js_dir(&standalone_dir).unwrap_or_else(|| standalone_dir.clone())
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
                let app_dir = find_ssr_entry(&workspace_dir, "dist/server/entry.mjs").await;
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
                        transition_to_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal("Astro SSR output not found".into()));
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

                RuntimeKind::NodeServer {
                    dir: app_dir,
                    entry,
                }
            }
            BuildOutput::SvelteKitSSR => {
                let app_dir = find_ssr_entry(&workspace_dir, "build/index.js").await;
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
                        transition_to_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal("SvelteKit SSR output not found".into()));
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

                RuntimeKind::NodeServer {
                    dir: app_dir,
                    entry,
                }
            }
            BuildOutput::RemixSSR => {
                let app_dir = find_ssr_entry(&workspace_dir, "build/server/index.js").await;
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
                        transition_to_failed(
                            &self.pool,
                            deployment_id,
                            Some(elapsed_ms(started_at)),
                        )
                        .await?;
                        return Err(AppError::Internal("Remix server output not found".into()));
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

                RuntimeKind::NodeServer {
                    dir: app_dir,
                    entry,
                }
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
                    transition_to_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
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
            BuildOutput::Functions => {
                // Functions-only project: bundle each function with esbuild,
                // generate per-function Web Worker wrappers, and write dispatcher entry.
                let output_dir = workspace_dir.join("_rift_functions_output");
                let template_dir = std::path::PathBuf::from(&self.config.worker_loader)
                    .parent()
                    .unwrap_or(std::path::Path::new("/opt/rift/templates"))
                    .to_path_buf();
                let function_routes = build_function_bundle(
                    &workspace_dir,
                    &output_dir,
                    &template_dir,
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                )
                .await?;

                if function_routes.is_empty() {
                    insert_and_broadcast_log(
                        &self.pool,
                        &self.log_broadcaster,
                        deployment_id,
                        "error",
                        "No function files found in rift/functions/",
                        "build",
                    )
                    .await?;
                    transition_to_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                        .await?;
                    return Err(AppError::Internal("no function files found".into()));
                }

                insert_and_broadcast_log(
                    &self.pool,
                    &self.log_broadcaster,
                    deployment_id,
                    "info",
                    &format!(
                        "Bundled {} serverless function route(s): {}",
                        function_routes.len(),
                        function_routes
                            .iter()
                            .map(|r| r.pattern.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    "build",
                )
                .await?;

                RuntimeKind::Functions { dir: output_dir }
            }
        };

        // Bundle serverless functions alongside the framework if rift/functions/ exists.
        // Each function gets its own Web Worker isolate; non-matching requests
        // fall through to the framework handler.
        if functions::has_functions(&workspace_dir)
            && !matches!(plan.output, BuildOutput::Functions)
        {
            let fn_output_dir = workspace_dir.join("_rift_functions_output");
            let template_dir = std::path::PathBuf::from(&self.config.worker_loader)
                .parent()
                .unwrap_or(std::path::Path::new("/opt/rift/templates"))
                .to_path_buf();
            match build_function_bundle(
                &workspace_dir,
                &fn_output_dir,
                &template_dir,
                &self.pool,
                &self.log_broadcaster,
                deployment_id,
            )
            .await
            {
                Ok(routes) if !routes.is_empty() => {
                    // Determine the framework's entry point for the combined dispatcher
                    let framework_entry = match &runtime_kind {
                        RuntimeKind::StaticDeno { dir } => Some(dir.join("_entry.ts")),
                        RuntimeKind::Functions { dir } => Some(dir.join("_entry.ts")),
                        RuntimeKind::Combined { entry, .. } => Some(entry.clone()),
                        RuntimeKind::NextDeno { .. } | RuntimeKind::NodeServer { .. } => {
                            let pool_entry = workspace_dir.join("_rift_pool_entry.ts");
                            if pool_entry.exists() {
                                Some(pool_entry)
                            } else {
                                None
                            }
                        }
                    };

                    if let Some(fw_entry) = framework_entry {
                        match functions::generate_combined_entry(
                            &routes,
                            &fn_output_dir,
                            &fw_entry,
                            &template_dir,
                        )
                        .await
                        {
                            Ok(combined_code) => {
                                let combined_path = fn_output_dir.join("_rift_combined_entry.ts");
                                if let Err(e) = fs::write(&combined_path, combined_code).await {
                                    tracing::warn!(
                                        error = %e,
                                        "failed to write combined entry"
                                    );
                                } else {
                                    // Swap runtime kind to Combined so the runtime
                                    // launches the combined entry instead of the
                                    // framework-only entry.
                                    runtime_kind = RuntimeKind::Combined {
                                        entry: combined_path,
                                        functions_dir: fn_output_dir.clone(),
                                    };

                                    insert_and_broadcast_log(
                                        &self.pool,
                                        &self.log_broadcaster,
                                        deployment_id,
                                        "info",
                                        &format!(
                                            "Bundled {} function route(s) with {} (isolated Workers + combined entry)",
                                            routes.len(),
                                            plan.framework
                                        ),
                                        "build",
                                    )
                                    .await?;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "failed to generate combined entry"
                                );
                            }
                        }
                    } else {
                        insert_and_broadcast_log(
                            &self.pool,
                            &self.log_broadcaster,
                            deployment_id,
                            "info",
                            &format!(
                                "Bundled {} serverless function route(s) alongside {}",
                                routes.len(),
                                plan.framework
                            ),
                            "build",
                        )
                        .await?;
                    }
                }
                Ok(_) => {} // empty, skip
                Err(e) => {
                    tracing::warn!(error = %e, "failed to bundle serverless functions");
                }
            }
        }

        // Generate pool-compatible entry wrapper if in pool mode
        if self.config.runtime_mode == "pool" {
            let wrapper_dir = std::path::PathBuf::from(&self.config.worker_loader)
                .parent()
                .unwrap_or(std::path::Path::new("/opt/rift/templates"))
                .to_path_buf();
            if let Err(e) = generate_pool_entry(&runtime_kind, &workspace_dir, &wrapper_dir).await {
                tracing::warn!(
                    error = %e,
                    "failed to generate pool entry, will use direct entry"
                );
            }
        }

        // Write artifact manifest — records which files are essential for runtime.
        if let Err(e) = write_artifact_manifest(&workspace_dir, &runtime_kind).await {
            tracing::warn!(error = %e, "failed to write artifact manifest");
        }

        // Create immutable artifact directory with only runtime-required files.
        // Runtime processes execute from this read-only copy, not from the mutable workspace.
        let copy_mode = self
            .config
            .artifact_copy_mode
            .parse::<CopyMode>()
            .map_err(|_| {
                AppError::Internal(format!(
                    "invalid RIFT_ARTIFACT_COPY_MODE '{}'",
                    self.config.artifact_copy_mode
                ))
            })?;
        runtime_kind = create_immutable_artifact(&workspace_dir, runtime_kind, copy_mode).await;

        stage_timings.push(("artifact", started_at.elapsed().as_millis()));

        // Capture function info for V8 isolate pool registration (before runtime_kind is moved)
        #[cfg(feature = "v8-isolate")]
        let isolate_fn_info = if let RuntimeKind::Functions { ref dir } = runtime_kind {
            Some((dir.clone(), user_env_vars.clone()))
        } else {
            None
        };

        let (url, port) = match self
            .runtime_backend
            .deploy(RuntimeLaunchSpec {
                project_id: project.id,
                deployment_id,
                kind: runtime_kind,
                env_vars: user_env_vars,
            })
            .await
        {
            Ok(result) => (result.url, result.port),
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
                transition_to_failed(&self.pool, deployment_id, Some(elapsed_ms(started_at)))
                    .await?;
                return Err(error);
            }
        };

        // Register with V8 isolate pool for direct invocation (no HTTP hop)
        #[cfg(feature = "v8-isolate")]
        if let (Some(ref isolate_pool), Some((ref fn_dir, ref env_vars))) =
            (&self.isolate_pool, &isolate_fn_info)
        {
            let manifest_path = fn_dir.join("_routes.json");
            let routes: Vec<crate::build::functions::FunctionRoute> = if manifest_path.exists() {
                match tokio::fs::read_to_string(&manifest_path).await {
                    Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };

            if let Err(e) = isolate_pool
                .register(
                    project.id,
                    deployment_id,
                    &routes,
                    env_vars,
                    &fn_dir.to_string_lossy(),
                )
                .await
            {
                tracing::warn!(error = %e, "failed to register with V8 isolate pool — falling back to Deno dispatcher");
            } else {
                tracing::info!(
                    project_id = %project.id,
                    routes = routes.len(),
                    "registered with V8 isolate pool"
                );
            }
        }

        stage_timings.push(("runtime_start", started_at.elapsed().as_millis()));

        let total_ms = started_at.elapsed().as_millis();
        stage_timings.push(("total", total_ms));

        // Compute per-stage deltas from cumulative timestamps and emit metrics
        let mut timing_log = String::from("Deploy timing:");
        let mut prev: u128 = 0;
        for (stage, cumulative) in &stage_timings {
            let delta = cumulative.saturating_sub(prev);
            timing_log.push_str(&format!(" {stage}={delta}ms"));
            if *stage != "total" {
                crate::metrics::DEPLOY_STAGE_DURATION
                    .with_label_values(&[stage])
                    .observe(delta as f64 / 1000.0);
            }
            prev = *cumulative;
        }
        tracing::info!(
            deployment_id = %deployment_id,
            total_ms = total_ms as u64,
            "{timing_log}"
        );
        insert_and_broadcast_log(
            &self.pool,
            &self.log_broadcaster,
            deployment_id,
            "info",
            &timing_log,
            "build",
        )
        .await?;

        crate::metrics::DEPLOY_OUTCOME
            .with_label_values(&["success"])
            .inc();
        crate::metrics::BUILD_DURATION
            .with_label_values(&["success"])
            .observe(total_ms as f64 / 1000.0);
        transition_to_ready(
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
                    // Remove workspace directory (restore write perms on immutable artifact first)
                    let old_dir = deploy_root.join(old_deployment.id.to_string());
                    if old_dir.exists() {
                        let artifact_dir = old_dir.join("_rift_artifact");
                        if artifact_dir.exists() {
                            let _ = set_dir_writable(&artifact_dir).await;
                        }
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

/// Write `_rift_manifest.json` recording which files are essential for runtime.
///
/// This manifest makes the boundary between "build workspace" and "runtime artifact"
/// explicit. A future step can use it to copy only the listed files to an immutable
/// artifact directory.
async fn write_artifact_manifest(
    workspace_dir: &std::path::Path,
    kind: &crate::runtime::RuntimeKind,
) -> Result<(), AppError> {
    use crate::runtime::RuntimeKind;

    let (runtime_type, entry_point, functions_dir) = match kind {
        RuntimeKind::StaticDeno { dir } => (
            "static",
            dir.join("_entry.ts").to_string_lossy().to_string(),
            None,
        ),
        RuntimeKind::NextDeno { dir } => {
            let standalone = dir.join(".next/standalone");
            ("next", standalone.to_string_lossy().to_string(), None)
        }
        RuntimeKind::NodeServer { entry, .. } => {
            ("node_ssr", entry.to_string_lossy().to_string(), None)
        }
        RuntimeKind::Functions { dir } => (
            "functions",
            dir.join("_entry.ts").to_string_lossy().to_string(),
            Some(dir.to_string_lossy().to_string()),
        ),
        RuntimeKind::Combined {
            entry,
            functions_dir,
        } => (
            "combined",
            entry.to_string_lossy().to_string(),
            Some(functions_dir.to_string_lossy().to_string()),
        ),
    };

    let manifest = serde_json::json!({
        "version": 1,
        "runtime_type": runtime_type,
        "entry_point": entry_point,
        "functions_dir": functions_dir,
    });

    let manifest_path = workspace_dir.join("_rift_manifest.json");
    let content = serde_json::to_string_pretty(&manifest)
        .map_err(|e| AppError::Internal(format!("failed to serialize manifest: {e}")))?;
    fs::write(&manifest_path, content)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write manifest: {e}")))?;

    Ok(())
}

/// Copy only the runtime-required paths for a Node SSR deployment instead of
/// the entire workspace. This dramatically reduces artifact size and copy time.
///
/// Required paths vary by framework:
///   - Nuxt:      `.output/` + `node_modules/` + `package.json`
///   - Astro:     `dist/` + `node_modules/` + `package.json`
///   - SvelteKit: `build/` + `node_modules/` + `package.json`
///   - Remix:     `build/` + `node_modules/` + `package.json` + `public/`
async fn copy_node_ssr_artifact(
    workspace_dir: &std::path::Path,
    entry: &std::path::Path,
    artifact_dir: &std::path::Path,
    copy_mode: CopyMode,
) -> Result<(), AppError> {
    // Determine the output directory from the entry point path.
    // e.g. entry = "/builds/xyz/.output/server/index.mjs" → output_root = ".output"
    let relative_entry = entry.strip_prefix(workspace_dir).unwrap_or(entry);
    let output_root = relative_entry
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| ".output".to_string());

    // Always copy: output dir, node_modules, package.json
    let output_src = workspace_dir.join(&output_root);
    if output_src.exists() {
        copy_dir_with_mode(&output_src, &artifact_dir.join(&output_root), copy_mode).await?;
    }

    let node_modules_src = workspace_dir.join("node_modules");
    if node_modules_src.exists() {
        copy_dir_with_mode(
            &node_modules_src,
            &artifact_dir.join("node_modules"),
            copy_mode,
        )
        .await?;
    }

    let pkg_json = workspace_dir.join("package.json");
    if pkg_json.exists() {
        fs::copy(&pkg_json, &artifact_dir.join("package.json"))
            .await
            .map_err(|e| AppError::Internal(format!("failed to copy package.json: {e}")))?;
    }

    // Copy public/ if it exists (needed by Remix, sometimes SvelteKit)
    let public_src = workspace_dir.join("public");
    if public_src.exists() {
        let _ = copy_dir_with_mode(&public_src, &artifact_dir.join("public"), copy_mode).await;
    }

    Ok(())
}

/// Create an immutable runtime artifact directory containing only the files
/// needed at runtime. Reads `_rift_manifest.json` to determine what to copy.
///
/// Returns the updated RuntimeKind pointing to the artifact directory, or the
/// original kind if artifact creation fails (with a warning logged).
async fn create_immutable_artifact(
    workspace_dir: &std::path::Path,
    kind: crate::runtime::RuntimeKind,
    copy_mode: CopyMode,
) -> crate::runtime::RuntimeKind {
    use crate::runtime::RuntimeKind;

    let artifact_dir = workspace_dir.join("_rift_artifact");

    // Clean up any previous artifact
    if artifact_dir.exists() {
        let _ = set_dir_writable(&artifact_dir).await;
        let _ = fs::remove_dir_all(&artifact_dir).await;
    }

    if let Err(e) = fs::create_dir_all(&artifact_dir).await {
        tracing::warn!(error = %e, "failed to create artifact directory, running from mutable workspace");
        return kind;
    }

    let result = match &kind {
        RuntimeKind::StaticDeno { dir } => {
            match copy_dir_with_mode(dir, &artifact_dir, copy_mode).await {
                Ok(()) => Some(RuntimeKind::StaticDeno {
                    dir: artifact_dir.clone(),
                }),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to copy static artifact");
                    None
                }
            }
        }
        RuntimeKind::Functions { dir } => {
            match copy_dir_with_mode(dir, &artifact_dir.join("_rift_functions_output"), copy_mode)
                .await
            {
                Ok(()) => Some(RuntimeKind::Functions {
                    dir: artifact_dir.join("_rift_functions_output"),
                }),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to copy functions artifact");
                    None
                }
            }
        }
        RuntimeKind::Combined {
            entry,
            functions_dir,
        } => {
            let fn_artifact = artifact_dir.join("_rift_functions_output");
            match copy_dir_with_mode(functions_dir, &fn_artifact, copy_mode).await {
                Ok(()) => {
                    let entry_name = entry.file_name().unwrap_or_default();
                    Some(RuntimeKind::Combined {
                        entry: fn_artifact.join(entry_name),
                        functions_dir: fn_artifact,
                    })
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to copy combined artifact");
                    None
                }
            }
        }
        RuntimeKind::NextDeno { dir } => {
            let standalone_src = dir.join(".next/standalone");
            let standalone_dst = artifact_dir.join(".next/standalone");
            if let Err(e) = copy_dir_with_mode(&standalone_src, &standalone_dst, copy_mode).await {
                tracing::warn!(error = %e, "failed to copy Next.js standalone artifact");
                return kind;
            }
            let public_src = dir.join("public");
            if public_src.exists() {
                let _ =
                    copy_dir_with_mode(&public_src, &artifact_dir.join("public"), copy_mode).await;
            }
            let pool_entry = dir.join("_rift_pool_entry.ts");
            if pool_entry.exists() {
                let _ = fs::copy(&pool_entry, &artifact_dir.join("_rift_pool_entry.ts")).await;
            }
            Some(RuntimeKind::NextDeno {
                dir: artifact_dir.clone(),
            })
        }
        RuntimeKind::NodeServer { dir, entry } => {
            // Selective copy: only runtime-required paths instead of entire workspace
            if let Err(e) = copy_node_ssr_artifact(dir, entry, &artifact_dir, copy_mode).await {
                tracing::warn!(error = %e, "failed to copy Node SSR artifact");
                return kind;
            }
            let relative = entry.strip_prefix(dir).unwrap_or(entry.as_path());
            Some(RuntimeKind::NodeServer {
                dir: artifact_dir.clone(),
                entry: artifact_dir.join(relative),
            })
        }
    };

    match result {
        Some(new_kind) => {
            // Set artifact directory to read-only
            if let Err(e) = set_dir_readonly(&artifact_dir).await {
                tracing::warn!(error = %e, "failed to set artifact directory read-only");
            }
            // Copy manifest to artifact dir
            let manifest_src = workspace_dir.join("_rift_manifest.json");
            if manifest_src.exists() {
                let _ = fs::copy(&manifest_src, &artifact_dir.join("_rift_manifest.json")).await;
            }
            tracing::info!(
                artifact_dir = %artifact_dir.display(),
                "created immutable runtime artifact"
            );
            new_kind
        }
        None => kind,
    }
}

/// Set a directory tree to read-only (best effort, non-fatal).
async fn set_dir_readonly(dir: &std::path::Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        set_permissions_recursive(dir, 0o555).await?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(dir)
            .await
            .map_err(|e| AppError::Internal(format!("failed to read metadata: {e}")))?
            .permissions();
        perms.set_readonly(true);
        fs::set_permissions(dir, perms)
            .await
            .map_err(|e| AppError::Internal(format!("failed to set readonly: {e}")))?;
    }
    Ok(())
}

/// Restore write permissions on a directory tree (for cleanup).
async fn set_dir_writable(dir: &std::path::Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        set_permissions_recursive(dir, 0o755).await?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(dir)
            .await
            .map_err(|e| AppError::Internal(format!("failed to read metadata: {e}")))?
            .permissions();
        perms.set_readonly(false);
        fs::set_permissions(dir, perms)
            .await
            .map_err(|e| AppError::Internal(format!("failed to set writable: {e}")))?;
    }
    Ok(())
}

#[cfg(unix)]
async fn set_permissions_recursive(dir: &std::path::Path, mode: u32) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;

    let perms = std::fs::Permissions::from_mode(mode);
    fs::set_permissions(dir, perms.clone()).await.map_err(|e| {
        AppError::Internal(format!(
            "failed to set permissions on {}: {e}",
            dir.display()
        ))
    })?;

    let mut entries = fs::read_dir(dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read dir {}: {e}", dir.display())))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read entry: {e}")))?
    {
        let ft = entry
            .file_type()
            .await
            .map_err(|e| AppError::Internal(format!("failed to get file type: {e}")))?;
        if ft.is_dir() {
            Box::pin(set_permissions_recursive(&entry.path(), mode)).await?;
        } else {
            let file_mode = if mode & 0o200 != 0 { 0o644 } else { 0o444 };
            let file_perms = std::fs::Permissions::from_mode(file_mode);
            fs::set_permissions(entry.path(), file_perms)
                .await
                .map_err(|e| {
                    AppError::Internal(format!(
                        "failed to set permissions on {}: {e}",
                        entry.path().display()
                    ))
                })?;
        }
    }

    Ok(())
}

/// Try to reflink (CoW) a single file. Returns true on success.
#[cfg(target_os = "linux")]
fn try_reflink_file(src: &std::path::Path, dst: &std::path::Path) -> bool {
    use std::os::unix::io::AsRawFd;
    // FICLONE ioctl number
    const FICLONE: libc::c_ulong = 0x40049409;

    let Ok(src_file) = std::fs::File::open(src) else {
        return false;
    };
    let Ok(dst_file) = std::fs::File::create(dst) else {
        return false;
    };

    // Safety: FICLONE is a well-defined ioctl on btrfs/xfs/bcachefs.
    unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) == 0 }
}

#[cfg(target_os = "macos")]
fn try_reflink_file(src: &std::path::Path, dst: &std::path::Path) -> bool {
    use std::ffi::CString;
    extern "C" {
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32)
            -> libc::c_int;
    }
    let Ok(src_c) = CString::new(src.to_string_lossy().as_bytes()) else {
        return false;
    };
    let Ok(dst_c) = CString::new(dst.to_string_lossy().as_bytes()) else {
        return false;
    };
    // Safety: clonefile is available on macOS 10.12+ (APFS).
    unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) == 0 }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn try_reflink_file(_src: &std::path::Path, _dst: &std::path::Path) -> bool {
    false
}

/// Copy mode for artifact creation, matching `RIFT_ARTIFACT_COPY_MODE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMode {
    /// Try CoW/reflink first, fall back to recursive copy.
    Auto,
    /// Require CoW/reflink — fail if unsupported.
    Reflink,
    /// Always use traditional recursive copy.
    Recursive,
}

impl std::str::FromStr for CopyMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "reflink" => Self::Reflink,
            "recursive" => Self::Recursive,
            _ => Self::Auto,
        })
    }
}

/// Recursively copy a directory tree, optionally using CoW/reflink for files.
pub async fn copy_dir_with_mode(
    src: &std::path::Path,
    dst: &std::path::Path,
    mode: CopyMode,
) -> Result<(), AppError> {
    let skip_child = if dst.starts_with(src) {
        dst.strip_prefix(src)
            .ok()
            .and_then(|relative| relative.components().next())
            .map(|component| component.as_os_str().to_owned())
    } else {
        None
    };

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
        if let Some(skip_name) = &skip_child {
            if entry.file_name() == *skip_name {
                continue;
            }
        }
        let dest_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            Box::pin(copy_dir_with_mode(&entry.path(), &dest_path, mode)).await?;
        } else {
            let src_path = entry.path();
            match mode {
                CopyMode::Auto => {
                    if !try_reflink_file(&src_path, &dest_path) {
                        // Fallback to regular copy
                        fs::copy(&src_path, &dest_path).await.map_err(|e| {
                            AppError::Internal(format!(
                                "failed to copy {} to {}: {e}",
                                src_path.display(),
                                dest_path.display()
                            ))
                        })?;
                    }
                }
                CopyMode::Reflink => {
                    if !try_reflink_file(&src_path, &dest_path) {
                        return Err(AppError::Internal(format!(
                            "reflink not supported for {} -> {}",
                            src_path.display(),
                            dest_path.display()
                        )));
                    }
                }
                CopyMode::Recursive => {
                    fs::copy(&src_path, &dest_path).await.map_err(|e| {
                        AppError::Internal(format!(
                            "failed to copy {} to {}: {e}",
                            src_path.display(),
                            dest_path.display()
                        ))
                    })?;
                }
            }
        }
    }

    Ok(())
}

/// Recursively copy a directory tree (always uses regular copy — for dependency
/// cache where CoW provides no benefit since files are later modified by npm).
async fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), AppError> {
    copy_dir_with_mode(src, dst, CopyMode::Recursive).await
}

/// Compute a cache key from the project's lockfile contents.
/// Returns None if no lockfile is found.
async fn compute_cache_key(
    workspace_dir: &std::path::Path,
    package_manager: &PackageManager,
) -> Option<String> {
    use sha2::{Digest, Sha256};

    let lockfile = match package_manager {
        PackageManager::Npm => "package-lock.json",
        PackageManager::Yarn => "yarn.lock",
        PackageManager::Pnpm => "pnpm-lock.yaml",
        PackageManager::Bun => "bun.lockb",
    };

    let lockfile_path = workspace_dir.join(lockfile);
    let content = fs::read(&lockfile_path).await.ok()?;
    let hash = Sha256::digest(&content);
    Some(format!("{:x}", hash))
}

/// Return environment variables that configure the package manager's native
/// cache to live inside `cache_dir`, making it persistent across builds.
pub fn native_cache_env(cache_dir: &std::path::Path, pm: &PackageManager) -> Vec<(String, String)> {
    let cache_root = cache_dir.join("native");
    match pm {
        PackageManager::Npm => vec![(
            "npm_config_cache".into(),
            cache_root.join("npm").to_string_lossy().into(),
        )],
        PackageManager::Pnpm => vec![
            (
                "PNPM_HOME".into(),
                cache_root.join("pnpm").to_string_lossy().into(),
            ),
            (
                "npm_config_store_dir".into(),
                cache_root.join("pnpm-store").to_string_lossy().into(),
            ),
        ],
        PackageManager::Yarn => vec![(
            "YARN_CACHE_FOLDER".into(),
            cache_root.join("yarn").to_string_lossy().into(),
        )],
        PackageManager::Bun => vec![(
            "BUN_INSTALL_CACHE_DIR".into(),
            cache_root.join("bun").to_string_lossy().into(),
        )],
    }
}

/// Upgrade an install command to use frozen lockfiles and offline-first mode
/// when the native cache is present. Only applied when the install command is
/// the auto-detected default (not a user-provided override).
pub fn optimized_install_command(pm: &PackageManager, original: &str) -> String {
    match pm {
        PackageManager::Npm => {
            if original == "npm install" {
                "npm ci --prefer-offline".to_owned()
            } else {
                original.to_owned()
            }
        }
        PackageManager::Pnpm => {
            if original == "pnpm install" {
                "pnpm install --frozen-lockfile --prefer-offline".to_owned()
            } else {
                original.to_owned()
            }
        }
        PackageManager::Yarn => original.to_owned(),
        PackageManager::Bun => original.to_owned(),
    }
}

fn select_install_command(
    pm: &PackageManager,
    original: &str,
    cache_enabled: bool,
    install_is_custom: bool,
    has_lockfile: bool,
) -> String {
    if cache_enabled && !install_is_custom && has_lockfile {
        optimized_install_command(pm, original)
    } else {
        original.to_owned()
    }
}

/// Restore cached node_modules into the workspace via symlink to the cache.
/// Returns Ok(true) if cache was restored, Ok(false) if no cache exists.
async fn restore_dependency_cache(
    cache_dir: &std::path::Path,
    cache_key: &str,
    workspace_dir: &std::path::Path,
) -> Result<bool, AppError> {
    let cached_dir = cache_dir.join(cache_key).join("node_modules");
    if !cached_dir.exists() {
        return Ok(false);
    }

    let target = workspace_dir.join("node_modules");
    if target.exists() {
        return Ok(false);
    }

    copy_dir_recursive(&cached_dir, &target).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_respects_custom_override() {
        let cmd = select_install_command(
            &PackageManager::Npm,
            "npm install",
            true,
            true,
            true,
        );
        assert_eq!(cmd, "npm install");
    }

    #[test]
    fn install_command_requires_lockfile_for_optimization() {
        let cmd = select_install_command(
            &PackageManager::Npm,
            "npm install",
            true,
            false,
            false,
        );
        assert_eq!(cmd, "npm install");
    }

    #[test]
    fn install_command_optimizes_default_npm_with_lockfile() {
        let cmd = select_install_command(
            &PackageManager::Npm,
            "npm install",
            true,
            false,
            true,
        );
        assert_eq!(cmd, "npm ci --prefer-offline");
    }
}

/// Save node_modules to the cache directory, keyed by lockfile hash.
async fn save_dependency_cache(
    cache_dir: &std::path::Path,
    cache_key: &str,
    workspace_dir: &std::path::Path,
) -> Result<(), AppError> {
    let source = workspace_dir.join("node_modules");
    if !source.exists() {
        return Ok(());
    }

    let target_dir = cache_dir.join(cache_key);
    let target = target_dir.join("node_modules");

    if target.exists() {
        return Ok(());
    }

    fs::create_dir_all(&target_dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create cache dir: {e}")))?;

    copy_dir_recursive(&source, &target).await?;

    // Prune old cache entries (keep at most 10)
    if let Ok(mut entries) = fs::read_dir(cache_dir).await {
        let mut dirs = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                if let Ok(meta) = entry.metadata().await {
                    dirs.push((
                        entry.path(),
                        meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    ));
                }
            }
        }
        if dirs.len() > 10 {
            dirs.sort_by_key(|(_, modified)| *modified);
            for (path, _) in dirs.iter().take(dirs.len() - 10) {
                let _ = fs::remove_dir_all(path).await;
            }
        }
    }

    Ok(())
}

/// Returns true if `dir` looks like a Next.js app root (has a next.config.* file).
fn is_next_app_dir(dir: &std::path::Path) -> bool {
    dir.join("next.config.js").exists()
        || dir.join("next.config.ts").exists()
        || dir.join("next.config.mjs").exists()
        || dir.join("next.config.cjs").exists()
}

/// Derive a stable cache sub-key from the app dir relative to the workspace root.
/// e.g. workspace root → "root", apps/web → "apps__web".
fn next_app_cache_key(workspace_dir: &std::path::Path, app_dir: &std::path::Path) -> String {
    match app_dir.strip_prefix(workspace_dir) {
        Ok(rel) if rel == std::path::Path::new("") => "root".to_string(),
        Ok(rel) => rel.to_string_lossy().replace(['/', '\\'], "__"),
        Err(_) => "root".to_string(),
    }
}

/// Collect Next.js app roots: the workspace root itself and any qualifying
/// subdirectory under apps/, packages/, or sites/.
async fn find_next_app_dirs(workspace_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    if is_next_app_dir(workspace_dir) {
        dirs.push(workspace_dir.to_path_buf());
    }

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
                let path = entry.path();
                if is_next_app_dir(&path) {
                    dirs.push(path);
                }
            }
        }
    }

    dirs
}

/// Restore Next.js build cache from persistent storage into the workspace so
/// the compiler can skip recompiling unchanged modules. Returns true if any
/// cache was restored.
async fn restore_next_build_caches(
    cache_dir: &std::path::Path,
    project_id: Uuid,
    workspace_dir: &std::path::Path,
) -> bool {
    let framework_cache = cache_dir.join("framework").join(project_id.to_string());
    if !framework_cache.exists() {
        return false;
    }

    let app_dirs = find_next_app_dirs(workspace_dir).await;
    let mut restored = false;

    for app_dir in &app_dirs {
        let key = next_app_cache_key(workspace_dir, app_dir);
        let src = framework_cache.join(&key);
        if !src.exists() {
            continue;
        }
        let dst = app_dir.join(".next").join("cache");
        if dst.exists() {
            continue;
        }
        let _ = fs::create_dir_all(app_dir.join(".next")).await;
        if copy_dir_recursive(&src, &dst).await.is_ok() {
            restored = true;
        }
    }

    restored
}

/// Save .next/cache to persistent storage after a successful build so the next
/// deploy can reuse it for incremental compilation.
async fn save_next_build_caches(
    cache_dir: &std::path::Path,
    project_id: Uuid,
    workspace_dir: &std::path::Path,
) {
    let framework_cache = cache_dir.join("framework").join(project_id.to_string());
    let app_dirs = find_next_app_dirs(workspace_dir).await;

    for app_dir in &app_dirs {
        let src = app_dir.join(".next").join("cache");
        if !src.exists() {
            continue;
        }
        let key = next_app_cache_key(workspace_dir, app_dir);
        let dst = framework_cache.join(&key);
        if dst.exists() {
            let _ = fs::remove_dir_all(&dst).await;
        }
        let _ = fs::create_dir_all(&framework_cache).await;
        if let Err(e) = copy_dir_recursive(&src, &dst).await {
            tracing::debug!(error = %e, "failed to save Next.js build cache for {key}");
        }
    }
}
