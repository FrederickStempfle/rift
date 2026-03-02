use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

use super::models::{DeployArtifact, DeployRelease, EdgeNode, Region, RouteBinding};

pub async fn create_or_update_artifact(
    pool: &PgPool,
    deployment_id: Uuid,
    digest: &str,
    size_bytes: i64,
    manifest_json: &JsonValue,
) -> Result<DeployArtifact, AppError> {
    sqlx::query_as::<_, DeployArtifact>(
        r#"
        INSERT INTO deploy_artifacts (deployment_id, digest, size_bytes, manifest_json)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (deployment_id)
        DO UPDATE SET
            digest = EXCLUDED.digest,
            size_bytes = EXCLUDED.size_bytes,
            manifest_json = EXCLUDED.manifest_json,
            signed_at = now()
        RETURNING
            id, deployment_id, digest, size_bytes, manifest_json, signed_at, created_at
        "#,
    )
    .bind(deployment_id)
    .bind(digest)
    .bind(size_bytes)
    .bind(manifest_json)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn create_release_for_deployment(
    pool: &PgPool,
    project_id: Uuid,
    deployment_id: Uuid,
    artifact_id: Uuid,
) -> Result<DeployRelease, AppError> {
    sqlx::query_as::<_, DeployRelease>(
        r#"
        WITH next_version AS (
            SELECT COALESCE(MAX(version), 0) + 1 AS version
            FROM deploy_releases
            WHERE project_id = $1
        )
        INSERT INTO deploy_releases (project_id, deployment_id, artifact_id, version, state)
        VALUES ($1, $2, $3, (SELECT version FROM next_version), 'packaged')
        ON CONFLICT (deployment_id)
        DO UPDATE SET artifact_id = EXCLUDED.artifact_id
        RETURNING
            id, project_id, deployment_id, artifact_id, version,
            state::text AS state, promoted_at, created_at
        "#,
    )
    .bind(project_id)
    .bind(deployment_id)
    .bind(artifact_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_releases_for_project_user(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<DeployRelease>, AppError> {
    sqlx::query_as::<_, DeployRelease>(
        r#"
        SELECT
            r.id, r.project_id, r.deployment_id, r.artifact_id, r.version,
            r.state::text AS state, r.promoted_at, r.created_at
        FROM deploy_releases r
        JOIN projects p ON p.id = r.project_id
        WHERE r.project_id = $1 AND p.user_id = $2
        ORDER BY r.version DESC
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_release_for_user(
    pool: &PgPool,
    release_id: Uuid,
    user_id: Uuid,
) -> Result<Option<DeployRelease>, AppError> {
    sqlx::query_as::<_, DeployRelease>(
        r#"
        SELECT
            r.id, r.project_id, r.deployment_id, r.artifact_id, r.version,
            r.state::text AS state, r.promoted_at, r.created_at
        FROM deploy_releases r
        JOIN projects p ON p.id = r.project_id
        WHERE r.id = $1 AND p.user_id = $2
        "#,
    )
    .bind(release_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn mark_release_promoted(
    pool: &PgPool,
    release_id: Uuid,
) -> Result<Option<DeployRelease>, AppError> {
    sqlx::query_as::<_, DeployRelease>(
        r#"
        UPDATE deploy_releases
        SET state = 'promoted', promoted_at = now()
        WHERE id = $1
        RETURNING
            id, project_id, deployment_id, artifact_id, version,
            state::text AS state, promoted_at, created_at
        "#,
    )
    .bind(release_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn mark_release_rollback(
    pool: &PgPool,
    release_id: Uuid,
) -> Result<Option<DeployRelease>, AppError> {
    sqlx::query_as::<_, DeployRelease>(
        r#"
        UPDATE deploy_releases
        SET state = 'rollback'
        WHERE id = $1
        RETURNING
            id, project_id, deployment_id, artifact_id, version,
            state::text AS state, promoted_at, created_at
        "#,
    )
    .bind(release_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn upsert_route_binding(
    pool: &PgPool,
    host: &str,
    project_id: Uuid,
    release_id: Uuid,
) -> Result<RouteBinding, AppError> {
    sqlx::query_as::<_, RouteBinding>(
        r#"
        INSERT INTO route_bindings (host, project_id, release_id, version, updated_at)
        VALUES ($1, $2, $3, 1, now())
        ON CONFLICT (host)
        DO UPDATE SET
            project_id = EXCLUDED.project_id,
            release_id = EXCLUDED.release_id,
            version = route_bindings.version + 1,
            updated_at = now()
        RETURNING host, project_id, release_id, version, updated_at
        "#,
    )
    .bind(host)
    .bind(project_id)
    .bind(release_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn get_route_binding(pool: &PgPool, host: &str) -> Result<Option<RouteBinding>, AppError> {
    sqlx::query_as::<_, RouteBinding>(
        r#"
        SELECT host, project_id, release_id, version, updated_at
        FROM route_bindings
        WHERE host = $1
        "#,
    )
    .bind(host)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_regions(pool: &PgPool) -> Result<Vec<Region>, AppError> {
    sqlx::query_as::<_, Region>(
        r#"
        SELECT id, name, status, created_at
        FROM regions
        ORDER BY name
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_edge_nodes(pool: &PgPool) -> Result<Vec<EdgeNode>, AppError> {
    sqlx::query_as::<_, EdgeNode>(
        r#"
        SELECT id, region_id, addr, status, capacity, last_heartbeat_at, created_at
        FROM edge_nodes
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}
