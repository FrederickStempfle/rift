# Rift Engine SLO Definitions

## 1. Deploy Success Rate

**Metric**: `rift_deploy_outcome_total{outcome="success"}` / sum(`rift_deploy_outcome_total`)
**Target**: >= 95% rolling 7-day
**Window**: 7 days

### Alert Rules

- **Warning** (`deploy_success_rate_low`): success rate < 95% over 1h
  ```yaml
  expr: |
    sum(rate(rift_deploy_outcome_total{outcome="success"}[1h]))
    / sum(rate(rift_deploy_outcome_total[1h])) < 0.95
  for: 5m
  severity: warning
  ```

- **Critical** (`deploy_success_rate_critical`): success rate < 80% over 15m
  ```yaml
  expr: |
    sum(rate(rift_deploy_outcome_total{outcome="success"}[15m]))
    / sum(rate(rift_deploy_outcome_total[15m])) < 0.80
  for: 5m
  severity: critical
  ```

## 2. Cold Start Latency (p95)

**Metric**: `histogram_quantile(0.95, rate(rift_cold_start_duration_seconds_bucket[5m]))`
**Target**: <= 3s (p95)
**Window**: rolling 5m

### Alert Rules

- **Warning** (`cold_start_p95_high`): p95 > 3s
  ```yaml
  expr: |
    histogram_quantile(0.95,
      rate(rift_cold_start_duration_seconds_bucket{kind="wake"}[5m])
    ) > 3
  for: 10m
  severity: warning
  ```

- **Critical** (`cold_start_p95_critical`): p95 > 10s
  ```yaml
  expr: |
    histogram_quantile(0.95,
      rate(rift_cold_start_duration_seconds_bucket{kind="wake"}[5m])
    ) > 10
  for: 5m
  severity: critical
  ```

## 3. Proxy Routing Latency (Cache Hit Rate)

**Metric**: `rift_routing_cache_result_total{result="hit"}` / sum(`rift_routing_cache_result_total`)
**Target**: >= 90% cache hit rate
**Window**: rolling 5m

### Alert Rules

- **Warning** (`routing_cache_hit_low`): hit rate < 80%
  ```yaml
  expr: |
    sum(rate(rift_routing_cache_result_total{result="hit"}[5m]))
    / sum(rate(rift_routing_cache_result_total[5m])) < 0.80
  for: 10m
  severity: warning
  ```

## 4. Worker Heartbeat Freshness

**Metric**: `rift_heartbeat_send_total{outcome="ok"}` rate vs expected
**Target**: no worker heartbeat gap > 30s
**Window**: continuous

### Alert Rules

- **Warning** (`heartbeat_failures`): error rate > 10%
  ```yaml
  expr: |
    sum(rate(rift_heartbeat_send_total{outcome="error"}[5m]))
    / sum(rate(rift_heartbeat_send_total[5m])) > 0.10
  for: 5m
  severity: warning
  ```

- **Critical** (`heartbeat_all_failing`): no successful heartbeats for 60s
  ```yaml
  expr: |
    sum(rate(rift_heartbeat_send_total{outcome="ok"}[1m])) == 0
    and sum(rate(rift_heartbeat_send_total{outcome="error"}[1m])) > 0
  for: 1m
  severity: critical
  ```

## 5. Critical Event Alerts

### Redis Disconnect Loops

```yaml
- alert: redis_disconnect_loop
  expr: |
    increase(rift_heartbeat_send_total{outcome="error"}[5m]) > 10
  for: 2m
  severity: critical
  annotations:
    summary: "Redis connectivity issues — repeated heartbeat failures"
```

### Scheduler Placement Failures

```yaml
- alert: scheduler_placement_failures
  expr: |
    sum(rate(rift_scheduler_placement_total{outcome="failed"}[5m]))
    / sum(rate(rift_scheduler_placement_total[5m])) > 0.20
  for: 5m
  severity: warning
  annotations:
    summary: "High scheduler placement failure rate — capacity issue"
```

### Repeated CAS Conflicts

```yaml
- alert: operation_cas_conflicts
  expr: |
    sum(rate(rift_operation_outcome_total{outcome="in_progress"}[5m])) > 0.5
  for: 10m
  severity: warning
  annotations:
    summary: "Elevated CAS conflict rate — possible duplicate operations"
```

### Resource Limit Violations

```yaml
- alert: resource_violations_elevated
  expr: |
    sum(rate(rift_resource_violation_total[5m])) > 0.1
  for: 5m
  severity: warning
  annotations:
    summary: "Resource limit violations detected — check cgroup health"
```

### Build Queue Depth

```yaml
- alert: build_queue_depth_high
  expr: rift_build_queue_depth > 10
  for: 5m
  severity: warning
  annotations:
    summary: "Build queue backing up — consider increasing RIFT_BUILD_CONCURRENCY"
```
