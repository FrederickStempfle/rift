pub mod firewall_cache;
pub mod handler;
pub mod router;

use std::net::SocketAddr;

use anyhow::Context;

use crate::api::AppState;

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let bind_addr = state.config.proxy_addr();
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind proxy listener on {bind_addr}"))?;

    tracing::info!(address = %bind_addr, "proxy server listening");

    axum::serve(
        listener,
        router::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("proxy server failed")?;

    Ok(())
}
