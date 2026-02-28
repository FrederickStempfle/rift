use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use instant_acme::{
    Account, AccountCredentials, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder,
    OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use sqlx::PgPool;

use crate::config::Config;
use crate::db::domains;
use crate::proxy::acme::AcmeChallengeStore;
use crate::proxy::tls::CertResolver;

#[derive(Clone)]
pub struct SslManager {
    pool: PgPool,
    config: Arc<Config>,
    cert_resolver: CertResolver,
    challenge_store: AcmeChallengeStore,
}

impl SslManager {
    pub fn new(
        pool: PgPool,
        config: Arc<Config>,
        cert_resolver: CertResolver,
        challenge_store: AcmeChallengeStore,
    ) -> Self {
        Self {
            pool,
            config,
            cert_resolver,
            challenge_store,
        }
    }

    pub async fn load_existing_certs(&self) -> anyhow::Result<()> {
        let ssl_dir = PathBuf::from(&self.config.ssl_dir);
        let count = self.cert_resolver.load_from_disk(&ssl_dir).await?;
        if count > 0 {
            tracing::info!(count, "loaded existing TLS certificates");
        }
        Ok(())
    }

    pub async fn provision_cert(&self, domain: &str) -> anyhow::Result<()> {
        let email = self.config.acme_email.as_deref().unwrap_or_default();
        if email.is_empty() {
            anyhow::bail!("RIFT_ACME_EMAIL is required for SSL certificate provisioning");
        }

        tracing::info!(domain = %domain, "starting SSL certificate provisioning");

        // Mark as provisioning
        domains::update_ssl_provisioning(&self.pool, domain)
            .await
            .map_err(|e| anyhow::anyhow!("failed to update ssl_status to provisioning: {e}"))?;

        match self.do_provision(domain, email).await {
            Ok(()) => {
                tracing::info!(domain = %domain, "SSL certificate provisioned successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("{e:#}");
                tracing::error!(domain = %domain, error = %err_msg, "SSL provisioning failed");
                let _ = domains::update_ssl_failed(&self.pool, domain, &err_msg).await;
                Err(e)
            }
        }
    }

    async fn do_provision(&self, domain: &str, email: &str) -> anyhow::Result<()> {
        let ssl_dir = PathBuf::from(&self.config.ssl_dir);
        std::fs::create_dir_all(&ssl_dir)?;

        // Get or create ACME account
        let account = self.get_or_create_account(email, &ssl_dir).await?;

        // Create order
        let mut order = account
            .new_order(&NewOrder {
                identifiers: &[Identifier::Dns(domain.to_owned())],
            })
            .await
            .map_err(|e| anyhow::anyhow!("failed to create ACME order: {e}"))?;

        // Get authorization and HTTP-01 challenge
        let authorizations = order
            .authorizations()
            .await
            .map_err(|e| anyhow::anyhow!("failed to get authorizations: {e}"))?;

        let authorization = authorizations
            .first()
            .ok_or_else(|| anyhow::anyhow!("no authorization returned"))?;

        let challenge = authorization
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .ok_or_else(|| anyhow::anyhow!("no HTTP-01 challenge found"))?;

        let key_auth = order.key_authorization(challenge).as_str().to_owned();
        let token = challenge.token.clone();

        // Store challenge for the HTTP responder
        self.challenge_store.set(token.clone(), key_auth).await;

        // Tell ACME server the challenge is ready
        order
            .set_challenge_ready(&challenge.url)
            .await
            .map_err(|e| anyhow::anyhow!("failed to set challenge ready: {e}"))?;

        // Poll until order is ready
        let mut tries = 0u8;
        let mut delay = Duration::from_millis(500);
        loop {
            tokio::time::sleep(delay).await;
            let state = order
                .refresh()
                .await
                .map_err(|e| anyhow::anyhow!("failed to refresh order: {e}"))?;

            match state.status {
                OrderStatus::Ready => break,
                OrderStatus::Invalid => {
                    self.challenge_store.remove(&token).await;
                    anyhow::bail!("ACME order became invalid — domain verification failed");
                }
                OrderStatus::Valid => break,
                _ => {}
            }

            delay = std::cmp::min(delay * 2, Duration::from_secs(10));
            tries += 1;
            if tries >= 20 {
                self.challenge_store.remove(&token).await;
                anyhow::bail!("timed out waiting for ACME order to become ready");
            }
        }

        // Clean up challenge
        self.challenge_store.remove(&token).await;

        // Generate CSR and finalize
        let mut params = CertificateParams::new(vec![domain.to_owned()])
            .map_err(|e| anyhow::anyhow!("failed to create cert params: {e}"))?;
        params.distinguished_name = DistinguishedName::new();
        let private_key =
            KeyPair::generate().map_err(|e| anyhow::anyhow!("failed to generate key pair: {e}"))?;
        let signing_request = params
            .serialize_request(&private_key)
            .map_err(|e| anyhow::anyhow!("failed to serialize CSR: {e}"))?;

        order
            .finalize(signing_request.der())
            .await
            .map_err(|e| anyhow::anyhow!("failed to finalize order: {e}"))?;

        // Poll for certificate
        let mut cert_chain_pem: Option<String> = None;
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            match order.certificate().await {
                Ok(Some(cert)) => {
                    cert_chain_pem = Some(cert);
                    break;
                }
                Ok(None) => continue,
                Err(e) => anyhow::bail!("failed to download certificate: {e}"),
            }
        }

        let cert_pem = cert_chain_pem
            .ok_or_else(|| anyhow::anyhow!("certificate not available after polling"))?;
        let key_pem = private_key.serialize_pem();

        // Save to disk
        let domain_dir = ssl_dir.join(domain);
        std::fs::create_dir_all(&domain_dir)?;
        std::fs::write(domain_dir.join("cert.pem"), cert_pem.as_bytes())?;
        std::fs::write(domain_dir.join("key.pem"), key_pem.as_bytes())?;

        // Load into resolver
        self.cert_resolver
            .load_cert(domain, cert_pem.as_bytes(), key_pem.as_bytes())
            .await?;

        // Parse expiry from the certificate
        let expires_at = parse_cert_expiry(cert_pem.as_bytes())
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::days(90));

        // Update DB
        domains::update_ssl_active_with_cert(&self.pool, domain, expires_at)
            .await
            .map_err(|e| anyhow::anyhow!("failed to update domain SSL status: {e}"))?;

        Ok(())
    }

    async fn get_or_create_account(
        &self,
        email: &str,
        ssl_dir: &PathBuf,
    ) -> anyhow::Result<Account> {
        let account_path = ssl_dir.join("account.json");
        let acme_url = if self.config.acme_staging {
            LetsEncrypt::Staging.url()
        } else {
            LetsEncrypt::Production.url()
        };

        // Try to load existing account
        if account_path.exists() {
            let data = std::fs::read_to_string(&account_path)?;
            let credentials: AccountCredentials = serde_json::from_str(&data)
                .map_err(|e| anyhow::anyhow!("failed to parse account credentials: {e}"))?;
            let account = Account::from_credentials(credentials)
                .await
                .map_err(|e| anyhow::anyhow!("failed to restore ACME account: {e}"))?;
            return Ok(account);
        }

        // Create new account
        let new_account = NewAccount {
            contact: &[&format!("mailto:{email}")],
            terms_of_service_agreed: true,
            only_return_existing: false,
        };

        let (account, credentials) = Account::create(&new_account, &acme_url, None)
            .await
            .map_err(|e| anyhow::anyhow!("failed to create ACME account: {e}"))?;

        // Save credentials
        let data = serde_json::to_string_pretty(&credentials)?;
        std::fs::write(&account_path, data)?;

        Ok(account)
    }

    pub async fn renew_expiring(&self) {
        let domains = match domains::list_domains_needing_renewal(&self.pool).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "failed to query domains needing renewal");
                return;
            }
        };

        if domains.is_empty() {
            tracing::debug!("no certificates need renewal");
            return;
        }

        tracing::info!(count = domains.len(), "renewing expiring certificates");

        for domain in domains {
            if let Err(e) = self.provision_cert(&domain.domain).await {
                tracing::error!(
                    domain = %domain.domain,
                    error = %e,
                    "certificate renewal failed"
                );
            }
        }
    }

    pub fn spawn_renewal_task(self) {
        tokio::spawn(async move {
            // Initial delay — don't renew right on startup
            tokio::time::sleep(Duration::from_secs(60)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            loop {
                interval.tick().await;
                self.renew_expiring().await;
            }
        });
    }
}

fn parse_cert_expiry(cert_pem: &[u8]) -> Option<chrono::DateTime<chrono::Utc>> {
    use std::io::BufReader;
    let certs = rustls_pemfile::certs(&mut BufReader::new(cert_pem))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let cert = certs.first()?;
    // Parse the X.509 certificate to extract notAfter
    // We'll extract the notAfter field from the DER-encoded certificate
    // using a simple ASN.1 approach via the x509-parser-like manual extraction.
    // For simplicity, we'll default to 90 days from now if parsing fails.
    parse_x509_not_after(cert.as_ref())
}

fn parse_x509_not_after(der: &[u8]) -> Option<chrono::DateTime<chrono::Utc>> {
    // Minimal ASN.1 DER parsing for X.509 notAfter.
    // The validity field is the 5th element in the TBSCertificate sequence.
    // Rather than implementing a full parser, we scan for the common ASN.1
    // UTCTime (tag 0x17) or GeneralizedTime (tag 0x18) pattern.
    // In a proper X.509 cert, validity contains two time values: notBefore and notAfter.
    // We want the second one.

    let mut times = Vec::new();
    let mut i = 0;
    while i < der.len() {
        if der[i] == 0x17 {
            // UTCTime
            if i + 1 < der.len() {
                let len = der[i + 1] as usize;
                if i + 2 + len <= der.len() {
                    if let Ok(s) = std::str::from_utf8(&der[i + 2..i + 2 + len]) {
                        if let Some(dt) = parse_utc_time(s) {
                            times.push(dt);
                        }
                    }
                }
            }
        } else if der[i] == 0x18 {
            // GeneralizedTime
            if i + 1 < der.len() {
                let len = der[i + 1] as usize;
                if i + 2 + len <= der.len() {
                    if let Ok(s) = std::str::from_utf8(&der[i + 2..i + 2 + len]) {
                        if let Some(dt) = parse_generalized_time(s) {
                            times.push(dt);
                        }
                    }
                }
            }
        }
        i += 1;
    }

    // The second time value in the cert is notAfter
    times.get(1).copied()
}

fn parse_utc_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // Format: YYMMDDHHMMSSZ
    use chrono::NaiveDateTime;
    let s = s.trim_end_matches('Z');
    let dt = NaiveDateTime::parse_from_str(s, "%y%m%d%H%M%S").ok()?;
    Some(dt.and_utc())
}

fn parse_generalized_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // Format: YYYYMMDDHHMMSSZ
    use chrono::NaiveDateTime;
    let s = s.trim_end_matches('Z');
    let dt = NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S").ok()?;
    Some(dt.and_utc())
}
