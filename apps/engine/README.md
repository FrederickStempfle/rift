<p align="center">
  <h1 align="center">Rift Engine</h1>
  <p align="center">
    A high-performance serverless deployment engine written in Rust.
    <br />
    The core runtime that powers the Rift platform &mdash; build, deploy, and serve JavaScript applications on your own infrastructure.
  </p>
</p>

<p align="center">
  <a href="#features"><strong>Features</strong></a> &nbsp;&middot;&nbsp;
  <a href="#architecture"><strong>Architecture</strong></a> &nbsp;&middot;&nbsp;
  <a href="#getting-started"><strong>Getting Started</strong></a> &nbsp;&middot;&nbsp;
  <a href="#configuration"><strong>Configuration</strong></a> &nbsp;&middot;&nbsp;
  <a href="#api-reference"><strong>API</strong></a>
</p>

---

## Why Rift Engine?

Rift Engine is the self-hosted alternative to platforms like Vercel and Netlify. It gives you the same push-to-deploy workflow, automatic TLS, and serverless scaling &mdash; without the vendor lock-in or the bill. One Rust binary handles everything: building your app, running it in an isolated sandbox, routing traffic to it, and managing TLS certificates. Deploy it on a $5 VPS or a bare-metal server. You own the entire stack.

---

## Features

### Zero-Config Framework Detection

Rift automatically detects your framework and builds accordingly. No config files, no adapters, no vendor-specific plugins required.

| Framework | SSR | Static |
|-----------|:---:|:------:|
| **Next.js** | Yes | Yes |
| **Nuxt** | Yes | Yes |
| **Astro** | Yes | Yes |
| **SvelteKit** | Yes | Yes |
| **Remix** | Yes | Yes |
| **Vite** | &mdash; | Yes |
| **Static Sites** | &mdash; | Yes |

Package managers are auto-detected too &mdash; npm, yarn, pnpm, and bun all work out of the box.

### Pre-Warmed Worker Pool Runtime

Slash cold start times with Rift's pool-based runtime. Workers are pre-warmed and standing by before your first request arrives. When a deployment comes in, a warm worker is specialized via IPC in milliseconds instead of booting a fresh process from scratch.

- **Pre-warmed Deno workers** ready to serve immediately
- **IPC-based specialization** &mdash; workers receive deployment context at request time
- **Automatic crash recovery** via health monitoring
- **Scale-to-zero** &mdash; idle deployments suspend after 5 minutes, wake on the next request

### Cgroup Sandboxing

Every worker runs inside strict cgroup v2 resource limits and a seccomp BPF filter. Untrusted user code cannot escape its sandbox.

- **Memory limits** &mdash; 512 MB per worker (configurable)
- **CPU limits** &mdash; 1 core per worker (configurable)
- **PID limits** &mdash; 64 processes max (fork bomb prevention)
- **Seccomp BPF** &mdash; allowlist-based syscall filtering blocks `ptrace`, `mount`, module loading, and more

### Automatic TLS with Let's Encrypt

Custom domains get free TLS certificates provisioned and renewed automatically via ACME HTTP-01 challenges. No manual cert management. No cron jobs.

- **Automatic provisioning** on domain attachment
- **Background renewal** before expiry
- **rustls-based** TLS termination &mdash; no OpenSSL dependency
- **Staging mode** for testing without hitting rate limits

### Reverse Proxy

A hyper-powered HTTP reverse proxy routes incoming requests to the correct deployment by `Host` header. It handles TLS termination, ACME challenges, firewall enforcement, and request analytics &mdash; all in a single async task.

- **Host-based routing** with in-memory lookup (O(1))
- **Wake-on-request** for suspended deployments
- **Per-project firewall rules** (IP allowlist/blocklist)
- **Connection pooling** with configurable idle timeout
- **Request analytics** collected non-blocking and flushed in batches

### Zero-Downtime Deployments

New deployments are health-checked before traffic is switched over. The old process gets a 5-second graceful drain period. Users never see downtime.

### Security-First Architecture

Rift was designed with security as a foundational concern, not an afterthought.

| Layer | Implementation |
|-------|---------------|
| **Authentication** | EdDSA (Ed25519) JWT tokens with 15-minute access / 7-day refresh rotation |
| **Passwords** | Argon2id with per-user salt |
| **Secrets** | AES-256-GCM encrypted env vars with random nonce; master key never persisted |
| **Memory** | Sensitive values wiped via `zeroize` |
| **Webhooks** | HMAC-SHA256 signature verification |
| **Rate limiting** | Per-endpoint limits (login, registration, webhooks) |
| **Audit logging** | Every security-relevant action recorded |
| **SQL** | Compile-time checked queries via sqlx &mdash; SQL injection is structurally impossible |
| **Unsafe code** | `#![forbid(unsafe_code)]` &mdash; zero unsafe blocks |

### Real-Time Log Streaming

Build and runtime logs are streamed to the dashboard via WebSocket in real time. Every deployment gets its own broadcast channel backed by `tokio::broadcast`, so multiple dashboard tabs can follow the same build simultaneously.

### GitHub Integration

Push to your repo. Rift builds and deploys automatically.

- **Webhook receiver** for push events with HMAC-SHA256 verification
- **Private repo support** via GitHub token injection
- **Webhook lifecycle management** through the GitHub API

---

## Architecture

Rift Engine runs three concurrent services inside a single Tokio runtime:

```
                        ┌──────────────────────────────────────┐
                        │            Rift Engine               │
                        │                                      │
  ┌──────────┐          │  ┌────────────┐   ┌───────────────┐  │
  │  GitHub   │─webhook─▶  │  API Server │   │ Build Worker  │  │
  │          │          │  │  (axum)     │──▶│  (semaphore)  │  │
  └──────────┘          │  │  :3001      │   │               │  │
                        │  └─────┬──────┘   └───────┬───────┘  │
                        │        │                  │          │
                        │        │ WebSocket         │ deploy   │
  ┌──────────┐          │        │ logs             ▼          │
  │Dashboard │◀─ws──────│        │          ┌───────────────┐  │
  │          │          │        │          │Runtime Manager│  │
  └──────────┘          │        │          │  (pool/proc)  │  │
                        │        │          └───────┬───────┘  │
                        │        │                  │          │
  ┌──────────┐          │  ┌─────▼──────────────────▼───────┐  │
  │ Browser  │─request─▶  │        Reverse Proxy            │  │
  │          │◀─────────│  │  (hyper) :8080 / :8443 (TLS)   │  │
  └──────────┘          │  └────────────────────────────────┘  │
                        │                                      │
                        │  ┌────────────────────────────────┐  │
                        │  │         PostgreSQL              │  │
                        │  │  (sqlx, compile-time queries)   │  │
                        │  └────────────────────────────────┘  │
                        └──────────────────────────────────────┘
```

### Module Map

```
src/
├── api/           REST API endpoints + WebSocket log streaming (axum)
├── build/         Build pipeline: clone → detect → install → build → bundle
├── runtime/       Deployment process management, worker pool, scale-to-zero
├── proxy/         Reverse proxy, TLS termination, ACME, firewall, analytics
├── db/            Database layer + migrations (sqlx + PostgreSQL)
├── services/      Auth (JWT), passwords (argon2), rate limiting, audit logging
├── secrets/       AES-256-GCM encryption for environment variables
├── ssl/           TLS certificate management + Let's Encrypt integration
├── git/           Git clone/fetch operations
├── ws/            WebSocket broadcast channels for real-time logs
├── validation/    Input validation rules
├── config.rs      Configuration from environment variables / CLI (clap)
└── error.rs       Unified error types (thiserror)
```

---

## Getting Started

### Prerequisites

- **Rust** (stable toolchain)
- **PostgreSQL** 15+
- **Deno** (for serving deployments)

### Build

```bash
cargo build --release
```

The compiled binary is at `target/release/rift-engine`.

### Run

```bash
# Required
export DATABASE_URL="postgres://user:pass@localhost/rift"
export RIFT_MASTER_KEY="your-256-bit-hex-key"
export RIFT_JWT_PRIVATE_KEY_PEM="..."
export RIFT_JWT_PUBLIC_KEY_PEM="..."

# Start the engine
./target/release/rift-engine
```

The engine starts three listeners:

| Service | Default Port | Purpose |
|---------|:------------:|---------|
| API     | `3001`       | REST API + WebSocket |
| HTTP Proxy | `8080`    | Reverse proxy |
| HTTPS Proxy | `8443`   | TLS-terminated reverse proxy |

---

## Configuration

All settings are configurable via environment variables or CLI flags.

### Required

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | PostgreSQL connection string |
| `RIFT_MASTER_KEY` | AES-256 master encryption key for secrets |
| `RIFT_JWT_PRIVATE_KEY_PEM` | Ed25519 private key (PEM) |
| `RIFT_JWT_PUBLIC_KEY_PEM` | Ed25519 public key (PEM) |

### Server

| Variable | Default | Description |
|----------|---------|-------------|
| `RIFT_API_PORT` | `3001` | API server port |
| `RIFT_PROXY_PORT` | `8080` | HTTP proxy port |
| `RIFT_HTTPS_PORT` | `8443` | HTTPS proxy port |
| `RIFT_BASE_DOMAIN` | `localhost` | Base domain for project subdomains |
| `RIFT_PROXY_SCHEME` | `http` | `http` or `https` |
| `RIFT_CORS_ORIGIN` | &mdash; | Restrict CORS to a specific origin |

### Runtime

| Variable | Default | Description |
|----------|---------|-------------|
| `RIFT_RUNTIME_MODE` | `process` | `process` (legacy) or `pool` (pre-warmed workers) |
| `RIFT_POOL_WARM_SIZE` | `3` | Number of pre-warmed workers to maintain |
| `RIFT_POOL_MAX_ACTIVE` | `50` | Maximum specialized workers |
| `RIFT_WORKER_MEMORY_LIMIT_MB` | `512` | Per-worker memory limit |

### TLS / ACME

| Variable | Default | Description |
|----------|---------|-------------|
| `RIFT_ACME_EMAIL` | &mdash; | Contact email for Let's Encrypt |
| `RIFT_ACME_STAGING` | `false` | Use ACME staging environment |
| `RIFT_SSL_DIR` | `/var/rift/ssl` | Certificate storage directory |
| `RIFT_COOKIE_SECURE` | `false` | Set `Secure` flag on auth cookies |

### Auth

| Variable | Default | Description |
|----------|---------|-------------|
| `RIFT_ACCESS_TOKEN_TTL_MINUTES` | `15` | JWT access token lifetime |
| `RIFT_REFRESH_TOKEN_TTL_DAYS` | `7` | Refresh token lifetime |

### Paths

| Variable | Default | Description |
|----------|---------|-------------|
| `RIFT_BUILD_ROOT` | `/var/rift/builds` | Temporary build workspace |
| `RIFT_DEPLOY_ROOT` | `/var/rift/deployments` | Deployment bundles directory |

---

## API Reference

### Authentication

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/users/register` | Create a new user |
| `POST` | `/api/users/login` | Authenticate and receive tokens |

### Projects

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/api/projects` | List all projects |
| `POST` | `/api/projects` | Create a new project |
| `GET` | `/api/projects/:id` | Get project details |
| `PATCH` | `/api/projects/:id` | Update project settings |
| `DELETE` | `/api/projects/:id` | Delete a project |

### Deployments

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/api/deployments` | List deployments |
| `POST` | `/api/deployments` | Trigger a new deployment |

### Environment Variables

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/api/env-vars/:project_id` | List env vars for a project |
| `POST` | `/api/env-vars` | Create an env var |
| `DELETE` | `/api/env-vars/:id` | Delete an env var |

### Domains

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/api/domains/:project_id` | List domains for a project |
| `POST` | `/api/domains` | Attach a custom domain |
| `DELETE` | `/api/domains/:id` | Remove a custom domain |

### Webhooks

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/webhooks/github` | GitHub push event receiver |

### Monitoring

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/healthz` | Health check |
| `GET` | `/api/server-info` | Public IP and server info |
| `GET` | `/api/analytics` | Request analytics |
| `GET` | `/api/runtime/stats` | Runtime statistics |
| `GET` | `/api/logs` | Paginated deployment logs |
| `WS` | `/api/ws/logs` | Real-time log stream |

### Firewall

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/api/firewall/:project_id` | List firewall rules |
| `POST` | `/api/firewall` | Create a firewall rule |

---

## Tech Stack

| Category | Crates |
|----------|--------|
| **Async Runtime** | `tokio` (multithreaded) |
| **HTTP Framework** | `axum` 0.8 |
| **Reverse Proxy** | `hyper` 1.x |
| **Database** | `sqlx` 0.8 (PostgreSQL, compile-time checked) |
| **TLS** | `rustls` + `tokio-rustls` |
| **ACME** | `instant-acme` |
| **Auth** | `jsonwebtoken` (EdDSA), `argon2` |
| **Encryption** | `aes-gcm` (AES-256-GCM) |
| **Observability** | `tracing` + `tracing-subscriber` |
| **CLI** | `clap` (derive) |

---

## License

See the [LICENSE](../../LICENSE) file in the repository root for details.
