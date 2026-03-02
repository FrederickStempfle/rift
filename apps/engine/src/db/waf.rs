use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct WafPolicy {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub mode: String,
    pub fail_open: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct WafRuleRow {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub match_field: String,
    pub match_op: String,
    pub match_value: String,
    pub header_name: Option<String>,
    pub action: String,
    pub priority: i32,
    pub enabled: bool,
    pub is_managed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct WafEvent {
    pub id: i64,
    pub project_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub client_ip: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub rule_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

pub struct NewWafRule {
    pub project_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub match_field: String,
    pub match_op: String,
    pub match_value: String,
    pub header_name: Option<String>,
    pub action: String,
    pub priority: i32,
    pub is_managed: bool,
}

pub struct NewWafEvent {
    pub project_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub client_ip: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub rule_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Policy CRUD
// ---------------------------------------------------------------------------

pub async fn get_policy(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Option<WafPolicy>, AppError> {
    let row = if let Some(pid) = project_id {
        sqlx::query_as::<_, WafPolicy>("SELECT * FROM waf_policies WHERE project_id = $1")
            .bind(pid)
            .fetch_optional(pool)
            .await
            .map_err(AppError::Db)?
    } else {
        sqlx::query_as::<_, WafPolicy>("SELECT * FROM waf_policies WHERE project_id IS NULL")
            .fetch_optional(pool)
            .await
            .map_err(AppError::Db)?
    };
    Ok(row)
}

pub async fn upsert_policy(
    pool: &PgPool,
    project_id: Option<Uuid>,
    mode: &str,
    fail_open: bool,
) -> Result<WafPolicy, AppError> {
    sqlx::query_as::<_, WafPolicy>(
        r#"
        INSERT INTO waf_policies (project_id, mode, fail_open)
        VALUES ($1, $2, $3)
        ON CONFLICT (COALESCE(project_id, '00000000-0000-0000-0000-000000000000'))
        DO UPDATE SET mode = $2, fail_open = $3, updated_at = NOW()
        RETURNING *
        "#,
    )
    .bind(project_id)
    .bind(mode)
    .bind(fail_open)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

// ---------------------------------------------------------------------------
// Rule CRUD
// ---------------------------------------------------------------------------

pub async fn create_rule(pool: &PgPool, input: NewWafRule) -> Result<WafRuleRow, AppError> {
    sqlx::query_as::<_, WafRuleRow>(
        r#"
        INSERT INTO waf_rules (project_id, name, description, match_field, match_op, match_value, header_name, action, priority, is_managed)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING *
        "#,
    )
    .bind(input.project_id)
    .bind(&input.name)
    .bind(&input.description)
    .bind(&input.match_field)
    .bind(&input.match_op)
    .bind(&input.match_value)
    .bind(&input.header_name)
    .bind(&input.action)
    .bind(input.priority)
    .bind(input.is_managed)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)
}

pub struct UpdateWafRule<'a> {
    pub rule_id: Uuid,
    pub name: &'a str,
    pub description: &'a str,
    pub match_field: &'a str,
    pub match_op: &'a str,
    pub match_value: &'a str,
    pub header_name: Option<&'a str>,
    pub action: &'a str,
    pub priority: i32,
    pub enabled: bool,
}

pub async fn update_rule(
    pool: &PgPool,
    input: UpdateWafRule<'_>,
) -> Result<Option<WafRuleRow>, AppError> {
    sqlx::query_as::<_, WafRuleRow>(
        r#"
        UPDATE waf_rules
        SET name = $2, description = $3, match_field = $4, match_op = $5,
            match_value = $6, header_name = $7, action = $8, priority = $9,
            enabled = $10, updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(input.rule_id)
    .bind(input.name)
    .bind(input.description)
    .bind(input.match_field)
    .bind(input.match_op)
    .bind(input.match_value)
    .bind(input.header_name)
    .bind(input.action)
    .bind(input.priority)
    .bind(input.enabled)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

pub async fn delete_rule(pool: &PgPool, rule_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM waf_rules WHERE id = $1")
        .bind(rule_id)
        .execute(pool)
        .await
        .map_err(AppError::Db)?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_rule(pool: &PgPool, rule_id: Uuid) -> Result<Option<WafRuleRow>, AppError> {
    sqlx::query_as::<_, WafRuleRow>("SELECT * FROM waf_rules WHERE id = $1")
        .bind(rule_id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::Db)
}

pub async fn list_rules(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<WafRuleRow>, AppError> {
    let rows = if let Some(pid) = project_id {
        sqlx::query_as::<_, WafRuleRow>(
            "SELECT * FROM waf_rules WHERE project_id = $1 ORDER BY priority ASC, created_at ASC",
        )
        .bind(pid)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?
    } else {
        sqlx::query_as::<_, WafRuleRow>(
            "SELECT * FROM waf_rules WHERE project_id IS NULL ORDER BY priority ASC, created_at ASC",
        )
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?
    };
    Ok(rows)
}

/// List only enabled rules for a scope, sorted by priority ASC then created_at ASC.
pub async fn list_enabled_rules(
    pool: &PgPool,
    scope: Option<Uuid>,
) -> Result<Vec<WafRuleRow>, AppError> {
    let rows = if let Some(pid) = scope {
        sqlx::query_as::<_, WafRuleRow>(
            "SELECT * FROM waf_rules WHERE project_id = $1 AND enabled = true ORDER BY priority ASC, created_at ASC",
        )
        .bind(pid)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?
    } else {
        sqlx::query_as::<_, WafRuleRow>(
            "SELECT * FROM waf_rules WHERE project_id IS NULL AND enabled = true ORDER BY priority ASC, created_at ASC",
        )
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?
    };
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub async fn insert_event(pool: &PgPool, event: NewWafEvent) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO waf_events (project_id, rule_id, action, client_ip, method, host, path, user_agent, rule_name)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(event.project_id)
    .bind(event.rule_id)
    .bind(&event.action)
    .bind(&event.client_ip)
    .bind(&event.method)
    .bind(&event.host)
    .bind(&event.path)
    .bind(&event.user_agent)
    .bind(&event.rule_name)
    .execute(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

pub async fn list_events(
    pool: &PgPool,
    project_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<WafEvent>, AppError> {
    let rows = if let Some(pid) = project_id {
        sqlx::query_as::<_, WafEvent>(
            "SELECT * FROM waf_events WHERE project_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(pid)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?
    } else {
        sqlx::query_as::<_, WafEvent>(
            "SELECT * FROM waf_events ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?
    };
    Ok(rows)
}

/// Delete events older than the given number of days.
pub async fn cleanup_old_events(pool: &PgPool, retention_days: i32) -> Result<u64, AppError> {
    let result =
        sqlx::query("DELETE FROM waf_events WHERE created_at < NOW() - make_interval(days => $1)")
            .bind(retention_days)
            .execute(pool)
            .await
            .map_err(AppError::Db)?;
    Ok(result.rows_affected())
}

/// Seed managed baseline rules if they don't already exist.
pub async fn seed_managed_rules(pool: &PgPool) -> Result<u32, AppError> {
    let defs = crate::proxy::waf::baseline_managed_rules();
    let mut seeded = 0u32;

    for def in defs {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM waf_rules WHERE name = $1 AND is_managed = true)",
        )
        .bind(def.name)
        .fetch_one(pool)
        .await
        .map_err(AppError::Db)?;

        if !exists {
            create_rule(
                pool,
                NewWafRule {
                    project_id: None,
                    name: def.name.into(),
                    description: def.description.into(),
                    match_field: def.field.as_str().into(),
                    match_op: def.op.as_str().into(),
                    match_value: def.value.into(),
                    header_name: None,
                    action: def.action.as_str().into(),
                    priority: def.priority,
                    is_managed: true,
                },
            )
            .await?;
            seeded += 1;
        }
    }

    Ok(seeded)
}
