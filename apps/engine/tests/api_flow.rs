use std::{net::SocketAddr, sync::Arc};

use reqwest::StatusCode;
use rift_engine::{
    api::{self, AppState},
    build::BuildManager,
    config::Config,
    db,
    proxy::{
        acme::AcmeChallengeStore, analytics_collector::AnalyticsCollector,
        firewall_cache::FirewallCache, routing_cache::RoutingCache, tls::CertResolver,
    },
    runtime::{backend::ProcessBackend, RuntimeManager},
    scheduler::Scheduler,
    services::{
        audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
    ssl::SslManager,
    state::local::LocalStateStore,
    ws::LogBroadcaster,
};
use serde_json::json;
use serial_test::serial;

struct TestServer {
    base_url: String,
    _join_handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn start() -> anyhow::Result<Option<Self>> {
        let database_url = match std::env::var("TEST_DATABASE_URL") {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let private_key = match std::env::var("TEST_ED25519_PRIVATE_KEY_PEM") {
            Ok(value) => value.replace("\\n", "\n"),
            Err(_) => return Ok(None),
        };
        let public_key = match std::env::var("TEST_ED25519_PUBLIC_KEY_PEM") {
            Ok(value) => value.replace("\\n", "\n"),
            Err(_) => return Ok(None),
        };

        let pool = db::connect_and_migrate(&database_url).await?;
        sqlx::query(
            "TRUNCATE TABLE audit_log, deploy_logs, deployments, env_vars, domains, projects, refresh_tokens, users RESTART IDENTITY CASCADE",
        )
        .execute(&pool)
        .await?;

        let config = Arc::new(Config {
            database_url,
            master_key: "test-master-key".into(),
            jwt_private_key_pem: private_key,
            jwt_public_key_pem: public_key,
            internal_api_token: "test-internal-token".into(),
            api_bind: "127.0.0.1".into(),
            api_port: 0,
            proxy_bind: "127.0.0.1".into(),
            proxy_port: 0,
            base_domain: "localhost".into(),
            proxy_scheme: "http".into(),
            access_token_ttl_minutes: 15,
            refresh_token_ttl_days: 7,
            cookie_secure: false,
            cors_origin: None,
            build_root: "/tmp/rift-test-builds".into(),
            deploy_root: "/tmp/rift-test-deployments".into(),
            public_port: None,
            public_ip: Some("127.0.0.1".into()),
            ssl_dir: "/tmp/rift-test-ssl".into(),
            acme_email: None,
            acme_staging: false,
            https_port: 0,
            runtime_mode: "process".into(),
            pool_warm_size: 3,
            pool_max_active: 50,
            worker_memory_limit_mb: 512,
            worker_loader: "/opt/rift/templates/worker_loader.ts".into(),
            global_dispatcher_port: 9999,
            function_mode: "isolate".into(),
            isolate_max_concurrent: 50,
            isolate_timeout_secs: 30,
            isolate_heap_limit_mb: 128,
            seccomp_enforce: false,
            namespace_isolate: false,
            build_concurrency: 4,
            build_cache_dir: "/tmp/rift-test-cache".into(),
            build_clean_cache: false,
            install_skip_on_cache_hit: true,
            artifact_copy_mode: "auto".into(),
            healthcheck_interval_ms: 200,
            healthcheck_attempts: 50,
            state_store: "local".into(),
            redis_url: "redis://127.0.0.1:6379".into(),
            worker_id: None,
        });
        let runtime_manager = RuntimeManager::new();
        let log_broadcaster = LogBroadcaster::new();

        let runtime_backend: Arc<dyn rift_engine::runtime::backend::RuntimeBackend> =
            Arc::new(ProcessBackend::new(runtime_manager.clone()));

        let build_manager = BuildManager::new(
            pool.clone(),
            Arc::clone(&config),
            runtime_backend.clone(),
            config.build_root.clone().into(),
            config.deploy_root.clone().into(),
            log_broadcaster.clone(),
            #[cfg(feature = "v8-isolate")]
            None,
        );
        let analytics_collector = AnalyticsCollector::new(pool.clone());
        let cert_resolver = CertResolver::new();
        let challenge_store = AcmeChallengeStore::new();
        let ssl_manager = SslManager::new(
            pool.clone(),
            Arc::clone(&config),
            cert_resolver.clone(),
            challenge_store.clone(),
        );

        let state_store: Arc<dyn rift_engine::state::StateStore> = Arc::new(LocalStateStore::new());
        let scheduler = Arc::new(Scheduler::new(
            state_store.clone(),
            "test-worker".to_string(),
        ));

        let state = AppState {
            pool: pool.clone(),
            config: Arc::clone(&config),
            token_service: TokenService::new(
                &config.jwt_private_key_pem(),
                &config.jwt_public_key_pem(),
                config.access_ttl(),
                config.refresh_ttl(),
            )?,
            password_service: PasswordService::new()?,
            auth_rate_limiters: AuthRateLimiters::new(),
            audit_logger: AuditLogger::new(pool),
            runtime_backend,
            build_manager,
            public_ip: config.public_ip.clone(),
            firewall_cache: FirewallCache::new(),
            analytics_collector,
            log_broadcaster,
            ssl_manager,
            challenge_store,
            cert_resolver,
            routing_cache: RoutingCache::new(),
            state_store,
            scheduler,
            #[cfg(feature = "v8-isolate")]
            isolate_pool: None,
        };

        let app = api::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        let join_handle = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test server should run");
        });

        Ok(Some(Self {
            base_url: format!("http://{}", local_addr),
            _join_handle: join_handle,
        }))
    }
}

#[tokio::test]
#[serial]
async fn register_login_me_flow() -> anyhow::Result<()> {
    let Some(server) = TestServer::start().await? else {
        return Ok(());
    };

    let client = reqwest::Client::builder().cookie_store(true).build()?;
    let email = format!("user-{}@example.com", uuid::Uuid::new_v4());

    let register_resp = client
        .post(format!("{}/api/users/register", server.base_url))
        .json(&json!({ "email": email, "password": "supersecurepassword" }))
        .send()
        .await?;

    assert_eq!(register_resp.status(), StatusCode::CREATED);
    let register_json: serde_json::Value = register_resp.json().await?;
    let token = register_json["access_token"]
        .as_str()
        .expect("access token should be present");

    let me_resp = client
        .get(format!("{}/api/users/me", server.base_url))
        .bearer_auth(token)
        .send()
        .await?;

    assert_eq!(me_resp.status(), StatusCode::OK);
    let me_json: serde_json::Value = me_resp.json().await?;
    assert_eq!(me_json["email"].as_str().unwrap_or_default(), email);

    let refresh_resp = client
        .post(format!("{}/api/users/refresh", server.base_url))
        .send()
        .await?;
    assert_eq!(refresh_resp.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
#[serial]
async fn project_crud_is_user_scoped() -> anyhow::Result<()> {
    let Some(server) = TestServer::start().await? else {
        return Ok(());
    };

    let client_a = reqwest::Client::builder().cookie_store(true).build()?;
    let client_b = reqwest::Client::builder().cookie_store(true).build()?;

    let token_a = register_user(&client_a, &server.base_url).await?;
    let token_b = register_user(&client_b, &server.base_url).await?;

    let create_resp = client_a
        .post(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token_a)
        .json(&json!({
            "name": "my-app",
            "repo_url": "https://github.com/example/repo",
            "subdomain": format!("sub-{}", uuid::Uuid::new_v4().simple()),
            "framework": "nextjs"
        }))
        .send()
        .await?;

    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let create_json: serde_json::Value = create_resp.json().await?;
    let project_id = create_json["id"]
        .as_str()
        .expect("project id should be present");

    let own_get_resp = client_a
        .get(format!("{}/api/projects/{}", server.base_url, project_id))
        .bearer_auth(&token_a)
        .send()
        .await?;
    assert_eq!(own_get_resp.status(), StatusCode::OK);

    let other_get_resp = client_b
        .get(format!("{}/api/projects/{}", server.base_url, project_id))
        .bearer_auth(&token_b)
        .send()
        .await?;
    assert_eq!(other_get_resp.status(), StatusCode::NOT_FOUND);

    let delete_resp = client_a
        .delete(format!("{}/api/projects/{}", server.base_url, project_id))
        .bearer_auth(&token_a)
        .send()
        .await?;
    assert_eq!(delete_resp.status(), StatusCode::NO_CONTENT);

    Ok(())
}

#[tokio::test]
#[serial]
async fn list_projects_includes_latest_deployment_summary() -> anyhow::Result<()> {
    let Some(server) = TestServer::start().await? else {
        return Ok(());
    };

    let client = reqwest::Client::builder().cookie_store(true).build()?;
    let token = register_user(&client, &server.base_url).await?;
    let subdomain = format!("sub-{}", uuid::Uuid::new_v4().simple());

    let create_project_resp = client
        .post(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token)
        .json(&json!({
            "name": "summary-app",
            "repo_url": "https://github.com/example/repo",
            "subdomain": subdomain,
            "framework": "nextjs"
        }))
        .send()
        .await?;

    assert_eq!(create_project_resp.status(), StatusCode::CREATED);
    let project_json: serde_json::Value = create_project_resp.json().await?;
    let project_id = project_json["id"]
        .as_str()
        .expect("project id should be present");

    let create_deployment_resp = client
        .post(format!("{}/api/deployments", server.base_url))
        .bearer_auth(&token)
        .json(&json!({ "project_id": project_id }))
        .send()
        .await?;
    assert_eq!(create_deployment_resp.status(), StatusCode::CREATED);

    let list_projects_resp = client
        .get(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(list_projects_resp.status(), StatusCode::OK);

    let projects: serde_json::Value = list_projects_resp.json().await?;
    let first = projects
        .as_array()
        .and_then(|items| items.first())
        .expect("project list should include one item");

    assert_eq!(
        first["runtime_status"].as_str().unwrap_or_default(),
        "inactive"
    );
    assert_eq!(first["primary_domain"], serde_json::Value::Null);
    assert_eq!(
        first["public_url"].as_str().unwrap_or_default(),
        format!("http://{}.localhost", subdomain)
    );
    assert_eq!(
        first["latest_deployment"]["status"]
            .as_str()
            .unwrap_or_default(),
        "queued"
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn list_projects_exposes_pending_primary_domain() -> anyhow::Result<()> {
    let Some(server) = TestServer::start().await? else {
        return Ok(());
    };

    let client = reqwest::Client::builder().cookie_store(true).build()?;
    let token = register_user(&client, &server.base_url).await?;
    let subdomain = format!("sub-{}", uuid::Uuid::new_v4().simple());

    let create_project_resp = client
        .post(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token)
        .json(&json!({
            "name": "pending-domain-app",
            "repo_url": "https://github.com/example/repo",
            "subdomain": subdomain,
            "framework": "nextjs"
        }))
        .send()
        .await?;

    assert_eq!(create_project_resp.status(), StatusCode::CREATED);
    let project_json: serde_json::Value = create_project_resp.json().await?;
    let project_id = project_json["id"]
        .as_str()
        .expect("project id should be present");

    let primary_domain = format!("pending-{}.example.com", uuid::Uuid::new_v4().simple());
    let create_domain_resp = client
        .post(format!("{}/api/domains/", server.base_url))
        .bearer_auth(&token)
        .json(&json!({
            "domain": primary_domain,
            "project_id": project_id
        }))
        .send()
        .await?;
    assert_eq!(create_domain_resp.status(), StatusCode::CREATED);

    let list_projects_resp = client
        .get(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(list_projects_resp.status(), StatusCode::OK);

    let projects: serde_json::Value = list_projects_resp.json().await?;
    let first = projects
        .as_array()
        .and_then(|items| items.first())
        .expect("project list should include one item");

    assert_eq!(
        first["primary_domain"].as_str().unwrap_or_default(),
        primary_domain
    );
    assert_eq!(
        first["public_url"].as_str().unwrap_or_default(),
        format!("http://{}.localhost", subdomain)
    );

    Ok(())
}

#[tokio::test]
#[serial]
async fn list_projects_uses_deterministic_latest_deployment_on_tied_timestamps(
) -> anyhow::Result<()> {
    let Some(server) = TestServer::start().await? else {
        return Ok(());
    };

    let client = reqwest::Client::builder().cookie_store(true).build()?;
    let token = register_user(&client, &server.base_url).await?;
    let subdomain = format!("sub-{}", uuid::Uuid::new_v4().simple());

    let create_project_resp = client
        .post(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token)
        .json(&json!({
            "name": "deterministic-summary-app",
            "repo_url": "https://github.com/example/repo",
            "subdomain": subdomain,
            "framework": "nextjs"
        }))
        .send()
        .await?;

    assert_eq!(create_project_resp.status(), StatusCode::CREATED);
    let project_json: serde_json::Value = create_project_resp.json().await?;
    let project_id = uuid::Uuid::parse_str(
        project_json["id"]
            .as_str()
            .expect("project id should be present"),
    )?;

    let database_url = std::env::var("TEST_DATABASE_URL")?;
    let pool = db::connect_and_migrate(&database_url).await?;
    let created_at = chrono::Utc::now();
    let lower_id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001")?;
    let higher_id = uuid::Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff")?;

    for (deployment_id, commit_sha) in [(lower_id, "aaa111"), (higher_id, "bbb222")] {
        sqlx::query(
            r#"
            INSERT INTO deployments (
                id,
                project_id,
                commit_sha,
                commit_message,
                branch,
                status,
                created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6::deployment_status, $7)
            "#,
        )
        .bind(deployment_id)
        .bind(project_id)
        .bind(commit_sha)
        .bind(format!("commit {commit_sha}"))
        .bind("main")
        .bind("queued")
        .bind(created_at)
        .execute(&pool)
        .await?;
    }

    let list_projects_resp = client
        .get(format!("{}/api/projects/", server.base_url))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(list_projects_resp.status(), StatusCode::OK);

    let projects: serde_json::Value = list_projects_resp.json().await?;
    let first = projects
        .as_array()
        .and_then(|items| items.first())
        .expect("project list should include one item");

    assert_eq!(
        first["latest_deployment"]["id"]
            .as_str()
            .unwrap_or_default(),
        higher_id.to_string()
    );
    assert_eq!(
        first["latest_deployment"]["commit_sha"]
            .as_str()
            .unwrap_or_default(),
        "bbb222"
    );

    Ok(())
}

async fn register_user(client: &reqwest::Client, base_url: &str) -> anyhow::Result<String> {
    let email = format!("user-{}@example.com", uuid::Uuid::new_v4());
    let response = client
        .post(format!("{base_url}/api/users/register"))
        .json(&json!({ "email": email, "password": "supersecurepassword" }))
        .send()
        .await?;

    if response.status() != StatusCode::CREATED {
        anyhow::bail!("register failed with status {}", response.status());
    }

    let body: serde_json::Value = response.json().await?;
    Ok(body["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing access token"))?
        .to_owned())
}
