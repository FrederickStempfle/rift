use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

#[derive(Clone, Debug)]
pub struct AcmeChallengeStore {
    challenges: Arc<RwLock<HashMap<String, String>>>,
}

impl AcmeChallengeStore {
    pub fn new() -> Self {
        Self {
            challenges: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn set(&self, token: String, key_authorization: String) {
        self.challenges.write().await.insert(token, key_authorization);
    }

    pub async fn get(&self, token: &str) -> Option<String> {
        self.challenges.read().await.get(token).cloned()
    }

    pub async fn remove(&self, token: &str) {
        self.challenges.write().await.remove(token);
    }
}
