use sqlx::PgPool;

use crate::{error::AppError, services::audit::AuditEvent};

pub async fn insert_event(pool: &PgPool, event: AuditEvent) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO audit_log (
            user_id,
            event,
            resource_id,
            ip_address,
            user_agent,
            metadata
        )
        VALUES ($1, $2, $3, $4::inet, $5, $6)
        "#,
    )
    .bind(event.user_id)
    .bind(event.event)
    .bind(event.resource_id)
    .bind(event.ip_address.map(|ip| ip.to_string()))
    .bind(event.user_agent)
    .bind(event.metadata)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(())
}
