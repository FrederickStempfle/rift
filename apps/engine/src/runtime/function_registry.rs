use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::build::functions::FunctionRoute;
use crate::error::AppError;

use super::process::spawn_global_dispatcher;

/// Tracks which projects are served by the global function dispatcher
/// and manages the dispatcher's lifecycle.
#[derive(Clone)]
pub struct FunctionRegistry {
    inner: Arc<RwLock<RegistryState>>,
    dispatcher: Arc<GlobalDispatcher>,
}

struct RegistryState {
    projects: HashMap<Uuid, RegisteredProject>,
}

#[derive(Clone)]
struct RegisteredProject {
    deployment_id: Uuid,
    routes: Vec<FunctionRoute>,
    env_vars: Vec<(String, String)>,
    output_dir: String,
}

struct GlobalDispatcher {
    child: Mutex<Option<tokio::process::Child>>,
    port: u16,
    template_dir: String,
}

impl FunctionRegistry {
    pub async fn start(template_dir: &Path, port: u16) -> Result<Self, AppError> {
        let child = spawn_global_dispatcher(template_dir, port)?;

        let dispatcher = Arc::new(GlobalDispatcher {
            child: Mutex::new(Some(child)),
            port,
            template_dir: template_dir.to_string_lossy().to_string(),
        });

        // Wait for the dispatcher to become healthy
        let url = format!("http://127.0.0.1:{port}/_rift/health");
        let client = reqwest::Client::new();
        let mut healthy = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    healthy = true;
                    break;
                }
            }
        }

        if !healthy {
            return Err(AppError::Internal(
                "global function dispatcher failed to start".into(),
            ));
        }

        tracing::info!(port = port, "global function dispatcher started");

        Ok(Self {
            inner: Arc::new(RwLock::new(RegistryState {
                projects: HashMap::new(),
            })),
            dispatcher,
        })
    }

    /// Get the internal URL for the global dispatcher.
    pub fn dispatcher_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.dispatcher.port)
    }

    /// Check if a project is registered as function-only.
    pub async fn is_function_project(&self, project_id: Uuid) -> bool {
        self.inner.read().await.projects.contains_key(&project_id)
    }

    /// Register a function-only project with the global dispatcher.
    pub async fn register(
        &self,
        project_id: Uuid,
        deployment_id: Uuid,
        routes: &[FunctionRoute],
        env_vars: &[(String, String)],
        output_dir: &str,
    ) -> Result<(), AppError> {
        // Build worker paths from routes
        let route_entries: Vec<serde_json::Value> = routes
            .iter()
            .map(|r| {
                let sanitized = sanitize_route_name(&r.pattern);
                let worker_path = format!(
                    "file://{}/_worker_{sanitized}.ts",
                    output_dir
                );
                serde_json::json!({
                    "pattern": r.pattern,
                    "workerPath": worker_path,
                })
            })
            .collect();

        let env_map: HashMap<&str, &str> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let body = serde_json::json!({
            "projectId": project_id.to_string(),
            "routes": route_entries,
            "envVars": env_map,
        });

        let url = format!(
            "http://127.0.0.1:{}/_rift/register",
            self.dispatcher.port
        );

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                AppError::Internal(format!("failed to register with global dispatcher: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Internal(format!(
                "global dispatcher registration failed: {text}"
            )));
        }

        // Store locally for re-registration after restart
        self.inner.write().await.projects.insert(
            project_id,
            RegisteredProject {
                deployment_id,
                routes: routes.to_vec(),
                env_vars: env_vars.to_vec(),
                output_dir: output_dir.to_string(),
            },
        );

        tracing::info!(
            project_id = %project_id,
            deployment_id = %deployment_id,
            routes = routes.len(),
            "registered function project with global dispatcher"
        );

        Ok(())
    }

    /// Unregister a project from the global dispatcher.
    pub async fn unregister(&self, project_id: Uuid) -> Result<(), AppError> {
        self.inner.write().await.projects.remove(&project_id);

        let url = format!(
            "http://127.0.0.1:{}/_rift/unregister/{}",
            self.dispatcher.port, project_id
        );

        let client = reqwest::Client::new();
        let _ = client.delete(&url).send().await;

        tracing::info!(project_id = %project_id, "unregistered function project");

        Ok(())
    }

    /// Get the deployment ID for a registered function project.
    pub async fn deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.inner
            .read()
            .await
            .projects
            .get(&project_id)
            .map(|p| p.deployment_id)
    }

    /// Re-register all projects with the dispatcher (after a restart).
    async fn re_register_all(&self) -> Result<(), AppError> {
        let projects: Vec<(Uuid, RegisteredProject)> = self
            .inner
            .read()
            .await
            .projects
            .iter()
            .map(|(&id, p)| (id, p.clone()))
            .collect();

        for (project_id, project) in projects {
            if let Err(e) = self
                .register(
                    project_id,
                    project.deployment_id,
                    &project.routes,
                    &project.env_vars,
                    &project.output_dir,
                )
                .await
            {
                tracing::error!(
                    project_id = %project_id,
                    error = %e,
                    "failed to re-register project after dispatcher restart"
                );
            }
        }

        Ok(())
    }

    /// Spawn a background task that monitors the global dispatcher's health
    /// and restarts it if it crashes.
    pub fn spawn_health_monitor(&self) {
        let registry = self.clone();
        let dispatcher = Arc::clone(&self.dispatcher);

        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            let health_url = format!("http://127.0.0.1:{}/_rift/health", dispatcher.port);
            let mut consecutive_failures: u32 = 0;

            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                match client.get(&health_url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        consecutive_failures = 0;
                    }
                    _ => {
                        consecutive_failures += 1;
                        tracing::warn!(
                            failures = consecutive_failures,
                            "global function dispatcher health check failed"
                        );

                        if consecutive_failures >= 3 {
                            tracing::error!("global function dispatcher unresponsive, restarting");

                            // Kill old process
                            if let Some(mut child) = dispatcher.child.lock().await.take() {
                                let _ = child.kill().await;
                            }

                            // Respawn
                            let template_dir = Path::new(&dispatcher.template_dir);
                            match spawn_global_dispatcher(template_dir, dispatcher.port) {
                                Ok(child) => {
                                    *dispatcher.child.lock().await = Some(child);

                                    // Wait for it to come up
                                    tokio::time::sleep(Duration::from_secs(2)).await;

                                    // Re-register all projects
                                    if let Err(e) = registry.re_register_all().await {
                                        tracing::error!(
                                            error = %e,
                                            "failed to re-register projects after dispatcher restart"
                                        );
                                    }

                                    consecutive_failures = 0;
                                    tracing::info!("global function dispatcher restarted");
                                }
                                Err(e) => {
                                    tracing::error!(
                                        error = %e,
                                        "failed to restart global function dispatcher"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

/// Sanitize a route pattern into a valid filename.
fn sanitize_route_name(pattern: &str) -> String {
    pattern
        .trim_start_matches('/')
        .replace('/', "_")
        .replace(':', "_")
        .replace('*', "_star")
}

impl std::fmt::Debug for FunctionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionRegistry").finish()
    }
}
