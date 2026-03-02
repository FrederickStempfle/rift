//! DB-backed lifecycle operation tracking for idempotent command execution.
//!
//! Each lifecycle command (deploy, wake, suspend, stop) is assigned an
//! `op_id` (UUID). Before executing, the caller checks if the operation
//! already exists:
//! - If completed/failed: return the prior result without re-executing.
//! - If pending/running: return conflict (operation already in progress).
//! - If absent: insert as "running" and proceed.

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::AppError;

/// A lifecycle operation record.
#[derive(Debug, Clone, FromRow)]
pub struct LifecycleOperation {
    pub op_id: Uuid,
    pub action: String,
    pub project_id: Uuid,
    pub deployment_id: Option<Uuid>,
    pub status: String,
    pub result: Option<JsonValue>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Outcome of attempting to begin an operation.
pub enum BeginOutcome {
    /// Fresh operation — caller should proceed with execution.
    Proceed,
    /// Operation already completed successfully — return prior result.
    Completed(Box<LifecycleOperation>),
    /// Operation already failed — caller must replay the failure.
    Failed(Box<LifecycleOperation>),
    /// Operation currently in progress — caller should not re-execute.
    InProgress,
}

/// Attempt to begin a lifecycle operation.
///
/// If `op_id` already exists, returns the prior state. Otherwise inserts
/// a new row with status = 'running'.
#[tracing::instrument(skip(pool), fields(%op_id, %project_id))]
pub async fn begin_operation(
    pool: &PgPool,
    op_id: Uuid,
    action: &str,
    project_id: Uuid,
    deployment_id: Option<Uuid>,
) -> Result<BeginOutcome, AppError> {
    // Check for existing operation first.
    if let Some(existing) = get_operation(pool, op_id).await? {
        let outcome = classify_existing(existing, action, project_id, deployment_id);
        if let Ok(ref o) = outcome {
            let label = match o {
                BeginOutcome::Proceed => "proceed",
                BeginOutcome::Completed(_) => "completed",
                BeginOutcome::Failed(_) => "failed",
                BeginOutcome::InProgress => "in_progress",
            };
            crate::metrics::OPERATION_OUTCOME
                .with_label_values(&[label])
                .inc();
        }
        return outcome;
    }

    // Insert new operation as running.
    let insert_result = sqlx::query(
        r#"
        INSERT INTO lifecycle_operations (op_id, action, project_id, deployment_id, status)
        VALUES ($1, $2, $3, $4, 'running')
        ON CONFLICT (op_id) DO NOTHING
        "#,
    )
    .bind(op_id)
    .bind(action)
    .bind(project_id)
    .bind(deployment_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    if insert_result.rows_affected() == 1 {
        crate::metrics::OPERATION_OUTCOME
            .with_label_values(&["proceed"])
            .inc();
        return Ok(BeginOutcome::Proceed);
    }

    // Lost the insert race — classify the existing row.
    if let Some(existing) = get_operation(pool, op_id).await? {
        let outcome = classify_existing(existing, action, project_id, deployment_id);
        if let Ok(ref o) = outcome {
            let label = match o {
                BeginOutcome::Proceed => "proceed",
                BeginOutcome::Completed(_) => "completed",
                BeginOutcome::Failed(_) => "failed",
                BeginOutcome::InProgress => "in_progress",
            };
            crate::metrics::OPERATION_OUTCOME
                .with_label_values(&[label])
                .inc();
        }
        return outcome;
    }

    Err(AppError::Internal(
        "operation row missing after insert conflict".into(),
    ))
}

/// Mark an operation as completed with a result payload.
pub async fn complete_operation(
    pool: &PgPool,
    op_id: Uuid,
    result: JsonValue,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE lifecycle_operations
        SET status = 'completed', result = $2, completed_at = now()
        WHERE op_id = $1 AND status = 'running'
        "#,
    )
    .bind(op_id)
    .bind(result)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

/// Mark an operation as failed with an error message.
pub async fn fail_operation(pool: &PgPool, op_id: Uuid, error: &str) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE lifecycle_operations
        SET status = 'failed', error = $2, completed_at = now()
        WHERE op_id = $1 AND status = 'running'
        "#,
    )
    .bind(op_id)
    .bind(error)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

/// Get an operation by its ID.
pub async fn get_operation(
    pool: &PgPool,
    op_id: Uuid,
) -> Result<Option<LifecycleOperation>, AppError> {
    sqlx::query_as::<_, LifecycleOperation>(
        r#"
        SELECT op_id, action, project_id, deployment_id, status,
               result, error, created_at, completed_at
        FROM lifecycle_operations
        WHERE op_id = $1
        "#,
    )
    .bind(op_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

fn classify_existing(
    existing: LifecycleOperation,
    action: &str,
    project_id: Uuid,
    deployment_id: Option<Uuid>,
) -> Result<BeginOutcome, AppError> {
    if existing.action != action || existing.project_id != project_id {
        return Err(AppError::Conflict(
            "op_id already used for a different action/project".into(),
        ));
    }
    if let (Some(expected), Some(current)) = (deployment_id, existing.deployment_id) {
        if expected != current {
            return Err(AppError::Conflict(
                "op_id already used for a different deployment".into(),
            ));
        }
    }

    match existing.status.as_str() {
        "completed" => Ok(BeginOutcome::Completed(Box::new(existing))),
        "failed" => Ok(BeginOutcome::Failed(Box::new(existing))),
        _ => Ok(BeginOutcome::InProgress),
    }
}
