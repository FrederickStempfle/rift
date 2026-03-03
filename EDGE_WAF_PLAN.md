# EdgeWAF Implementation Plan

## Goal
Implement a first-party EdgeWAF in the Rift engine that blocks/challenges common web attacks at the proxy edge while preserving low latency and safe rollout controls.

## Scope
- In scope:
  - Layer-7 WAF rule engine at proxy layer
  - Global and per-project rules
  - Actions: `allow`, `challenge`, `block`, `log`
  - Managed baseline scanner/exploit signatures
  - API + persistence + metrics + auditability
  - Safe rollout modes and emergency kill switch
- Out of scope (MVP):
  - ASN/geo/risk-provider integrations
  - ML/anomaly scoring
  - Third-party managed rules ingestion

## Implementation Phases

### Phase 1: Core WAF Engine (Day 1)
1. Define deterministic rule evaluation model:
   - Scope precedence: global then project
   - Rule order by priority, then creation time
   - First terminal action wins (`allow`, `challenge`, `block`)
   - `log` action is non-terminal
2. Add WAF request context in proxy path:
   - Source IP, method, host, path, query, headers, user-agent
3. Implement `apps/engine/src/proxy/waf.rs`:
   - Rule matcher compiler (regex/exact/prefix/CIDR operators)
   - Fast evaluation function returning decision + matched rule metadata
4. Wire proxy enforcement in `apps/engine/src/proxy/handler.rs`:
   - Global WAF check before route/project lookup
   - Project WAF check after project resolution, before firewall/runtime forwarding
5. Reuse existing abuse challenge system for `challenge` decisions:
   - Ticket generation/verification
   - Challenge cookie issuance

### Phase 2: Data Model and Control Plane (Day 2)
1. Add DB migrations:
   - `waf_policies` (scope defaults, mode, fail-open/fail-closed)
   - `waf_rules` (scope, project_id nullable, matcher fields, action, priority, enabled)
   - `waf_events` (timestamp, project_id, action, rule_id, sample request metadata)
2. Add DB access layer:
   - CRUD for policies/rules
   - Event insertion and bounded retention helpers
3. Add API routes:
   - `/api/waf/rules` (create/list/update/delete)
   - `/api/waf/policy` (get/set)
   - `/api/waf/events` (list recent matches)
4. Add authz + audit log parity with existing firewall APIs.

### Phase 3: Caching, Metrics, and Baseline Rules (Day 2)
1. Add `WafCache` (patterned after `FirewallCache`):
   - Cache compiled rules per scope/project
   - TTL + explicit invalidation on writes
2. Add metrics:
   - `rift_waf_decision_total{scope,action}`
   - `rift_waf_rule_match_total{rule_id}`
   - `rift_waf_eval_duration_seconds{scope}`
3. Seed managed baseline signatures (initial profile):
   - Path traversal probes
   - Sensitive file probes (`/.env`, `/.git/config`)
   - Common CMS exploit probes (`/wp-login.php`, `/xmlrpc.php`)
   - Admin and scanner probes (`/phpmyadmin`, `/cgi-bin/`)
4. Default baseline action profile:
   - Start as `log` or `challenge` by environment
   - Escalate specific high-confidence signatures to `block`

### Phase 4: Validation and Rollout (Day 3)
1. Tests:
   - Unit tests for matcher semantics and precedence
   - Proxy integration tests for evaluation order and action effects
   - Regression tests for trusted bypass/challenge flow compatibility
2. Load/perf:
   - Extend abuse/load tests to include WAF paths
   - Verify p95/p99 overhead stays within target (e.g. < 1ms median added)
3. Rollout strategy:
   - Stage 1: `log` mode (24h telemetry)
   - Stage 2: `challenge` for medium confidence signatures
   - Stage 3: `block` for high-confidence signatures
4. Operational controls:
   - Global kill switch env (`RIFT_WAF_ENABLED=false`)
   - Per-rule quick-disable
   - Fail-open policy for parser/compiler errors during initial rollout

## Deliverables
- `waf.rs` rule engine integrated in proxy path
- WAF DB schema + migrations
- WAF API endpoints with auth and audit logging
- WAF cache + metrics + events
- Managed baseline ruleset
- Test coverage and load-test report
- Runbook update for incident response/tuning

## Acceptance Criteria
- WAF can enforce global and per-project rules in production path.
- Decisions are observable via metrics and event logs.
- Existing traffic and challenge mechanisms remain functional.
- Rollout can be safely toggled and quickly rolled back.
- Bot/scanner probe traffic shows measurable reduction before upstream routing.

## Risks and Mitigations
- False positives:
  - Mitigate with staged rollout (`log` -> `challenge` -> `block`) and per-rule disable.
- Performance regression:
  - Mitigate with precompiled matchers, cache, and perf gates in CI/load tests.
- Rule complexity drift:
  - Mitigate with constrained MVP matcher schema and explicit rule lint/validation.

## Immediate Next Step
Start Phase 1 implementation: add `waf.rs`, request context extraction, and proxy hook with `log/challenge/block` decisions using existing challenge plumbing.
