use std::sync::Arc;

use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessTokenClaims {
    pub sub: Uuid,
    pub iat: i64,
    pub exp: i64,
    pub jti: Uuid,
}

#[derive(Clone)]
pub struct TokenService {
    encoding_key: Arc<EncodingKey>,
    decoding_key: Arc<DecodingKey>,
    access_ttl: Duration,
    refresh_ttl: Duration,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuedAccessToken {
    pub token: String,
    pub expires_at: i64,
}

impl TokenService {
    pub fn new(
        private_key_pem: &str,
        public_key_pem: &str,
        access_ttl: Duration,
        refresh_ttl: Duration,
    ) -> Result<Self, AppError> {
        let encoding_key = EncodingKey::from_ed_pem(private_key_pem.as_bytes())
            .map_err(|e| AppError::Internal(format!("invalid private key pem: {e}")))?;
        let decoding_key = DecodingKey::from_ed_pem(public_key_pem.as_bytes())
            .map_err(|e| AppError::Internal(format!("invalid public key pem: {e}")))?;

        Ok(Self {
            encoding_key: Arc::new(encoding_key),
            decoding_key: Arc::new(decoding_key),
            access_ttl,
            refresh_ttl,
        })
    }

    pub fn issue_access_token(&self, user_id: Uuid) -> Result<IssuedAccessToken, AppError> {
        let issued_at = Utc::now();
        let expires_at = issued_at + self.access_ttl;
        let claims = AccessTokenClaims {
            sub: user_id,
            iat: issued_at.timestamp(),
            exp: expires_at.timestamp(),
            jti: Uuid::new_v4(),
        };
        let mut header = Header::new(Algorithm::EdDSA);
        header.typ = Some("JWT".to_owned());

        let token = encode(&header, &claims, &self.encoding_key)
            .map_err(|e| AppError::Internal(format!("jwt encode failed: {e}")))?;

        Ok(IssuedAccessToken {
            token,
            expires_at: claims.exp,
        })
    }

    pub fn verify_access_token(&self, token: &str) -> Result<AccessTokenClaims, AppError> {
        let validation = Validation::new(Algorithm::EdDSA);
        decode::<AccessTokenClaims>(token, &self.decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|_| AppError::Unauthorized("invalid or expired access token".into()))
    }

    pub fn generate_refresh_token(&self) -> String {
        let mut bytes = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    }

    pub fn hash_refresh_token(&self, token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn refresh_ttl(&self) -> Duration {
        self.refresh_ttl
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::TokenService;

    #[test]
    fn issues_and_verifies_access_token() {
        let (private_key, public_key) = match test_keys() {
            Some(keys) => keys,
            None => return,
        };

        let svc = TokenService::new(
            &private_key,
            &public_key,
            Duration::minutes(15),
            Duration::days(7),
        )
        .expect("token service should initialize");

        let issued = svc
            .issue_access_token(uuid::Uuid::new_v4())
            .expect("token should issue");
        let claims = svc
            .verify_access_token(&issued.token)
            .expect("token should verify");

        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn refresh_token_hash_is_deterministic() {
        let (private_key, public_key) = match test_keys() {
            Some(keys) => keys,
            None => return,
        };

        let svc = TokenService::new(
            &private_key,
            &public_key,
            Duration::minutes(15),
            Duration::days(7),
        )
        .expect("token service should initialize");

        let token = "abc";
        assert_eq!(svc.hash_refresh_token(token), svc.hash_refresh_token(token));
    }

    fn test_keys() -> Option<(String, String)> {
        let private_key = std::env::var("TEST_ED25519_PRIVATE_KEY_PEM").ok()?;
        let public_key = std::env::var("TEST_ED25519_PUBLIC_KEY_PEM").ok()?;
        Some((
            private_key.replace("\\n", "\n"),
            public_key.replace("\\n", "\n"),
        ))
    }
}
