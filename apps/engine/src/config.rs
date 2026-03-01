use chrono::Duration;
use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(author, version, about = "Rift engine")]
pub struct Config {
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    #[arg(long, env = "RIFT_MASTER_KEY")]
    pub master_key: String,

    #[arg(long, env = "RIFT_JWT_PRIVATE_KEY_PEM")]
    pub jwt_private_key_pem: String,

    #[arg(long, env = "RIFT_JWT_PUBLIC_KEY_PEM")]
    pub jwt_public_key_pem: String,

    #[arg(long, env = "RIFT_INTERNAL_API_TOKEN")]
    pub internal_api_token: String,

    #[arg(long, env = "RIFT_API_BIND", default_value = "0.0.0.0")]
    pub api_bind: String,

    #[arg(long, env = "RIFT_API_PORT", default_value_t = 3001)]
    pub api_port: u16,

    #[arg(long, env = "RIFT_PROXY_BIND", default_value = "0.0.0.0")]
    pub proxy_bind: String,

    #[arg(long, env = "RIFT_PROXY_PORT", default_value_t = 8080)]
    pub proxy_port: u16,

    /// The externally-visible port (after Docker port mapping). Defaults to proxy_port.
    #[arg(long, env = "RIFT_PUBLIC_PORT")]
    pub public_port: Option<u16>,

    #[arg(long, env = "RIFT_BASE_DOMAIN", default_value = "localhost")]
    pub base_domain: String,

    #[arg(long, env = "RIFT_PROXY_SCHEME", default_value = "http")]
    pub proxy_scheme: String,

    #[arg(long, env = "RIFT_ACCESS_TOKEN_TTL_MINUTES", default_value_t = 15)]
    pub access_token_ttl_minutes: i64,

    #[arg(long, env = "RIFT_REFRESH_TOKEN_TTL_DAYS", default_value_t = 7)]
    pub refresh_token_ttl_days: i64,

    #[arg(long, env = "RIFT_COOKIE_SECURE", default_value_t = false)]
    pub cookie_secure: bool,

    #[arg(long, env = "RIFT_CORS_ORIGIN")]
    pub cors_origin: Option<String>,

    #[arg(long, env = "RIFT_BUILD_ROOT", default_value = "/var/rift/builds")]
    pub build_root: String,

    #[arg(
        long,
        env = "RIFT_DEPLOY_ROOT",
        default_value = "/var/rift/deployments"
    )]
    pub deploy_root: String,

    /// Optional override; if unset, auto-detected on startup via external service.
    #[arg(long, env = "RIFT_PUBLIC_IP")]
    pub public_ip: Option<String>,

    #[arg(long, env = "RIFT_SSL_DIR", default_value = "/var/rift/ssl")]
    pub ssl_dir: String,

    #[arg(long, env = "RIFT_ACME_EMAIL")]
    pub acme_email: Option<String>,

    #[arg(long, env = "RIFT_ACME_STAGING", default_value_t = false)]
    pub acme_staging: bool,

    #[arg(long, env = "RIFT_HTTPS_PORT", default_value_t = 8443)]
    pub https_port: u16,

    // --- Worker Pool Configuration ---

    /// Runtime mode: "process" (legacy subprocess) or "pool" (pre-warmed worker pool).
    #[arg(long, env = "RIFT_RUNTIME_MODE", default_value = "process")]
    pub runtime_mode: String,

    /// Number of pre-warmed Deno workers to maintain.
    #[arg(long, env = "RIFT_POOL_WARM_SIZE", default_value_t = 3)]
    pub pool_warm_size: usize,

    /// Maximum number of specialized (active) workers.
    #[arg(long, env = "RIFT_POOL_MAX_ACTIVE", default_value_t = 50)]
    pub pool_max_active: usize,

    /// Maximum memory per worker in MB.
    #[arg(long, env = "RIFT_WORKER_MEMORY_LIMIT_MB", default_value_t = 512)]
    pub worker_memory_limit_mb: u64,

    /// Path to the worker loader TypeScript file.
    #[arg(
        long,
        env = "RIFT_WORKER_LOADER",
        default_value = "/opt/rift/templates/worker_loader.ts"
    )]
    pub worker_loader: String,

    /// Fixed port for the global function dispatcher (not allocated from the deployment range).
    #[arg(long, env = "RIFT_GLOBAL_DISPATCHER_PORT", default_value_t = 9999)]
    pub global_dispatcher_port: u16,

    // --- V8 Isolate Pool Configuration ---

    /// Function execution mode: "isolate" (in-process V8) or "deno" (subprocess dispatcher).
    #[arg(long, env = "RIFT_FUNCTION_MODE", default_value = "isolate")]
    pub function_mode: String,

    /// Maximum concurrent V8 isolate executions.
    #[arg(long, env = "RIFT_ISOLATE_MAX_CONCURRENT", default_value_t = 50)]
    pub isolate_max_concurrent: usize,

    /// Per-isolate execution timeout in seconds.
    #[arg(long, env = "RIFT_ISOLATE_TIMEOUT_SECS", default_value_t = 30)]
    pub isolate_timeout_secs: u64,

    /// Per-isolate V8 heap size limit in MB.
    #[arg(long, env = "RIFT_ISOLATE_HEAP_LIMIT_MB", default_value_t = 128)]
    pub isolate_heap_limit_mb: usize,

    // --- Security Configuration ---

    /// Enforce seccomp BPF profile on worker processes.
    /// Default: true (production). Set to false for local development without seccomp support.
    #[arg(long, env = "RIFT_SECCOMP_ENFORCE", default_value_t = true)]
    pub seccomp_enforce: bool,

    // --- Build Configuration ---

    /// Maximum number of concurrent builds. Set to 1 for serial builds.
    #[arg(long, env = "RIFT_BUILD_CONCURRENCY", default_value_t = 4)]
    pub build_concurrency: usize,

    /// Directory for caching build dependencies (node_modules). Empty string disables caching.
    #[arg(long, env = "RIFT_BUILD_CACHE_DIR", default_value = "/var/rift/cache")]
    pub build_cache_dir: String,

    /// Run package-manager cache clean after install (destroys warm-cache benefit).
    /// Default: false — leave native caches intact for faster subsequent installs.
    #[arg(long, env = "RIFT_BUILD_CLEAN_CACHE", default_value_t = false)]
    pub build_clean_cache: bool,

    /// Skip `npm install` / `pnpm install` when lockfile hash matches the cached
    /// node_modules. Default: true.
    #[arg(long, env = "RIFT_INSTALL_SKIP_ON_CACHE_HIT", default_value_t = true)]
    pub install_skip_on_cache_hit: bool,

    /// Artifact copy strategy: "auto" tries CoW/reflink first then falls back to
    /// recursive copy; "reflink" fails if CoW is unsupported; "recursive" always
    /// uses the traditional recursive copy.
    #[arg(long, env = "RIFT_ARTIFACT_COPY_MODE", default_value = "auto")]
    pub artifact_copy_mode: String,

    // --- Health Check Configuration ---

    /// Milliseconds between health-check TCP probes during runtime startup.
    #[arg(long, env = "RIFT_HEALTHCHECK_INTERVAL_MS", default_value_t = 200)]
    pub healthcheck_interval_ms: u64,

    /// Maximum number of health-check attempts before declaring a runtime unhealthy.
    #[arg(long, env = "RIFT_HEALTHCHECK_ATTEMPTS", default_value_t = 50)]
    pub healthcheck_attempts: usize,
}

impl Config {
    pub fn from_env() -> Self {
        Self::parse()
    }

    pub fn api_addr(&self) -> String {
        format!("{}:{}", self.api_bind, self.api_port)
    }

    pub fn proxy_addr(&self) -> String {
        format!("{}:{}", self.proxy_bind, self.proxy_port)
    }

    pub fn https_addr(&self) -> String {
        format!("{}:{}", self.proxy_bind, self.https_port)
    }

    pub fn public_url_for_subdomain(&self, subdomain: &str) -> String {
        self.public_url_for_host(&format!("{subdomain}.{}", self.base_domain))
    }

    pub fn public_url_for_host(&self, host: &str) -> String {
        let port = self.public_port.unwrap_or(self.proxy_port);
        let include_port = match self.proxy_scheme.as_str() {
            "http" => port != 80,
            "https" => port != 443,
            _ => true,
        };

        if include_port {
            format!("{}://{}:{}", self.proxy_scheme, host, port)
        } else {
            format!("{}://{}", self.proxy_scheme, host)
        }
    }

    pub fn access_ttl(&self) -> Duration {
        Duration::minutes(self.access_token_ttl_minutes)
    }

    pub fn refresh_ttl(&self) -> Duration {
        Duration::days(self.refresh_token_ttl_days)
    }

    pub fn jwt_private_key_pem(&self) -> String {
        self.jwt_private_key_pem.replace("\\n", "\n")
    }

    pub fn jwt_public_key_pem(&self) -> String {
        self.jwt_public_key_pem.replace("\\n", "\n")
    }

    /// Resolve the server's public IP: use the explicit override if set,
    /// otherwise auto-detect by calling an external service.
    pub async fn resolve_public_ip(&self) -> Option<String> {
        if let Some(ip) = &self.public_ip {
            return Some(ip.clone());
        }

        for url in [
            "https://api.ipify.org",
            "https://ifconfig.me/ip",
            "https://icanhazip.com",
        ] {
            match reqwest::get(url).await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(text) = resp.text().await {
                        let ip = text.trim().to_owned();
                        if !ip.is_empty() {
                            return Some(ip);
                        }
                    }
                }
                _ => continue,
            }
        }

        None
    }
}
