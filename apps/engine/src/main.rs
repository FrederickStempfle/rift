use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use rift_engine::{
    api::{self, AppState},
    build::BuildManager,
    config::Config,
    db,
    proxy::{
        self, acme::AcmeChallengeStore, analytics_collector::AnalyticsCollector,
        firewall_cache::FirewallCache, tls::CertResolver,
    },
    runtime::{
        backend::{PoolBackend, ProcessBackend},
        pool::{PoolConfig, WorkerPool},
        RuntimeManager,
    },
    services::{
        audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
    ssl::SslManager,
    ws::LogBroadcaster,
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
    let mut runtime_manager = RuntimeManager::new();

    // Start the global function dispatcher
    let template_dir = std::path::Path::new("/opt/rift/templates");
    let function_registry = match rift_engine::runtime::function_registry::FunctionRegistry::start(
        template_dir,
        config.global_dispatcher_port,
    )
    .await
    {
        Ok(registry) => {
            registry.spawn_health_monitor();
            tracing::info!("global function dispatcher ready");
            Some(registry)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to start global function dispatcher — functions will use per-project processes"
            );
            None
        }
    };

    // Wire registry into RuntimeManager for ProcessBackend
    if let Some(ref registry) = function_registry {
        runtime_manager.set_function_registry(registry.clone());
    }

    let runtime_backend: Arc<dyn rift_engine::runtime::backend::RuntimeBackend> =
        match config.runtime_mode.as_str() {
            "pool" => {
                let pool_config = PoolConfig {
                    warm_pool_size: config.pool_warm_size,
                    max_active_workers: config.pool_max_active,
                    idle_timeout: std::time::Duration::from_secs(300),
                    worker_memory_limit: config.worker_memory_limit_mb * 1024 * 1024,
                    loader_script: config.worker_loader.clone().into(),
                    deploy_root: config.deploy_root.clone().into(),
                };
                let worker_pool = WorkerPool::new(pool_config)
                    .await
                    .context("failed to initialize worker pool")?;
                worker_pool.spawn_health_monitor();
                tracing::info!("runtime mode: pool (pre-warmed workers)");
                Arc::new(PoolBackend::new(worker_pool, function_registry))
            }
            _ => {
                tracing::info!("runtime mode: process (legacy subprocesses)");
                Arc::new(ProcessBackend::new(runtime_manager.clone()))
            }
        };
    let analytics_collector = AnalyticsCollector::new(pool.clone());
    let log_broadcaster = LogBroadcaster::new();
    let build_manager = BuildManager::new(
        pool.clone(),
        Arc::clone(&config),
        runtime_backend.clone(),
        config.build_root.clone().into(),
        config.deploy_root.clone().into(),
        log_broadcaster.clone(),
    );

    let public_ip = config.resolve_public_ip().await;
    match &public_ip {
        Some(ip) => tracing::info!(ip = %ip, "resolved public IP"),
        None => tracing::warn!("could not detect public IP — DNS verification will be unavailable"),
    }

    let cert_resolver = CertResolver::new();
    let challenge_store = AcmeChallengeStore::new();
    let ssl_manager = SslManager::new(
        pool.clone(),
        Arc::clone(&config),
        cert_resolver.clone(),
        challenge_store.clone(),
    );

    // Load existing TLS certificates from disk
    if let Err(e) = ssl_manager.load_existing_certs().await {
        tracing::warn!(error = %e, "failed to load existing TLS certificates");
    }

    let state = AppState {
        pool: pool.clone(),
        config: Arc::clone(&config),
        token_service,
        password_service,
        auth_rate_limiters: AuthRateLimiters::new(),
        audit_logger: AuditLogger::new(pool),
        runtime_backend: runtime_backend.clone(),
        build_manager,
        public_ip,
        firewall_cache: FirewallCache::new(),
        analytics_collector,
        log_broadcaster,
        ssl_manager: ssl_manager.clone(),
        challenge_store,
        cert_resolver,
    };

    // Restore deployments that were running before the engine restarted
    let restored = state
        .runtime_backend
        .restore(&state.pool, &config)
        .await;
    if restored > 0 {
        tracing::info!(count = restored, "restored deployments from previous run");
    }

    // Spawn certificate renewal background task
    ssl_manager.spawn_renewal_task();

    // Spawn scale-to-zero background task
    rift_engine::runtime::scaler::spawn_scaler(state.runtime_backend.clone());

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
