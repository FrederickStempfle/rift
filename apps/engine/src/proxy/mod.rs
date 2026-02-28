pub mod analytics_collector;
pub mod firewall_cache;
pub mod handler;

use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::{client::legacy::Client, rt::TokioExecutor, rt::TokioIo};

use crate::api::AppState;

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let bind_addr = state.config.proxy_addr();
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind proxy listener on {bind_addr}"))?;

    tracing::info!(address = %bind_addr, "proxy server listening");

    // Connection-pooled HTTP client shared across all requests.
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .build_http();

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let client = client.clone();
        let state = state.clone();

        tokio::spawn(async move {
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
                    tracing::debug!(error = %e, peer = %remote_addr, "proxy connection error");
                }
            }
        });
    }
}

/// Filter out noisy connection reset / broken pipe errors that happen
/// naturally when clients disconnect.
fn is_benign_connection_error(e: &hyper::Error) -> bool {
    use std::error::Error;
    if let Some(io) = e.source().and_then(|s| {
        s.downcast_ref::<std::io::Error>()
    }) {
        return matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
        );
    }
    false
}
