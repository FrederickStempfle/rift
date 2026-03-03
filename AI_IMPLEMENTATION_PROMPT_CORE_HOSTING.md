# AI Implementation Prompt - Core Hosting

Use this prompt with your coding AI agent.

```text
You are an expert distributed systems and Rust platform engineer working in the repo at:
/Users/frederickstempfle/Desktop/dev/projects/rift

Your task is to implement the core hosting architecture described in:
/Users/frederickstempfle/Desktop/dev/projects/rift/CORE_HOSTING_PLATFORM_SPEC.md

Read that file first, then execute the work end-to-end.

Hard requirements:
1. Treat the spec as source of truth for scope and acceptance criteria.
2. Focus only on core hosting runtime/platform work (no UI polish or marketing docs).
3. Keep changes incremental and shippable by phase.
4. Do not hand-wave: implement code, tests, and docs for each completed item.
5. Prefer Rust-first solutions in `apps/engine`.
6. Preserve existing functionality while migrating toward pool-first distributed runtime.

Execution workflow:
1. Inspect current runtime/proxy/build paths and identify exact files to change.
2. Create a concrete implementation plan mapped to spec phases.
3. Implement Phase 0 fully before starting Phase 1.
4. For each phase:
   - implement required code paths
   - add or update tests (unit + integration)
   - run validation commands
   - update progress in a new markdown status file
5. Continue through as many phases as possible in this run without breaking the build.

Mandatory validation commands after each meaningful change set:
- cargo fmt --all
- cargo clippy -p rift-engine --all-targets -- -D warnings
- cargo test -p rift-engine

If load/chaos tests are introduced, provide runnable commands and scripts in-repo.

Implementation constraints:
- Make runtime lifecycle operations idempotent using operation IDs.
- Introduce distributed placement/lease state (Redis or etcd abstraction) behind clear interfaces.
- Move proxy hot path to cache-first routing with invalidation events.
- Keep fallback behavior explicit and bounded.
- Add robust observability (structured logs/metrics hooks) for scheduler, placement, and routing.
- Maintain backward compatibility where feasible; gate risky behavior behind config flags.

Expected outputs:
1. Code changes across engine runtime/proxy/build modules.
2. New/updated tests proving acceptance criteria.
3. A new file:
   /Users/frederickstempfle/Desktop/dev/projects/rift/CORE_HOSTING_IMPLEMENTATION_STATUS.md
   containing:
   - completed phases
   - pending phases
   - deviations from spec (with reasons)
   - benchmark/latency notes
4. A final summary with:
   - what was implemented
   - exact commands run
   - test/lint results
   - remaining risks
   - recommended next phase

Quality bar:
- No clippy warnings.
- No failing tests.
- No placeholder TODO-only changes.
- No dead code introduced without feature gating and explanation.

Now begin by reading:
/Users/frederickstempfle/Desktop/dev/projects/rift/CORE_HOSTING_PLATFORM_SPEC.md
and produce the implementation plan before editing files.
```

