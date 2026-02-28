use sqlx::PgPool;
use uuid::Uuid;

use crate::{db::models::FirewallRule, error::AppError};

pub struct NewFirewallRule {
    pub project_id: Uuid,
    pub cidr: String,
    pub action: String,
    pub description: String,
}

pub async fn create_rule(pool: &PgPool, input: NewFirewallRule) -> Result<FirewallRule, AppError> {
    sqlx::query_as::<_, FirewallRule>(
        r#"
        INSERT INTO firewall_rules (project_id, cidr, action, description)
        VALUES ($1, $2::INET, $3, $4)
        RETURNING id, project_id, cidr::text AS cidr, action, description, created_at
        "#,
    )
    .bind(input.project_id)
    .bind(&input.cidr)
    .bind(&input.action)
    .bind(&input.description)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn list_rules(pool: &PgPool, project_id: Uuid) -> Result<Vec<FirewallRule>, AppError> {
    sqlx::query_as::<_, FirewallRule>(
        r#"
        SELECT id, project_id, cidr::text AS cidr, action, description, created_at
        FROM firewall_rules
        WHERE project_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn delete_rule(
    pool: &PgPool,
    rule_id: Uuid,
    project_id: Uuid,
) -> Result<bool, AppError> {
    let result =
        sqlx::query("DELETE FROM firewall_rules WHERE id = $1 AND project_id = $2")
            .bind(rule_id)
            .bind(project_id)
            .execute(pool)
            .await
            .map_err(AppError::Db)?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_firewall_mode(pool: &PgPool, project_id: Uuid) -> Result<String, AppError> {
    sqlx::query_scalar::<_, String>(
        "SELECT firewall_mode::text FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn set_firewall_mode(
    pool: &PgPool,
    project_id: Uuid,
    mode: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE projects SET firewall_mode = $1::firewall_mode WHERE id = $2")
        .bind(mode)
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(AppError::Db)?;
    Ok(())
}
