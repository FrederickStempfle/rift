use std::{convert::Infallible, net::SocketAddr};

use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, header::HOST, HeaderMap, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use uuid::Uuid;

use crate::{
    api::AppState,
    db::{domains, projects},
    error::AppError,
    proxy::{analytics_collector::RequestEvent, routing_cache::CacheLookup},
    state::RoutingEntry,
};

type HttpClient = Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>;

const MAX_PROXY_BODY_BYTES: usize = 10 * 1024 * 1024;
const UPSTREAM_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub async fn handle_request(
    req: Request<Incoming>,
    remote_addr: SocketAddr,
    client: HttpClient,
    state: AppState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let start = std::time::Instant::now();

    // Extract analytics metadata before forwarding (req headers will be consumed)
    let analytics_path = req.uri().path().to_owned();
    let analytics_referer = req
        .headers()
        .get(hyper::header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|url| {
            url.split("//")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .and_then(|host| host.split(':').next())
                .map(|domain| domain.to_lowercase())
        });

    let result = route_and_forward(req, remote_addr, &client, &state).await;

    let (status_code, project_id, cold_start) = match &result {
        Ok((resp, pid, cs)) => (resp.status().as_u16(), *pid, *cs),
        Err((sc, pid)) => (sc.as_u16(), *pid, false),
    };

    if let Some(pid) = project_id {
        state.analytics_collector.record(RequestEvent {
            project_id: pid,
            status: status_code,
            duration_ms: start.elapsed().as_millis() as u64,
            cold_start,
            path: Some(analytics_path),
            referer: analytics_referer,
        });
    }

    match result {
        Ok((resp, _, _)) => Ok(resp),
        Err((sc, _)) => Ok(error_response(sc)),
    }
}

/// Return type includes (response, project_id, cold_start).
#[tracing::instrument(skip_all, fields(remote_addr = %remote_addr))]
#[allow(clippy::type_complexity)]
async fn route_and_forward(
    req: Request<Incoming>,
    remote_addr: SocketAddr,
    client: &HttpClient,
    state: &AppState,
) -> Result<(Response<Full<Bytes>>, Option<Uuid>, bool), (StatusCode, Option<Uuid>)> {
    let host = extract_host(req.headers()).ok_or((StatusCode::BAD_REQUEST, None))?;

    let project_id = resolve_project_id(state, &host)
        .await
        .map_err(|e| (map_app_error(e), None))?
        .ok_or((StatusCode::NOT_FOUND, None))?;

    let pid = Some(project_id);

    // Firewall check
    let allowed = state
        .firewall_cache
        .is_allowed(&state.pool, project_id, remote_addr.ip())
        .await
        .map_err(|e| (map_app_error(e), pid))?;
    if !allowed {
        return Err((StatusCode::FORBIDDEN, pid));
    }

    // V8 isolate pool: handle function-only projects directly (no HTTP hop)
    #[cfg(feature = "v8-isolate")]
    if let Some(ref isolate_pool) = state.isolate_pool {
        if isolate_pool.is_registered(project_id).await {
            return handle_isolate_invoke(req, remote_addr, isolate_pool, project_id, &host, state)
                .await;
        }
    }

    // Look up active runtime URL (in-memory, no DB hit)
    let (target_base, cold_start) = match state.runtime_backend.active_url(project_id).await {
        Some(url) => {
            state.runtime_backend.touch(project_id).await;
            (url, false)
        }
        None => {
            // Not running — try waking a suspended deployment (cold start)
            let wake_start = std::time::Instant::now();
            match state.runtime_backend.wake(project_id).await {
                Ok(Some(url)) => {
                    let duration = wake_start.elapsed().as_secs_f64();
                    crate::metrics::COLD_START_DURATION
                        .with_label_values(&["wake"])
                        .observe(duration);
                    crate::metrics::RUNTIME_EVENT
                        .with_label_values(&["cold_start"])
                        .inc();
                    tracing::info!(%project_id, duration_ms = %wake_start.elapsed().as_millis(), "cold start wake complete");
                    (url, true)
                }
                Ok(None) => {
                    tracing::warn!(%project_id, "wake requested but project is not suspended");
                    return Err((StatusCode::SERVICE_UNAVAILABLE, pid));
                }
                Err(error) => {
                    tracing::warn!(%project_id, error = %error, "wake failed");
                    return Err((map_app_error(error), pid));
                }
            }
        }
    };

    // Build target URL
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let target_url: Uri = format!("{target_base}{path_and_query}")
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, pid))?;

    // Decompose request
    let (parts, body) = req.into_parts();

    // Read body (bounded)
    let body_bytes = collect_body_limited(body, StatusCode::BAD_REQUEST)
        .await
        .map_err(|status| (status, pid))?;

    // Build upstream request
    let mut upstream = Request::builder()
        .method(parts.method.clone())
        .uri(&target_url);

    // Copy headers, filtering hop-by-hop and forwarding headers
    for (name, value) in &parts.headers {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "host"
            || lower.starts_with("x-forwarded-")
            || lower == "forwarded"
            || HOP_BY_HOP.contains(&lower.as_str())
        {
            continue;
        }
        upstream = upstream.header(name, value);
    }

    // Set forwarding headers
    upstream = upstream
        .header("x-forwarded-for", remote_addr.ip().to_string())
        .header("x-forwarded-host", &host)
        .header("x-forwarded-proto", &state.config.proxy_scheme)
        .header(HOST, &host);

    // Inject project ID for function-only projects (global dispatcher needs it)
    if state.runtime_backend.is_function_only(project_id).await {
        upstream = upstream.header("x-rift-project-id", project_id.to_string());
    }

    let upstream_req = upstream
        .body(Full::new(body_bytes))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, pid))?;

    // Forward
    let upstream_resp = tokio::time::timeout(UPSTREAM_REQUEST_TIMEOUT, client.request(upstream_req))
        .await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, pid))?
        .map_err(|_| (StatusCode::BAD_GATEWAY, pid))?;

    // Build response
    let status = upstream_resp.status();
    let mut response = Response::builder().status(status);

    for (name, value) in upstream_resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&lower.as_str()) {
            continue;
        }
        response = response.header(name, value);
    }

    let resp_bytes = collect_body_limited(upstream_resp.into_body(), StatusCode::BAD_GATEWAY)
        .await
        .map_err(|status| (status, pid))?;

    let resp = response
        .body(Full::new(resp_bytes))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, pid))?;

    Ok((resp, pid, cold_start))
}

async fn resolve_project_id(state: &AppState, host: &str) -> Result<Option<Uuid>, AppError> {
    // 1. Check the routing cache first (hot path — no DB hit).
    match state.routing_cache.lookup(host).await {
        CacheLookup::Hit(project_id) => return Ok(Some(project_id)),
        CacheLookup::NegativeHit => return Ok(None),
        CacheLookup::Miss => {}
    }

    // 2. Check distributed state store routing entries (multi-node).
    if let Some(entry) = state.state_store.get_routing(host).await? {
        state
            .routing_cache
            .insert(host.to_owned(), entry.project_id)
            .await;
        return Ok(Some(entry.project_id));
    }

    // 3. Cache miss — fall through to DB queries.
    if let Some(project_id) = domains::get_project_id_by_domain(&state.pool, host).await? {
        state
            .routing_cache
            .insert(host.to_owned(), project_id)
            .await;
        sync_distributed_route(state, host, project_id).await;
        return Ok(Some(project_id));
    }

    if let Some(subdomain) = match_subdomain(host, &state.config.base_domain) {
        if let Some(project) = projects::get_project_by_subdomain(&state.pool, subdomain).await? {
            state
                .routing_cache
                .insert(host.to_owned(), project.id)
                .await;
            sync_distributed_route(state, host, project.id).await;
            return Ok(Some(project.id));
        }
    }

    // 4. Not found — insert negative entry.
    state.routing_cache.insert_negative(host.to_owned()).await;
    Ok(None)
}

async fn sync_distributed_route(state: &AppState, host: &str, project_id: Uuid) {
    let entry = RoutingEntry {
        host: host.to_owned(),
        project_id,
        deployment_id: state
            .runtime_backend
            .active_deployment_id(project_id)
            .await
            .unwrap_or_else(Uuid::nil),
        worker_addr: state.config.proxy_addr(),
        version: 1,
    };

    if let Err(e) = state.state_store.set_routing(&entry).await {
        tracing::warn!(host = %host, error = %e, "failed to persist distributed route");
        return;
    }
    if let Err(e) = state.state_store.publish_routing_update(&entry).await {
        tracing::warn!(host = %host, error = %e, "failed to publish routing update");
    }
}

fn extract_host(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(HOST)?.to_str().ok()?.trim().to_lowercase();
    Some(host.split(':').next()?.to_owned())
}

/// Extract the subdomain from a host given a base domain suffix.
///
/// Returns `None` if the host does not end with `.{base_domain}` or
/// if the subdomain portion is empty.
pub(crate) fn match_subdomain<'a>(host: &'a str, base_domain: &str) -> Option<&'a str> {
    let suffix = format!(".{base_domain}");
    host.strip_suffix(&suffix).filter(|s| !s.is_empty())
}

fn error_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(
            status.canonical_reason().unwrap_or("Error"),
        )))
        .unwrap()
}

/// Handle a function request via the V8 isolate pool — no HTTP hop.
#[cfg(feature = "v8-isolate")]
async fn handle_isolate_invoke(
    req: Request<Incoming>,
    remote_addr: SocketAddr,
    isolate_pool: &crate::runtime::isolate::IsolatePool,
    project_id: Uuid,
    host: &str,
    state: &AppState,
) -> Result<(Response<Full<Bytes>>, Option<Uuid>, bool), (StatusCode, Option<Uuid>)> {
    let pid = Some(project_id);

    // Decompose request
    let (parts, body) = req.into_parts();

    // Read body
    let body_bytes = collect_body_limited(body, StatusCode::BAD_REQUEST)
        .await
        .map_err(|status| (status, pid))?;

    // Build the full URL the handler expects
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let url = format!("{}://{}{}", state.config.proxy_scheme, host, path_and_query);

    // Collect headers as (String, String) pairs
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in &parts.headers {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "host"
            || lower.starts_with("x-forwarded-")
            || lower == "forwarded"
            || HOP_BY_HOP.contains(&lower.as_str())
        {
            continue;
        }
        if let Ok(v) = value.to_str() {
            headers.push((name.to_string(), v.to_string()));
        }
    }
    headers.push(("x-forwarded-for".to_string(), remote_addr.ip().to_string()));
    headers.push(("x-forwarded-host".to_string(), host.to_string()));
    headers.push((
        "x-forwarded-proto".to_string(),
        state.config.proxy_scheme.clone(),
    ));
    headers.push(("host".to_string(), host.to_string()));

    let method = parts.method.as_str();
    let body_opt = if body_bytes.is_empty() {
        None
    } else {
        Some(body_bytes)
    };

    // Invoke directly in V8 — no HTTP hop
    let isolate_resp = isolate_pool
        .invoke(project_id, method, &url, &headers, body_opt)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, project_id = %project_id, "isolate invoke failed");
            (map_app_error(e), pid)
        })?;

    // Build hyper Response from IsolateResponse
    let mut response = Response::builder().status(
        StatusCode::from_u16(isolate_resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
    );
    for (k, v) in &isolate_resp.headers {
        if let (Ok(name), Ok(val)) = (
            hyper::header::HeaderName::from_bytes(k.as_bytes()),
            hyper::header::HeaderValue::from_str(v),
        ) {
            response = response.header(name, val);
        }
    }

    let resp = response
        .body(Full::new(isolate_resp.body))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, pid))?;

    Ok((resp, pid, false))
}

fn map_app_error(error: AppError) -> StatusCode {
    match error {
        AppError::NotFound(_) => StatusCode::NOT_FOUND,
        AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
        AppError::Conflict(_) => StatusCode::CONFLICT,
        AppError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
        AppError::Unauthorized(_) | AppError::Forbidden(_) => StatusCode::FORBIDDEN,
        AppError::Db(_) | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn collect_body_limited(
    mut body: Incoming,
    read_error_status: StatusCode,
) -> Result<Bytes, StatusCode> {
    let mut buffer = BytesMut::new();

    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| read_error_status)?;
        if let Ok(data) = frame.into_data() {
            if buffer.len() + data.len() > MAX_PROXY_BODY_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            buffer.extend_from_slice(&data);
        }
    }

    Ok(buffer.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_host tests ---

    #[test]
    fn extract_host_strips_port() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "example.com:8080".parse().unwrap());
        assert_eq!(extract_host(&headers), Some("example.com".to_owned()));
    }

    #[test]
    fn extract_host_lowercases() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "Example.COM".parse().unwrap());
        assert_eq!(extract_host(&headers), Some("example.com".to_owned()));
    }

    #[test]
    fn extract_host_none_on_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_host(&headers), None);
    }

    #[test]
    fn extract_host_no_port() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "mysite.rift.dev".parse().unwrap());
        assert_eq!(extract_host(&headers), Some("mysite.rift.dev".to_owned()));
    }

    // --- match_subdomain tests ---

    #[test]
    fn match_subdomain_basic() {
        assert_eq!(match_subdomain("myapp.rift.dev", "rift.dev"), Some("myapp"));
    }

    #[test]
    fn match_subdomain_nested() {
        assert_eq!(
            match_subdomain("sub.myapp.rift.dev", "rift.dev"),
            Some("sub.myapp")
        );
    }

    #[test]
    fn match_subdomain_exact_base_returns_none() {
        assert_eq!(match_subdomain("rift.dev", "rift.dev"), None);
    }

    #[test]
    fn match_subdomain_different_domain() {
        assert_eq!(match_subdomain("myapp.example.com", "rift.dev"), None);
    }

    #[test]
    fn match_subdomain_partial_suffix_mismatch() {
        assert_eq!(match_subdomain("myapp.notrift.dev", "rift.dev"), None);
    }

    #[test]
    fn match_subdomain_empty_host() {
        assert_eq!(match_subdomain("", "rift.dev"), None);
    }
}
