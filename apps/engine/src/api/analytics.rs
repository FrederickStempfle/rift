use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::AuthUser, AppState},
    db::{analytics, projects},
    error::{AppError, AppResult},
};

#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    pub project_id: Uuid,
    #[serde(default = "default_period")]
    pub period: String,
}

fn default_period() -> String {
    "24h".into()
}

#[derive(Debug, Serialize)]
pub struct BucketResponse {
    pub bucket: String,
    pub requests: i64,
    pub errors: i64,
    pub avg_ms: f64,
    pub cold_starts: i64,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsResponse {
    pub buckets: Vec<BucketResponse>,
    pub total_requests: i64,
    pub total_errors: i64,
    pub total_cold_starts: i64,
    pub avg_response_ms: f64,
    pub error_rate: f64,
}

pub async fn get_analytics(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<AnalyticsQuery>,
) -> AppResult<Json<AnalyticsResponse>> {
    projects::get_project_for_user(&state.pool, query.project_id, auth_user.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("project not found".into()))?;

    let since = match query.period.as_str() {
        "7d" => Utc::now() - Duration::days(7),
        "30d" => Utc::now() - Duration::days(30),
        _ => Utc::now() - Duration::hours(24),
    };

    let buckets = analytics::query_hourly(&state.pool, query.project_id, since).await?;

    let total_requests: i64 = buckets.iter().map(|b| b.requests).sum();
    let total_errors: i64 = buckets.iter().map(|b| b.errors).sum();
    let total_ms: i64 = buckets.iter().map(|b| b.total_ms).sum();
    let total_cold_starts: i64 = buckets.iter().map(|b| b.cold_starts).sum();

    let avg_response_ms = if total_requests > 0 {
        total_ms as f64 / total_requests as f64
    } else {
        0.0
    };
    let error_rate = if total_requests > 0 {
        total_errors as f64 / total_requests as f64 * 100.0
    } else {
        0.0
    };

    let response_buckets = buckets
        .into_iter()
        .map(|b| {
            let avg = if b.requests > 0 {
                b.total_ms as f64 / b.requests as f64
            } else {
                0.0
            };
            BucketResponse {
                bucket: b.bucket.to_rfc3339(),
                requests: b.requests,
                errors: b.errors,
                avg_ms: avg,
                cold_starts: b.cold_starts,
            }
        })
        .collect();

    Ok(Json(AnalyticsResponse {
        buckets: response_buckets,
        total_requests,
        total_errors,
        total_cold_starts,
        avg_response_ms,
        error_rate,
    }))
}
