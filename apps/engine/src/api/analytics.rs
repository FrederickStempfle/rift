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
    /// When `None`, aggregate across all of the user's projects.
    pub project_id: Option<Uuid>,
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
pub struct ReferrerResponse {
    pub referrer: String,
    pub requests: i64,
}

#[derive(Debug, Serialize)]
pub struct PathResponse {
    pub path: String,
    pub requests: i64,
    pub errors: i64,
    pub avg_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsResponse {
    pub buckets: Vec<BucketResponse>,
    pub total_requests: i64,
    pub total_errors: i64,
    pub total_cold_starts: i64,
    pub avg_response_ms: f64,
    pub error_rate: f64,
    pub top_referrers: Vec<ReferrerResponse>,
    pub top_paths: Vec<PathResponse>,
}

pub async fn get_analytics(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Query(query): Query<AnalyticsQuery>,
) -> AppResult<Json<AnalyticsResponse>> {
    let since = match query.period.as_str() {
        "7d" => Utc::now() - Duration::days(7),
        "30d" => Utc::now() - Duration::days(30),
        _ => Utc::now() - Duration::hours(24),
    };

    let (buckets, top_referrers_raw, top_paths_raw) = if let Some(project_id) = query.project_id {
        // Single-project analytics
        projects::get_project_for_user(&state.pool, project_id, auth_user.user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("project not found".into()))?;

        let buckets = analytics::query_hourly(&state.pool, project_id, since).await?;
        let referrers =
            analytics::query_top_referrers(&state.pool, project_id, since, 10).await?;
        let paths = analytics::query_top_paths(&state.pool, project_id, since, 10).await?;
        (buckets, referrers, paths)
    } else {
        // Platform-wide analytics (aggregate all user's projects)
        let buckets =
            analytics::query_hourly_for_user(&state.pool, auth_user.user_id, since).await?;
        let referrers =
            analytics::query_top_referrers_for_user(&state.pool, auth_user.user_id, since, 10)
                .await?;
        let paths =
            analytics::query_top_paths_for_user(&state.pool, auth_user.user_id, since, 10)
                .await?;
        (buckets, referrers, paths)
    };

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

    let top_referrers = top_referrers_raw
        .into_iter()
        .map(|r| ReferrerResponse {
            referrer: r.referrer,
            requests: r.requests,
        })
        .collect();

    let top_paths = top_paths_raw
        .into_iter()
        .map(|p| {
            let avg = if p.requests > 0 {
                p.total_ms as f64 / p.requests as f64
            } else {
                0.0
            };
            PathResponse {
                path: p.path,
                requests: p.requests,
                errors: p.errors,
                avg_ms: avg,
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
        top_referrers,
        top_paths,
    }))
}
