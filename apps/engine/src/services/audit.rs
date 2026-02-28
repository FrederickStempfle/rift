use std::net::IpAddr;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{db::audit, error::AppError};

#[derive(Clone)]
pub struct AuditLogger {
    pool: PgPool,
}

#[derive(Clone)]
pub struct AuditEvent {
    pub user_id: Option<Uuid>,
    pub event: &'static str,
    pub resource_id: Option<Uuid>,
    pub ip_address: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub metadata: Value,
}

impl AuditLogger {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn log(&self, event: AuditEvent) -> Result<(), AppError> {
        audit::insert_event(&self.pool, event).await
    }
}
