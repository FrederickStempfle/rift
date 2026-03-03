# Rift Core Hosting Platform Spec

Last updated: 2026-03-01

## Goal

Evolve Rift core hosting from a single-node runtime manager into a distributed, multi-node hosting platform that can support serious production workloads with predictable latency, high availability, and clear SLOs.

## Scope

This spec covers core hosting only:

- runtime placement and scheduling
- request routing on hot path
- deploy, wake, suspend lifecycle correctness
- multi-node operations and failover
- tenant isolation and quotas at runtime

Out of scope:

- dashboard redesign
- billing UX
- marketing/docs site

## Current Constraints (As Implemented)

- Runtime truth is in-memory per process (`active`/`suspended` maps), which limits HA and horizontal scale.
- Request routing path can require DB lookups for host resolution.
- Scale-to-zero policy is fixed by constants, not dynamic policy.
- Two runtime paths (legacy process + pool) increase operational surface.

## Target Architecture

### Components

1. Control Plane API
- accepts deploy/suspend/wake commands
- validates tenancy, quotas, and policies
- publishes lifecycle commands to workers

2. Scheduler + Placement Service
- tracks worker heartbeats and capacity
- acquires placement leases for deployments
- supports deterministic re-placement on node loss

3. Worker Nodes
- run project runtimes (SSR/static/function workloads)
- report health and resource usage
- execute idempotent lifecycle operations

4. Routing Layer
- memory-resident host -> deployment -> worker map
- cache invalidation over pub/sub
- no DB dependency on hot path for normal traffic

5. Distributed State Store (Redis/etcd)
- leases, placement state, lifecycle state machine
- transient runtime metadata

6. Durable SQL Store (Postgres)
- authoritative project/deployment history
- audit and analytics persistence

## Required Outcomes

1. Multi-node runtime placement with failover.
2. Hot path routing independent of direct DB queries.
3. Idempotent lifecycle transitions with rollback safety.
4. One production runtime model for SSR/static (pool-first).
5. Runtime-level tenant isolation and quota enforcement.
6. Measurable SLOs with automated regression gating.

## SLOs (Initial)

- Availability: 99.95% monthly for routing layer.
- Warm request p95 latency overhead (proxy+routing): <= 20ms.
- Cold wake p95 (suspended -> first byte): <= 2.5s.
- Failed deploy rollback correctness: 100% (no traffic to unhealthy runtime).
- Control-plane recovery after single node loss: <= 60s.

## Implementation Plan

## Phase 0 - Quality Gate Baseline

Deliverables:

- clippy clean under `-D warnings`
- reproducible integration test harness for deploy/wake/suspend flows
- load test baseline for routing + wake behavior

Acceptance:

- CI blocks merges if lint/test/load smoke fails
- baseline performance report committed in repo

## Phase 1 - Distributed Runtime State + Scheduler

Deliverables:

- worker heartbeat protocol
- scheduler service with placement decisions
- placement lease model in Redis/etcd
- worker registration + capacity accounting

Acceptance:

- when a worker dies, placement leases are re-assigned without manual intervention
- deployments can be placed across >= 3 workers in test environment

## Phase 2 - Hot Path Routing Cache + Invalidation

Deliverables:

- in-memory routing table in proxy
- pub/sub invalidation channel for host/deployment changes
- fallback strategy if cache miss or stale entry occurs

Acceptance:

- steady-state request routing uses cache only (no DB round trip)
- cache invalidation propagates within <= 1s p95 in local cluster tests

## Phase 3 - Lifecycle State Machine + Idempotency

Deliverables:

- explicit state machine: `queued -> building -> deploying -> ready | failed | cancelled`
- operation IDs for idempotent deploy/wake/suspend/stop
- safe rollback path on failed health checks
- retriable, exactly-once-effective lifecycle handlers

Acceptance:

- replaying any lifecycle command does not corrupt state
- failed deploy leaves previous ready deployment serving traffic

## Phase 4 - Runtime Consolidation (Pool-First)

Deliverables:

- production SSR/static path standardized on pool runtime backend
- legacy process backend behind feature flag (non-default)
- migration and compatibility tests for existing projects

Acceptance:

- default runtime mode runs pool path only
- canary projects show no functional regression

## Phase 5 - Tenant Isolation + Quotas

Deliverables:

- per-project/team CPU, memory, concurrency budgets
- admission control on deploy and wake
- enforcement telemetry and clear error surfaces

Acceptance:

- over-quota projects are throttled predictably
- one tenant cannot starve others in saturation tests

## Phase 6 - HA + Failure Drills

Deliverables:

- N-node proxy/runtime deployment reference
- control plane redundancy
- chaos drills: worker kill, scheduler restart, network partition

Acceptance:

- platform meets recovery SLO under defined failure drills
- no manual operator action required for single-node failures

## Data and Interface Contracts

- Placement Lease Key: `placement:{project_id}` -> `{worker_id, deployment_id, version, ttl}`
- Worker Heartbeat: `{worker_id, ts, cpu_free, mem_free, active_runtimes}`
- Routing Update Event: `{host, project_id, deployment_id, worker_addr, version}`
- Lifecycle Command: `{op_id, action, project_id, deployment_id, desired_state}`
- Lifecycle Result: `{op_id, status, worker_id, error?, observed_state}`

All write operations must be versioned and compare-and-set where applicable.

## Testing Strategy

1. Unit tests
- scheduler placement scoring
- state machine transition validity
- quota evaluator rules

2. Integration tests
- deploy + route + wake + suspend across multi-worker harness
- invalidation propagation correctness
- rollback correctness on unhealthy release

3. Resilience tests
- worker crash during deploy
- duplicate lifecycle command delivery
- stale cache and delayed invalidation

4. Load tests
- warm request throughput/latency
- burst wake events across many suspended deployments

## Definition of Done

- SLO dashboards exist and are tracked in CI/perf pipeline.
- Core hosting defaults to distributed, pool-first architecture.
- Single worker/node failures are self-healed.
- Hot path is cache-first with bounded fallback behavior.
- Lifecycle operations are idempotent and auditable.

## Deliverable Checklist

- [ ] Distributed scheduler merged
- [ ] Routing cache + invalidation merged
- [ ] Lifecycle idempotency merged
- [ ] Pool-first runtime default merged
- [ ] Quota enforcement merged
- [ ] HA drills documented and passing
- [ ] SLO report published
