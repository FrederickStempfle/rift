use std::{net::SocketAddr, sync::Arc};

use reqwest::StatusCode;
use rift_engine::{
    api::{self, AppState},
    build::BuildManager,
    config::Config,
    db,
    runtime::RuntimeManager,
    services::{
        audit::AuditLogger, auth::TokenService, password::PasswordService,
        rate_limit::AuthRateLimiters,
    },
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
        });
        let runtime_manager = RuntimeManager::new();
        let build_manager = BuildManager::new(
            pool.clone(),
            runtime_manager.clone(),
            config.build_root.clone().into(),
            config.deploy_root.clone().into(),
        );

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
            runtime_manager,
            build_manager,
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
