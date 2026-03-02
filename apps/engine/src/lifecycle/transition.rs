//! CAS (compare-and-set) deployment state transition functions.
//!
//! These functions atomically transition a deployment from one state to
//! another using SQL `WHERE status = $expected`. If a concurrent mutation
//! has already moved the deployment to a different state, the transition
//! returns `Ok(false)` instead of silently succeeding.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

use super::state_machine::DeploymentState;

/// Atomically transition a deployment from `expected` to `new_state`.
///
/// Returns `Ok(true)` if the transition succeeded (row matched),
/// or `Ok(false)` if the deployment was no longer in `expected` state
/// (concurrent mutation or already terminal).
///
/// Returns `Err` only on database/application errors.
pub async fn transition(
    pool: &PgPool,
    deployment_id: Uuid,
    expected: DeploymentState,
    new_state: DeploymentState,
) -> Result<bool, AppError> {
    if !expected.can_transition_to(new_state) {
        return Err(AppError::Internal(format!(
            "invalid state transition: {} → {}",
            expected, new_state,
        )));
    }

    let result = sqlx::query(
        r#"
        UPDATE deployments
        SET status = $2::deployment_status
        WHERE id = $1 AND status = $3::deployment_status
        "#,
    )
    .bind(deployment_id)
    .bind(new_state.as_str())
    .bind(expected.as_str())
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

/// Transition to `ready` with metadata (url, port, build duration).
///
/// Same CAS semantics as [`transition`] — returns `Ok(false)` if the
/// deployment is no longer in `deploying` state.
pub async fn transition_to_ready(
    pool: &PgPool,
    deployment_id: Uuid,
    url: &str,
    port: u16,
    build_duration_ms: i32,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        UPDATE deployments
        SET status = 'ready',
            url = $2,
            port = $3,
            build_duration_ms = $4,
            finished_at = now()
        WHERE id = $1 AND status = 'deploying'
        "#,
    )
    .bind(deployment_id)
    .bind(url)
    .bind(port as i32)
    .bind(build_duration_ms)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

/// Transition to `suspended` from `ready`, setting `suspended_at`.
///
/// Same CAS semantics — returns `Ok(false)` if the deployment is no
/// longer in `ready` state.
pub async fn transition_to_suspended(pool: &PgPool, deployment_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        UPDATE deployments
        SET status = 'suspended',
            suspended_at = now()
        WHERE id = $1 AND status = 'ready'
        "#,
    )
    .bind(deployment_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

/// Transition from `suspended` back to `ready`, clearing `suspended_at`.
///
/// Same CAS semantics — returns `Ok(false)` if the deployment is no
/// longer in `suspended` state.
pub async fn transition_from_suspended_to_ready(
    pool: &PgPool,
    deployment_id: Uuid,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        UPDATE deployments
        SET status = 'ready',
            suspended_at = NULL
        WHERE id = $1 AND status = 'suspended'
        "#,
    )
    .bind(deployment_id)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}

/// Transition to `failed` with optional build duration.
///
/// Accepts transitions from any non-terminal state (queued, cloning,
/// building, deploying). Returns `Ok(false)` if already terminal.
pub async fn transition_to_failed(
    pool: &PgPool,
    deployment_id: Uuid,
    build_duration_ms: Option<i32>,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        UPDATE deployments
        SET status = 'failed',
            build_duration_ms = COALESCE($2, build_duration_ms),
            finished_at = now()
        WHERE id = $1
          AND status IN ('queued', 'cloning', 'building', 'deploying', 'ready', 'suspended')
        "#,
    )
    .bind(deployment_id)
    .bind(build_duration_ms)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;

    Ok(result.rows_affected() > 0)
}
