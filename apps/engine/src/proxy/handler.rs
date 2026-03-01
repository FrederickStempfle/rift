use std::{convert::Infallible, net::SocketAddr};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, header::HOST, HeaderMap, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use uuid::Uuid;

use crate::{
    api::AppState,
    db::{domains, projects},
    error::AppError,
    proxy::analytics_collector::RequestEvent,
};

type HttpClient = Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>;

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
        });
    }

    match result {
        Ok((resp, _, _)) => Ok(resp),
        Err((sc, _)) => Ok(error_response(sc)),
    }
}

/// Return type includes (response, project_id, cold_start).
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

    // Look up active runtime URL (in-memory, no DB hit)
    let (target_base, cold_start) = match state.runtime_backend.active_url(project_id).await {
        Some(url) => {
            state.runtime_backend.touch(project_id).await;
            (url, false)
        }
        None => {
            // Not running — try waking a suspended deployment (cold start)
            match state.runtime_backend.wake(project_id).await {
                Ok(Some(url)) => (url, true),
                Ok(None) => return Err((StatusCode::SERVICE_UNAVAILABLE, pid)),
                Err(_) => return Err((StatusCode::SERVICE_UNAVAILABLE, pid)),
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
    let body_bytes = body
        .collect()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, pid))?
        .to_bytes();

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
    let upstream_resp = client
        .request(upstream_req)
        .await
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

    let resp_bytes = upstream_resp
        .into_body()
        .collect()
        .await
        .map_err(|_| (StatusCode::BAD_GATEWAY, pid))?
        .to_bytes();

    let resp = response
        .body(Full::new(resp_bytes))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, pid))?;

    Ok((resp, pid, cold_start))
}

async fn resolve_project_id(state: &AppState, host: &str) -> Result<Option<Uuid>, AppError> {
    if let Some(project_id) = domains::get_project_id_by_domain(&state.pool, host).await? {
        return Ok(Some(project_id));
    }

    let suffix = format!(".{}", state.config.base_domain);
    if let Some(subdomain) = host.strip_suffix(&suffix) {
        return Ok(projects::get_project_by_subdomain(&state.pool, subdomain)
            .await?
            .map(|project| project.id));
    }

    Ok(None)
}

fn extract_host(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(HOST)?.to_str().ok()?.trim().to_lowercase();
    Some(host.split(':').next()?.to_owned())
}

fn error_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(
            status.canonical_reason().unwrap_or("Error"),
        )))
        .unwrap()
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
