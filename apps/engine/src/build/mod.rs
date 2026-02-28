pub mod bundler;
pub mod detect;
pub mod pipeline;

use std::{path::PathBuf, sync::Arc, time::Instant};

use chrono::Utc;
use tokio::{fs, sync::Semaphore};
use uuid::Uuid;

use crate::{
    db::{deployments, models::Project},
    error::AppError,
    runtime::{RuntimeKind, RuntimeLaunchSpec, RuntimeManager},
};

use self::{
    detect::{detect_build_plan, BuildOutput},
    pipeline::{elapsed_ms, read_git_metadata, run_command_and_log},
};

#[derive(Clone, Debug)]
pub struct BuildManager {
    pool: sqlx::PgPool,
    runtime_manager: RuntimeManager,
    build_root: PathBuf,
    deploy_root: PathBuf,
    concurrency: Arc<Semaphore>,
}

impl BuildManager {
    pub fn new(
        pool: sqlx::PgPool,
        runtime_manager: RuntimeManager,
        build_root: PathBuf,
        deploy_root: PathBuf,
    ) -> Self {
        Self {
            pool,
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

        run_command_and_log(
            &self.pool,
            deployment_id,
            "build",
            &self.build_root,
            &format!(
                "git clone --depth 1 --branch '{}' '{}' '{}'",
                project.branch,
                project.repo_url,
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

        deployments::update_status(&self.pool, deployment_id, "building").await?;
        if let Err(error) = run_command_and_log(
            &self.pool,
            deployment_id,
            "build",
            &workspace_dir,
            &plan.install_command,
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

        if let Err(error) = run_command_and_log(
            &self.pool,
            deployment_id,
            "build",
            &workspace_dir,
            &plan.build_command,
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
            BuildOutput::Next => RuntimeKind::NextApp {
                dir: workspace_dir.clone(),
            },
            BuildOutput::Static { dir } => {
                let output_dir = workspace_dir.join(dir);
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
                RuntimeKind::StaticDir { dir: output_dir }
            }
        };

        let (url, port) = match self
            .runtime_manager
            .deploy(RuntimeLaunchSpec {
                project_id: project.id,
                deployment_id,
                kind: runtime_kind,
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
}
