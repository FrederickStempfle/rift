# Core Hosting Next Plan (Post Phase 0-4)

## Current State

Implemented and validated:
- Phase 0: test baseline, mock runtime backend, proxy unit coverage.
- Phase 1: `StateStore` abstraction, local/redis stores, scheduler + heartbeat wiring.
- Phase 2: routing cache + invalidation + subscriber wiring.
- Phase 3: lifecycle state machine + CAS transitions + operation idempotency.
- Phase 4: suspend/wake persistence with DB state.
- Critical correctness hardening: idempotency race fix, failed-op replay fix, stop-on-suspended persistence fix, distributed routing write/remove wiring.

Validation status:
- `cargo fmt --all` passes
- `cargo clippy -p rift-engine --all-targets -- -D warnings` passes
- `cargo test -p rift-engine` passes

## Goal

Finish the remaining core-hosting foundation to be production-credible for multi-tenant hosting at scale.

## Phase 5: Tenant Resource Governance

### 5.1 Hard Limits + Policy Surface
- Add per-project runtime limits (CPU quota, memory, max concurrent requests, max open files, timeout budget).
- Add defaults in config and optional per-project overrides in DB.
- Enforce limits in both process mode and pool mode worker launch paths.

### 5.2 Runtime Enforcement
- Ensure cgroup v2 setup is deterministic and enforced for each runtime.
- Add circuit-break behavior when resource setup fails (no silent fallback in production mode).
- Add explicit error taxonomy for limit-enforcement failures.

### 5.3 Build/Run Separation
- Separate build-time limits from runtime limits (distinct policies, logs, and metrics).
- Ensure build container/process cannot inherit runtime policy accidentally.

### 5.4 Tests
- Unit tests for policy resolution and override precedence.
- Integration tests for enforcement behavior (success, reject, and teardown paths).
- Regression tests for pool replenishment under constrained worker budgets.

### 5.5 Exit Criteria
- A project cannot exceed configured resource envelope.
- Failed enforcement paths are observable and return deterministic API errors.
- No regressions in existing runtime/build test suites.

## Phase 6: Operational Observability

### 6.1 Metrics
- Add structured metrics for:
  - deploy lifecycle latency per stage
  - cold starts/wakes/suspends/stops
  - routing cache hit/miss/negative-hit
  - scheduler placement outcomes
  - operation idempotency outcomes (`proceed/completed/failed/in_progress`)
  - resource limit violations
- Expose `/metrics` (Prometheus format) on API process.

### 6.2 Tracing
- Add trace spans across:
  - API lifecycle endpoints
  - build pipeline stage transitions
  - proxy host resolution and wake path
  - state store (redis/local) operations
- Propagate request ID/correlation ID across API/proxy/background tasks.

### 6.3 SLOs + Alerts
- Define baseline SLOs:
  - successful deploy rate
  - p95 cold-start latency
  - p95 proxy routing latency
  - worker heartbeat freshness
- Add alert rules for SLO breach and critical events (redis disconnect loops, placement failures, repeated CAS conflicts).

### 6.4 Dashboards + Runbooks
- Ship starter dashboards for deploy/runtime/proxy/store views.
- Add runbooks for:
  - Redis outage/degradation
  - stale routing/cross-node mismatch
  - runaway tenant resource usage
  - cold-start latency spikes

### 6.5 Exit Criteria
- Every critical control plane and runtime path emits metrics + traces.
- On-call can detect and triage core failures without attaching debugger to live nodes.

## Phase 7: Multi-Node Hardening (Recommended Before Public Scale)

### 7.1 Consistency + Failover
- Add integration tests against real Redis for:
  - cross-node route propagation
  - pub/sub invalidation under churn
  - lease renewal/expiry takeover
- Add chaos tests: redis restarts, subscriber disconnect loops, worker heartbeat drops.

### 7.2 Performance Validation
- Add load test scenarios:
  - hot routing path
  - cold-start bursts
  - concurrent deploy + stop/suspend/wake storms
- Define and enforce pass/fail thresholds in CI (or nightly).

### 7.3 Exit Criteria
- Cross-node behavior remains correct under failure and burst load.
- Latency and error-rate budgets stay within target bounds.

## Suggested Delivery Sequence (PR Slices)

1. Phase 5.1 + 5.2 policy model and enforcement plumbing.
2. Phase 5.3 + 5.4 tests and regressions.
3. Phase 6.1 metrics instrumentation.
4. Phase 6.2 tracing propagation.
5. Phase 6.3/6.4 SLO docs, alerts, dashboards, runbooks.
6. Phase 7 integration + chaos + load validation.

## Mandatory Validation Per Slice

Run after each slice:
- `cargo fmt --all`
- `cargo clippy -p rift-engine --all-targets -- -D warnings`
- `cargo test -p rift-engine`

Before merge of final slice:
- execute multi-node integration test suite
- execute load/chaos scenarios and publish results in markdown report
