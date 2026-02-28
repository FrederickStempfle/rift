pub mod crypto;

#[derive(Clone, Debug, Default)]
pub struct SecretsManager;

impl SecretsManager {
    pub fn new() -> Self {
        Self
    }
}
