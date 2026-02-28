use std::net::SocketAddr;

use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, OriginalUri, Request, State},
    http::{
        header::{HOST, HeaderName},
        HeaderMap, Response, StatusCode, Uri,
    },
};
use uuid::Uuid;

use crate::{
    api::AppState,
    db::{deployments, domains, projects},
    error::AppError,
};

const MAX_PROXY_BODY_BYTES: usize = 10 * 1024 * 1024;
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub async fn proxy_request(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    original_uri: OriginalUri,
    request: Request,
) -> Result<Response<Body>, StatusCode> {
    let host = extract_host(request.headers()).ok_or(StatusCode::BAD_REQUEST)?;
    let project_id = resolve_project_id(&state, &host)
        .await
        .map_err(map_proxy_error)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let deployment = deployments::latest_ready_deployment_for_project(&state.pool, project_id)
        .await
        .map_err(map_proxy_error)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let target_base = deployment.url.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let target_url = format!("{}{}", target_base, path_and_query(&original_uri));

    let (parts, body) = request.into_parts();
    let body = to_bytes(body, MAX_PROXY_BODY_BYTES)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    let client = reqwest::Client::new();
    let mut upstream = client.request(parts.method.clone(), target_url);
    upstream = upstream.body(body.to_vec());

    for (name, value) in &parts.headers {
        if should_skip_request_header(name) {
            continue;
        }
        upstream = upstream.header(name, value);
    }

    upstream = upstream
        .header("x-forwarded-for", addr.ip().to_string())
        .header("x-forwarded-host", host.clone())
        .header("x-forwarded-proto", state.config.proxy_scheme.clone())
        .header(HOST, host);

    let upstream_response = upstream
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let status = upstream_response.status();
    let mut response = Response::builder().status(status);
    for (name, value) in upstream_response.headers() {
        if should_skip_response_header(name) {
            continue;
        }
        response = response.header(name, value);
    }

    let bytes = upstream_response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    response
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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

fn path_and_query(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned())
}

fn should_skip_request_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    if lower == "host" || lower.starts_with("x-forwarded-") || lower == "forwarded" {
        return true;
    }
    HOP_BY_HOP_HEADERS.contains(&lower.as_str())
}

fn should_skip_response_header(name: &HeaderName) -> bool {
    HOP_BY_HOP_HEADERS.contains(&name.as_str().to_ascii_lowercase().as_str())
}

fn map_proxy_error(error: AppError) -> StatusCode {
    match error {
        AppError::NotFound(_) => StatusCode::NOT_FOUND,
        AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
        AppError::Conflict(_) => StatusCode::CONFLICT,
        AppError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
        AppError::Unauthorized(_) | AppError::Forbidden(_) => StatusCode::FORBIDDEN,
        AppError::Db(_) | AppError::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
