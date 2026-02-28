use std::collections::HashMap;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::sync::RwLock;

#[derive(Clone, Debug)]
pub struct CertResolver {
    certs: Arc<RwLock<HashMap<String, Arc<CertifiedKey>>>>,
}

impl CertResolver {
    pub fn new() -> Self {
        Self {
            certs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn load_cert(
        &self,
        domain: &str,
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> anyhow::Result<()> {
        let certs = rustls_pemfile::certs(&mut BufReader::new(cert_pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("failed to parse cert PEM: {e}"))?;

        let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem))
            .map_err(|e| anyhow::anyhow!("failed to parse key PEM: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("no private key found in PEM"))?;

        let signing_key = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key)
            .map_err(|e| anyhow::anyhow!("unsupported private key type: {e}"))?;

        let certified = Arc::new(CertifiedKey::new(certs, signing_key));
        self.certs
            .write()
            .await
            .insert(domain.to_lowercase(), certified);
        Ok(())
    }

    pub async fn load_from_disk(&self, ssl_dir: &Path) -> anyhow::Result<usize> {
        let mut count = 0;
        let entries = match std::fs::read_dir(ssl_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let domain = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) if name != "account.json" => name.to_owned(),
                _ => continue,
            };
            let cert_path = path.join("cert.pem");
            let key_path = path.join("key.pem");
            if cert_path.exists() && key_path.exists() {
                let cert_pem = std::fs::read(&cert_path)?;
                let key_pem = std::fs::read(&key_path)?;
                match self.load_cert(&domain, &cert_pem, &key_pem).await {
                    Ok(()) => {
                        tracing::info!(domain = %domain, "loaded TLS certificate from disk");
                        count += 1;
                    }
                    Err(e) => {
                        tracing::warn!(domain = %domain, error = %e, "failed to load TLS certificate");
                    }
                }
            }
        }
        Ok(count)
    }

    pub async fn has_cert(&self, domain: &str) -> bool {
        self.certs.read().await.contains_key(&domain.to_lowercase())
    }

    pub async fn has_any_certs(&self) -> bool {
        !self.certs.read().await.is_empty()
    }

    /// Synchronous resolve for the rustls trait.
    fn resolve_sync(&self, server_name: Option<&str>) -> Option<Arc<CertifiedKey>> {
        let name = server_name?.to_lowercase();
        // Try blocking read — this is called from the rustls handshake path which is sync.
        // The RwLock should rarely be contended since writes only happen during cert provisioning.
        let certs = self.certs.try_read().ok()?;
        certs.get(&name).cloned()
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.resolve_sync(client_hello.server_name())
    }
}
