use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};

use crate::error::AppError;

#[derive(Clone)]
pub struct PasswordService {
    argon2: Argon2<'static>,
    dummy_hash: String,
}

impl PasswordService {
    pub fn new() -> Result<Self, AppError> {
        let params = Params::new(65_536, 3, 4, None)
            .map_err(|e| AppError::Internal(format!("argon2 params error: {e}")))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        let salt = SaltString::generate(&mut OsRng);
        let dummy_hash = argon2
            .hash_password(b"rift-dummy-password", &salt)
            .map_err(|e| AppError::Internal(format!("argon2 hash error: {e}")))?
            .to_string();

        Ok(Self { argon2, dummy_hash })
    }

    pub fn hash_password(&self, password: &str) -> Result<String, AppError> {
        let salt = SaltString::generate(&mut OsRng);
        self.argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|v| v.to_string())
            .map_err(|e| AppError::Internal(format!("argon2 hash error: {e}")))
    }

    pub fn verify_password(&self, hash: &str, password: &str) -> bool {
        PasswordHash::new(hash)
            .ok()
            .and_then(|parsed| {
                self.argon2
                    .verify_password(password.as_bytes(), &parsed)
                    .ok()
            })
            .is_some()
    }

    pub fn verify_or_dummy(&self, maybe_hash: Option<&str>, password: &str) -> bool {
        if let Some(hash) = maybe_hash {
            return self.verify_password(hash, password);
        }

        self.verify_password(&self.dummy_hash, password)
    }
}

#[cfg(test)]
mod tests {
    use super::PasswordService;

    #[test]
    fn hashes_and_verifies_password() {
        let svc = PasswordService::new().expect("password service init should work");
        let hash = svc
            .hash_password("averysecurepassword")
            .expect("hash should be generated");
        assert!(svc.verify_password(&hash, "averysecurepassword"));
        assert!(!svc.verify_password(&hash, "wrong-password"));
    }
}
