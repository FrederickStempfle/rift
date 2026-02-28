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
