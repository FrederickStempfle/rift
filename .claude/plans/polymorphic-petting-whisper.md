# Plan: `rift` CLI Tool

## Context

Rift has a full REST API but no CLI — users must use `curl` or the frontend to manage projects, trigger deployments, and configure domains. A CLI is the highest-priority gap vs Vercel/Cloudflare. This plan creates a `rift` binary that developers install locally and use to deploy, manage, and monitor their projects from the terminal.

## Architecture

New Rust crate at `apps/cli/`, added to the Cargo workspace. The CLI talks to the engine's REST API over HTTP(S). Auth uses JWT access tokens (auto-refreshed via the internal refresh endpoint). Config and credentials stored in `~/.rift/`.

```
Developer machine                    VPS
┌──────────────┐                ┌──────────────┐
│  rift CLI    │── HTTP(S) ───▶│  Rift Engine  │
│  ~/.rift/    │                │  :3001 API    │
└──────────────┘                └──────────────┘
```

## Command Tree

```
rift login                        # Email/password login (interactive)
rift logout                       # Revoke tokens, clear credentials
rift whoami                       # Show current user

rift projects                     # List all projects (table)
rift projects create              # Create project (interactive or flags)
rift projects info [project]      # Show project details
rift projects delete [project]    # Delete (with confirmation)

rift link                         # Associate cwd with a Rift project (.rift/project.json)
rift unlink                       # Remove link

rift deploy                       # Trigger deployment + stream build logs
rift deployments                  # List deployment history

rift env list                     # List env vars (masked values)
rift env set <KEY> <VALUE>        # Create env var
rift env unset <KEY>              # Delete env var

rift domains list                 # List custom domains
rift domains add <domain>         # Add domain
rift domains remove <domain>      # Remove domain
rift domains verify <domain>      # Verify DNS + trigger SSL

rift logs [deployment_id]         # View logs (latest if omitted)
rift logs --follow                # Stream logs via WebSocket
```

Global flags: `--json` (machine output), `--api-url <URL>` (override), `--project <ID>` (override linked project).

## Auth Flow

1. `rift login` prompts for API URL (first run), internal API token (first run), email, password
2. Calls `POST /api/users/login` — extracts refresh token from `Set-Cookie: rift_refresh_token=...` header
3. Stores access token + refresh token in `~/.rift/credentials.json` (mode 0600)
4. Before each request, checks token expiry. If <60s remaining, calls `POST /api/users/refresh/internal` with `x-rift-internal-token` header
5. On 401/expired refresh: clears credentials, prints "Session expired, run `rift login`"

### File formats

**`~/.rift/config.json`**:
```json
{ "api_url": "https://rift.example.com:3001", "internal_api_token": "..." }
```

**`~/.rift/credentials.json`** (0600 permissions):
```json
{ "access_token": "eyJ...", "refresh_token": "a1b2...", "expires_at": 1709312400, "user": { "id": "...", "email": "..." } }
```

**`.rift/project.json`** (in project directory):
```json
{ "project_id": "uuid", "project_name": "my-app" }
```

## Crate Structure

```
apps/cli/
├── Cargo.toml
└── src/
    ├── main.rs              # Clap CLI definition, command dispatch
    ├── error.rs             # CliError enum (thiserror)
    ├── config.rs            # ~/.rift/config.json load/save
    ├── credentials.rs       # ~/.rift/credentials.json load/save (0600)
    ├── output.rs            # Table formatting, colored status, --json
    ├── client/
    │   ├── mod.rs           # RiftClient: reqwest wrapper, auto-refresh
    │   ├── auth.rs          # login, refresh, logout API calls
    │   ├── projects.rs      # project CRUD API calls
    │   ├── deployments.rs   # deployment API calls
    │   ├── env_vars.rs      # env var API calls
    │   ├── domains.rs       # domain API calls
    │   └── logs.rs          # log fetch + WebSocket streaming
    └── commands/
        ├── mod.rs           # resolve_project_id() helper
        ├── auth.rs          # login, logout, whoami handlers
        ├── projects.rs      # projects list/create/info/delete, link/unlink
        ├── deploy.rs        # deploy (trigger + stream), deployments list
        ├── env.rs           # env list/set/unset
        ├── domains.rs       # domains list/add/remove/verify
        └── logs.rs          # logs (REST) + logs --follow (WebSocket)
```

## Dependencies

```toml
clap = { version = "4", features = ["derive"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4", "serde"] }
colored = "3"
dialoguer = "0.11"
dirs = "6"
thiserror = "2"
url = "2"
futures-util = "0.3"
```

## Key Implementation Details

### RiftClient (`client/mod.rs`)
- Wraps `reqwest::Client` with stored credentials
- `authenticated_request()` method: checks expiry, auto-refreshes, attaches Bearer header
- `check_response()`: parses non-2xx as `{ "error": "..." }` → `CliError::Api`

### Project resolution (`commands/mod.rs`)
Resolution order: `--project` flag → `.rift/project.json` in cwd → error with help message.

### `rift deploy` flow
1. Resolve project ID
2. `POST /api/deployments` → get deployment ID
3. Open WebSocket to `ws(s)://host/api/ws/logs?token=...&deployment_id=...`
4. Stream colored log lines until deployment completes or fails
5. Exit 0 on success, 1 on failure

### Table output (`output.rs`)
Manual column formatter — calculate max widths, pad, colorize status cells. No table library needed.

### Refresh token extraction from login
Parse `Set-Cookie` header to extract `rift_refresh_token` value:
```rust
resp.headers().get_all("set-cookie")
    .iter()
    .find(|s| s.starts_with("rift_refresh_token="))
    // strip prefix, split on ';', take first segment
```

## Files to Modify

| File | Change |
|------|--------|
| `Cargo.toml` (root) | Add `"apps/cli"` to workspace members |

## Implementation Order

1. **Scaffold**: Create crate, Cargo.toml, add to workspace, stub all commands, verify `cargo build -p rift-cli`
2. **Config + credentials**: `config.rs`, `credentials.rs` with load/save/permissions
3. **HTTP client + auth**: `client/mod.rs`, `client/auth.rs`, `commands/auth.rs` — login/logout/whoami working
4. **Projects + linking**: `client/projects.rs`, `commands/projects.rs` — list/create/info/delete/link/unlink
5. **Deploy + logs**: `client/deployments.rs`, `client/logs.rs`, `commands/deploy.rs`, `commands/logs.rs` — deploy with WebSocket streaming
6. **Env vars + domains**: `client/env_vars.rs`, `client/domains.rs`, `commands/env.rs`, `commands/domains.rs`
7. **Output polish**: Table formatting, colors, help text

## Verification

1. `cargo build -p rift-cli` — compiles with no warnings
2. `./target/debug/rift --help` — shows command tree
3. `rift login` → enter credentials → verify `~/.rift/credentials.json` written
4. `rift whoami` → shows user email
5. `rift projects` → lists projects in table
6. `rift link` → select project → verify `.rift/project.json`
7. `rift deploy` → triggers build + streams logs
8. `rift env set FOO bar` → `rift env list` → shows FOO
9. `rift domains add test.com` → `rift domains list` → shows domain
10. `rift logs --follow` → streams WebSocket logs
11. Deploy to VPS: `cargo build --release -p rift-cli`, distribute binary

## Critical Engine Files (for reference during implementation)

- `apps/engine/src/api/users.rs` — Auth response types, cookie handling
- `apps/engine/src/api/projects.rs` — ProjectResponse shape
- `apps/engine/src/api/deployments.rs` — DeploymentResponse shape
- `apps/engine/src/api/env_vars.rs` — EnvVar response shape
- `apps/engine/src/api/domains.rs` — DomainResponse shape
- `apps/engine/src/api/logs.rs` — Log response + WebSocket query params
- `apps/engine/src/ws/handler.rs` — WebSocket protocol details
- `apps/engine/src/config.rs` — internal_api_token config
