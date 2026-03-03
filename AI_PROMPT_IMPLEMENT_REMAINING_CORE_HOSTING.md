# AI Prompt: Implement Remaining Core Hosting Work

You are continuing work on the Rift engine in this repository.

Primary objective:
Implement the remaining core-hosting roadmap described in:
`/Users/frederickstempfle/Desktop/dev/projects/rift/CORE_HOSTING_NEXT_PLAN.md`

## Context You Must Assume

- Phases 0-4 are already implemented and passing.
- Recent correctness hardening is also implemented (idempotency race, failed-op replay, stop persistence, distributed routing write/remove).
- This is a Rust codebase centered at `apps/engine`.

## Non-Negotiable Constraints

1. Do not regress existing behavior.
2. Keep changes incremental in small PR-sized slices.
3. Preserve current API compatibility unless explicitly required by the plan.
4. Prefer explicit, testable behavior over implicit fallback logic.
5. Every slice must end with:
   - `cargo fmt --all`
   - `cargo clippy -p rift-engine --all-targets -- -D warnings`
   - `cargo test -p rift-engine`

## Execution Order

Follow this exact sequence:

1. Phase 5.1 + 5.2 (policy model and runtime enforcement).
2. Phase 5.3 + 5.4 (build/runtime separation and tests).
3. Phase 6.1 (metrics instrumentation + `/metrics` exposure).
4. Phase 6.2 (trace spans + request/correlation propagation).
5. Phase 6.3 + 6.4 (SLO definitions, alert rules, dashboards, runbooks).
6. Phase 7 (multi-node integration, chaos, and load hardening).

## Implementation Requirements

### For each slice

- Add/update code comments only where they reduce ambiguity.
- Add tests that fail before and pass after the change.
- Update docs for config/env vars and operational implications.
- If touching lifecycle logic, ensure idempotency semantics remain correct:
  - `Proceed`
  - completed replay
  - failed replay
  - in-progress conflict
- If touching routing/state logic, verify cross-node invalidation behavior remains consistent.

### For observability work

- Metrics names must be stable and scoped (prefix with `rift_`).
- Add labels conservatively (`project_id` only when cardinality is safe or sampled).
- Traces must include operation IDs and project IDs when available.
- Emit explicit events for cold-start/wake/suspend/stop outcomes.

### For resource-governance work

- Separate build-time and runtime policy paths.
- Add deterministic failure behavior when enforcement is required and unavailable.
- Ensure teardown paths always release cgroup/resource artifacts.

## Deliverables

1. Code changes implementing each slice.
2. Tests for each slice.
3. Documentation updates.
4. A running status file updated after each slice:
   - `/Users/frederickstempfle/Desktop/dev/projects/rift/CORE_HOSTING_IMPLEMENTATION_STATUS.md`
5. Final report file:
   - `/Users/frederickstempfle/Desktop/dev/projects/rift/CORE_HOSTING_FINAL_VALIDATION_REPORT.md`
   - Include test outputs, integration/chaos/load results, unresolved risks, and follow-up tasks.

## Output Format While Working

After each slice, output:
- What changed
- Why it changed
- Validation results
- Remaining work vs plan

If blocked, stop and report:
- exact blocker
- attempted paths
- smallest safe unblock proposal
