use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use rift_engine::{
    api::{self, AppState},
    build::BuildManager,
    config::Config,
    db,
    proxy::{
        self, acme::AcmeChallengeStore, analytics_collector::AnalyticsCollector,
        firewall_cache::FirewallCache, routing_cache::RoutingCache, tls::CertResolver,
    },
    runtime::{
        backend::{PoolBackend, ProcessBackend},
        policy::{self, EnforcementMode},
        pool::{PoolConfig, WorkerPool},
        RuntimeManager,
    },
    scheduler::{self, Scheduler},
    services::{
        audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
    ssl::SslManager,
    state,
    ws::LogBroadcaster,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    // Install the rustls crypto provider before any TLS operations
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

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

    // Initialize process-level seccomp for worker processes
    runtime_manager.init_seccomp(
        std::path::Path::new(&config.deploy_root),
        config.seccomp_enforce,
    );

    // Configure health-check parameters
    runtime_manager.set_healthcheck(config.healthcheck_interval_ms, config.healthcheck_attempts);

    // Set DB pool for suspend/wake state persistence
    runtime_manager.set_db_pool(pool.clone());

    // Configure namespace isolation (disabled by default inside Docker)
    runtime_manager.set_namespace_isolate(config.namespace_isolate);

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
                    seccomp_enforce: config.seccomp_enforce,
                };
                let enforcement_mode = EnforcementMode::from_config(&config);
                let default_policy = policy::resolve_runtime_policy(&config, None);
                let worker_pool = WorkerPool::new(
                    pool_config,
                    Some(pool.clone()),
                    enforcement_mode,
                    default_policy,
                )
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
    // Initialize V8 isolate pool for serverless functions (if enabled)
    #[cfg(feature = "v8-isolate")]
    let isolate_pool = if config.function_mode == "isolate" {
        use rift_engine::runtime::isolate::{IsolatePool, IsolatePoolConfig};
        let isolate_config = IsolatePoolConfig {
            max_concurrent: config.isolate_max_concurrent,
            execution_timeout: std::time::Duration::from_secs(config.isolate_timeout_secs),
            heap_limit_bytes: config.isolate_heap_limit_mb * 1024 * 1024,
        };
        match IsolatePool::new(isolate_config).await {
            Ok(pool) => {
                tracing::info!("V8 isolate pool initialized");
                Some(pool)
            }
            Err(e) => {
                tracing::warn!(error = %e, "V8 isolate pool failed to initialize — falling back to Deno subprocess");
                None
            }
        }
    } else {
        tracing::info!(mode = %config.function_mode, "V8 isolate pool disabled — using Deno subprocess dispatcher");
        None
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
        #[cfg(feature = "v8-isolate")]
        isolate_pool.clone(),
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

    // Bootstrap a self-signed fallback so the HTTPS listener can always start.
    // Real ACME certs replace it once provisioned.
    if !cert_resolver.has_any_certs().await {
        match proxy::tls::generate_self_signed(&config.base_domain) {
            Ok((cert_pem, key_pem)) => {
                if let Err(e) = cert_resolver
                    .load_cert(&config.base_domain, &cert_pem, &key_pem)
                    .await
                {
                    tracing::warn!(error = %e, "failed to load self-signed fallback cert");
                } else {
                    tracing::info!(
                        domain = %config.base_domain,
                        "bootstrapped self-signed TLS certificate (HTTPS listener ready)"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to generate self-signed fallback cert");
            }
        }
    }

    // --- Distributed state store + scheduler ---
    let state_store: Arc<dyn state::StateStore> = match config.state_store.as_str() {
        "redis" => {
            let rs = state::redis_store::RedisStateStore::new(&config.redis_url)
                .context("failed to connect to Redis state store")?;
            tracing::info!(url = %config.redis_url, "state store: redis");
            Arc::new(rs)
        }
        _ => {
            tracing::info!("state store: local (in-memory)");
            Arc::new(state::local::LocalStateStore::new())
        }
    };

    let worker_id = config
        .worker_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    tracing::info!(worker_id = %worker_id, "engine worker identity");

    let scheduler = Arc::new(Scheduler::new(state_store.clone(), worker_id.clone()));

    // Spawn background heartbeat
    scheduler::heartbeat::spawn_heartbeat(
        state_store.clone(),
        runtime_backend.clone(),
        worker_id,
        config.pool_max_active as u32,
    );

    let routing_cache = RoutingCache::new();
    routing_cache.spawn_evictor();
    let subscriber_redis_url = if config.state_store == "redis" {
        Some(config.redis_url.clone())
    } else {
        None
    };
    crate::proxy::routing_subscriber::spawn_routing_subscriber(
        subscriber_redis_url,
        routing_cache.clone(),
    );

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
        routing_cache,
        state_store,
        scheduler,
        #[cfg(feature = "v8-isolate")]
        isolate_pool,
    };

    // Restore deployments that were running before the engine restarted
    let restored = state.runtime_backend.restore(&state.pool, &config).await;
    if restored > 0 {
        tracing::info!(count = restored, "restored deployments from previous run");
    }

    // Restore function projects into the V8 isolate pool
    #[cfg(feature = "v8-isolate")]
    if let Some(ref isolate_pool) = state.isolate_pool {
        use rift_engine::db::{deployments, env_vars};

        if let Ok(ready) = deployments::list_latest_ready_per_project(&state.pool).await {
            let mut isolate_restored = 0u32;
            for deployment in &ready {
                let workspace_dir =
                    std::path::PathBuf::from(&config.deploy_root).join(deployment.id.to_string());
                let fn_dir = workspace_dir.join("_rift_functions_output");
                if !fn_dir.join("bundles").is_dir() {
                    continue; // Not a function project
                }

                let manifest_path = fn_dir.join("_routes.json");
                let routes: Vec<rift_engine::build::functions::FunctionRoute> =
                    if manifest_path.exists() {
                        tokio::fs::read_to_string(&manifest_path)
                            .await
                            .ok()
                            .and_then(|c| serde_json::from_str(&c).ok())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };

                let env = env_vars::get_decrypted_env_vars(
                    &state.pool,
                    deployment.project_id,
                    &config.master_key,
                )
                .await
                .unwrap_or_default();

                if let Err(e) = isolate_pool
                    .register(
                        deployment.project_id,
                        deployment.id,
                        &routes,
                        &env,
                        &fn_dir.to_string_lossy(),
                    )
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        project_id = %deployment.project_id,
                        "failed to restore function project into isolate pool"
                    );
                } else {
                    isolate_restored += 1;
                }
            }
            if isolate_restored > 0 {
                tracing::info!(
                    count = isolate_restored,
                    "restored function projects into V8 isolate pool"
                );
            }
        }
    }

    // Spawn certificate renewal background task
    ssl_manager.spawn_renewal_task();

    // Spawn scale-to-zero background task
    rift_engine::runtime::scaler::spawn_scaler(state.runtime_backend.clone());

    let api_state = state.clone();
    let proxy_state = state;

    match config.role.as_str() {
        "edge-agent" => {
            tracing::info!(
                role = %config.role,
                region = %config.region_id,
                node_id = ?config.node_id,
                "starting in edge-agent mode (proxy only)"
            );
            proxy::serve(proxy_state).await?;
        }
        _ => {
            tracing::info!(
                role = %config.role,
                region = %config.region_id,
                node_id = ?config.node_id,
                "starting in control-plane mode (api + proxy)"
            );
            tokio::try_join!(serve_api(api_state), proxy::serve(proxy_state))?;
        }
    }

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
