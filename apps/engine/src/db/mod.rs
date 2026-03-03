use sqlx::{postgres::PgPoolOptions, PgPool};

use crate::error::AppError;

pub mod access_logs;
pub mod analytics;
pub mod audit;
pub mod deployments;
pub mod domains;
pub mod edge;
pub mod env_vars;
pub mod firewall;
pub mod models;
pub mod projects;
pub mod refresh_tokens;
pub mod services;
pub mod users;
pub mod waf;

pub type DbPool = PgPool;

pub async fn connect_and_migrate(database_url: &str) -> Result<DbPool, AppError> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .map_err(AppError::Db)?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|error| AppError::Internal(format!("migration error: {error}")))?;

    Ok(pool)
}

pub fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(db_error) => db_error.code().is_some_and(|code| code == "23505"),
        _ => false,
    }
}
