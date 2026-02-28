# Rift Progress Checklist

Last updated: 2026-02-28

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
| Phase 1 | 5 | 0 | 2 |
| Phase 2 | 5 | 1 | 2 |
| Phase 3 | 4 | 1 | 2 |
| Phase 4 | 6 | 1 | 1 |
| Phase 5 | 4 | 1 | 2 |
| Phase 6 | 3 | 2 | 4 |

## Phase 1: Scaffolding + Database + API

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Init monorepo (Cargo workspace, pnpm workspace) | Partial | `Cargo.toml` | Rust workspace exists, but the planned pnpm workspace files are not present. |
| Scaffold Rust engine with all deps | Done | `apps/engine/Cargo.toml` | Engine crate and dependency set are in place. |
| Write 6 SQL migrations | Done | `apps/engine/migrations/` | There are 12 migrations, which exceeds the roadmap target. |
| Implement db layer (pool, models, query modules) | Done | `apps/engine/src/db/` | Pool setup, models, and query modules are implemented. |
| Auth: argon2 password hashing, JWT issue/verify, middleware | Done | `apps/engine/src/services/password.rs`, `apps/engine/src/services/auth.rs`, `apps/engine/src/api/auth.rs` | Password hashing, JWTs, refresh tokens, cookies, and auth extraction exist. |
| Project CRUD API | Done | `apps/engine/src/api/projects.rs` | Create, list, get, update, and delete are implemented. |
| Verify: curl register -> login -> create/list projects | Not verified | `apps/engine/tests/api_flow.rs` | The integration test file exists, but `cargo test -p rift-engine` currently fails because the test fixture is stale. |

## Phase 2: Git + Webhooks + Build Pipeline

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Git clone/fetch/checkout operations | Partial | `apps/engine/src/build/mod.rs`, `apps/engine/src/build/pipeline.rs` | Git clone is implemented through shell commands, but there is no dedicated fetch/checkout service matching the planned module layout. |
| GitHub webhook creation via API | Done | `apps/engine/src/api/projects.rs`, `apps/engine/src/services/github.rs` | Project creation attempts to register a GitHub push webhook automatically. |
| Webhook receiver with HMAC verification | Done | `apps/engine/src/api/webhooks.rs` | Push webhooks are received and HMAC signatures are verified when a project secret exists. |
| Framework detection (Next.js, Vite, Remix, Astro, Svelte, static) | Partial | `apps/engine/src/build/detect.rs` | Next.js, Vite-like apps, and generic static output are handled; Remix, Astro, and Svelte are not explicitly implemented. |
| Full build pipeline: clone -> install -> build -> log capture | Done | `apps/engine/src/build/mod.rs`, `apps/engine/src/build/pipeline.rs` | Build orchestration, command execution, and log persistence are implemented. |
| Build queue (tokio mpsc, configurable concurrency) | Done | `apps/engine/src/build/mod.rs` | Semaphore-based concurrency limiting is implemented. |
| Log broadcaster (tokio broadcast channels) | Done | `apps/engine/src/ws/broadcast.rs`, `apps/engine/src/ws/mod.rs`, `apps/engine/src/ws/handler.rs` | Full implementation: `LogBroadcaster` with per-deployment `tokio::broadcast` channels, WebSocket endpoint at `/api/ws/logs`, integrated into build pipeline. |
| Verify: create project -> push to GitHub -> build runs with streamed logs | Not verified | `apps/engine/src/api/webhooks.rs`, `apps/engine/src/api/logs.rs` | Build triggering exists, but streamed logs are not implemented and this flow has not been verified end-to-end. |

## Phase 3: Deno Runtime + Reverse Proxy

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Deno bundle creation (`_entry.ts` per framework type) | Done | `apps/engine/src/build/bundler.rs` | Generates `_entry.ts` with Deno static file server (SPA routing, PORT env). |
| Deno process spawning with sandboxed permissions | Partial | `apps/engine/src/runtime/mod.rs` | Deno processes are spawned (`spawn_deno_static`, `spawn_deno_next`), but sandboxed permissions (namespaces, seccomp) are not yet applied. |
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
| Auto-SSL via Let's Encrypt | Missing | `apps/engine/src/api/domains.rs`, `apps/engine/src/db/domains.rs` | Domain status exists, but certificate provisioning is not implemented. |
| Dashboard pages for env vars and domains | Partial | `templates/template-app/src/app/(dashboard)/domains/page.tsx`, `templates/template-app/src/app/(dashboard)/environment/page.tsx` | Domains UI is substantial; environment page is static placeholder content. |
| Verify: set env var -> redeploy -> app reads it; custom domain with SSL works | Not verified | `apps/engine/src/api/env_vars.rs` | Env var CRUD is implemented; SSL is still missing. Partial verification possible. |

## Phase 6: Scale to Zero + Docker + Polish

| Item | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Idle detection loop, process suspend/wake | Missing | `apps/engine/src/runtime/scaler.rs` | Scaler module is still a stub. |
| Wake-on-request in proxy | Missing | `apps/engine/src/proxy/handler.rs` | Proxy forwards only to already-running ready deployments. |
| Fail-safe runtime reconciliation and automatic restart after engine/app crashes or restarts | Missing | `apps/engine/src/runtime/`, `apps/engine/src/proxy/` | There is no startup reconciliation or crash recovery flow yet for restoring healthy deployments and cleaning up ghost runtimes or stale routing state. |
| Multi-stage Dockerfile (engine + web + deno + node) | Partial | `docker/Dockerfile`, `docker/Dockerfile.frontend` | Multi-stage engine image exists and includes Deno and Node; frontend is built from a separate Dockerfile. |
| docker-compose.yml with PostgreSQL | Done | `docker/docker-compose.yml` | Compose file includes `db`, `engine`, and `frontend` services. |
| entrypoint.sh (migrations + service start) | Done | `docker/entrypoint.sh`, `apps/engine/src/db/mod.rs` | Entrypoint starts the engine; migrations run on engine startup. |
| CORS, rate limiting, request logging | Done | `apps/engine/src/api/mod.rs`, `apps/engine/src/api/users.rs` | CORS, request tracing, request IDs, body limits, and auth rate limiting are in place. |
| Host firewall policy, kernel network hardening, basic anti-DDoS protections | Partial | `apps/engine/src/api/firewall.rs`, `apps/engine/src/proxy/firewall_cache.rs`, `docker/docker-compose.yml` | There is project-level firewalling and some container hardening, but not the full host or kernel hardening described in the roadmap. |
| Verify: `docker-compose up` -> full platform working | Not verified | `docker/docker-compose.yml` | Docker assets exist, but the full platform verification step has not been completed. |

## Extra Progress Not Explicitly Captured by PLAN.md

- Request analytics collection and hourly aggregation exist in `apps/engine/src/proxy/analytics_collector.rs` and `apps/engine/src/api/analytics.rs`.
- GitHub identity exchange and backend session bridging for the dashboard exist in `apps/engine/src/api/users.rs` and `templates/template-app/src/lib/rift.ts`.
- Project-level firewall management exists in `apps/engine/src/api/firewall.rs` and `apps/engine/src/proxy/firewall_cache.rs`.

## Current Verification State

- `cargo check -p rift-engine --lib --bins`: passes
- `cargo test -p rift-engine --lib`: passes
- `cargo test -p rift-engine`: fails because `apps/engine/tests/api_flow.rs` is out of date with current `Config` and `AppState` fields
