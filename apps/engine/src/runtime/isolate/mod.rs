//! In-process V8 isolate pool for serverless function execution.
//!
//! Replaces the Deno subprocess global dispatcher with direct V8 execution
//! inside the Rust engine process. Each function invocation gets a fresh
//! `deno_core::JsRuntime` — true per-request isolation with near-zero overhead.

mod ops;
pub mod route;
mod runtime;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use crate::build::functions::FunctionRoute;
use crate::error::AppError;

/// Configuration for the V8 isolate pool.
#[derive(Clone, Debug)]
pub struct IsolatePoolConfig {
    /// Maximum concurrent isolate executions.
    pub max_concurrent: usize,
    /// Per-invocation timeout.
    pub execution_timeout: Duration,
    /// Per-isolate heap size limit in bytes.
    pub heap_limit_bytes: usize,
}

impl Default for IsolatePoolConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 50,
            execution_timeout: Duration::from_secs(30),
            heap_limit_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Result of executing a function in an isolate.
#[derive(Debug)]
pub struct IsolateResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: bytes::Bytes,
}

/// A registered function project in the isolate pool.
#[derive(Clone)]
struct RegisteredProject {
    deployment_id: Uuid,
    routes: Vec<FunctionRoute>,
    env_vars: Vec<(String, String)>,
    #[allow(dead_code)]
    output_dir: PathBuf,
    /// Pre-loaded bundle source code, keyed by route pattern.
    bundles: HashMap<String, Arc<String>>,
}

/// In-process V8 isolate pool for serverless function execution.
#[derive(Clone)]
pub struct IsolatePool {
    config: Arc<IsolatePoolConfig>,
    projects: Arc<Mutex<HashMap<Uuid, RegisteredProject>>>,
    semaphore: Arc<Semaphore>,
}

impl IsolatePool {
    /// Create the isolate pool.
    pub async fn new(config: IsolatePoolConfig) -> Result<Self, AppError> {
        let semaphore = Semaphore::new(config.max_concurrent);

        // Verify that deno_core can create a runtime (catches V8 init errors early).
        // JsRuntime is !Send, so we verify in a spawn_blocking which runs on
        // a thread pool thread (no Send needed for the return — we drop the runtime).
        tokio::task::spawn_blocking(|| -> Result<(), AppError> {
            let _runtime = runtime::create_function_runtime()
                .map_err(|e| AppError::Internal(format!("V8 runtime creation failed: {e}")))?;
            drop(_runtime);
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("V8 init task panicked: {e}")))?
        .map_err(|e| AppError::Internal(format!("V8 init failed: {e}")))?;

        tracing::info!(
            max_concurrent = config.max_concurrent,
            timeout_secs = config.execution_timeout.as_secs(),
            heap_limit_mb = config.heap_limit_bytes / (1024 * 1024),
            "V8 isolate pool ready"
        );

        Ok(Self {
            config: Arc::new(config),
            projects: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(semaphore),
        })
    }

    /// Register a function project. Pre-loads all bundle JS from disk.
    pub async fn register(
        &self,
        project_id: Uuid,
        deployment_id: Uuid,
        routes: &[FunctionRoute],
        env_vars: &[(String, String)],
        output_dir: &str,
    ) -> Result<(), AppError> {
        let output_path = PathBuf::from(output_dir);
        let mut bundles = HashMap::new();

        for route in routes {
            let sanitized = route
                .pattern
                .trim_start_matches('/')
                .replace(['/', ':'], "_")
                .replace('*', "_star");
            let bundle_path = output_path.join(format!("bundles/{sanitized}.js"));

            if bundle_path.exists() {
                let source = tokio::fs::read_to_string(&bundle_path).await.map_err(|e| {
                    AppError::Internal(format!(
                        "failed to read bundle {}: {e}",
                        bundle_path.display()
                    ))
                })?;
                bundles.insert(route.pattern.clone(), Arc::new(source));
            } else {
                tracing::warn!(
                    route = %route.pattern,
                    path = %bundle_path.display(),
                    "bundle file not found, route will return 500"
                );
            }
        }

        let project = RegisteredProject {
            deployment_id,
            routes: routes.to_vec(),
            env_vars: env_vars.to_vec(),
            output_dir: output_path,
            bundles,
        };

        self.projects.lock().await.insert(project_id, project);

        tracing::info!(
            project_id = %project_id,
            deployment_id = %deployment_id,
            routes = routes.len(),
            "registered project with V8 isolate pool"
        );

        Ok(())
    }

    /// Unregister a project.
    pub async fn unregister(&self, project_id: Uuid) {
        self.projects.lock().await.remove(&project_id);
        tracing::info!(project_id = %project_id, "unregistered project from isolate pool");
    }

    /// Check if a project is registered.
    pub async fn is_registered(&self, project_id: Uuid) -> bool {
        self.projects.lock().await.contains_key(&project_id)
    }

    /// Get the deployment ID for a registered project.
    pub async fn deployment_id(&self, project_id: Uuid) -> Option<Uuid> {
        self.projects
            .lock()
            .await
            .get(&project_id)
            .map(|p| p.deployment_id)
    }

    /// Execute a function handler for an incoming request.
    ///
    /// This is the hot path — called directly from the proxy handler.
    /// Creates a fresh V8 isolate, loads the bundle, executes the handler,
    /// and returns the response.
    pub async fn invoke(
        &self,
        project_id: Uuid,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<bytes::Bytes>,
    ) -> Result<IsolateResponse, AppError> {
        // 1. Acquire concurrency permit
        let _permit = self
            .semaphore
            .try_acquire()
            .map_err(|_| AppError::RateLimited("isolate pool at capacity".into()))?;

        // 2. Look up project and match route
        let (bundle_source, env_vars, extended_headers) = {
            let projects = self.projects.lock().await;
            let project = projects.get(&project_id).ok_or_else(|| {
                AppError::NotFound("project not registered in isolate pool".into())
            })?;

            // Parse path from URL
            let path = url
                .find("://")
                .and_then(|i| url[i + 3..].find('/').map(|j| &url[i + 3 + j..]))
                .unwrap_or("/");
            // Strip query string
            let path = path.split('?').next().unwrap_or("/");

            // Match route (static routes first, then parameterized)
            let mut matched = None;
            for r in &project.routes {
                if route::route_matches(&r.pattern, path) {
                    matched = Some(r);
                    break;
                }
            }

            let matched_route =
                matched.ok_or_else(|| AppError::NotFound("no matching function route".into()))?;

            let bundle = project
                .bundles
                .get(&matched_route.pattern)
                .ok_or_else(|| AppError::Internal("bundle not loaded for route".into()))?;

            // Extend headers with route params
            let mut ext_headers = headers.to_vec();
            if let Some(params) = route::extract_route_params(&matched_route.pattern, path) {
                for (k, v) in params {
                    ext_headers.push((format!("x-rift-param-{k}"), v));
                }
            }

            (Arc::clone(bundle), project.env_vars.clone(), ext_headers)
        };

        // 3. Execute in a blocking task (V8 is single-threaded per isolate)
        let timeout = self.config.execution_timeout;
        let method = method.to_string();
        let url = url.to_string();

        let result = tokio::task::spawn_blocking(move || {
            // Create a single-threaded tokio runtime for async ops (fetch, sleep)
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| AppError::Internal(format!("failed to create tokio runtime: {e}")))?;

            rt.block_on(async move {
                execute_handler(
                    &bundle_source,
                    &method,
                    &url,
                    &extended_headers,
                    body,
                    &env_vars,
                    timeout,
                )
                .await
            })
        })
        .await
        .map_err(|e| AppError::Internal(format!("isolate task panicked: {e}")))?;

        result
    }
}

/// Execute a function handler inside a fresh V8 isolate.
async fn execute_handler(
    bundle_source: &str,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<bytes::Bytes>,
    env_vars: &[(String, String)],
    timeout: Duration,
) -> Result<IsolateResponse, AppError> {
    use deno_core::serde_v8;
    use deno_core::v8;

    // Create fresh runtime
    let mut runtime = runtime::create_function_runtime()
        .map_err(|e| AppError::Internal(format!("failed to create V8 runtime: {e}")))?;

    // Inject env vars into OpState
    {
        let state = runtime.op_state();
        let mut state = state.borrow_mut();
        let env_map: HashMap<String, String> = env_vars.iter().cloned().collect();
        state.put(env_map);
    }

    // Set up timeout
    let isolate_handle = runtime.v8_isolate().thread_safe_handle();
    let timeout_handle = tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        isolate_handle.terminate_execution();
    });

    // Build the JS code that loads the bundle and invokes the handler
    let headers_json = serde_json::to_string(headers).unwrap_or_else(|_| "[]".to_string());
    let body_expr = match &body {
        Some(b) => format!("new Uint8Array({:?})", b.as_ref()),
        None => "null".to_string(),
    };

    // Use a data: URL to load the ESM bundle inline
    let bundle_b64 = base64_encode_str(bundle_source);
    let invoke_code = format!(
        r#"(async () => {{
    const mod = await import("data:application/javascript;base64,{bundle_b64}");

    let handler = null;
    const d = mod.default;
    if (d && typeof d === 'object' && typeof d.fetch === 'function') {{
        handler = d.fetch.bind(d);
    }} else if (typeof d === 'function') {{
        handler = d;
    }} else if (typeof mod.fetch === 'function') {{
        handler = mod.fetch;
    }} else if (typeof mod.handler === 'function') {{
        handler = mod.handler;
    }}

    if (!handler) {{
        return {{ status: 500, headers: [['content-type', 'application/json']], body: Array.from(new TextEncoder().encode(JSON.stringify({{ error: 'No handler found' }}))) }};
    }}

    const req = new Request("{url}", {{
        method: "{method}",
        headers: {headers_json},
        body: {body_expr},
    }});

    try {{
        const resp = await handler(req);
        const respBody = new Uint8Array(await resp.arrayBuffer());
        return {{
            status: resp.status,
            headers: [...resp.headers.entries()],
            body: Array.from(respBody),
        }};
    }} catch (e) {{
        console.error('Handler error:', e);
        return {{
            status: 500,
            headers: [['content-type', 'application/json']],
            body: Array.from(new TextEncoder().encode(JSON.stringify({{ error: String(e) }}))),
        }};
    }}
}})()"#,
        bundle_b64 = bundle_b64,
        url = url.replace('\\', "\\\\").replace('"', "\\\""),
        method = method,
        headers_json = headers_json,
        body_expr = body_expr,
    );

    // Execute the handler
    let result = runtime.execute_script("[rift:invoke]", invoke_code);

    let global = match result {
        Ok(g) => g,
        Err(e) => {
            timeout_handle.abort();
            return Err(AppError::Internal(format!("JS execution error: {e}")));
        }
    };

    // Run the event loop to completion (handles promises, async ops)
    if let Err(e) = runtime
        .run_event_loop(deno_core::PollEventLoopOptions::default())
        .await
    {
        timeout_handle.abort();
        return Err(AppError::Internal(format!("event loop error: {e}")));
    }

    // Cancel the timeout
    timeout_handle.abort();

    // Extract the response from the resolved promise
    let scope = &mut runtime.handle_scope();
    let local = v8::Local::new(scope, global);

    // The result should be a promise — resolve it
    let result_value = if let Ok(promise) = v8::Local::<v8::Promise>::try_from(local) {
        match promise.state() {
            v8::PromiseState::Fulfilled => promise.result(scope),
            v8::PromiseState::Rejected => {
                let err = promise.result(scope);
                let err_str = err.to_rust_string_lossy(scope);
                return Err(AppError::Internal(format!("handler rejected: {err_str}")));
            }
            v8::PromiseState::Pending => {
                return Err(AppError::Internal("handler promise still pending".into()));
            }
        }
    } else {
        local
    };

    // Deserialize the response object
    let response: JsHandlerResponse = serde_v8::from_v8(scope, result_value)
        .map_err(|e| AppError::Internal(format!("failed to deserialize response: {e}")))?;

    let body_bytes = match response.body {
        Some(arr) => bytes::Bytes::from(arr.into_iter().map(|n| n as u8).collect::<Vec<u8>>()),
        None => bytes::Bytes::new(),
    };

    Ok(IsolateResponse {
        status: response.status,
        headers: response.headers,
        body: body_bytes,
    })
}

/// Response shape returned by the JS handler invocation code.
#[derive(serde::Deserialize)]
struct JsHandlerResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Option<Vec<i32>>,
}

/// Base64 encode a string (for data: URL module loading).
fn base64_encode_str(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = input.as_bytes();
    let mut result = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

impl std::fmt::Debug for IsolatePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsolatePool").finish()
    }
}
