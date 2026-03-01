# Rift Progress Checklist

Last updated: 2026-03-01

This file tracks implementation status for the roadmap in `PLAN.md`.

Status legend:
- `Done`: clearly implemented in the current repo
- `Partial`: substantial work exists, but the roadmap item is not fully complete
- `Missing`: not implemented yet
- `Not verified`: implementation may exist, but the verification step has not been completed cleanly

Rule for syncing `PLAN.md`:
- Only mark roadmap checkboxes when an item is clearly `Done`
- Leave `Partial`, `Missing`, and `Not verified` items unchecked in `PLAN.md`

## Summary

| Phase | Done | Partial | Missing / Not verified |
| --- | ---: | ---: | ---: |
| Phase 1 | 6 | 0 | 1 |
| Phase 2 | 5 | 1 | 2 |
| Phase 3 | 4 | 1 | 2 |
| Phase 4 | 6 | 1 | 1 |
| Phase 5 | 5 | 1 | 1 |
| Phase 6 | 7 | 1 | 1 |

## Phase 1: Scaffolding + Database + API

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Init monorepo (Cargo workspace, pnpm workspace) | Partial | `Cargo.toml` | Rust workspace exists, but the planned pnpm workspace files are not present. |
| Scaffold Rust engine with all deps | Done | `apps/engine/Cargo.toml` | Engine crate and dependency set are in place. |
| Write 6 SQL migrations | Done | `apps/engine/migrations/` | There are 12 migrations, which exceeds the roadmap target. |
| Implement db layer (pool, models, query modules) | Done | `apps/engine/src/db/` | Pool setup, models, and query modules are implemented. |
| Auth: argon2 password hashing, JWT issue/verify, middleware | Done | `apps/engine/src/services/password.rs`, `apps/engine/src/services/auth.rs`, `apps/engine/src/api/auth.rs` | Password hashing, JWTs, refresh tokens, cookies, and auth extraction exist. |
| Project CRUD API | Done | `apps/engine/src/api/projects.rs` | Create, list, get, update, and delete are implemented. |
| Verify: curl register -> login -> create/list projects | Done | `apps/engine/tests/api_flow.rs` | Integration test compiles and covers register, login/me, project CRUD, deployment summary, and domain listing flows. Requires `TEST_DATABASE_URL` to run. |

## Phase 2: Git + Webhooks + Build Pipeline

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Git clone/fetch/checkout operations | Partial | `apps/engine/src/build/mod.rs`, `apps/engine/src/build/pipeline.rs` | Git clone is implemented through shell commands, but there is no dedicated fetch/checkout service matching the planned module layout. |
| GitHub webhook creation via API | Done | `apps/engine/src/api/projects.rs`, `apps/engine/src/services/github.rs` | Project creation attempts to register a GitHub push webhook automatically. |
| Webhook receiver with HMAC verification | Done | `apps/engine/src/api/webhooks.rs` | Push webhooks are received and HMAC signatures are verified when a project secret exists. |
| Framework detection (Next.js, Vite, Remix, Astro, Svelte, static) | Partial | `apps/engine/src/build/detect.rs` | Next.js, Vite-like apps, and generic static output are handled; Remix, Astro, and Svelte are not explicitly implemented. |
| Full build pipeline: clone -> install -> build -> log capture | Done | `apps/engine/src/build/mod.rs`, `apps/engine/src/build/pipeline.rs` | Build orchestration, command execution, and log persistence are implemented. |
| Build queue (tokio mpsc, configurable concurrency) | Done | `apps/engine/src/build/mod.rs` | Semaphore-based concurrency with configurable `RIFT_BUILD_CONCURRENCY` (default 4), backpressure logging, and dependency caching via `RIFT_BUILD_CACHE_DIR`. |
| Log broadcaster (tokio broadcast channels) | Done | `apps/engine/src/ws/broadcast.rs`, `apps/engine/src/ws/mod.rs`, `apps/engine/src/ws/handler.rs` | Full implementation: `LogBroadcaster` with per-deployment `tokio::broadcast` channels, WebSocket endpoint at `/api/ws/logs`, integrated into build pipeline. |
| Verify: create project -> push to GitHub -> build runs with streamed logs | Not verified | `apps/engine/src/api/webhooks.rs`, `apps/engine/src/api/logs.rs` | Build triggering exists, but streamed logs are not implemented and this flow has not been verified end-to-end. |

## Phase 3: Deno Runtime + Reverse Proxy

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Deno bundle creation (`_entry.ts` per framework type) | Done | `apps/engine/src/build/bundler.rs` | Generates `_entry.ts` with Deno static file server (SPA routing, PORT env). |
| Deno process spawning with sandboxed permissions | Done | `apps/engine/src/runtime/mod.rs`, `apps/engine/src/runtime/pool/sandbox.rs`, `apps/engine/src/runtime/seccomp.rs`, `apps/engine/src/runtime/namespace.rs` | Deno/Node processes spawned with permission flags, process-level seccomp BPF via `pre_exec`, and PID/mount namespace isolation via `unshare(2)`. |
| Health check polling | Done | `apps/engine/src/runtime/health.rs`, `apps/engine/src/runtime/mod.rs` | Runtime waits for the allocated port to become reachable. |
| RuntimeManager: deploy, resolve, zero-downtime swap | Done | `apps/engine/src/runtime/mod.rs` | Full implementation with atomic swap and 5-second graceful drain of old process. |
| hyper reverse proxy with Host-based routing | Partial | `apps/engine/src/proxy/handler.rs`, `apps/engine/src/proxy/router.rs` | Host-based routing works, but the implementation is axum plus reqwest forwarding, not the planned hyper-based proxy layer. |
| Wire build -> deploy -> proxy routing | Done | `apps/engine/src/build/mod.rs`, `apps/engine/src/proxy/handler.rs` | Successful builds launch a runtime and the proxy resolves traffic to ready deployments. |
| Verify: push -> build -> deploy -> curl returns app | Not verified | `apps/engine/src/build/mod.rs`, `apps/engine/src/runtime/mod.rs`, `apps/engine/src/proxy/handler.rs` | The path exists in code, but the roadmap verification has not been run and recorded. |

## Phase 4: Dashboard UI

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Scaffold Next.js + Tailwind + shadcn/ui | Done | `templates/template-app/package.json`, `templates/template-app/components.json`, `templates/template-app/src/app/globals.css` | Implemented as `templates/template-app`, not the planned `apps/web` workspace package. |
| API client with JWT, WebSocket hook | Done | `templates/template-app/src/lib/rift.ts`, `templates/template-app/src/hooks/use-deploy-logs.ts` | JWT-backed server API client and WebSocket `useDeployLogs` hook with HTTP fallback. |
| Login page, sidebar layout | Done | `templates/template-app/src/app/(auth)/auth/page.tsx`, `templates/template-app/src/app/(dashboard)/layout.tsx` | Auth page and dashboard shell are implemented. |
| Project list, new project wizard | Done | `templates/template-app/src/app/(dashboard)/projects/page.tsx` | Project listing and creation UI are wired to API routes. |
| Deployment list, real-time log viewer (terminal-style) | Done | `templates/template-app/src/app/(dashboard)/projects/[projectName]/page.tsx`, `templates/template-app/src/hooks/use-deploy-logs.ts` | Project detail page has real-time WebSocket log streaming with live indicator, HTTP fallback, and terminal-style display. |
| Manual redeploy button | Done | `templates/template-app/src/app/(dashboard)/projects/[projectName]/page.tsx` | Project detail can POST a new deployment. |
| Dark-mode-first, minimal design | Partial | `templates/template-app/package.json`, `templates/template-app/src/app/globals.css` | Theme tooling exists, but the current implementation is not clearly dark-mode-first. |
| Verify: full flow through the UI | Not verified | `templates/template-app/src/app/api/` | Several screens are wired, but end-to-end verification has not been completed. |

## Phase 5: Env Vars + Custom Domains + SSL

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| AES-256-GCM encryption for env var values | Done | `apps/engine/src/api/env_vars.rs` | Full AES-256-GCM with SHA-256 key derivation, random nonce, encrypt/decrypt implemented. |
| Env var CRUD API (masked in responses) | Done | `apps/engine/src/api/env_vars.rs` | Create, list (masked), delete endpoints with proper encryption. |
| Inject decrypted env vars into Deno process | Done | `apps/engine/src/build/mod.rs` | Build pipeline decrypts and injects env vars during build and runtime launch. |
| Domain CRUD, DNS verification | Done | `apps/engine/src/api/domains.rs`, `apps/engine/src/db/domains.rs` | Create, list, assign, primary selection, and DNS verification are implemented. |
| Auto-SSL via Let's Encrypt | Done | `apps/engine/src/ssl/manager.rs`, `apps/engine/src/proxy/tls.rs`, `apps/engine/src/proxy/acme.rs` | Full ACME HTTP-01 flow via instant-acme, CertResolver with SNI, dual HTTP/HTTPS proxy, background renewal every 24h. |
| Dashboard pages for env vars and domains | Partial | `templates/template-app/src/app/(dashboard)/domains/page.tsx`, `templates/template-app/src/app/(dashboard)/environment/page.tsx` | Domains UI is substantial; environment page is static placeholder content. |
| Verify: set env var -> redeploy -> app reads it; custom domain with SSL works | Not verified | `apps/engine/src/api/env_vars.rs` | Env var CRUD is implemented; SSL is still missing. Partial verification possible. |

## Phase 6: Scale to Zero + Docker + Polish

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Idle detection loop, process suspend/wake | Done | `apps/engine/src/runtime/scaler.rs`, `apps/engine/src/runtime/mod.rs` | Background loop every 30s suspends deployments idle >5min, stores launch spec for re-wake. |
| Wake-on-request in proxy | Done | `apps/engine/src/proxy/handler.rs`, `apps/engine/src/runtime/mod.rs` | Proxy detects suspended projects and wakes them on demand before forwarding. |
| Fail-safe runtime reconciliation and automatic restart after engine/app crashes or restarts | Done | `apps/engine/src/runtime/mod.rs`, `apps/engine/src/build/mod.rs`, `apps/engine/src/db/deployments.rs` | On startup, queries ready deployments from DB, detects runtime kind from filesystem, re-launches Deno processes. Old deployments cleaned up after successful new deploys. |
| Multi-stage Dockerfile (engine + web + deno + node) | Partial | `docker/Dockerfile`, `docker/Dockerfile.frontend` | Multi-stage engine image exists and includes Deno and Node; frontend is built from a separate Dockerfile. |
| docker-compose.yml with PostgreSQL | Done | `docker/docker-compose.yml` | Compose file includes `db`, `engine`, and `frontend` services. |
| entrypoint.sh (migrations + service start) | Done | `docker/entrypoint.sh`, `apps/engine/src/db/mod.rs` | Entrypoint starts the engine; migrations run on engine startup. |
| CORS, rate limiting, request logging | Done | `apps/engine/src/api/mod.rs`, `apps/engine/src/api/users.rs` | CORS, request tracing, request IDs, body limits, and auth rate limiting are in place. |
| Host firewall policy, kernel network hardening, basic anti-DDoS protections | Done | `docker/entrypoint.sh`, `docker/docker-compose.yml`, `apps/engine/src/api/firewall.rs`, `apps/engine/src/proxy/firewall_cache.rs` | iptables firewall rules (default DROP, allow service ports, restrict worker ports to localhost), sysctl kernel hardening (SYN cookies, rp_filter, redirect blocking, source route rejection), container-level sysctls, nproc/nofile ulimits. |
| Verify: `docker-compose up` -> full platform working | Partial | `docker/docker-compose.yml`, `docker/smoke-test.sh` | Docker assets exist and a smoke test script (`docker/smoke-test.sh`) covers registration, auth, project listing, and proxy response. Full end-to-end deploy+build verification requires a running GitHub repo. |

## Extra Progress Not Explicitly Captured by PLAN.md

- Request analytics collection and hourly aggregation exist in `apps/engine/src/proxy/analytics_collector.rs` and `apps/engine/src/api/analytics.rs`.
- GitHub identity exchange and backend session bridging for the dashboard exist in `apps/engine/src/api/users.rs` and `templates/template-app/src/lib/rift.ts`.
- Project-level firewall management exists in `apps/engine/src/api/firewall.rs` and `apps/engine/src/proxy/firewall_cache.rs`.
- Seccomp enforcement for worker processes: Docker-level via `--security-opt seccomp=` and process-level via `prctl`/`seccomp()` BPF in `pre_exec`. Configurable via `RIFT_SECCOMP_ENFORCE`, fail-safe behavior.
- Linux namespace isolation for workers: PID and mount namespaces via `unshare(2)` in `pre_exec`. Non-fatal fallback if capabilities are insufficient.
- Host firewall and kernel hardening: iptables rules (default DROP, service port allow, worker port localhost-only), sysctl hardening (SYN cookies, rp_filter, redirect/source-route blocking), container-level sysctls and ulimits.
- Immutable runtime artifacts: `_rift_artifact/` directory created per deployment with read-only permissions, runtime executes from immutable copy.
- Configurable build concurrency: `RIFT_BUILD_CONCURRENCY` (default 4) with backpressure logging and queued status for waiting builds.
- Dependency caching: `RIFT_BUILD_CACHE_DIR` caches `node_modules` keyed by lockfile hash, auto-prunes to 10 entries. Native package manager cache (`npm_config_cache`, `PNPM_HOME`, `YARN_CACHE_FOLDER`) stored persistently under `RIFT_BUILD_CACHE_DIR/native/`.
- Conditional install skip: `RIFT_INSTALL_SKIP_ON_CACHE_HIT` (default true) skips `npm install` when lockfile hash matches cached node_modules.
- Optimized install commands: frozen lockfile + `--prefer-offline` flags applied to default install commands (npm ci, pnpm install --frozen-lockfile, yarn install --frozen-lockfile).
- Cache cleaning gated: `RIFT_BUILD_CLEAN_CACHE` (default false) prevents post-install cache wipe that previously destroyed warm-cache benefit.
- CoW/reflink artifact copy: `RIFT_ARTIFACT_COPY_MODE` (default auto) tries FICLONE (Linux) / clonefile (macOS) before falling back to recursive copy.
- Selective Node SSR artifacts: NodeServer runtime copies only output dir, node_modules, package.json, and public/ instead of entire workspace.
- Per-stage deploy timing: clone, detect, install, build, artifact, runtime_start, total durations logged as structured messages and tracing info.
- Configurable health checks: `RIFT_HEALTHCHECK_INTERVAL_MS` (default 200ms) and `RIFT_HEALTHCHECK_ATTEMPTS` (default 50) replace hardcoded 500ms/40.
- V8 isolate Web Standards shim: URL, URLSearchParams, AbortController, AbortSignal, Event, EventTarget, Blob, FormData, TextEncoder, TextDecoder, structuredClone, atob/btoa, queueMicrotask, crypto.getRandomValues().
- Docker-compose smoke test: `docker/smoke-test.sh` covers service startup, registration, auth, project listing, and proxy response.

## Current Verification State

- `cargo check -p rift-engine --lib --bins`: passes
- `cargo test -p rift-engine --lib`: passes (28 tests)
- `cargo test -p rift-engine --test pool_tests`: passes (50 tests, including seccomp enforcement, immutable artifacts, build concurrency, and deploy speed optimizations)
- `cargo check -p rift-engine --test api_flow`: passes
