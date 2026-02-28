pub mod health;
pub mod process;
pub mod scaler;

use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, to_bytes},
    extract::{ConnectInfo, OriginalUri, Request},
    http::{Response, StatusCode, Uri},
    routing::any,
    Router,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    error::AppError,
    proxy::analytics_collector::{AnalyticsCollector, RequestEvent},
};

use self::{
    health::wait_for_port,
    process::{allocate_port, spawn_shell, INTERNAL_PORT_OFFSET},
};

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RuntimeManager {
    inner: Arc<Mutex<HashMap<Uuid, ActiveRuntime>>>,
}

#[derive(Debug)]
struct ActiveRuntime {
    deployment_id: Uuid,
    /// External port users connect to (10000-10100).
    port: u16,
    child: Arc<Mutex<tokio::process::Child>>,
    /// Handle to the analytics proxy task listening on the external port.
    proxy_handle: Arc<tokio::task::JoinHandle<()>>,
}

#[derive(Clone, Debug)]
pub enum RuntimeKind {
    StaticDir { dir: PathBuf },
    NextApp { dir: PathBuf },
}

#[derive(Clone, Debug)]
pub struct RuntimeLaunchSpec {
    pub project_id: Uuid,
    pub deployment_id: Uuid,
    pub kind: RuntimeKind,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Deploy a project and return `(internal_url, external_port)`.
    ///
    /// The app binds to an *internal* port (external + offset). A lightweight
    /// reverse-proxy listens on the *external* port, records analytics, and
    /// forwards each request to the app.
    pub async fn deploy(
        &self,
        spec: RuntimeLaunchSpec,
        analytics: AnalyticsCollector,
    ) -> Result<(String, u16), AppError> {
        self.stop_project(spec.project_id).await?;

        let external_port = allocate_port()?;
        let internal_port = external_port + INTERNAL_PORT_OFFSET;

        let child = match &spec.kind {
            RuntimeKind::StaticDir { dir } => spawn_shell(
                &format!("serve -s '{}' -l {internal_port} -n", dir.display()),
                dir,
                &[],
            )?,
            RuntimeKind::NextApp { dir } => spawn_shell(
                &format!("npx next start -H 0.0.0.0 -p {internal_port}"),
                dir,
                &[
                    ("PORT", internal_port.to_string()),
                    ("HOSTNAME", "0.0.0.0".to_owned()),
                    ("NODE_ENV", "production".to_owned()),
                ],
            )?,
        };

        if !wait_for_port("127.0.0.1", internal_port, 40).await {
            return Err(AppError::Internal(
                "runtime failed to become healthy".into(),
            ));
        }

        // Spawn a lightweight analytics proxy on the external port
        let proxy_handle = Arc::new(spawn_analytics_proxy(
            external_port,
            internal_port,
            spec.project_id,
            analytics,
        ));

        let child = Arc::new(Mutex::new(child));
        self.inner.lock().await.insert(
            spec.project_id,
            ActiveRuntime {
                deployment_id: spec.deployment_id,
                port: external_port,
                child,
                proxy_handle,
            },
        );

        Ok((format!("http://127.0.0.1:{internal_port}"), external_port))
    }

    pub async fn stop_project(&self, project_id: Uuid) -> Result<(), AppError> {
        if let Some(runtime) = self.inner.lock().await.remove(&project_id) {
            runtime.proxy_handle.abort();
            let mut child = runtime.child.lock().await;
            let _ = child.kill().await;
        }
        Ok(())
    }

    pub async fn active_url(&self, project_id: Uuid) -> Option<String> {
        self.inner
            .lock()
            .await
            .get(&project_id)
            .map(|runtime| {
                let internal_port = runtime.port + INTERNAL_PORT_OFFSET;
                format!("http://127.0.0.1:{internal_port}")
            })
    }

    pub async fn active_deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.inner
            .lock()
            .await
            .get(&project_id)
            .map(|runtime| runtime.deployment_id)
    }
}

impl Default for RuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Per-deployment analytics reverse proxy
// ---------------------------------------------------------------------------

fn spawn_analytics_proxy(
    external_port: u16,
    internal_port: u16,
    project_id: Uuid,
    analytics: AnalyticsCollector,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let target_base = format!("http://127.0.0.1:{internal_port}");

        let target1 = target_base.clone();
        let target2 = target_base.clone();
        let analytics1 = analytics.clone();
        let analytics2 = analytics;
        let app = Router::new()
            .route("/", any(move |conn, uri, req| {
                analytics_proxy_handler(
                    conn, uri, req, target1.clone(), project_id, analytics1.clone(),
                )
            }))
            .route("/{*path}", any(move |conn, uri, req| {
                analytics_proxy_handler(
                    conn, uri, req, target2.clone(), project_id, analytics2.clone(),
                )
            }));

        let addr: SocketAddr = ([0, 0, 0, 0], external_port).into();
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(port = external_port, error = %e, "analytics proxy bind failed");
                return;
            }
        };

        tracing::debug!(external_port, internal_port, %project_id, "analytics proxy listening");

        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    })
}

async fn analytics_proxy_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    OriginalUri(uri): OriginalUri,
    request: Request,
    target_base: String,
    project_id: Uuid,
    analytics: AnalyticsCollector,
) -> Result<Response<Body>, StatusCode> {
    let start = std::time::Instant::now();

    let target_url = format!("{}{}", target_base, path_and_query(&uri));

    let (parts, body) = request.into_parts();
    let body_bytes = to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    let client = reqwest::Client::new();
    let mut upstream = client.request(parts.method.clone(), &target_url);
    upstream = upstream.body(body_bytes.to_vec());

    for (name, value) in &parts.headers {
        let n = name.as_str().to_ascii_lowercase();
        if n == "host" || n.starts_with("x-forwarded-") || n == "connection" || n == "transfer-encoding" {
            continue;
        }
        upstream = upstream.header(name, value);
    }
    upstream = upstream.header("x-forwarded-for", addr.ip().to_string());

    let upstream_response = match upstream.send().await {
        Ok(r) => r,
        Err(_) => {
            analytics.record(RequestEvent { project_id, status: 502, duration_ms: start.elapsed().as_millis() as u64 });
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    let status = upstream_response.status();
    analytics.record(RequestEvent {
        project_id,
        status: status.as_u16(),
        duration_ms: start.elapsed().as_millis() as u64,
    });

    let mut response = Response::builder().status(status);
    for (name, value) in upstream_response.headers() {
        let n = name.as_str().to_ascii_lowercase();
        if n == "connection" || n == "transfer-encoding" {
            continue;
        }
        response = response.header(name, value);
    }

    let bytes = upstream_response.bytes().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    response.body(Body::from(bytes)).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn path_and_query(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|v| v.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned())
}
