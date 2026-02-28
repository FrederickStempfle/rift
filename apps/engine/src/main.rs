use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use rift_engine::{
    api::{self, AppState},
    build::BuildManager,
    config::Config,
    db,
    proxy::{self, analytics_collector::AnalyticsCollector, firewall_cache::FirewallCache},
    runtime::RuntimeManager,
    services::{
        audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Arc::new(Config::from_env());
    let pool = db::connect_and_migrate(&config.database_url)
        .await
        .context("failed to initialize database")?;

    let token_service = TokenService::new(
        &config.jwt_private_key_pem(),
        &config.jwt_public_key_pem(),
        config.access_ttl(),
        config.refresh_ttl(),
    )
    .context("failed to initialize token service")?;

    let password_service =
        PasswordService::new().context("failed to initialize password service")?;
    let runtime_manager = RuntimeManager::new();
    let analytics_collector = AnalyticsCollector::new(pool.clone());
    let build_manager = BuildManager::new(
        pool.clone(),
        Arc::clone(&config),
        runtime_manager.clone(),
        analytics_collector.clone(),
        config.build_root.clone().into(),
        config.deploy_root.clone().into(),
    );

    let public_ip = config.resolve_public_ip().await;
    match &public_ip {
        Some(ip) => tracing::info!(ip = %ip, "resolved public IP"),
        None => tracing::warn!("could not detect public IP — DNS verification will be unavailable"),
    }

    let state = AppState {
        pool: pool.clone(),
        config: Arc::clone(&config),
        token_service,
        password_service,
        auth_rate_limiters: AuthRateLimiters::new(),
        audit_logger: AuditLogger::new(pool),
        runtime_manager,
        build_manager,
        public_ip,
        firewall_cache: FirewallCache::new(),
        analytics_collector,
    };

    let api_state = state.clone();
    let proxy_state = state;

    tokio::try_join!(serve_api(api_state), proxy::serve(proxy_state))?;

    Ok(())
}

async fn serve_api(state: AppState) -> anyhow::Result<()> {
    let app = api::router(state.clone());
    let bind_addr = state.config.api_addr();
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind API listener on {bind_addr}"))?;

    tracing::info!(address = %bind_addr, "api server listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("api server failed")?;

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rift_engine=debug,tower_http=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
}
