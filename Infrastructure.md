# Infrastructure

Current state as implemented on March 1, 2026.

This document describes how Rift currently deploys and serves user workloads. It is an implementation document, not a product pitch. Where the code has gaps or mismatches, they are called out explicitly.

## What "serverless" means in this repo

Rift is not serverless as a platform. The platform itself is stateful and always-on:

- PostgreSQL stores projects, deployments, logs, domains, analytics, auth state, and encrypted environment variables.
- A long-lived Rust engine runs the API, reverse proxy, build pipeline, TLS manager, analytics collector, and runtime control plane.
- A long-lived frontend container serves the dashboard.

What is serverless-like is the execution model for user deployments:

- Static and SSR deployments can scale to zero and wake on the next request.
- Function-only deployments are routed through a single global dispatcher that creates a fresh Deno `Worker` per invocation.
- Users do not manage their own public app server or reverse proxy.

In practice, the current implementation is closer to "self-hosted scale-to-zero hosting" than Cloudflare Workers style edge serverless.

## Top-level components

### `db`

Postgres is the source of truth for:

- projects
- deployments
- deploy logs
- domains
- analytics buckets
- users and auth artifacts
- encrypted environment variables
- function route metadata

### `engine`

The Rust engine starts:

- an Axum API server on port `3001`
- an HTTP reverse proxy on `8080`
- an HTTPS reverse proxy on `8443` when TLS is configured
- a background TLS renewal task
- a background scale-to-zero task
- a global function dispatcher when available

The engine also runs DB migrations on startup.

### `frontend`

The dashboard is built as a Next.js standalone app and served by a long-lived Node process on port `3000`.

## Directory and artifact layout

The engine uses these persistent paths:

- `/var/rift/builds`: temporary build workspace root
- `/var/rift/deployments/<deployment_id>`: cloned repo and build output for a specific deployment
- `/var/rift/ssl`: persisted certificates
- `/opt/rift/templates`: TypeScript templates for worker loaders, wrappers, and function dispatchers

Important generated artifacts inside a deployment:

- `_entry.ts`: generated Deno entry for static sites or function dispatchers
- `_rift_pool_entry.ts`: generated wrapper for pool mode SSR serving
- `_rift_functions_output/`: bundled serverless function artifacts
- `_rift_functions_output/_routes.json`: persisted function route manifest
- `_rift_functions_output/_worker_<route>.ts`: per-route worker wrapper
- `_rift_functions_output/bundles/*.js`: esbuild output for functions

## Deployment lifecycle

### 1. Build is queued

Creating a deployment inserts a `deployments` row with status `queued`, then spawns an async build task.

### 2. Repo is cloned into a deployment workspace

The engine clones the selected branch into:

`/var/rift/deployments/<deployment_id>`

If the user has a stored GitHub token, the clone URL is rewritten so private repos can be fetched.

### 3. Framework and package manager are detected

The build detector inspects `package.json`, lockfiles, monorepo layout, and known framework outputs.

Supported primary runtime outputs today:

- Next.js
- Nuxt
- Astro SSR
- SvelteKit SSR
- Remix SSR
- static sites
- function-only projects under `rift/functions`

### 4. User environment variables are decrypted and injected

Environment variables are decrypted from Postgres and provided to install, build, and runtime steps.

### 5. Install and build commands run

The engine runs the chosen install and build commands inside the deployment workspace and streams logs into both Postgres and WebSocket subscribers.

Build concurrency is currently serialized with a semaphore of size `1`.

### 6. Runtime artifacts are generated

Depending on the detected output:

- Static sites get a generated `_entry.ts` that serves files with Deno and does SPA fallback.
- Function-only projects get:
  - route scanning from `rift/functions`
  - esbuild bundling
  - one worker wrapper per route
  - a generated dispatcher `_entry.ts`
  - a persisted `_routes.json`
- Pool mode can generate `_rift_pool_entry.ts` wrappers for SSR frameworks.

### 7. The deployment is launched

This is where current behavior splits by runtime type.

#### Static sites

Static output is served by a Deno process that runs `_entry.ts`.

#### Next.js

The engine runs the Next standalone server with Deno Node compatibility by executing the built `server.js`.

#### Nuxt, Astro, SvelteKit, Remix

These run as Node-style SSR servers. Remix may be wrapped with `remix-serve`; the others are started with `node`.

#### Function-only projects

If the global function dispatcher is running, the project is not given its own runtime port. Instead:

- the generated route manifest is loaded
- routes are registered into the global dispatcher over HTTP
- the deployment URL becomes the dispatcher's loopback URL

If the global dispatcher is unavailable, the code falls back to spawning a per-project Deno function dispatcher process.

### 8. Deployment is marked ready

When launch succeeds:

- the deployment row is marked `ready`
- internal URL and port are stored
- build duration is stored
- older ready deployments for the same project are marked `cancelled`
- old deployment workspaces are removed asynchronously

## Request routing path

Incoming traffic always hits the Rust reverse proxy first.

### 1. Host resolution

The proxy resolves the target project from:

- a custom domain
- or `<subdomain>.<base_domain>`

### 2. Firewall check

The proxy checks the project firewall rules before routing to runtime.

### 3. Runtime lookup

The proxy asks the configured runtime backend for an active URL.

- If a runtime is active, it forwards immediately.
- If not, it tries to wake a suspended deployment.
- If wake fails, the proxy returns `503`.

### 4. Function-only project header injection

If the runtime backend identifies the project as function-only, the proxy adds:

`x-rift-project-id: <project_id>`

The global dispatcher uses that header to select the correct registered route table.

### 5. Forwarding

The proxy forwards the request to the internal loopback runtime URL, strips hop-by-hop headers, and adds forwarding headers.

### 6. Analytics

Each request emits a non-blocking analytics event containing:

- project id
- HTTP status
- request duration
- whether the request was considered a cold start

Those events are aggregated and flushed into hourly Postgres buckets every 10 seconds.

## Runtime backends

There are two backend models in the codebase.

### Process backend

This is the older and currently more complete runtime path.

It tracks:

- `active`: currently running per-project runtimes
- `suspended`: scale-to-zero metadata needed to relaunch

Behavior:

- deploy allocates a loopback port
- the chosen runtime process is spawned
- health is checked before traffic is swapped over
- the old process gets a 5 second drain before being killed
- idle runtimes are killed and moved into `suspended`
- the next request causes a re-spawn via `wake`

This is process-per-project hosting, not per-request isolation.

### Pool backend

This is the newer pre-warmed worker-pool path.

It keeps:

- `warm`: Deno workers with no deployment loaded
- `active`: workers specialized to a project
- `suspended`: deployment metadata used to re-specialize later

Behavior:

- warm Deno workers are pre-spawned in the background
- a deployment acquires a warm worker or spawns one on demand
- the worker receives a POST to `/__rift/specialize`
- the worker dynamically imports the deployment bundle and starts serving requests
- idle active workers are killed and the deployment is moved to `suspended`
- the next request re-specializes a warm worker

This reduces startup overhead relative to process-per-project serving, but it is still not Cloudflare-style per-request execution for SSR apps.

## How scale-to-zero works

Scale-to-zero is implemented by the engine, not by the runtime process itself.

### Timing

- scaler starts 60 seconds after engine boot
- it checks every 30 seconds
- deployments idle for more than 5 minutes are suspended

### Suspend

For process mode:

- the active child process is killed
- deployment id, runtime kind, and env vars are stored in memory

For pool mode:

- the specialized worker is killed
- deployment id, runtime kind, env vars, and bundle path are stored in memory
- the warm pool is replenished

### Wake

On the next request:

- the proxy sees no active runtime
- it calls `wake(project_id)`
- the backend relaunches the runtime or re-specializes a worker
- the request is then forwarded

Cold starts are therefore request-triggered, not queue-triggered.

## Function-only serverless model

Function-only projects are the closest thing in the repo to true serverless.

### Build-time shape

The build system scans `rift/functions` recursively.

Examples:

- `rift/functions/api/hello.ts` becomes `/api/hello`
- `rift/functions/api/users/[id].ts` becomes `/api/users/:id`
- `rift/functions/index.ts` becomes `/`

Each function file is:

- bundled with `esbuild`
- wrapped in its own Deno `Worker` entry file
- registered into a route table

### Runtime shape

A single always-running Deno process acts as the global function dispatcher.

For each incoming function request:

- the project is selected by `x-rift-project-id`
- the route table is matched with `URLPattern`
- route params are copied into headers as `x-rift-param-<name>`
- the request body is materialized in memory
- a new Deno `Worker` is created for the matched route
- project env vars are injected inside that worker
- the bundled handler is invoked
- the worker is terminated after the response or timeout

### Handler conventions

Function workers support these exports:

- `export default { fetch(req) {} }`
- `export default function handler(req) {}`
- `export function fetch(req) {}`
- `export function handler(req) {}`

### Timeout and concurrency

Current behavior:

- timeout is 30 seconds per invocation
- concurrency is bounded per route by `RIFT_MAX_CONCURRENT`, default `50`
- excess requests get `429`

### Isolation properties

Good:

- each invocation gets a fresh `Worker`
- no JS heap is shared across requests
- route-level concurrency is tracked

Important caveat:

- worker permissions are `inherit`, so the isolate inherits the dispatcher's Deno permissions instead of getting a tighter per-function permission set

## Static and SSR serverless model

Static and SSR apps are "serverless" only in the sense that they can scale to zero and be woken automatically.

### Static sites

Static sites get a generated Deno file server entry. The runtime is a long-lived Deno HTTP server while active.

### Next.js

Next is executed through Deno with Node compatibility against the standalone output.

### Nuxt, Astro, SvelteKit, Remix

These run as long-lived Node-style server processes while active.

### Consequence

For these deployment types:

- one project maps to one live runtime while warm
- requests are forwarded into that live runtime
- the runtime is reused across requests until suspended

That is not per-request serverless execution. It is reusable process or reusable worker hosting with scale-to-zero.

## Restore behavior after engine restart

### Process backend

On engine startup:

- the engine queries the latest `ready` deployment per project
- it infers runtime kind from on-disk artifacts
- it decrypts env vars
- it eagerly relaunches the runtime

Function-only projects are restored by re-registering routes into the global dispatcher.

### Pool backend

On engine startup:

- the engine queries the latest `ready` deployment per project
- it infers runtime kind from on-disk artifacts
- it decrypts env vars
- it stores the deployment as suspended

So pool mode restore is lazy. The runtime is only materialized on the first incoming request.

## Isolation and resource controls

### Process mode permissions

Process mode uses Deno permission flags where applicable:

- static sites get narrow `--allow-net=0.0.0.0:<port>` and read access to their output dir
- Next and function dispatchers get broader permissions
- Node SSR processes run with normal Node process privileges inside the engine container

### Pool mode controls

Pool workers run as Deno processes and can be placed into cgroups v2.

Current cgroup limits include:

- hard memory cap
- memory high watermark
- CPU quota
- PID count cap

The engine mounts the cgroup filesystem into the container and sets up the base worker cgroup directory at startup.

### Seccomp

There is a seccomp profile in the repo for worker processes, but it is not currently attached during worker spawn. At the moment it is defined and tested, not enforced in the runtime launch path.

## Storage and state

Runtime state is split between memory, disk, and Postgres.

### In memory

- active runtime maps
- suspended runtime maps
- function registry contents
- warm worker pool contents
- analytics aggregation buffer

This state is lost when the engine restarts.

### On disk

- deployment workspaces and compiled output
- generated Deno entry points and worker wrappers
- function route manifest `_routes.json`
- TLS certificates

### In Postgres

- deployment history
- logs
- domains
- analytics aggregates
- user secrets and metadata

## Resolved caveats

These were previously architectural gaps. They have been resolved.

### 1. Build launch path now uses the selected runtime backend (resolved)

`BuildManager` holds `Arc<dyn RuntimeBackend>` and calls `runtime_backend.deploy(...)` at the end of a build. This means:

- initial deployment launch uses the same backend as proxy, scaler, and restore
- pool mode is fully authoritative when configured — deploy, wake, suspend, stop, and restore all go through `PoolBackend`
- `RuntimeManager` is no longer exposed outside of `ProcessBackend`; it is not in `AppState`

### 2. Function+framework combined entry is wired into runtime launch (resolved)

When a project has both framework output and `rift/functions`, the build generates `_rift_combined_entry.ts` and swaps the runtime kind to `RuntimeKind::Combined`. The combined entry:

- dispatches function route requests to per-request Web Worker isolates
- falls through to the framework handler for non-matching requests
- is launched as the sole Deno process for that deployment

The restore path detects combined entries from the filesystem and restores them correctly.

### 3. `max_active_workers` is enforced (resolved)

`WorkerPool::deploy()` now rejects new deployments when the pool is at capacity (`active.len() >= config.max_active_workers`). Re-deployments of an already-active project are allowed (they replace the existing worker, not add a new one).

`idle_timeout` in `PoolConfig` is still set but the scaler controls the actual idle threshold (5 minutes). The config value is available for future per-pool override.

## Current implementation caveats

These describe the code as it exists today.

### 1. Seccomp is defined but not applied at runtime

The seccomp profile exists in `runtime/pool/sandbox.rs` and is tested. However, it is not attached during worker spawn.

To enable it:

- Docker-level: `--security-opt seccomp=<path>` using the profile written by `write_seccomp_profile()`
- Process-level: call `seccomp()` syscall before exec in the worker spawn path

Until one of these is implemented, worker isolation relies on Deno permission flags, cgroup limits, and the restricted Worker permission set.

### 2. Function dispatcher "cold starts" count every invocation

The global function dispatcher increments its `totalColdStarts` counter every time it creates a new `Worker`. That is consistent with "fresh isolate per request", but it does not distinguish between platform cold starts and normal invocation startup.

### 3. Deployment workspaces are still mutable

Deployments execute from the cloned workspace directory (`/var/rift/deployments/<id>/`), which contains source code, `node_modules`, build caches, and generated artifacts. This is not immutable.

As a first step, each deployment now writes `_rift_manifest.json` after build, listing:

- runtime type
- entry point path
- functions output directory (if any)

This manifest establishes the boundary between "build workspace" and "runtime artifact". A future step can use it to copy only the listed files to an immutable read-only artifact directory and mount that for execution.

## Function isolation model

### Worker permissions (tightened)

Function Workers in both the global dispatcher and combined entries now run with restricted Deno permissions:

- `net: true` — functions need outbound HTTP access
- `read: true` — functions need to read their bundle files
- `env: true` — functions receive project environment variables
- `write: false` — no filesystem mutation
- `run: false` — no subprocess spawning
- `ffi: false` — no foreign function interface
- `sys: false` — no system information access

Previously, Workers used `permissions: "inherit"`, which gave function isolates the full dispatcher permissions including write, run, and sys.

### Pool backend function handling

`PoolBackend` now carries the `FunctionRegistry` and handles function-only projects the same way as `ProcessBackend`: by delegating to the global function dispatcher rather than trying to specialize a pool worker.

## Summary

Rift now implements four distinct execution patterns:

- Function-only projects: closest to serverless, using a persistent global dispatcher and a fresh Deno `Worker` per request with restricted permissions.
- Hybrid framework + functions projects: a single Deno entry dispatches function requests to per-request Workers and falls through to the framework handler for everything else.
- Static and SSR projects in process mode: one active runtime per project, suspended after inactivity, re-spawned on the next request.
- Static and SSR projects in pool mode: one specialized warm worker per active project, suspended after inactivity, re-specialized on the next request. Pool capacity is enforced.

The platform is:

- serverful at the control-plane level
- scale-to-zero for user app runtimes
- per-request isolated for function routes (both standalone and hybrid)
- unified runtime control: build, proxy, scaler, and restore all use `RuntimeBackend`
- moving toward immutable artifacts (manifest written, execution not yet decoupled)
