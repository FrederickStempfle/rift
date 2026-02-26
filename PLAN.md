# Rift - Development Roadmap

> Self-hosted serverless deployment platform. Like Vercel, but yours.

## Tech Stack

- **Backend**: Rust (axum, tokio, sqlx, hyper)
- **Frontend**: Next.js 15, TypeScript, Tailwind CSS, shadcn/ui
- **Serverless Runtime**: Deno (V8 isolates, subprocess-based)
- **Database**: PostgreSQL
- **Deployment**: Single Docker image

---

## Monorepo Structure

```
rift/
├── apps/
│   ├── engine/                       # Rust backend
│   │   ├── Cargo.toml
│   │   ├── migrations/               # sqlx PostgreSQL migrations
│   │   └── src/
│   │       ├── main.rs               # CLI args (clap), boot 3 services
│   │       ├── config.rs             # Env/CLI configuration
│   │       ├── error.rs              # Unified error type -> HTTP responses
│   │       ├── api/
│   │       │   ├── mod.rs            # Router assembly
│   │       │   ├── auth.rs           # JWT middleware
│   │       │   ├── projects.rs       # Project CRUD
│   │       │   ├── deployments.rs    # Deployment endpoints
│   │       │   ├── env_vars.rs       # Env var management
│   │       │   ├── domains.rs        # Domain management
│   │       │   ├── webhooks.rs       # GitHub webhook receiver (HMAC verified)
│   │       │   ├── logs.rs           # WebSocket log streaming
│   │       │   └── users.rs          # Register/login
│   │       ├── db/
│   │       │   ├── mod.rs            # Pool init + migration runner
│   │       │   ├── models.rs         # Row structs (FromRow + Serialize)
│   │       │   ├── projects.rs
│   │       │   ├── deployments.rs
│   │       │   ├── env_vars.rs
│   │       │   ├── domains.rs
│   │       │   └── users.rs
│   │       ├── build/
│   │       │   ├── mod.rs            # Top-level build flow
│   │       │   ├── detect.rs         # Framework detection
│   │       │   ├── pipeline.rs       # Clone -> install -> build -> bundle
│   │       │   └── bundler.rs        # Produce Deno-compatible output
│   │       ├── runtime/
│   │       │   ├── mod.rs            # RuntimeManager: deploy, resolve, swap
│   │       │   ├── process.rs        # Deno subprocess wrapper
│   │       │   ├── health.rs         # Readiness polling
│   │       │   └── scaler.rs         # Scale-to-zero idle detection
│   │       ├── proxy/
│   │       │   ├── mod.rs            # hyper-based reverse proxy
│   │       │   ├── router.rs         # Domain -> deployment routing
│   │       │   └── handler.rs        # Request forwarding
│   │       ├── git/
│   │       │   ├── mod.rs
│   │       │   ├── clone.rs          # Clone/fetch/checkout
│   │       │   └── webhook.rs        # GitHub API webhook management
│   │       ├── secrets/
│   │       │   ├── mod.rs
│   │       │   └── crypto.rs         # AES-256-GCM encrypt/decrypt
│   │       └── ws/
│   │           ├── mod.rs
│   │           └── broadcast.rs      # tokio::broadcast per deployment
│   │
│   └── web/                          # Next.js dashboard
│       └── src/
│           ├── app/
│           │   ├── layout.tsx        # Root layout with sidebar
│           │   ├── page.tsx          # Dashboard home
│           │   ├── login/page.tsx
│           │   ├── projects/
│           │   │   ├── page.tsx
│           │   │   ├── new/page.tsx  # New project wizard
│           │   │   └── [id]/
│           │   │       ├── page.tsx
│           │   │       ├── deployments/[deployId]/page.tsx
│           │   │       ├── env/page.tsx
│           │   │       ├── domains/page.tsx
│           │   │       └── settings/page.tsx
│           │   └── settings/page.tsx
│           ├── components/           # sidebar, deploy-log, project-card, etc.
│           ├── lib/                  # api.ts, ws.ts, auth.ts
│           └── hooks/               # use-deploy-logs.ts, use-projects.ts
│
├── docker/
│   ├── Dockerfile                    # Multi-stage: engine + web + deno
│   ├── docker-compose.yml            # Engine + PostgreSQL
│   └── entrypoint.sh
│
├── Cargo.toml                        # Workspace root
├── package.json                      # pnpm workspace
├── pnpm-workspace.yaml
└── .env.example
```

---

## Database Schema

### users
| Column | Type | Notes |
|--------|------|-------|
| id | UUID | PK, gen_random_uuid() |
| email | TEXT | UNIQUE |
| password_hash | TEXT | argon2 |
| created_at | TIMESTAMPTZ | |

### projects
| Column | Type | Notes |
|--------|------|-------|
| id | UUID | PK |
| user_id | UUID | FK -> users |
| name | TEXT | |
| repo_url | TEXT | |
| branch | TEXT | default 'main' |
| framework | ENUM | nextjs, vite, remix, astro, svelte, static, unknown |
| build_command | TEXT | nullable, override auto-detect |
| output_dir | TEXT | nullable, override auto-detect |
| install_command | TEXT | nullable |
| subdomain | TEXT | UNIQUE, used for routing |
| webhook_id | BIGINT | GitHub webhook ID |
| webhook_secret | TEXT | HMAC secret |

### deployments
| Column | Type | Notes |
|--------|------|-------|
| id | UUID | PK |
| project_id | UUID | FK -> projects |
| commit_sha | TEXT | |
| commit_message | TEXT | |
| branch | TEXT | |
| status | ENUM | queued, cloning, building, deploying, ready, failed, cancelled |
| build_duration_ms | INTEGER | |
| url | TEXT | assigned deploy URL |
| started_at | TIMESTAMPTZ | |
| finished_at | TIMESTAMPTZ | |

### env_vars
| Column | Type | Notes |
|--------|------|-------|
| id | UUID | PK |
| project_id | UUID | FK -> projects |
| key | TEXT | UNIQUE with project_id |
| encrypted_value | BYTEA | AES-256-GCM |
| nonce | BYTEA | 12-byte GCM nonce |

### domains
| Column | Type | Notes |
|--------|------|-------|
| id | UUID | PK |
| project_id | UUID | FK -> projects |
| domain | TEXT | UNIQUE |
| is_primary | BOOLEAN | |
| ssl_status | ENUM | pending, provisioning, active, failed |

### deploy_logs
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL | PK |
| deployment_id | UUID | FK -> deployments |
| timestamp | TIMESTAMPTZ | |
| level | TEXT | info, warn, error |
| message | TEXT | |
| source | TEXT | build, runtime |

---

## Architecture

### Three Concurrent Services

The Rust engine runs three services via `tokio::select!`:

1. **API server** (axum, port 3001) - REST API + webhook receiver + WebSocket log streaming
2. **Reverse proxy** (hyper, port 8080) - Routes by Host header to Deno processes
3. **Build worker** - Background task processor from `tokio::sync::mpsc` queue

Shared state: db pool, config, SecretsManager, LogBroadcaster, RuntimeManager, BuildQueue

### Deploy Flow

```
GitHub push
  -> webhook POST /api/webhooks/github/{project_id}
  -> HMAC verify
  -> enqueue build
  -> Build worker:
      1. Clone/fetch repo, checkout commit
      2. Detect framework (package.json + config files)
      3. Install deps (npm/pnpm)
      4. Run build command
      5. Generate _entry.ts + copy assets for Deno
      6. RuntimeManager.deploy():
         - Find free port
         - Spawn Deno subprocess (sandboxed permissions)
         - Health check (poll until 200)
         - Update routing table
         - Gracefully kill old process
      7. Mark deployment "ready"
  -> Build logs streamed to dashboard via WebSocket
```

### Deno Runtime

Each deployment = one Deno subprocess:
```
deno run --allow-net --allow-read={bundle_dir} --allow-env --no-prompt _entry.ts
```
- Port via `PORT` env var
- Env vars injected as process environment
- Health check before routing traffic

### Scale to Zero

- Background loop every 30s checks `last_request` timestamp per process
- Idle > threshold (default 5min) -> stop process
- On next request, proxy wakes the process, waits for health check, forwards
- Cold start ~100-500ms

### Reverse Proxy

hyper-based for MVP:
1. Extract Host header
2. Lookup in RuntimeManager routing table
3. If suspended -> wake + wait
4. Forward to `localhost:{port}`

Post-MVP: swap for pingora (connection pooling, HTTP/2)

---

## Rust Dependencies

```
axum 0.8 (ws, json)          tokio 1 (full)
hyper 1                       reqwest 0.12 (json)
sqlx 0.8 (postgres, migrate)  serde / serde_json
aes-gcm 0.10                  argon2 0.5
jsonwebtoken 9                hmac 0.12 / sha2 0.10
clap 4 (derive, env)          tracing / tracing-subscriber
uuid 1 (v4, serde)            chrono 0.4 (serde)
thiserror 2                    hex 0.4
tower-http 0.6 (cors, trace)
```

## Frontend Dependencies

```
Next.js 15        TypeScript       Tailwind CSS
shadcn/ui         SWR or TanStack Query
```

---

## Implementation Phases

### Phase 1: Scaffolding + Database + API
- [ ] Init monorepo (Cargo workspace, pnpm workspace)
- [ ] Scaffold Rust engine with all deps
- [ ] Write 6 SQL migrations
- [ ] Implement db layer (pool, models, query modules)
- [ ] Auth: argon2 password hashing, JWT issue/verify, middleware
- [ ] Project CRUD API
- [ ] **Verify**: curl register -> login -> create/list projects

### Phase 2: Git + Webhooks + Build Pipeline
- [ ] Git clone/fetch/checkout operations
- [ ] GitHub webhook creation via API
- [ ] Webhook receiver with HMAC verification
- [ ] Framework detection (Next.js, Vite, Remix, Astro, Svelte, static)
- [ ] Full build pipeline: clone -> install -> build -> log capture
- [ ] Build queue (tokio mpsc, configurable concurrency)
- [ ] Log broadcaster (tokio broadcast channels)
- [ ] **Verify**: create project -> push to GitHub -> build runs with streamed logs

### Phase 3: Deno Runtime + Reverse Proxy
- [ ] Deno bundle creation (_entry.ts per framework type)
- [ ] Deno process spawning with sandboxed permissions
- [ ] Health check polling
- [ ] RuntimeManager: deploy, resolve, zero-downtime swap
- [ ] hyper reverse proxy with Host-based routing
- [ ] Wire build -> deploy -> proxy routing
- [ ] **Verify**: push -> build -> deploy -> curl returns app

### Phase 4: Dashboard UI
- [ ] Scaffold Next.js + Tailwind + shadcn/ui
- [ ] API client with JWT, WebSocket hook
- [ ] Login page, sidebar layout
- [ ] Project list, new project wizard
- [ ] Deployment list, real-time log viewer (terminal-style)
- [ ] Manual redeploy button
- [ ] Dark-mode-first, minimal design
- [ ] **Verify**: full flow through the UI

### Phase 5: Env Vars + Custom Domains + SSL
- [ ] AES-256-GCM encryption for env var values
- [ ] Env var CRUD API (masked in responses)
- [ ] Inject decrypted env vars into Deno process
- [ ] Domain CRUD, DNS verification
- [ ] Auto-SSL via Let's Encrypt (rustls-acme / instant_acme)
- [ ] Dashboard pages for env vars and domains
- [ ] **Verify**: set env var -> redeploy -> app reads it; custom domain with SSL works

### Phase 6: Scale to Zero + Docker + Polish
- [ ] Idle detection loop, process suspend/wake
- [ ] Wake-on-request in proxy
- [ ] Multi-stage Dockerfile (engine + web + deno + node)
- [ ] docker-compose.yml with PostgreSQL
- [ ] entrypoint.sh (migrations + service start)
- [ ] CORS, rate limiting, request logging
- [ ] **Verify**: `docker-compose up` -> full platform working

---

## Future / Post-MVP

- [ ] Pingora reverse proxy upgrade (HTTP/2, connection pooling)
- [ ] GitHub App integration (instead of personal tokens)
- [ ] GitLab / Bitbucket support
- [ ] Preview deployments (per-PR)
- [ ] Rollback to previous deployment
- [ ] Build caching (node_modules, .next/cache)
- [ ] Multi-user / team support with RBAC
- [ ] Deployment analytics (request count, latency, errors)
- [ ] CLI tool (`rift deploy`, `rift logs`, `rift env set`)
- [ ] Monorepo support (detect and build specific packages)
- [ ] Docker-based builds as alternative to Deno (for non-JS apps)
- [ ] Notifications (Slack, Discord, email on deploy success/failure)
