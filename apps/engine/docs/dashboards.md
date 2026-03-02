# Rift Engine Grafana Dashboard Definitions

These dashboard panel definitions use Prometheus metrics exposed at `GET /metrics`.

## Dashboard 1: Deploy Overview

### Panel: Deploy Outcome Rate
```promql
# Stacked time series: success / failed / timeout
sum by (outcome) (rate(rift_deploy_outcome_total[5m]))
```

### Panel: Deploy Stage Duration (p95)
```promql
# Per-stage p95 latency
histogram_quantile(0.95,
  sum by (stage, le) (rate(rift_deploy_stage_duration_seconds_bucket[5m]))
)
```

### Panel: Build Duration (p50/p95/p99)
```promql
histogram_quantile(0.50, sum by (le) (rate(rift_build_duration_seconds_bucket[5m])))
histogram_quantile(0.95, sum by (le) (rate(rift_build_duration_seconds_bucket[5m])))
histogram_quantile(0.99, sum by (le) (rate(rift_build_duration_seconds_bucket[5m])))
```

### Panel: Build Queue Depth
```promql
rift_build_queue_depth
```

## Dashboard 2: Runtime Overview

### Panel: Runtime Events
```promql
# Stacked time series: cold_start / wake / suspend / stop
sum by (event) (rate(rift_runtime_event_total[5m]))
```

### Panel: Cold Start Latency (p50/p95/p99)
```promql
histogram_quantile(0.50, sum by (le) (rate(rift_cold_start_duration_seconds_bucket[5m])))
histogram_quantile(0.95, sum by (le) (rate(rift_cold_start_duration_seconds_bucket[5m])))
histogram_quantile(0.99, sum by (le) (rate(rift_cold_start_duration_seconds_bucket[5m])))
```

### Panel: Pool Worker Counts
```promql
rift_pool_warm_workers
rift_pool_active_workers
rift_pool_suspended_deployments
```

### Panel: Resource Violations
```promql
sum by (kind) (rate(rift_resource_violation_total[5m]))
```

## Dashboard 3: Proxy & Routing

### Panel: Routing Cache Performance
```promql
# Stacked: hit / negative_hit / miss
sum by (result) (rate(rift_routing_cache_result_total[5m]))
```

### Panel: Cache Hit Rate (%)
```promql
sum(rate(rift_routing_cache_result_total{result="hit"}[5m]))
/ sum(rate(rift_routing_cache_result_total[5m])) * 100
```

## Dashboard 4: Distributed State

### Panel: Scheduler Placement Outcomes
```promql
sum by (outcome) (rate(rift_scheduler_placement_total[5m]))
```

### Panel: Heartbeat Status
```promql
sum by (outcome) (rate(rift_heartbeat_send_total[5m]))
```

### Panel: Worker CPU Free (per worker)
```promql
rift_worker_cpu_free_pct
```

### Panel: Worker Memory Free (per worker)
```promql
rift_worker_mem_free_bytes
```

### Panel: Operation Idempotency Outcomes
```promql
sum by (outcome) (rate(rift_operation_outcome_total[5m]))
```
