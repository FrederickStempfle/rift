# Core Hosting Platform Implementation Status

## Overview

Phases 0–3 of the core hosting platform spec have been implemented and hardened. The engine now has: a test infrastructure for lifecycle flows, a distributed state abstraction, cache-first proxy routing with multi-layer resolution, an explicit deployment lifecycle state machine with CAS transitions, and DB-backed idempotent lifecycle operations.

## Phase 0 — Quality Gate Baseline

**Status: Complete**

- **MockBackend** (`src/runtime/mock_backend.rs`): In-memory implementation of `RuntimeBackend` for testing deploy/wake/suspend/stop flows without Deno or Postgres. 9 unit tests covering lifecycle permutations.
- **Proxy unit tests** (`src/proxy/handler.rs`): Extracted `match_subdomain` as a standalone function. 10 tests for host extraction and subdomain matching.
- **Clippy clean**: All pre-existing clippy warnings resolved across the codebase (30+ fixes).

## Phase 1 — Distributed Runtime State + Scheduler

**Status: Complete**

### StateStore trait (`src/state/mod.rs`)

Async trait covering:
- Placement leases: acquire (CAS), get, release, renew
- Worker heartbeats: send, list, remove
- Routing entries: set, get, remove
- Pub/sub: `publish_routing_update`

Data types: `PlacementLease`, `WorkerHeartbeat`, `RoutingEntry`.

### Local implementation (`src/state/local.rs`)

In-memory store using `tokio::sync::RwLock<HashMap>`. CAS enforcement on `acquire_placement` via version checking. 11 unit tests (including routing overwrite and pub/sub no-op verification).

### Redis implementation (`src/state/redis_store.rs`)

Redis-backed store with Lua CAS script for atomic placement acquisition. Key schema:
- `rift:placement:{project_id}` — JSON lease with TTL
- `rift:worker:{worker_id}` — JSON heartbeat, 30s TTL
- `rift:route:{host}` — JSON routing entry
- Pub/sub channel: `rift:routing_updates`

**Scalability fix**: `list_workers` uses `SCAN` (non-blocking, cursor-based) instead of `KEYS` to avoid O(N) blocking in production Redis instances.

### Scheduler (`src/scheduler/mod.rs`)

Least-loaded-first worker scoring. Falls back to self-placement when no heartbeats exist (single-node mode). 7 tests.

**Fixes applied:**
- Placement lease version is now derived from existing lease (current version + 1), not hardcoded to 1. This ensures CAS versioning works correctly across re-deployments.
- `place_on_self()` now checks the return value of `acquire_placement` and returns `Err(Conflict)` on failure instead of silently ignoring it.

### Heartbeat (`src/scheduler/heartbeat.rs`)

Background task sending heartbeats every 10 seconds.

**Fix applied**: `ProcessBackend` now overrides `pool_stats()` to return the actual active runtime count from `RuntimeManager::active_count()`. Previously it returned `None`, causing heartbeats to always report 0 active runtimes in subprocess mode.

### Config additions

- `RIFT_STATE_STORE` — `"local"` (default) or `"redis"`
- `RIFT_REDIS_URL` — Redis connection URL
- `RIFT_WORKER_ID` — Unique worker ID (auto-generated if unset)

## Phase 2 — Hot Path Routing Cache + Invalidation

**Status: Complete**

### RoutingCache (`src/proxy/routing_cache.rs`)

In-memory cache eliminating per-request Postgres queries for host → project_id resolution.

- **Positive cache**: host → project_id, 60s TTL
- **Negative cache**: host → not-found, 5s TTL
- **Background evictor**: 30s interval
- **Invalidation**: `invalidate_host()`, `invalidate_project()`
- 9 unit tests including concurrency stress test (100 concurrent readers)

### Handler integration (`src/proxy/handler.rs`)

`resolve_project_id` uses multi-layer resolution:
1. `RoutingCache` → `Hit`/`NegativeHit` return immediately (no DB)
2. `StateStore` routing entries → check distributed state (multi-node)
3. DB domain lookup → custom domains via `domains` table
4. DB subdomain lookup → subdomain-based resolution
5. `Miss` → insert negative entry

### Proactive invalidation

- `api/domains.rs`: Invalidate on domain create, delete, assign/unassign
- `api/projects.rs`: Invalidate on subdomain change, project delete

### Cross-node cache invalidation (`src/proxy/routing_subscriber.rs`)

Redis pub/sub subscriber that listens on `rift:routing_updates` and invalidates the local routing cache when another node publishes an update. Auto-reconnects on disconnect with 5s backoff. No-op in local state store mode.

## Phase 3 — Lifecycle State Machine + Idempotency

**Status: Complete**

### State machine (`src/lifecycle/state_machine.rs`)

`DeploymentState` enum with explicit valid transitions:

```
Queued → Cloning, Failed, Cancelled
Cloning → Building, Failed, Cancelled
Building → Deploying, Failed, Cancelled
Deploying → Ready, Failed
Ready → Suspended, Cancelled, Failed
Suspended → Ready, Cancelled, Failed
Failed, Cancelled → (terminal)
```

25 unit tests covering all transition rules, terminality, round-trip parsing, and self-transition prevention (including Suspended state).

### CAS transitions (`src/lifecycle/transition.rs`)

- `transition(pool, id, expected, new)` — atomic `WHERE status = $expected` update. Returns `Ok(false)` on concurrent mutation.
- `transition_to_ready(pool, id, url, port, duration)` — CAS from `deploying` with metadata.
- `transition_to_suspended(pool, id)` — CAS from `ready`, sets `suspended_at`.
- `transition_from_suspended_to_ready(pool, id)` — CAS from `suspended`, clears `suspended_at`.
- `transition_to_failed(pool, id, duration)` — CAS from any non-terminal state (including `ready` and `suspended`).

### Build pipeline integration (`src/build/mod.rs`)

All `update_status` calls in the build pipeline replaced with CAS transitions:
- `queued → cloning` via `transition()`
- `cloning → building` via `transition()`
- `building → deploying` via `transition()`
- `* → failed` via `transition_to_failed()`
- `deploying → ready` via `transition_to_ready()`

On failed CAS transition, the build aborts gracefully (deployment was likely cancelled by a concurrent operation).

### DB-backed lifecycle operations (`src/lifecycle/operations.rs`)

Idempotent operation tracking via `lifecycle_operations` table (migration `0017_lifecycle_operations.sql`):

- **`LifecycleOperation`** struct with op_id, action, project_id, deployment_id, status, result, error, timestamps
- **`BeginOutcome`** enum: `Proceed` / `AlreadyCompleted(Box<LifecycleOperation>)` / `InProgress`
- **`begin_operation`**: Check-then-insert with ON CONFLICT guard + race re-check
- **`complete_operation`**: CAS update (status = 'running' → 'completed')
- **`fail_operation`**: CAS update (status = 'running' → 'failed')

### Deploy API idempotency (`src/api/deployments.rs`)

`create_deployment` accepts optional `op_id: Option<Uuid>`. Flow:
1. Call `begin_operation` with op_id
2. `AlreadyCompleted` → return prior deployment via `get_deployment_by_id`
3. `InProgress` → return 409 Conflict
4. `Proceed` → execute build, call `complete_operation` or `fail_operation`
5. Backfill `deployment_id` on the operation row after build enqueue

## Phase 4 — Suspend/Wake with DB Persistence

**Status: Complete**

### Migrations

- `migrations/0018_suspended_status.sql` — Adds `suspended` value to `deployment_status` enum (uses `-- no-transaction` since `ALTER TYPE ... ADD VALUE` cannot run inside a transaction).
- `migrations/0019_suspended_at_column.sql` — Adds `suspended_at TIMESTAMPTZ` column to `deployments` table.

### DB model + helpers (`src/db/deployments.rs`, `src/db/models.rs`)

- `Deployment` struct gains `suspended_at: Option<DateTime<Utc>>` field.
- All 12 SELECT queries updated to include `suspended_at`.
- `mark_suspended(pool, id)` — CAS: `WHERE status = 'ready'`, sets `suspended_at = now()`.
- `mark_ready_from_suspended(pool, id)` — CAS: `WHERE status = 'suspended'`, clears `suspended_at`.
- `list_latest_suspended_per_project(pool)` — `DISTINCT ON (project_id)` for restore on startup.

### RuntimeBackend trait (`src/runtime/backend.rs`)

- New method: `async fn suspend(&self, project_id: Uuid) -> Result<bool, AppError>` — explicitly suspends a single project.
- Implemented on `ProcessBackend`, `PoolBackend`, and `MockBackend`.

### DB persistence in RuntimeManager + WorkerPool

- `RuntimeManager` and `WorkerPool` gain an `Option<PgPool>` field (`db_pool`).
- `suspend_idle()` — after suspending, calls `mark_suspended()` to persist the state.
- `suspend_project()` — same DB persistence for explicit suspend.
- `wake()` — after re-launching, calls `mark_ready_from_suspended()` to persist the state.
- `restore_deployments()` — now also queries `list_latest_suspended_per_project()` and inserts entries into the suspended HashMap without starting processes (lazy restore).

### API endpoints (`src/api/runtime.rs`)

Three new handlers, all using idempotent `lifecycle_operations` tracking:

- `POST /api/projects/{project_id}/stop` — stops the runtime, marks deployment as `cancelled`.
- `POST /api/projects/{project_id}/suspend` — explicitly suspends the runtime.
- `POST /api/projects/{project_id}/wake` — wakes a suspended runtime.

All accept `{ op_id?: Uuid }` for idempotency. Flow: `begin_operation` → action → `complete_operation` / `fail_operation`.

### API response (`src/api/deployments.rs`)

- `DeploymentResponse` gains `suspended_at` field.

## Test Summary

```
218 tests total:
  114 unit tests (lib.rs)
    5 integration tests (api_flow.rs)
   73 integration tests (pool_tests.rs)
   26 integration tests (multi_node_tests.rs)
```

All pass with `cargo clippy -p rift-engine --all-targets -- -D warnings`.

## Files Added/Modified

### New files (Phase 4)
| File | Purpose |
|------|---------|
| `migrations/0018_suspended_status.sql` | Add `suspended` to deployment_status enum |
| `migrations/0019_suspended_at_column.sql` | Add `suspended_at` column to deployments |

### New files (Phase 3 audit)
| File | Purpose |
|------|---------|
| `migrations/0017_lifecycle_operations.sql` | Lifecycle operations table for idempotency |
| `src/lifecycle/operations.rs` | DB helpers for idempotent operation tracking |
| `src/proxy/routing_subscriber.rs` | Redis pub/sub subscriber for cross-node cache invalidation |

### New files (prior phases)
| File | Phase | Purpose |
|------|-------|---------|
| `src/runtime/mock_backend.rs` | 0 | Test-only RuntimeBackend |
| `src/state/mod.rs` | 1 | StateStore trait + data types |
| `src/state/local.rs` | 1 | In-memory StateStore |
| `src/state/redis_store.rs` | 1 | Redis-backed StateStore |
| `src/scheduler/mod.rs` | 1 | Scheduler + scoring |
| `src/scheduler/heartbeat.rs` | 1 | Background heartbeat |
| `src/proxy/routing_cache.rs` | 2 | Hot path routing cache |
| `src/lifecycle/mod.rs` | 3 | Module declaration |
| `src/lifecycle/state_machine.rs` | 3 | State enum + transitions |
| `src/lifecycle/transition.rs` | 3 | CAS DB transitions |

### Modified files (Phase 4)
| File | Changes |
|------|---------|
| `src/lifecycle/state_machine.rs` | Added `Suspended` variant, updated transitions (Ready no longer terminal), 7 new tests |
| `src/lifecycle/transition.rs` | Added `transition_to_suspended`, `transition_from_suspended_to_ready`, expanded `transition_to_failed` to include `ready`/`suspended` |
| `src/db/models.rs` | Added `suspended_at` to `Deployment` struct |
| `src/db/deployments.rs` | All SELECTs include `suspended_at`, 3 new functions |
| `src/api/deployments.rs` | `suspended_at` in `DeploymentResponse` |
| `src/api/runtime.rs` | 3 new handlers: `stop_project`, `suspend_project`, `wake_project` |
| `src/api/mod.rs` | 3 new routes registered |
| `src/runtime/backend.rs` | `suspend()` trait method + impls on all backends |
| `src/runtime/mod.rs` | `db_pool` field, `suspend_project()`, DB persistence in suspend/wake/restore |
| `src/runtime/pool/mod.rs` | `db_pool` field, `suspend_project()`, DB persistence in suspend/wake/restore |
| `src/runtime/mock_backend.rs` | `suspend()` impl + 2 new tests |
| `src/main.rs` | Wire `db_pool` into RuntimeManager and WorkerPool |

### Modified files (Phase 3 audit)
| File | Changes |
|------|---------|
| `src/lifecycle/mod.rs` | Added `operations` module |
| `src/api/deployments.rs` | `op_id` field, idempotent deploy flow via operations |
| `src/db/deployments.rs` | Added `get_deployment_by_id` |
| `src/scheduler/mod.rs` | `next_version()` for lease versioning, `place_on_self` error handling |
| `src/state/redis_store.rs` | Replaced `KEYS` with `SCAN` in `list_workers` |
| `src/runtime/backend.rs` | `ProcessBackend::pool_stats()` returns real active count |
| `src/runtime/mod.rs` | Added `RuntimeManager::active_count()` |
| `src/proxy/handler.rs` | Added StateStore routing entry check in `resolve_project_id` |
| `src/proxy/mod.rs` | Added `routing_subscriber` module |
| `src/main.rs` | Spawn routing subscriber for Redis mode |
| `src/state/local.rs` | 2 additional tests |

### Modified files (prior phases)
| File | Changes |
|------|---------|
| `src/lib.rs` | Added `state`, `scheduler`, `lifecycle` modules |
| `src/config.rs` | Added `state_store`, `redis_url`, `worker_id` fields |
| `src/api/mod.rs` | Added `routing_cache`, `state_store`, `scheduler` to AppState |
| `src/main.rs` | Instantiate state store, scheduler, heartbeat, routing cache |
| `src/proxy/mod.rs` | Added `routing_cache` module |
| `src/proxy/handler.rs` | Cache-first `resolve_project_id`, extracted `match_subdomain` |
| `src/api/domains.rs` | Proactive cache invalidation on domain mutations |
| `src/api/projects.rs` | Proactive cache invalidation on subdomain/project changes |
| `src/build/mod.rs` | CAS lifecycle transitions replace raw `update_status` calls |
| `Cargo.toml` | Added `redis` dependency |
| `tests/api_flow.rs` | Updated for new AppState/Config fields |

## Phase 5.1 + 5.2 — Resource Policy Model & Runtime Enforcement

**Status: Complete**

### New files
| File | Purpose |
|------|---------|
| `src/runtime/policy.rs` | `RuntimePolicy`, `BuildPolicy`, `ProjectPolicyOverrides`, `EnforcementMode`, `ResourceError`, policy resolution, cgroup enforcement |

### Modified files
| File | Changes |
|------|---------|
| `src/config.rs` | 10 new env-configurable fields for runtime/build resource governance |
| `src/runtime/mod.rs` | Added `policy` module |
| `src/runtime/pool/mod.rs` | `WorkerPool` takes `EnforcementMode` + `RuntimePolicy`; cgroup setup uses `enforce_cgroup_limits()`; teardown uses `release_cgroup()` |
| `src/main.rs` | Wire policy and enforcement mode into pool construction |
| `tests/api_flow.rs` | Updated Config construction with new fields |

### New config env vars
| Env Var | Default | Description |
|---------|---------|-------------|
| `RIFT_WORKER_CPU_QUOTA_US` | 100000 | CPU quota per worker (us/100ms) |
| `RIFT_WORKER_MAX_PIDS` | 64 | Max PIDs per worker |
| `RIFT_WORKER_MAX_OPEN_FILES` | 1024 | Max open FDs per worker |
| `RIFT_WORKER_REQUEST_TIMEOUT_SECS` | 30 | Per-request timeout |
| `RIFT_WORKER_MAX_CONCURRENT_REQUESTS` | 100 | Max concurrent requests per project |
| `RIFT_RESOURCE_ENFORCEMENT` | best-effort | "strict" or "best-effort" |
| `RIFT_BUILD_MEMORY_LIMIT_MB` | 2048 | Build memory limit |
| `RIFT_BUILD_CPU_QUOTA_US` | 200000 | Build CPU quota |
| `RIFT_BUILD_MAX_PIDS` | 256 | Build max PIDs |
| `RIFT_BUILD_TIMEOUT_SECS` | 600 | Build timeout |

### Tests added: 13 unit tests in `policy.rs`
- Policy resolution with/without overrides
- Build vs runtime policy separation
- ResourceLimits conversion
- Error taxonomy and AppError mapping
- EnforcementMode parsing

## Phase 5.3 + 5.4 — Build/Runtime Separation & Tests

**Status: Complete**

### Changes
- `BuildManager` now resolves and stores a `BuildPolicy` (separate from runtime)
- Build timeout enforcement via `tokio::time::timeout` wrapping the build task, using `build_policy.build_timeout_secs`
- Timeout triggers `transition_to_failed` on the deployment

### Tests added: 23 new tests in `pool_tests.rs::resource_policy`
- Build/runtime independence (overrides don't leak between them)
- Override precedence (all fields, partial, empty)
- Config-driven defaults propagation
- ResourceLimits conversion (high watermark calculation)
- EnforcementMode parsing (strict, best-effort, unknown)
- Error taxonomy mapping (conflict, rate-limited, internal)
- Enforcement behavior (best-effort succeeds without cgroups, strict fails)
- Teardown idempotency
- Serialization round-trips
- Error display formatting

### Validation
- 73 tests pass (50 prior + 23 new)
- clippy clean, fmt clean

## Phase 6.1 — Metrics Instrumentation & /metrics Endpoint

**Status: Complete**

### New files
| File | Purpose |
|------|---------|
| `src/metrics.rs` | Prometheus metrics registry with all `rift_`-prefixed metrics |

### Metrics registered
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `rift_deploy_stage_duration_seconds` | Histogram | stage | Duration of each deploy stage |
| `rift_deploy_outcome_total` | Counter | outcome | Deploy outcomes |
| `rift_runtime_event_total` | Counter | event | cold_start/wake/suspend/stop events |
| `rift_cold_start_duration_seconds` | Histogram | kind | Cold start/wake duration |
| `rift_routing_cache_result_total` | Counter | result | hit/negative_hit/miss |
| `rift_scheduler_placement_total` | Counter | outcome | success/failed/self_placed |
| `rift_operation_outcome_total` | Counter | outcome | proceed/completed/failed/in_progress |
| `rift_resource_violation_total` | Counter | kind | cgroup_unavailable/setup_failed/attach_failed |
| `rift_pool_warm_workers` | Gauge | — | Warm worker count |
| `rift_pool_active_workers` | Gauge | — | Active worker count |
| `rift_pool_suspended_deployments` | Gauge | — | Suspended deployment count |
| `rift_build_duration_seconds` | Histogram | outcome | Total build duration |
| `rift_build_queue_depth` | Gauge | — | Builds waiting for slot |
| `rift_heartbeat_send_total` | Counter | outcome | ok/error |
| `rift_worker_cpu_free_pct` | Gauge | worker_id | CPU free percentage |
| `rift_worker_mem_free_bytes` | Gauge | worker_id | Memory free bytes |

### Instrumented paths
- Routing cache lookup (hit/miss/negative_hit)
- Scheduler placement (success/failed/self_placed)
- Lifecycle operations (proceed/completed/failed/in_progress)
- Resource enforcement (cgroup violations)
- Runtime events (stop/suspend/wake)
- Heartbeat send (ok/error)

### Endpoint
- `GET /metrics` — Prometheus text format exposition

### Dependencies added
- `prometheus = "0.13"` (with `process` feature for process metrics)

### Tests: 3 new lib tests
- Metric encoding without panic
- All metric names rift_-prefixed
- Counter increment cumulativity

### Validation
- 76 tests pass (73 pool/integration + 3 lib metrics)
- clippy clean, fmt clean

## Phase 6.2 — Trace Spans & Correlation Propagation

**Status: Complete**

### `#[tracing::instrument]` annotations added
| Function | File | Fields |
|----------|------|--------|
| `stop_project` | `src/api/runtime.rs` | `project_id` |
| `suspend_project` | `src/api/runtime.rs` | `project_id` |
| `wake_project` | `src/api/runtime.rs` | `project_id` |
| `route_and_forward` | `src/proxy/handler.rs` | `remote_addr` |
| `enqueue_project_build` | `src/build/mod.rs` | `project_id` |
| `run_build` | `src/build/mod.rs` | `project_id`, `deployment_id` |
| `begin_operation` | `src/lifecycle/operations.rs` | `op_id`, `project_id` |
| `place` | `src/scheduler/mod.rs` | `project_id`, `deployment_id` |

### Build pipeline metrics instrumentation
- Per-stage duration metrics via `DEPLOY_STAGE_DURATION` histogram (clone, detect, install, build, artifact, runtime_start)
- Deploy outcome metrics: `DEPLOY_OUTCOME` counter (success, failed, timeout)
- Build duration histogram: `BUILD_DURATION` (success outcome)
- Build queue depth tracking: `BUILD_QUEUE_DEPTH` gauge (inc on enqueue, dec on permit acquire)
- Pool gauge updates in `WorkerPool::stats()`: `POOL_WARM_WORKERS`, `POOL_ACTIVE_WORKERS`, `POOL_SUSPENDED`

### Cold start metrics (proxy handler)
- `COLD_START_DURATION` histogram for wake latency
- `RUNTIME_EVENT` cold_start counter

### Validation
- 192 tests pass (114 unit + 5 integration + 73 pool tests)
- clippy clean, fmt clean

## Phase 6.3 + 6.4 — SLO Definitions, Alert Rules, Dashboards, Runbooks

**Status: Complete**

### New documentation files
| File | Purpose |
|------|---------|
| `docs/slos.md` | SLO definitions + Prometheus alert rules |
| `docs/dashboards.md` | Grafana dashboard panel definitions (PromQL) |
| `docs/runbooks.md` | Operational runbooks for 6 failure scenarios |

### SLOs defined
| SLO | Target | Metric |
|-----|--------|--------|
| Deploy success rate | >= 95% (7d rolling) | `rift_deploy_outcome_total` |
| Cold start p95 | <= 3s | `rift_cold_start_duration_seconds` |
| Routing cache hit rate | >= 90% | `rift_routing_cache_result_total` |
| Heartbeat freshness | No gap > 30s | `rift_heartbeat_send_total` |

### Alert rules (12 total)
- Deploy success rate warning/critical
- Cold start p95 warning/critical
- Routing cache hit rate warning
- Heartbeat failure warning/critical
- Redis disconnect loop
- Scheduler placement failures
- CAS conflict rate
- Resource violations
- Build queue depth

### Dashboard panels (4 dashboards, 14 panels)
1. Deploy Overview: outcome rate, stage duration, build duration, queue depth
2. Runtime Overview: events, cold start latency, pool counts, violations
3. Proxy & Routing: cache performance, hit rate
4. Distributed State: placement, heartbeat, worker CPU/memory, operations

### Runbooks (6 scenarios)
1. Redis outage/degradation
2. Stale routing / cross-node mismatch
3. Runaway tenant resource usage
4. Cold start latency spikes
5. Build failures / timeouts
6. Scheduler placement failures

### Validation
- 192 tests pass (no code changes in this phase)
- clippy clean, fmt clean

## Phase 7 — Multi-Node Integration, Chaos, and Load Hardening

**Status: Complete**

### New files
| File | Purpose |
|------|---------|
| `tests/multi_node_tests.rs` | 26 integration tests for distributed state, concurrency, and lifecycle chaos |

### Changes
| File | Changes |
|------|---------|
| `src/runtime/mod.rs` | Made `mock_backend` always compiled (not `#[cfg(test)]` only) for integration test access |

### Test modules (26 tests)

**cross_node_routing** (3 tests):
- Concurrent route writes for 50 different hosts converge correctly
- Route overwrite with higher version wins
- Route removal makes entry invisible

**lease_contention** (6 tests):
- CAS: only one of two concurrent version-1 acquires succeeds
- Higher version always wins
- Lower version rejected
- Release requires matching version
- Renew extends TTL
- Renew on non-existent returns false

**scheduler_under_load** (3 tests):
- Distributes 10 placements across 3 workers
- Rejects when all workers at capacity
- Concurrent placements for same project: at least one succeeds

**worker_heartbeat_churn** (2 tests):
- 20 workers × 10 rounds of rapid heartbeat updates converge
- Removing a worker mid-churn doesn't corrupt others

**routing_cache_stress** (3 tests):
- 50 writers + 50 readers + 10 invalidators concurrent (no panic/deadlock)
- 100 negative entries under load returned correctly
- Project invalidation clears all matching hosts

**lifecycle_storm** (7 tests):
- Concurrent deploy + stop doesn't panic
- Concurrent suspend + wake doesn't corrupt state (never both active and suspended)
- 50-project rapid deploy/stop cycle
- Full lifecycle: deploy → suspend → wake → stop
- Wake without suspend returns None
- Stop on non-existent is no-op

**state_machine_edge_cases** (2 tests):
- Terminal states are dead ends (no transitions out)
- No self-transitions possible
- Suspend/wake cycle validity

### Validation
- 218 tests pass (114 unit + 5 api_flow + 26 multi_node + 73 pool)
- clippy clean, fmt clean

## Known Partial/Risks

- **Redis pub/sub subscriber**: Tested structurally (compiles, wired) but not integration-tested against a real Redis instance. Relies on auto-reconnect on disconnect.
- **SCAN vs KEYS**: Functionally equivalent but SCAN pagination means worker lists may be slightly stale during iteration under high churn.
- **Proxy auto-wake**: The proxy handler's automatic wake on cold start (`src/proxy/handler.rs`) remains a fast path without `lifecycle_operations` tracking (by design — it would add latency to every first request).
- **cgroup enforcement**: On macOS/non-Linux, cgroups are unavailable. Best-effort mode allows development; strict mode is for Linux production.
