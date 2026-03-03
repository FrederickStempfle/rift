use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, header::HOST, HeaderMap, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::{
    api::AppState,
    db::{domains, projects},
    error::AppError,
    proxy::{
        access_bot_guard::MitigationAction,
        analytics_collector::RequestEvent,
        client_ip::extract_client_ip,
        routing_cache::CacheLookup,
        waf::{WafAction, WafRequestContext},
    },
    services::abuse::{AbuseDecision, AbuseLimit},
    state::RoutingEntry,
    ws::traffic::TrafficEvent,
};

type HttpClient = Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>;

const MAX_PROXY_BODY_BYTES: usize = 10 * 1024 * 1024;
const MAX_PROXY_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
const PROXY_GLOBAL_LIMIT: u64 = 2000;
const PROXY_GLOBAL_CHALLENGE: u64 = 1400;
const PROXY_ROUTE_LIMIT: u64 = 600;
const PROXY_ROUTE_CHALLENGE: u64 = 420;
const PROXY_TOKEN_LIMIT: u64 = 900;
const PROXY_TOKEN_CHALLENGE: u64 = 650;
const PROXY_PROJECT_LIMIT: u64 = 700;
const PROXY_PROJECT_CHALLENGE: u64 = 500;
const CHALLENGE_VERIFY_PATH: &str = "/__rift/challenge/verify";
const ROBOTS_TXT_PATH: &str = "/robots.txt";

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

enum RouteError {
    Status(StatusCode, Option<Uuid>),
    Response(Response<Full<Bytes>>, Option<Uuid>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HoneypotMode {
    Challenge,
    Block,
    Ban,
}

pub async fn handle_request(
    req: Request<Incoming>,
    remote_addr: SocketAddr,
    client: HttpClient,
    state: AppState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let start = std::time::Instant::now();
    let client_ip = extract_client_ip(
        remote_addr.ip(),
        req.headers(),
        state.trusted_proxy_cidrs.as_ref().as_slice(),
    );

    // Extract analytics metadata before forwarding (req headers will be consumed)
    let analytics_path = req.uri().path().to_owned();
    let analytics_method = req.method().as_str().to_owned();
    let analytics_host = extract_host(req.headers());
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

    let result = route_and_forward(req, remote_addr, &client, &state, client_ip).await;

    let (status_code, project_id, cold_start) = match &result {
        Ok((resp, pid, cs)) => (resp.status().as_u16(), *pid, *cs),
        Err(RouteError::Status(sc, pid)) => (sc.as_u16(), *pid, false),
        Err(RouteError::Response(resp, pid)) => (resp.status().as_u16(), *pid, false),
    };
    if analytics_path != CHALLENGE_VERIFY_PATH {
        state
            .access_bot_guard
            .observe(client_ip, &analytics_path, status_code);
    }

    let event_timestamp = Utc::now();
    let event_duration_ms = start.elapsed().as_millis() as u64;
    state.analytics_collector.record(RequestEvent {
        project_id,
        timestamp: event_timestamp,
        client_ip,
        host: analytics_host.clone(),
        method: analytics_method.clone(),
        status: status_code,
        duration_ms: event_duration_ms,
        cold_start,
        path: analytics_path,
        referer: analytics_referer,
    });

    // Fire-and-forget: geolocate client IP and broadcast live traffic event.
    {
        let geoip = state.geoip.clone();
        let broadcaster = state.traffic_broadcaster.clone();
        let dst_lat = state.config.server_lat;
        let dst_lng = state.config.server_lng;
        tokio::spawn(async move {
            if let Some((src_lat, src_lng, country)) = geoip.lookup(client_ip).await {
                broadcaster.broadcast(TrafficEvent {
                    timestamp: event_timestamp,
                    src_lat,
                    src_lng,
                    dst_lat,
                    dst_lng,
                    method: analytics_method,
                    status: status_code,
                    host: analytics_host,
                    country,
                    duration_ms: event_duration_ms,
                });
            }
        });
    }

    match result {
        Ok((resp, _, _)) => Ok(resp),
        Err(RouteError::Status(sc, _)) => Ok(error_response(sc)),
        Err(RouteError::Response(resp, _)) => Ok(resp),
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
    client_ip: IpAddr,
) -> Result<(Response<Full<Bytes>>, Option<Uuid>, bool), RouteError> {
    let _inflight_permit = acquire_proxy_inflight_permit(&state.proxy_inflight)
        .map_err(|response| RouteError::Response(response, None))?;

    if req.uri().path() == CHALLENGE_VERIFY_PATH {
        let response = handle_challenge_verify(req, client_ip, state).await;
        return Ok((response, None, false));
    }

    let host =
        extract_host(req.headers()).ok_or(RouteError::Status(StatusCode::BAD_REQUEST, None))?;
    let path_bucket = route_bucket(req.uri().path());
    let return_to = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_owned();
    let trusted = state
        .abuse_guard
        .should_bypass_proxy_limits(client_ip, req.headers())
        .await;
    if state.config.honeypot_robots_enabled {
        let honeypot_paths = state.config.parsed_honeypot_paths();
        if req.uri().path() == ROBOTS_TXT_PATH {
            return Ok((honeypot_robots_response(&honeypot_paths), None, false));
        }

        if !trusted && is_honeypot_probe_path(req.uri().path(), &honeypot_paths) {
            let reason = format!("honeypot path accessed: {}", req.uri().path());
            match parse_honeypot_mode(&state.config.honeypot_mode) {
                HoneypotMode::Challenge => {
                    return Err(RouteError::Response(
                        proxy_challenge_response(
                            state,
                            client_ip,
                            req.headers(),
                            60,
                            &reason,
                            &return_to,
                        ),
                        None,
                    ));
                }
                HoneypotMode::Block => {
                    return Err(RouteError::Response(
                        proxy_abuse_response(
                            StatusCode::FORBIDDEN,
                            "honeypot_block",
                            state.config.honeypot_ban_window_secs.max(1),
                            &reason,
                        ),
                        None,
                    ));
                }
                HoneypotMode::Ban => {
                    if let Some(response) = enforce_proxy_limit(
                        state,
                        AbuseLimit::per_ip(
                            "proxy.honeypot_ip",
                            client_ip,
                            "honeypot",
                            0,
                            Duration::from_secs(state.config.honeypot_ban_window_secs.max(60)),
                            None,
                        ),
                        client_ip,
                        req.headers(),
                        &return_to,
                    )
                    .await
                    .map_err(|e| RouteError::Status(map_app_error(e), None))?
                    {
                        return Err(RouteError::Response(response, None));
                    }
                    return Err(RouteError::Response(
                        proxy_abuse_response(
                            StatusCode::FORBIDDEN,
                            "honeypot_ban",
                            state.config.honeypot_ban_window_secs.max(1),
                            &reason,
                        ),
                        None,
                    ));
                }
            }
        }
    }

    if !trusted {
        if let Some(mitigation) = state
            .access_bot_guard
            .evaluate_with_path(client_ip, req.uri().path())
        {
            let response = match mitigation.action {
                MitigationAction::Challenge => proxy_challenge_response(
                    state,
                    client_ip,
                    req.headers(),
                    mitigation.retry_after_secs,
                    &mitigation.reason,
                    &return_to,
                ),
                MitigationAction::Block => proxy_abuse_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "access_bot_block",
                    mitigation.retry_after_secs,
                    &mitigation.reason,
                ),
            };
            return Err(RouteError::Response(response, None));
        }

        let global = state.abuse_guard.resolve_limit(
            "proxy.global_ip",
            None,
            PROXY_GLOBAL_LIMIT,
            Duration::from_secs(10),
            Some(PROXY_GLOBAL_CHALLENGE),
        );
        if global.enabled {
            if let Some(response) = enforce_proxy_limit(
                state,
                AbuseLimit::per_ip(
                    "proxy.global_ip",
                    client_ip,
                    "global",
                    global.limit,
                    global.window,
                    global.challenge_after,
                ),
                client_ip,
                req.headers(),
                &return_to,
            )
            .await
            .map_err(|e| RouteError::Status(map_app_error(e), None))?
            {
                return Err(RouteError::Response(response, None));
            }
        }

        let route = state.abuse_guard.resolve_limit(
            "proxy.route_ip",
            None,
            PROXY_ROUTE_LIMIT,
            Duration::from_secs(10),
            Some(PROXY_ROUTE_CHALLENGE),
        );
        if route.enabled {
            if let Some(response) = enforce_proxy_limit(
                state,
                AbuseLimit::per_ip(
                    "proxy.route_ip",
                    client_ip,
                    format!("route:{host}:{path_bucket}"),
                    route.limit,
                    route.window,
                    route.challenge_after,
                ),
                client_ip,
                req.headers(),
                &return_to,
            )
            .await
            .map_err(|e| RouteError::Status(map_app_error(e), None))?
            {
                return Err(RouteError::Response(response, None));
            }
        }

        if let Some(token_fingerprint) = bearer_fingerprint(req.headers()) {
            let token = state.abuse_guard.resolve_limit(
                "proxy.token",
                None,
                PROXY_TOKEN_LIMIT,
                Duration::from_secs(10),
                Some(PROXY_TOKEN_CHALLENGE),
            );
            if token.enabled {
                if let Some(response) = enforce_proxy_limit(
                    state,
                    AbuseLimit {
                        scope: "proxy.token",
                        actor_key: format!("token:{token_fingerprint}"),
                        bucket_key: format!("scope:proxy.token:token:{token_fingerprint}"),
                        limit: token.limit,
                        window: token.window,
                        challenge_after: token.challenge_after,
                    },
                    client_ip,
                    req.headers(),
                    &return_to,
                )
                .await
                .map_err(|e| RouteError::Status(map_app_error(e), None))?
                {
                    return Err(RouteError::Response(response, None));
                }
            }
        }
    }

    // --- Global WAF check (before project resolution) ---
    if state.config.waf_enabled {
        let waf_ctx = build_waf_context(client_ip, &req);
        if let Some(response) = evaluate_waf(state, None, &waf_ctx, &return_to).await {
            return Err(RouteError::Response(response, None));
        }
    }

    // --- Check for direct service routing (target_url) before project resolution ---
    if let Some(direct_url) = resolve_direct_target(state, &host).await
        .map_err(|e| RouteError::Status(map_app_error(e), None))?
    {
        return forward_direct(req, client_ip, &host, &direct_url, &return_to, state, client).await;
    }

    let project_id = resolve_project_id(state, &host)
        .await
        .map_err(|e| RouteError::Status(map_app_error(e), None))?
        .ok_or(RouteError::Status(StatusCode::NOT_FOUND, None))?;

    let pid = Some(project_id);

    if !trusted {
        let project = state.abuse_guard.resolve_limit(
            "proxy.project_ip",
            Some(project_id),
            PROXY_PROJECT_LIMIT,
            Duration::from_secs(10),
            Some(PROXY_PROJECT_CHALLENGE),
        );
        if project.enabled {
            if let Some(response) = enforce_proxy_limit(
                state,
                AbuseLimit::per_ip(
                    "proxy.project_ip",
                    client_ip,
                    format!("project:{project_id}"),
                    project.limit,
                    project.window,
                    project.challenge_after,
                ),
                client_ip,
                req.headers(),
                &return_to,
            )
            .await
            .map_err(|e| RouteError::Status(map_app_error(e), pid))?
            {
                return Err(RouteError::Response(response, pid));
            }
        }
    }

    // Firewall check
    let allowed = state
        .firewall_cache
        .is_allowed(&state.pool, project_id, client_ip)
        .await
        .map_err(|e| RouteError::Status(map_app_error(e), pid))?;
    if !allowed {
        return Err(RouteError::Status(StatusCode::FORBIDDEN, pid));
    }

    // --- Project WAF check (after project resolution + firewall) ---
    if state.config.waf_enabled {
        let waf_ctx = build_waf_context(client_ip, &req);
        if let Some(response) = evaluate_waf(state, Some(project_id), &waf_ctx, &return_to).await {
            return Err(RouteError::Response(response, pid));
        }
    }

    // V8 isolate pool: handle function-only projects directly (no HTTP hop)
    #[cfg(feature = "v8-isolate")]
    if let Some(ref isolate_pool) = state.isolate_pool {
        if isolate_pool.is_registered(project_id).await {
            return handle_isolate_invoke(req, client_ip, isolate_pool, project_id, &host, state)
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
                    return Err(RouteError::Status(StatusCode::SERVICE_UNAVAILABLE, pid));
                }
                Err(error) => {
                    tracing::warn!(%project_id, error = %error, "wake failed");
                    return Err(RouteError::Status(map_app_error(error), pid));
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
        .map_err(|_| RouteError::Status(StatusCode::BAD_REQUEST, pid))?;

    // Decompose request
    let (parts, body) = req.into_parts();

    // Read body (bounded)
    let body_bytes = collect_body_limited(body, StatusCode::BAD_REQUEST, MAX_PROXY_BODY_BYTES)
        .await
        .map_err(|status| RouteError::Status(status, pid))?;

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
        .header("x-forwarded-for", client_ip.to_string())
        .header("x-forwarded-host", &host)
        .header("x-forwarded-proto", &state.config.proxy_scheme)
        .header(HOST, &host);

    // Inject project ID for function-only projects (global dispatcher needs it)
    if state.runtime_backend.is_function_only(project_id).await {
        upstream = upstream.header("x-rift-project-id", project_id.to_string());
    }

    let upstream_req = upstream
        .body(Full::new(body_bytes))
        .map_err(|_| RouteError::Status(StatusCode::INTERNAL_SERVER_ERROR, pid))?;

    // Forward
    let upstream_timeout = Duration::from_millis(state.config.proxy_upstream_timeout_ms.max(500));
    let upstream_resp = tokio::time::timeout(upstream_timeout, client.request(upstream_req))
        .await
        .map_err(|_| RouteError::Status(StatusCode::GATEWAY_TIMEOUT, pid))?
        .map_err(|_| RouteError::Status(StatusCode::BAD_GATEWAY, pid))?;

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

    let resp_bytes = collect_body_limited(upstream_resp.into_body(), StatusCode::BAD_GATEWAY, MAX_PROXY_RESPONSE_BYTES)
        .await
        .map_err(|status| RouteError::Status(status, pid))?;

    let resp = response
        .body(Full::new(resp_bytes))
        .map_err(|_| RouteError::Status(StatusCode::INTERNAL_SERVER_ERROR, pid))?;

    Ok((resp, pid, cold_start))
}

pub(crate) async fn resolve_project_id(
    state: &AppState,
    host: &str,
) -> Result<Option<Uuid>, AppError> {
    // 1. Check the routing cache first (hot path — no DB hit).
    match state.routing_cache.lookup(host).await {
        CacheLookup::Hit(project_id) => return Ok(Some(project_id)),
        CacheLookup::DirectHit(_) => return Ok(None), // handled by resolve_direct_target
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

/// Resolve a direct target_url for service-assigned domains.
/// Returns `Some(target_url)` if this host routes directly to a service,
/// `None` if it should fall through to project resolution.
async fn resolve_direct_target(
    state: &AppState,
    host: &str,
) -> Result<Option<String>, AppError> {
    // Check cache first.
    match state.routing_cache.lookup(host).await {
        CacheLookup::DirectHit(url) => return Ok(Some(url)),
        CacheLookup::Hit(_) | CacheLookup::NegativeHit => return Ok(None),
        CacheLookup::Miss => {}
    }

    // Cache miss — check DB for a target_url.
    if let Some(target_url) = domains::get_target_url_by_domain(&state.pool, host).await? {
        state
            .routing_cache
            .insert_direct(host.to_owned(), target_url.clone())
            .await;
        return Ok(Some(target_url));
    }

    Ok(None)
}

/// Forward a request directly to a target URL (for service-assigned domains).
/// This bypasses all project-specific logic (firewall, WAF, runtime backend).
async fn forward_direct(
    req: Request<Incoming>,
    client_ip: IpAddr,
    host: &str,
    target_base: &str,
    _return_to: &str,
    state: &AppState,
    client: &HttpClient,
) -> Result<(Response<Full<Bytes>>, Option<Uuid>, bool), RouteError> {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let target_url: Uri = format!("{target_base}{path_and_query}")
        .parse()
        .map_err(|_| RouteError::Status(StatusCode::BAD_REQUEST, None))?;

    let (parts, body) = req.into_parts();

    let body_bytes = collect_body_limited(body, StatusCode::BAD_REQUEST, MAX_PROXY_BODY_BYTES)
        .await
        .map_err(|status| RouteError::Status(status, None))?;

    let mut upstream = Request::builder()
        .method(parts.method.clone())
        .uri(&target_url);

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

    upstream = upstream
        .header("x-forwarded-for", client_ip.to_string())
        .header("x-forwarded-host", host)
        .header("x-forwarded-proto", &state.config.proxy_scheme)
        .header(HOST, host);

    let upstream_req = upstream
        .body(Full::new(body_bytes))
        .map_err(|_| RouteError::Status(StatusCode::INTERNAL_SERVER_ERROR, None))?;

    let upstream_timeout = Duration::from_millis(state.config.proxy_upstream_timeout_ms.max(500));
    let upstream_resp = tokio::time::timeout(upstream_timeout, client.request(upstream_req))
        .await
        .map_err(|_| RouteError::Status(StatusCode::GATEWAY_TIMEOUT, None))?
        .map_err(|_| RouteError::Status(StatusCode::BAD_GATEWAY, None))?;

    let status = upstream_resp.status();
    let mut response = Response::builder().status(status);

    for (name, value) in upstream_resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&lower.as_str()) {
            continue;
        }
        response = response.header(name, value);
    }

    let resp_bytes = collect_body_limited(upstream_resp.into_body(), StatusCode::BAD_GATEWAY, MAX_PROXY_RESPONSE_BYTES)
        .await
        .map_err(|status| RouteError::Status(status, None))?;

    let resp = response
        .body(Full::new(resp_bytes))
        .map_err(|_| RouteError::Status(StatusCode::INTERNAL_SERVER_ERROR, None))?;

    Ok((resp, None, false))
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

async fn enforce_proxy_limit(
    state: &AppState,
    limit: AbuseLimit,
    client_ip: IpAddr,
    headers: &HeaderMap,
    return_to: &str,
) -> Result<Option<Response<Full<Bytes>>>, AppError> {
    match state.abuse_guard.enforce(limit).await? {
        AbuseDecision::Allow => Ok(None),
        AbuseDecision::Challenge {
            retry_after_secs,
            reason,
        } => Ok(Some(proxy_challenge_response(
            state,
            client_ip,
            headers,
            retry_after_secs,
            &reason,
            return_to,
        ))),
        AbuseDecision::Block {
            retry_after_secs,
            reason,
            tier: _,
        } => Ok(Some(proxy_abuse_response(
            StatusCode::TOO_MANY_REQUESTS,
            "block",
            retry_after_secs,
            &reason,
        ))),
    }
}

fn proxy_challenge_response(
    state: &AppState,
    client_ip: IpAddr,
    headers: &HeaderMap,
    retry_after_secs: u64,
    reason: &str,
    return_to: &str,
) -> Response<Full<Bytes>> {
    let return_to = sanitize_return_to(return_to);
    let ticket = state
        .abuse_guard
        .issue_challenge_ticket(client_ip, headers, &return_to);
    let reason_html = escape_html(reason);
    let return_to_html = escape_html_attr(&return_to);
    let ticket_html = escape_html_attr(&ticket);
    let turnstile_html = if let Some(site_key) = turnstile_site_key(state) {
        format!(
            "<div class=\"widget-wrap\">\
<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\" async defer></script>\
<div class=\"cf-turnstile\" data-sitekey=\"{}\"></div></div>",
            escape_html_attr(site_key)
        )
    } else {
        "<label class=\"check-label\">\
<input type=\"checkbox\" required>\
<div><div class=\"check-text\">I confirm this request is human-initiated</div>\
<div class=\"check-sub\">Automated tools and bots cannot proceed past this point</div></div>\
</label>"
            .to_owned()
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head>\
<meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>Security Check</title>\
<style>\
@keyframes fadein{{from{{opacity:0;transform:translateY(8px)}}to{{opacity:1;transform:translateY(0)}}}}\
@keyframes shrink{{from{{width:100%}}to{{width:0%}}}}\
*,*::before,*::after{{box-sizing:border-box;margin:0;padding:0}}\
body{{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif;background:#f8fafc;min-height:100dvh;display:flex;align-items:center;justify-content:center;padding:24px;color:#0f172a}}\
.wrap{{width:100%;max-width:400px;animation:fadein .3s cubic-bezier(.16,1,.3,1) both}}\
.brand{{display:flex;align-items:center;gap:7px;margin-bottom:24px;justify-content:center}}\
.brand-pip{{width:6px;height:6px;background:#0f172a;border-radius:50%}}\
.brand-label{{font-size:.6875rem;font-weight:600;letter-spacing:.08em;text-transform:uppercase;color:#94a3b8}}\
.card{{background:#fff;border:1px solid #e2e8f0;border-radius:20px;padding:36px;box-shadow:0 4px 6px -2px rgba(15,23,42,.04),0 16px 48px -8px rgba(15,23,42,.1)}}\
.icon-ring{{width:48px;height:48px;background:linear-gradient(145deg,#f0f9ff,#e0f2fe);border:1px solid #bae6fd;border-radius:13px;display:flex;align-items:center;justify-content:center;margin-bottom:22px}}\
h1{{font-size:1.125rem;font-weight:700;letter-spacing:-.025em;color:#0f172a;margin-bottom:8px}}\
.sub{{font-size:.875rem;color:#64748b;line-height:1.65;margin-bottom:22px}}\
.reason-box{{display:flex;align-items:flex-start;gap:10px;background:#f8fafc;border:1px solid #f1f5f9;border-left:3px solid #cbd5e1;border-radius:8px;padding:11px 13px;font-size:.8125rem;color:#475569;line-height:1.5;margin-bottom:20px}}\
.timer-row{{display:flex;align-items:center;gap:7px;font-size:.75rem;color:#94a3b8;margin-bottom:22px}}\
.bar-track{{flex:1;height:2px;background:#f1f5f9;border-radius:99px;overflow:hidden}}\
.bar-fill{{height:100%;width:100%;background:#e2e8f0;border-radius:99px;animation:shrink {retry_after_secs}s linear forwards}}\
.check-label{{display:flex;align-items:flex-start;gap:12px;background:#f8fafc;border:1px solid #e2e8f0;border-radius:12px;padding:16px;cursor:pointer;transition:border-color .15s,background .15s;margin-bottom:16px;-webkit-user-select:none;user-select:none}}\
.check-label:hover{{border-color:#cbd5e1}}\
.check-label input[type=checkbox]{{margin-top:2px;width:15px;height:15px;flex-shrink:0;accent-color:#0f172a;cursor:pointer}}\
.check-text{{font-size:.875rem;color:#334155;line-height:1.5}}\
.check-sub{{font-size:.75rem;color:#94a3b8;margin-top:3px}}\
.widget-wrap{{margin-bottom:16px}}\
button[type=submit]{{width:100%;background:#0f172a;color:#fff;border:none;border-radius:12px;padding:13px 18px;font-size:.9375rem;font-weight:600;letter-spacing:-.015em;cursor:pointer;display:flex;align-items:center;justify-content:center;gap:8px;transition:background .15s,transform .1s;margin-top:2px}}\
button[type=submit]:hover{{background:#1e293b}}\
button[type=submit]:active{{transform:scale(.98)}}\
.footer{{margin-top:20px;display:flex;align-items:center;justify-content:center;gap:5px;font-size:.6875rem;color:#cbd5e1;letter-spacing:.02em}}\
</style></head>\
<body><div class=\"wrap\">\
<div class=\"brand\"><div class=\"brand-pip\"></div><span class=\"brand-label\">Rift Security</span></div>\
<div class=\"card\">\
<div class=\"icon-ring\">\
<svg width=\"22\" height=\"22\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#38bdf8\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<path d=\"M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z\"/><polyline points=\"9 12 11 14 15 10\"/>\
</svg>\
</div>\
<h1>Verify you&#8217;re human</h1>\
<p class=\"sub\">An automated check flagged this request. Complete the step below to continue.</p>\
<div class=\"reason-box\">\
<svg width=\"14\" height=\"14\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#94a3b8\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" style=\"flex-shrink:0;margin-top:1px\">\
<circle cx=\"12\" cy=\"12\" r=\"10\"/><line x1=\"12\" y1=\"8\" x2=\"12\" y2=\"12\"/><line x1=\"12\" y1=\"16\" x2=\"12.01\" y2=\"16\"/>\
</svg>\
<span>{reason_html}</span>\
</div>\
<div class=\"timer-row\">\
<svg width=\"12\" height=\"12\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<circle cx=\"12\" cy=\"12\" r=\"10\"/><polyline points=\"12 6 12 12 16 14\"/>\
</svg>\
<span>{retry_after_secs}s window</span>\
<div class=\"bar-track\"><div class=\"bar-fill\"></div></div>\
</div>\
<form method=\"post\" action=\"{CHALLENGE_VERIFY_PATH}\">\
<input type=\"hidden\" name=\"ticket\" value=\"{ticket_html}\">\
<input type=\"hidden\" name=\"return_to\" value=\"{return_to_html}\">\
{turnstile_html}\
<button type=\"submit\">Continue\
<svg width=\"15\" height=\"15\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<path d=\"M5 12h14M12 5l7 7-7 7\"/>\
</svg>\
</button>\
</form>\
<div class=\"footer\">\
<svg width=\"11\" height=\"11\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<rect x=\"3\" y=\"11\" width=\"18\" height=\"11\" rx=\"2\"/><path d=\"M7 11V7a5 5 0 0 1 10 0v4\"/>\
</svg>\
Protected by Rift\
</div></div></div></body></html>"
    );
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-store")
        .header("retry-after", retry_after_secs.to_string())
        .header("x-rift-abuse-action", "challenge")
        .header("x-rift-challenge", "required")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| error_response(StatusCode::FORBIDDEN))
}

async fn handle_challenge_verify(
    req: Request<Incoming>,
    client_ip: IpAddr,
    state: &AppState,
) -> Response<Full<Bytes>> {
    if req.method() != hyper::Method::POST {
        return proxy_abuse_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "challenge_method_not_allowed",
            1,
            "challenge verification must use POST",
        );
    }

    let headers = req.headers().clone();
    let (_, body) = req.into_parts();
    let body_bytes = match collect_body_limited(body, StatusCode::BAD_REQUEST, MAX_PROXY_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(status) => return error_response(status),
    };
    let form: std::collections::HashMap<String, String> =
        serde_urlencoded::from_bytes(&body_bytes).unwrap_or_default();
    let return_to = sanitize_return_to(form.get("return_to").map(|s| s.as_str()).unwrap_or("/"));
    let ticket = form.get("ticket").map(|s| s.as_str()).unwrap_or_default();

    if ticket.is_empty()
        || !state
            .abuse_guard
            .verify_challenge_ticket(client_ip, &headers, &return_to, ticket)
    {
        return proxy_challenge_response(
            state,
            client_ip,
            &headers,
            1,
            "challenge verification failed",
            &return_to,
        );
    }

    if let Some(secret_key) = turnstile_secret_key(state) {
        let token = form
            .get("cf-turnstile-response")
            .map(|s| s.trim())
            .unwrap_or("");
        if token.is_empty() {
            return proxy_challenge_response(
                state,
                client_ip,
                &headers,
                1,
                "captcha token is required",
                &return_to,
            );
        }
        if !verify_turnstile(secret_key, token, client_ip).await {
            return proxy_challenge_response(
                state,
                client_ip,
                &headers,
                1,
                "captcha verification failed",
                &return_to,
            );
        }
    }

    let set_cookie = state.abuse_guard.build_challenge_set_cookie(
        client_ip,
        &headers,
        state.config.proxy_scheme == "https",
    );
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header("location", return_to)
        .header("cache-control", "no-store")
        .header("set-cookie", set_cookie)
        .header("x-rift-abuse-action", "verified")
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| error_response(StatusCode::SEE_OTHER))
}

fn proxy_abuse_response(
    status: StatusCode,
    action: &str,
    retry_after_secs: u64,
    reason: &str,
) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("retry-after", retry_after_secs.to_string())
        .header("x-rift-abuse-action", action)
        .header("x-rift-challenge", "required")
        .body(Full::new(Bytes::from(reason.to_owned())))
        .unwrap_or_else(|_| error_response(status))
}

fn acquire_proxy_inflight_permit(
    semaphore: &Arc<Semaphore>,
) -> Result<OwnedSemaphorePermit, Response<Full<Bytes>>> {
    semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| proxy_overload_response())
}

fn proxy_overload_response() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("retry-after", "1")
        .header("x-rift-overload", "shed")
        .body(Full::new(Bytes::from("proxy overloaded, retry soon")))
        .unwrap_or_else(|_| error_response(StatusCode::SERVICE_UNAVAILABLE))
}

fn honeypot_robots_response(paths: &[String]) -> Response<Full<Bytes>> {
    let mut body = String::from("User-agent: *\n");
    if paths.is_empty() {
        body.push_str("Disallow: /__trap_disabled__\n");
    } else {
        for path in paths {
            body.push_str("Disallow: ");
            body.push_str(path);
            body.push('\n');
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/plain; charset=utf-8")
        .header("cache-control", "public, max-age=300")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| error_response(StatusCode::OK))
}

fn is_honeypot_probe_path(path: &str, honeypot_paths: &[String]) -> bool {
    let path = path.trim().to_ascii_lowercase();
    honeypot_paths.iter().any(|trap| {
        if trap.ends_with('/') {
            path.starts_with(trap)
        } else {
            path == *trap
        }
    })
}

fn parse_honeypot_mode(raw: &str) -> HoneypotMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "challenge" => HoneypotMode::Challenge,
        "block" => HoneypotMode::Block,
        _ => HoneypotMode::Ban,
    }
}

fn sanitize_return_to(input: &str) -> String {
    if input.starts_with('/')
        && !input.starts_with("//")
        && !input.bytes().any(|b| b.is_ascii_control())
    {
        return input.to_owned();
    }
    "/".to_owned()
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html_attr(input: &str) -> String {
    escape_html(input)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn turnstile_site_key(state: &AppState) -> Option<&str> {
    match (
        state.config.abuse_turnstile_site_key.as_deref(),
        state.config.abuse_turnstile_secret_key.as_deref(),
    ) {
        (Some(site), Some(secret)) if !site.trim().is_empty() && !secret.trim().is_empty() => {
            Some(site.trim())
        }
        _ => None,
    }
}

fn turnstile_secret_key(state: &AppState) -> Option<&str> {
    state
        .config
        .abuse_turnstile_secret_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Deserialize)]
struct TurnstileResponse {
    success: bool,
}

async fn verify_turnstile(secret_key: &str, token: &str, client_ip: IpAddr) -> bool {
    let mut form = std::collections::HashMap::new();
    form.insert("secret", secret_key.to_owned());
    form.insert("response", token.to_owned());
    form.insert("remoteip", client_ip.to_string());

    let client = reqwest::Client::new();
    let resp = match client
        .post("https://challenges.cloudflare.com/turnstile/v0/siteverify")
        .form(&form)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(error) => {
            tracing::warn!(error = %error, "turnstile verification request failed");
            return false;
        }
    };

    match resp.json::<TurnstileResponse>().await {
        Ok(body) => body.success,
        Err(error) => {
            tracing::warn!(error = %error, "turnstile verification decode failed");
            false
        }
    }
}

fn route_bucket(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).take(2).collect();
    if segments.is_empty() {
        return "root".to_owned();
    }
    segments
        .iter()
        .map(|segment| normalize_segment(segment))
        .collect::<Vec<String>>()
        .join("/")
}

fn normalize_segment(segment: &str) -> String {
    if segment.len() > 32 {
        return ":var".to_owned();
    }
    if segment.chars().all(|c| c.is_ascii_digit()) {
        return ":id".to_owned();
    }
    if segment.contains('-')
        && segment.chars().filter(|c| *c == '-').count() >= 2
        && segment.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return ":uuid".to_owned();
    }
    segment.to_owned()
}

fn bearer_fingerprint(headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))?;

    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    Some(hex::encode(&digest[..8]))
}

/// Build WAF request context from an incoming request.
fn build_waf_context(client_ip: IpAddr, req: &Request<Incoming>) -> WafRequestContext {
    let method = req.method().as_str().to_owned();
    let host = extract_host(req.headers()).unwrap_or_default();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().unwrap_or("").to_owned();
    let user_agent = req
        .headers()
        .get(hyper::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|val| (k.as_str().to_owned(), val.to_owned()))
        })
        .collect();

    WafRequestContext {
        client_ip_str: client_ip.to_string(),
        client_ip,
        method,
        host,
        path,
        query,
        user_agent,
        headers,
    }
}

/// Evaluate WAF rules and return a response if the request should be blocked/challenged.
/// Returns None if the request is allowed. Respects WAF policy mode and fail-open settings.
async fn evaluate_waf(
    state: &AppState,
    project_id: Option<Uuid>,
    ctx: &WafRequestContext,
    return_to: &str,
) -> Option<Response<Full<Bytes>>> {
    let start = std::time::Instant::now();
    let scope_label = if project_id.is_some() {
        "project"
    } else {
        "global"
    };

    // --- Check policy: disabled means skip entirely ---
    let policy = match crate::db::waf::get_policy(&state.pool, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, scope = scope_label, "WAF policy fetch failed, failing open");
            None
        }
    };
    let mode = policy.as_ref().map_or("active", |p| p.mode.as_str());
    let fail_open = policy.as_ref().map_or(true, |p| p.fail_open);

    if mode == "disabled" {
        return None;
    }

    // --- Fetch compiled rules ---
    let scope_key = if project_id.is_some() {
        project_id
    } else {
        None
    };
    let rules = match state.waf_cache.get_rules(&state.pool, scope_key).await {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(error = %e, scope = scope_label, "WAF rule fetch failed");
            crate::metrics::WAF_DECISION
                .with_label_values(&[scope_label, "error"])
                .inc();
            if fail_open {
                return None;
            }
            // Fail closed — block the request
            return Some(proxy_abuse_response(
                StatusCode::FORBIDDEN,
                "waf_error",
                0,
                "WAF evaluation error",
            ));
        }
    };

    let decision = crate::proxy::waf::evaluate_rules(&rules, ctx);
    let duration = start.elapsed().as_secs_f64();
    crate::metrics::WAF_EVAL_DURATION
        .with_label_values(&[scope_label])
        .observe(duration);

    // --- Emit one event per matched log rule ---
    for (rule_id, rule_name) in &decision.logged_rules {
        crate::metrics::WAF_RULE_MATCH
            .with_label_values(&[rule_name])
            .inc();
        state.waf_cache.record_event(crate::db::waf::NewWafEvent {
            project_id,
            rule_id: Some(*rule_id),
            action: "log".into(),
            client_ip: ctx.client_ip_str.clone(),
            method: ctx.method.clone(),
            host: ctx.host.clone(),
            path: ctx.path.clone(),
            user_agent: ctx.user_agent.clone(),
            rule_name: Some(rule_name.clone()),
        });
    }

    // --- Determine effective action (policy mode can downgrade) ---
    let effective_action = if mode == "log_only" && decision.action.is_terminal() {
        // In log_only mode, record the would-be action but don't enforce it
        WafAction::Log
    } else {
        decision.action
    };

    // Emit event for the terminal match (if any)
    if let Some(ref rule_name) = decision.matched_rule_name {
        crate::metrics::WAF_RULE_MATCH
            .with_label_values(&[rule_name])
            .inc();
        state.waf_cache.record_event(crate::db::waf::NewWafEvent {
            project_id,
            rule_id: decision.matched_rule_id,
            action: decision.action.as_str().into(), // original action, not effective
            client_ip: ctx.client_ip_str.clone(),
            method: ctx.method.clone(),
            host: ctx.host.clone(),
            path: ctx.path.clone(),
            user_agent: ctx.user_agent.clone(),
            rule_name: Some(rule_name.clone()),
        });
    }

    crate::metrics::WAF_DECISION
        .with_label_values(&[scope_label, effective_action.as_str()])
        .inc();

    match effective_action {
        WafAction::Allow | WafAction::Log => None,
        WafAction::Challenge => {
            let mut headers = hyper::HeaderMap::new();
            if let Ok(ua_val) = hyper::header::HeaderValue::from_str(&ctx.user_agent) {
                headers.insert(hyper::header::USER_AGENT, ua_val);
            }
            Some(proxy_challenge_response(
                state,
                ctx.client_ip,
                &headers,
                60,
                &format!(
                    "WAF: {}",
                    decision
                        .matched_rule_name
                        .as_deref()
                        .unwrap_or("rule match")
                ),
                return_to,
            ))
        }
        WafAction::Block => {
            let reason = format!(
                "blocked by WAF rule: {}",
                decision.matched_rule_name.as_deref().unwrap_or("unknown")
            );
            Some(proxy_abuse_response(
                StatusCode::FORBIDDEN,
                "waf_block",
                0,
                &reason,
            ))
        }
    }
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
    client_ip: IpAddr,
    isolate_pool: &crate::runtime::isolate::IsolatePool,
    project_id: Uuid,
    host: &str,
    state: &AppState,
) -> Result<(Response<Full<Bytes>>, Option<Uuid>, bool), RouteError> {
    let pid = Some(project_id);

    // Decompose request
    let (parts, body) = req.into_parts();

    // Read body
    let body_bytes = collect_body_limited(body, StatusCode::BAD_REQUEST, MAX_PROXY_BODY_BYTES)
        .await
        .map_err(|status| RouteError::Status(status, pid))?;

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
    headers.push(("x-forwarded-for".to_string(), client_ip.to_string()));
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
            RouteError::Status(map_app_error(e), pid)
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
        .map_err(|_| RouteError::Status(StatusCode::INTERNAL_SERVER_ERROR, pid))?;

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
    max_bytes: usize,
) -> Result<Bytes, StatusCode> {
    let mut buffer = BytesMut::new();

    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| read_error_status)?;
        if let Ok(data) = frame.into_data() {
            if buffer.len() + data.len() > max_bytes {
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

    #[test]
    fn sanitize_return_to_accepts_internal_paths() {
        assert_eq!(sanitize_return_to("/projects/demo"), "/projects/demo");
        assert_eq!(
            sanitize_return_to("/projects/demo?tab=logs"),
            "/projects/demo?tab=logs"
        );
    }

    #[test]
    fn sanitize_return_to_rejects_external_targets() {
        assert_eq!(sanitize_return_to("https://evil.tld"), "/");
        assert_eq!(sanitize_return_to("//evil.tld"), "/");
    }

    #[test]
    fn honeypot_probe_matches_exact_and_prefix_paths() {
        let paths = vec!["/.env".to_owned(), "/cgi-bin/".to_owned()];
        assert!(is_honeypot_probe_path("/.env", &paths));
        assert!(is_honeypot_probe_path("/cgi-bin/test.sh", &paths));
        assert!(!is_honeypot_probe_path("/pricing", &paths));
    }

    #[test]
    fn honeypot_mode_defaults_to_ban() {
        assert_eq!(parse_honeypot_mode("challenge"), HoneypotMode::Challenge);
        assert_eq!(parse_honeypot_mode("block"), HoneypotMode::Block);
        assert_eq!(parse_honeypot_mode("ban"), HoneypotMode::Ban);
        assert_eq!(parse_honeypot_mode("unknown"), HoneypotMode::Ban);
    }
}
