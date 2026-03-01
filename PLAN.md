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

### audit_log
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL | PK |
| timestamp | TIMESTAMPTZ | default now() |
| user_id | UUID | nullable (not all events have a user) |
| event | TEXT | e.g. user.login, deployment.start |
| resource_id | UUID | nullable, the affected resource |
| ip_address | INET | request source IP |
| user_agent | TEXT | |
| metadata | JSONB | event-specific data |

### refresh_tokens
| Column | Type | Notes |
|--------|------|-------|
| id | UUID | PK |
| user_id | UUID | FK -> users |
| token_hash | TEXT | SHA-256 of refresh token |
| expires_at | TIMESTAMPTZ | |
| created_at | TIMESTAMPTZ | |
| revoked | BOOLEAN | default false |

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
- [x] Scaffold Rust engine with all deps
- [x] Write 6 SQL migrations
- [x] Implement db layer (pool, models, query modules)
- [x] Auth: argon2 password hashing, JWT issue/verify, middleware
- [x] Project CRUD API
- [ ] **Verify**: curl register -> login -> create/list projects

### Phase 2: Git + Webhooks + Build Pipeline
- [ ] Git clone/fetch/checkout operations
- [x] GitHub webhook creation via API
- [x] Webhook receiver with HMAC verification
- [ ] Framework detection (Next.js, Vite, Remix, Astro, Svelte, static)
- [x] Full build pipeline: clone -> install -> build -> log capture
- [x] Build queue (tokio mpsc, configurable concurrency)
- [x] Log broadcaster (tokio broadcast channels)
- [ ] **Verify**: create project -> push to GitHub -> build runs with streamed logs

### Phase 3: Deno Runtime + Reverse Proxy
- [x] Deno bundle creation (_entry.ts per framework type)
- [ ] Deno process spawning with sandboxed permissions
- [x] Health check polling
- [x] RuntimeManager: deploy, resolve, zero-downtime swap
- [ ] hyper reverse proxy with Host-based routing
- [x] Wire build -> deploy -> proxy routing
- [ ] **Verify**: push -> build -> deploy -> curl returns app

### Phase 4: Dashboard UI
- [x] Scaffold Next.js + Tailwind + shadcn/ui
- [x] API client with JWT, WebSocket hook
- [x] Login page, sidebar layout
- [x] Project list, new project wizard
- [x] Deployment list, real-time log viewer (terminal-style)
- [x] Manual redeploy button
- [ ] **Verify**: full flow through the UI

### Phase 5: Env Vars + Custom Domains + SSL
- [x] AES-256-GCM encryption for env var values
- [x] Env var CRUD API (masked in responses)
- [x] Inject decrypted env vars into Deno process
- [x] Domain CRUD, DNS verification
- [x] Auto-SSL via Let's Encrypt (rustls-acme / instant_acme)
- [ ] Dashboard pages for env vars and domains
- [ ] **Verify**: set env var -> redeploy -> app reads it; custom domain with SSL works

### Phase 6: Scale to Zero + Docker + Polish
- [x] Idle detection loop, process suspend/wake
- [x] Wake-on-request in proxy
- [x] Fail-safe runtime reconciliation and automatic restart after engine/app crashes or restarts (clean up ghost projects/processes, restore routing, recover healthy deployments)
- [ ] Multi-stage Dockerfile (engine + web + deno + node)
- [x] docker-compose.yml with PostgreSQL
- [x] entrypoint.sh (migrations + service start)
- [x] CORS, rate limiting, request logging
- [ ] Host firewall policy, kernel network hardening, basic anti-DDoS protections
- [ ] **Verify**: `docker-compose up` -> full platform working

---

## Security Architecture

Rift runs **arbitrary user code** — builds execute `npm install` (which runs lifecycle scripts) and deployments serve live traffic. Every layer must assume hostile input.

### Threat Model

| Threat | Vector | Impact |
|--------|--------|--------|
| Deployment escapes sandbox | Deno process reads host filesystem, reaches DB, or contacts other deployments | Full host compromise |
| Malicious build script | `postinstall` in npm package exfiltrates secrets or installs backdoor | Data theft, persistent access |
| SSRF from deployed function | Function calls `http://localhost:3001/api/...` or cloud metadata endpoint | API bypass, credential theft |
| Credential stuffing | Brute-force login endpoint | Account takeover |
| Secret exfiltration | Deployment logs env vars or sends them to external server | Leaked secrets |
| Deployment resource abuse | Fork bomb, memory exhaustion, disk fill | Denial of service to all tenants |
| Supply chain via webhook | Spoofed GitHub webhook triggers malicious build | Arbitrary code execution |
| SQL injection | Malformed input in API parameters | Data breach |

### 1. Runtime Isolation (Deno Processes)

**Sandboxed permissions** — each Deno subprocess runs with the minimum permission set:

```
deno run \
  --allow-net=0.0.0.0:{assigned_port} \  # ONLY listen on assigned port, no outbound by default
  --allow-read={bundle_dir} \             # ONLY the deployment's own bundle
  --allow-env \                           # Injected env vars only (filtered)
  --no-prompt \                           # Never prompt for permissions
  --no-remote \                           # No remote module imports at runtime
  _entry.ts
```

**Outbound network policy**: Deployments need outbound access (APIs, databases). Two-tier approach:
- MVP: `--allow-net` (allow all network) but block internal ranges via iptables/nftables rules on the host
- Post-MVP: per-project allowlists configured in dashboard

**Linux namespace isolation** (beyond Deno flags):
- **PID namespace**: process cannot see or signal other processes on host
- **Network namespace**: process gets its own network stack; veth pair routes only its assigned port
- **Mount namespace**: read-only root, tmpfs for `/tmp`, bundle dir bind-mounted read-only
- **User namespace**: map to unprivileged UID (e.g., `uid 65534`/`nobody`)

Implementation: use `unshare(2)` / `clone(2)` syscalls in Rust before `exec`-ing Deno, or wrap with a minimal `clone` shim. No full container runtime needed.

**Seccomp filter**: apply a BPF profile that blocks dangerous syscalls:
- `ptrace`, `mount`, `umount`, `reboot`, `kexec_load`, `pivot_root`
- `clone` with `CLONE_NEWUSER` (prevent further namespace creation)
- `socket` restricted to `AF_INET`/`AF_INET6` (no Unix domain sockets to host)

### 2. Resource Limits

Every Deno process gets hard limits enforced via **cgroups v2**:

| Resource | Default Limit | Configurable |
|----------|--------------|--------------|
| Memory | 512 MB | Per-project |
| CPU | 1 core (cpu.max) | Per-project |
| PIDs | 64 | No |
| Open files | 256 (ulimit -n) | No |
| Disk write | tmpfs only, 100 MB max | Per-project |
| Process lifetime | 30 min max (killed if exceeded) | Per-project |

Cgroup setup: create a cgroup per deployment under `/sys/fs/cgroup/rift/deployments/{deploy_id}/`, write limits, move the Deno PID into it.

### 3. Build Pipeline Isolation

Builds are **more dangerous** than runtime — `npm install` runs arbitrary scripts with full Node.js access.

**Build sandbox**:
- Each build runs in a **throwaway Linux namespace** (same approach as runtime but more restrictive)
- Dedicated build user (`uid 10000+`) with no access to engine data or other builds
- Filesystem: overlay mount with read-only base + writable upper layer, discarded after build
- Network: allow outbound HTTPS (npm registry, git) but block internal ranges (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `169.254.169.254/32`, `127.0.0.0/8`)
- Timeout: hard kill after configurable limit (default: 10 minutes)
- Resource limits: 2 GB memory, 2 cores, 1 GB disk

**Build output validation**:
- Maximum bundle size enforced (default 500 MB)
- No symlinks allowed outside bundle directory
- No setuid/setgid binaries
- No executable files except the entry point

### 4. Network Security

**Internal service protection**:
- API server (port 3001) binds to `127.0.0.1` only — not accessible from deployment network namespaces
- PostgreSQL listens on `127.0.0.1` or Unix socket only
- iptables rules on host:
  ```
  # Block deployments from reaching host services
  -A OUTPUT -m owner --uid-owner rift-sandbox -d 127.0.0.0/8 -j DROP
  -A OUTPUT -m owner --uid-owner rift-sandbox -d 169.254.169.254 -j DROP  # cloud metadata
  -A OUTPUT -m owner --uid-owner rift-sandbox -d 10.0.0.0/8 -j DROP       # private ranges
  -A OUTPUT -m owner --uid-owner rift-sandbox -d 172.16.0.0/12 -j DROP
  -A OUTPUT -m owner --uid-owner rift-sandbox -d 192.168.0.0/16 -j DROP
  ```

**Reverse proxy hardening**:
- Request size limit: 10 MB default (configurable per project)
- Header count limit: 100
- Request timeout: 30s (configurable)
- No hop-by-hop header forwarding
- Strip `X-Forwarded-*` headers from client, set them authoritatively
- Rate limit per source IP: 100 req/s default

**TLS**:
- All external traffic over TLS 1.2+ (rustls, no OpenSSL)
- Auto-SSL via ACME (Let's Encrypt) with DNS-01 or HTTP-01 challenge
- Certificate private keys stored encrypted at rest

### 5. Host Firewall & Anti-DDoS

Rift is internet-facing by default, so it needs a **default-deny host firewall** plus layered DDoS controls. Application rate limiting alone is not enough.

**Host firewall policy**:
- Default deny inbound, allow only `80/tcp`, `443/tcp`, and optional `22/tcp` for admin SSH
- Deny direct access to engine internals (`3001`, PostgreSQL, build worker internals)
- Allow loopback, `ESTABLISHED,RELATED`, and required ICMP/ICMPv6 for PMTU + health
- Restrict egress from sandbox/build UIDs to required destinations only

Example `nftables` shape for the host:

```nft
table inet filter {
  chain input {
    type filter hook input priority 0;
    policy drop;

    iif "lo" accept
    ct state established,related accept

    # Optional SSH management.
    tcp dport 22 ct state new limit rate 15/minute accept

    # Public edge.
    tcp dport { 80, 443 } ct state new accept

    # ICMP/ICMPv6 required for normal network operation.
    ip protocol icmp accept
    ip6 nexthdr ipv6-icmp accept
  }

  chain forward {
    type filter hook forward priority 0;
    policy drop;
  }

  chain output {
    type filter hook output priority 0;
    policy accept;
  }
}
```

**Volumetric DDoS stance**:
- MVP: strongly recommend fronting Rift with a CDN / L4 proxy (e.g. Cloudflare, Fly Proxy, AWS Shield-backed LB) so TLS termination and large floods are absorbed upstream
- If exposed directly, document that the host is only protected against **basic** SYN/connection floods, not carrier-scale volumetric attacks
- Trust forwarded client IPs only from configured proxy CIDRs; otherwise use the direct peer IP

**Kernel/network hardening**:

```conf
# /etc/sysctl.d/99-rift-network.conf
net.ipv4.tcp_syncookies = 1
net.ipv4.tcp_max_syn_backlog = 8192
net.core.somaxconn = 4096
net.ipv4.conf.all.rp_filter = 1
net.ipv4.conf.default.rp_filter = 1
net.ipv4.icmp_echo_ignore_broadcasts = 1
net.ipv4.conf.all.accept_redirects = 0
net.ipv4.conf.default.accept_redirects = 0
net.ipv4.conf.all.send_redirects = 0
net.ipv4.conf.default.send_redirects = 0
```

**Connection flood controls**:
- Per-IP concurrent connection cap at the proxy layer (default: 100 open connections/IP)
- Per-IP new connection rate cap (default: 30/sec burst 60) before requests hit app handlers
- Tight header/body read timeouts to limit slowloris-style attacks
- Global in-flight request cap with fast `503`/shed behavior when the proxy is saturated
- Optional `synproxy` / `nf_connlimit` / `hashlimit` rules for deployments that are directly exposed

Example host-level guards:

```nft
table inet rift_ddos {
  set edge_ports {
    type inet_service
    elements = { 80, 443 }
  }

  chain input {
    type filter hook input priority -5;

    tcp dport @edge_ports ct state new meter per_ip_conn_rate { ip saddr limit rate 30/second burst 60 packets } accept
    tcp dport @edge_ports ct count over 100 drop
  }
}
```

**Application-layer DDoS controls**:
- Request body size limit: 10 MB default, lower on auth/webhook endpoints
- Header read timeout: 5s; body read timeout: 15s; idle keep-alive timeout: 30s
- Per-route rate limits:
  - `POST /api/login`: 5 attempts per email / 15 min and 20 per IP / 15 min
  - `POST /api/register`: 3 per IP / hour
  - Webhooks: 10 per project / minute and 60 per IP / minute
  - Build trigger / redeploy endpoints: 5 per project / minute
- Ban or challenge IPs that repeatedly trip auth/webhook limits (temporary denylist)
- Queue and worker backpressure: bounded build queue, bounded websocket subscribers, bounded log buffer

**Proxy hardening for abusive traffic**:
- No request buffering beyond configured limits
- Drop malformed HTTP early
- Disable unlimited keep-alive reuse from abusive clients
- Strip duplicate/conflicting forwarding headers
- Emit `429` for rate limits and `503` for load shedding, with audit log entries

**Observability for attack detection**:
- Metrics: requests/sec, connection count, new connections/sec, rate-limit hits, 429s, 503s, SYN backlog pressure
- Audit events for firewall drops are optional, but proxy-level denies and auth/webhook abuse should be logged
- Alerts on sustained high 429/503 rates, elevated conntrack usage, or repeated webhook/login abuse

### 6. Authentication & Authorization

**Password security**:
- Argon2id with tuned parameters: memory 64 MB, iterations 3, parallelism 4
- Minimum password length: 12 characters
- Breached password check against HaveIBeenPwned k-anonymity API (optional, configurable)

**JWT hardening**:
- Algorithm: EdDSA (Ed25519) — not HS256 (symmetric = any service with the key can forge tokens)
- Short expiry: 15 minutes access token + 7 day refresh token (httpOnly cookie)
- Token includes: `sub` (user ID), `iat`, `exp`, `jti` (unique ID for revocation)
- Refresh token rotation: old refresh token invalidated on use
- Store refresh token hash in DB for server-side revocation

**Brute-force protection**:
- Rate limit login: 5 attempts per email per 15 minutes
- Rate limit register: 3 per IP per hour
- Constant-time password comparison (argon2 handles this)
- Generic error messages ("invalid email or password", never reveal which)

**API authorization**:
- Every resource query scoped to `WHERE user_id = $1` — no admin override in MVP
- UUIDs as IDs (not sequential integers) to prevent enumeration

### 7. Secrets Management

**Encryption**:
- AES-256-GCM with random 96-bit nonce per value (already in plan)
- Encryption key: derived from a master key via HKDF with per-project salt
- Master key: loaded from env var `RIFT_MASTER_KEY` (never stored in DB)
- If master key is lost, all encrypted env vars are irrecoverable (documented)

**Secret handling rules**:
- Env var values never appear in API responses (masked as `••••••`)
- Env var values never written to build logs (stdout/stderr filtered for known patterns)
- Env vars injected into Deno process environment, not written to disk
- Decrypted values held in memory only, zeroed after process spawn (`zeroize` crate)
- Build logs scrubbed: scan for values matching known env var values before storage

### 8. Webhook Security

- HMAC-SHA256 verification on every incoming webhook (already in plan)
- Per-project unique webhook secret (generated via `rand::OsRng`, 32 bytes, hex-encoded)
- Reject if signature missing or invalid — return 200 (don't leak valid/invalid project IDs)
- Verify `X-GitHub-Event` header matches expected event type
- Ignore events from branches other than the configured branch
- Webhook endpoint rate limit: 10 per project per minute

### 9. Input Validation & SQL Safety

- **All SQL via sqlx** with compile-time checked queries — parameterized by default, no string interpolation
- **Input validation layer** (tower middleware or extractor):
  - Project name: `^[a-z0-9-]{1,64}$`
  - Subdomain: `^[a-z0-9-]{1,63}$`, not in reserved list (`api`, `www`, `admin`, `static`, etc.)
  - Repo URL: must match `https://github.com/{owner}/{repo}` pattern
  - Domain: valid FQDN, max 253 chars
  - Env var key: `^[A-Z_][A-Z0-9_]{0,255}$`
  - All string fields: max length enforced, no null bytes

### 10. Audit Logging

Security-relevant events logged to a dedicated `audit_log` table:

| Event | Fields |
|-------|--------|
| `user.login` | user_id, ip, user_agent, success |
| `user.login_failed` | email (hashed), ip, user_agent |
| `user.register` | user_id, ip |
| `project.create` | user_id, project_id |
| `project.delete` | user_id, project_id |
| `env_var.create` | user_id, project_id, key (not value) |
| `env_var.update` | user_id, project_id, key (not value) |
| `deployment.start` | project_id, deployment_id, trigger (webhook/manual) |
| `deployment.fail` | project_id, deployment_id, reason |
| `domain.add` | user_id, project_id, domain |
| `webhook.invalid_signature` | project_id, ip |

Add `audit_log` table to migrations. Retention: 90 days default, configurable.

### 11. Docker Hardening

The single Docker container that runs Rift must itself be hardened:

```yaml
# docker-compose.yml security settings
services:
  engine:
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    cap_add:
      - NET_BIND_SERVICE    # bind ports 80/443
      - SYS_ADMIN           # needed for unshare/clone (namespace isolation)
      - NET_ADMIN            # iptables rules for network isolation
    read_only: true
    tmpfs:
      - /tmp:size=1G
      - /var/rift/builds:size=5G
    volumes:
      - deployments:/var/rift/deployments  # named volume for deployment bundles
    ulimits:
      nofile:
        soft: 65536
        hard: 65536
```

**Filesystem layout inside container**:
- `/var/rift/deployments/` — deployment bundles (read-only to Deno processes)
- `/var/rift/builds/` — tmpfs, build workspace (wiped after each build)
- `/var/rift/ssl/` — TLS certificates (read-only to engine)
- Engine binary and Deno binary: read-only

### 12. Security Implementation Phases

Security is **not a phase** — it's woven into every implementation phase:

| Phase | Security Tasks |
|-------|---------------|
| Phase 1 | Argon2id with tuned params, EdDSA JWT, rate limiting on auth, input validation, sqlx parameterized queries, audit log table |
| Phase 2 | HMAC webhook verification, build sandbox (namespaces + cgroups + timeout), log scrubbing for secrets, webhook rate limiting |
| Phase 3 | Deno namespace isolation, seccomp filter, resource limits (cgroups), network isolation (iptables/nftables), SSRF protection |
| Phase 4 | CSRF protection (SameSite cookies), CSP headers, XSS prevention (React handles most), httpOnly refresh tokens |
| Phase 5 | HKDF key derivation, zeroize secrets in memory, env var masking in API, build log scrubbing |
| Phase 6 | Docker cap_drop/no-new-privileges, read-only filesystem, host firewall default-deny policy, kernel hardening sysctls, connection flood controls, proxy load shedding, health check hardening |

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
- [ ] gVisor/Firecracker microVM isolation (stronger than namespaces)
- [ ] Per-project outbound network allowlists
- [ ] WAF rules on reverse proxy (OWASP Core Rule Set)
- [ ] Signed deployments (verify bundle integrity before execution)
- [ ] Security event alerting (anomalous login patterns, repeated webhook failures)
- [ ] Dependency vulnerability scanning during builds (npm audit integration)
- [ ] Automatic secret rotation support
