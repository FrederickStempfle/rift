pub mod acme;
pub mod analytics_collector;
pub mod firewall_cache;
pub mod handler;
pub mod routing_cache;
pub mod routing_subscriber;
pub mod tls;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::{client::legacy::Client, rt::TokioExecutor, rt::TokioIo};

use crate::api::AppState;

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    // Always run both HTTP and HTTPS listeners.
    // HTTP handles ACME challenges and redirects to HTTPS once real certs exist.
    // HTTPS serves traffic using either real ACME certs or the self-signed fallback.
    tokio::try_join!(serve_http(state.clone()), serve_https(state))?;
    Ok(())
}

/// HTTP listener that handles ACME challenges and redirects to HTTPS.
/// If no HTTPS listener is running, it falls through to normal proxying.
async fn serve_http(state: AppState) -> anyhow::Result<()> {
    let bind_addr = state.config.proxy_addr();
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind HTTP proxy listener on {bind_addr}"))?;

    tracing::info!(address = %bind_addr, "HTTP proxy server listening (ACME + redirect)");

    let client = build_upstream_client();

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let client = client.clone();
        let state = state.clone();

        tokio::spawn(async move {
            let client = client.clone();
            let state = state.clone();
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let client = client.clone();
                let state = state.clone();
                async move {
                    // Check for ACME challenge
                    if let Some(resp) = handle_acme_or_redirect(&req, &state).await {
                        return Ok::<_, std::convert::Infallible>(resp);
                    }

                    // Not an ACME challenge and we have HTTPS — redirect
                    if state.cert_resolver.has_any_certs().await {
                        let host = req
                            .headers()
                            .get(hyper::header::HOST)
                            .and_then(|v| v.to_str().ok())
                            .map(|h| h.split(':').next().unwrap_or(h).to_owned())
                            .unwrap_or_default();
                        let path = req
                            .uri()
                            .path_and_query()
                            .map(|pq| pq.as_str())
                            .unwrap_or("/");
                        let location = format!("https://{host}{path}");
                        return Ok(Response::builder()
                            .status(StatusCode::MOVED_PERMANENTLY)
                            .header("location", location)
                            .body(Full::new(Bytes::from("Redirecting to HTTPS")))
                            .unwrap());
                    }

                    // No certs yet — proxy normally on HTTP
                    handler::handle_request(req, remote_addr, client, state).await
                }
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                if !is_benign_connection_error(&e) {
                    tracing::debug!(error = %e, peer = %remote_addr, "HTTP proxy connection error");
                }
            }
        });
    }
}

/// HTTPS listener with TLS termination via tokio-rustls.
async fn serve_https(state: AppState) -> anyhow::Result<()> {
    let bind_addr = state.config.https_addr();

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(state.cert_resolver.clone()));

    let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind HTTPS proxy listener on {bind_addr}"))?;

    tracing::info!(address = %bind_addr, "HTTPS proxy server listening");

    let client = build_upstream_client();

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let client = client.clone();
        let state = state.clone();

        tokio::spawn(async move {
            let tls_stream = match tls_acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(error = %e, peer = %remote_addr, "TLS handshake failed");
                    return;
                }
            };

            let io = TokioIo::new(tls_stream);
            let client = client.clone();
            let state = state.clone();
            let service = service_fn(move |req| {
                handler::handle_request(req, remote_addr, client.clone(), state.clone())
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                if !is_benign_connection_error(&e) {
                    tracing::debug!(error = %e, peer = %remote_addr, "HTTPS proxy connection error");
                }
            }
        });
    }
}

async fn handle_acme_or_redirect(
    req: &Request<hyper::body::Incoming>,
    state: &AppState,
) -> Option<Response<Full<Bytes>>> {
    let path = req.uri().path();
    if !path.starts_with("/.well-known/acme-challenge/") {
        return None;
    }

    let token = path.strip_prefix("/.well-known/acme-challenge/")?;
    if token.is_empty() {
        return None;
    }

    let key_auth = state.challenge_store.get(token).await?;
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/plain")
            .body(Full::new(Bytes::from(key_auth)))
            .unwrap(),
    )
}

/// Filter out noisy connection reset / broken pipe errors that happen
/// naturally when clients disconnect.
fn is_benign_connection_error(e: &hyper::Error) -> bool {
    use std::error::Error;
    if let Some(io) = e.source().and_then(|s| s.downcast_ref::<std::io::Error>()) {
        return matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
        );
    }
    false
}

fn build_upstream_client() -> Client<HttpConnector, Full<Bytes>> {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(3)));
    connector.set_nodelay(true);
    Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .build(connector)
}
