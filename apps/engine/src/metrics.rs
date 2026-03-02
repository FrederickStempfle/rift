//! Operational metrics for the Rift engine.
//!
//! All metric names are prefixed with `rift_` for stable scoping.
//! Labels are added conservatively to control cardinality.

use once_cell::sync::Lazy;
use prometheus::{
    register_counter_vec, register_gauge_vec, register_histogram_vec, CounterVec, Encoder, Gauge,
    GaugeVec, HistogramVec, TextEncoder,
};

// --- Deploy lifecycle ---

pub static DEPLOY_STAGE_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "rift_deploy_stage_duration_seconds",
        "Duration of each deploy stage",
        &["stage"],
        vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0]
    )
    .expect("metric registration failed")
});

pub static DEPLOY_OUTCOME: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_deploy_outcome_total",
        "Total deploy outcomes",
        &["outcome"]
    )
    .expect("metric registration failed")
});

// --- Cold start / wake / suspend / stop ---

pub static RUNTIME_EVENT: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_runtime_event_total",
        "Runtime lifecycle events (cold_start, wake, suspend, stop)",
        &["event"]
    )
    .expect("metric registration failed")
});

pub static COLD_START_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "rift_cold_start_duration_seconds",
        "Duration of cold starts and wakes",
        &["kind"],
        vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0]
    )
    .expect("metric registration failed")
});

// --- Routing cache ---

pub static ROUTING_CACHE_RESULT: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_routing_cache_result_total",
        "Routing cache lookup outcomes (hit, negative_hit, miss)",
        &["result"]
    )
    .expect("metric registration failed")
});

// --- Scheduler placement ---

pub static SCHEDULER_PLACEMENT: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_scheduler_placement_total",
        "Scheduler placement outcomes (success, failed, self_placed)",
        &["outcome"]
    )
    .expect("metric registration failed")
});

// --- Operation idempotency ---

pub static OPERATION_OUTCOME: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_operation_outcome_total",
        "Lifecycle operation idempotency outcomes",
        &["outcome"]
    )
    .expect("metric registration failed")
});

// --- Resource limit violations ---

pub static RESOURCE_VIOLATION: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_resource_violation_total",
        "Resource limit violation events",
        &["kind"]
    )
    .expect("metric registration failed")
});

// --- Pool gauges ---

pub static POOL_WARM_WORKERS: Lazy<Gauge> = Lazy::new(|| {
    prometheus::register_gauge!(
        "rift_pool_warm_workers",
        "Number of pre-warmed workers in the pool"
    )
    .expect("metric registration failed")
});

pub static POOL_ACTIVE_WORKERS: Lazy<Gauge> = Lazy::new(|| {
    prometheus::register_gauge!(
        "rift_pool_active_workers",
        "Number of active (specialized) workers"
    )
    .expect("metric registration failed")
});

pub static POOL_SUSPENDED: Lazy<Gauge> = Lazy::new(|| {
    prometheus::register_gauge!(
        "rift_pool_suspended_deployments",
        "Number of suspended deployments"
    )
    .expect("metric registration failed")
});

// --- Build metrics ---

pub static BUILD_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "rift_build_duration_seconds",
        "Total build duration",
        &["outcome"],
        vec![1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0]
    )
    .expect("metric registration failed")
});

pub static BUILD_QUEUE_DEPTH: Lazy<Gauge> = Lazy::new(|| {
    prometheus::register_gauge!(
        "rift_build_queue_depth",
        "Number of builds waiting for a slot"
    )
    .expect("metric registration failed")
});

// --- Worker heartbeat ---

pub static HEARTBEAT_SEND: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_heartbeat_send_total",
        "Heartbeat send outcomes",
        &["outcome"]
    )
    .expect("metric registration failed")
});

pub static WORKER_CPU_FREE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "rift_worker_cpu_free_pct",
        "Worker CPU free percentage",
        &["worker_id"]
    )
    .expect("metric registration failed")
});

pub static WORKER_MEM_FREE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "rift_worker_mem_free_bytes",
        "Worker memory free in bytes",
        &["worker_id"]
    )
    .expect("metric registration failed")
});

// --- Abuse controls ---

pub static ABUSE_DECISION: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_abuse_decision_total",
        "Abuse guard decisions by scope and action",
        &["scope", "action"]
    )
    .expect("metric registration failed")
});

pub static ABUSE_BAN_TIER: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "rift_abuse_ban_tier_total",
        "Number of bans applied per escalation tier",
        &["tier"]
    )
    .expect("metric registration failed")
});

/// Render all registered metrics in Prometheus text format.
pub fn encode_metrics() -> String {
    let encoder = TextEncoder::new();
    let families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&families, &mut buffer).unwrap_or_default();
    String::from_utf8(buffer).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_encode_without_panic() {
        // Force initialization of all lazy metrics
        DEPLOY_STAGE_DURATION
            .with_label_values(&["cloning"])
            .observe(1.0);
        DEPLOY_OUTCOME.with_label_values(&["success"]).inc();
        RUNTIME_EVENT.with_label_values(&["cold_start"]).inc();
        COLD_START_DURATION
            .with_label_values(&["wake"])
            .observe(0.5);
        ROUTING_CACHE_RESULT.with_label_values(&["hit"]).inc();
        SCHEDULER_PLACEMENT.with_label_values(&["success"]).inc();
        OPERATION_OUTCOME.with_label_values(&["proceed"]).inc();
        RESOURCE_VIOLATION
            .with_label_values(&["cgroup_unavailable"])
            .inc();
        POOL_WARM_WORKERS.set(3.0);
        POOL_ACTIVE_WORKERS.set(5.0);
        POOL_SUSPENDED.set(2.0);
        BUILD_DURATION.with_label_values(&["success"]).observe(30.0);
        BUILD_QUEUE_DEPTH.set(1.0);
        HEARTBEAT_SEND.with_label_values(&["ok"]).inc();
        ABUSE_DECISION
            .with_label_values(&["proxy.global_ip", "allow"])
            .inc();
        ABUSE_BAN_TIER.with_label_values(&["5m"]).inc();

        let output = encode_metrics();
        assert!(!output.is_empty());
        assert!(output.contains("rift_deploy_outcome_total"));
        assert!(output.contains("rift_runtime_event_total"));
        assert!(output.contains("rift_routing_cache_result_total"));
        assert!(output.contains("rift_pool_warm_workers"));
        assert!(output.contains("rift_build_duration_seconds"));
        assert!(output.contains("rift_abuse_decision_total"));
    }

    #[test]
    fn all_metric_names_are_rift_prefixed() {
        // Force at least one metric to be registered
        DEPLOY_OUTCOME.with_label_values(&["test"]).inc();

        let output = encode_metrics();
        for line in output.lines() {
            if line.starts_with('#') {
                // Comment lines (HELP/TYPE) should reference rift_ metrics
                if line.contains("HELP") || line.contains("TYPE") {
                    assert!(
                        line.contains("rift_") || line.contains("process_"),
                        "non-rift metric found: {line}"
                    );
                }
            }
        }
    }

    #[test]
    fn counter_increments_are_cumulative() {
        DEPLOY_OUTCOME.with_label_values(&["test_cumulative"]).inc();
        DEPLOY_OUTCOME.with_label_values(&["test_cumulative"]).inc();
        DEPLOY_OUTCOME.with_label_values(&["test_cumulative"]).inc();

        let val = DEPLOY_OUTCOME.with_label_values(&["test_cumulative"]).get();
        assert!(val >= 3.0);
    }
}
