# Rift Engine Operational Runbooks

## 1. Redis Outage / Degradation

### Symptoms
- Alert: `redis_disconnect_loop`
- Alert: `heartbeat_all_failing`
- Heartbeat errors rising: `rift_heartbeat_send_total{outcome="error"}`
- Cross-node routing stale (requests hitting wrong node or 404)

### Impact
- Multi-node scheduling degraded: new deployments may fail to place
- Routing cache invalidation stops: route changes not propagated across nodes
- Worker heartbeats stop: scheduler sees stale worker capacities

### Single-node mode is unaffected
If `RIFT_STATE_STORE=local`, Redis is not used. Skip this runbook.

### Diagnosis
1. Check Redis connectivity: `redis-cli -u $RIFT_REDIS_URL ping`
2. Check Redis memory: `redis-cli info memory`
3. Check network: verify engine nodes can reach Redis host/port
4. Check `/metrics` for `rift_heartbeat_send_total` error rate

### Resolution
1. **Redis down**: restart Redis. Engine auto-reconnects.
2. **Redis full**: check `maxmemory` config, evict if needed.
3. **Network partition**: fix routing/firewall rules between engine and Redis.
4. **Persistent disconnects**: check Redis `timeout` config (should be 0 for pub/sub).

### Fallback
Switch to local mode temporarily: set `RIFT_STATE_STORE=local` and restart engine. This disables multi-node features but keeps single-node operation healthy.

---

## 2. Stale Routing / Cross-Node Mismatch

### Symptoms
- Requests returning 404 for recently deployed projects
- Requests hitting wrong worker node
- Routing cache hit rate dropping: `rift_routing_cache_result_total{result="miss"}`

### Diagnosis
1. Check `/api/runtime?project_id=...` to confirm runtime is active
2. Check routing cache state in metrics
3. Check Redis pub/sub subscriber is connected (logs for "routing subscriber connected")
4. Verify custom domain DNS points to correct engine instance

### Resolution
1. **Cache stale**: cache entries expire after 60s (positive) / 5s (negative). Wait or restart engine to clear.
2. **Pub/sub disconnected**: subscriber auto-reconnects with 5s backoff. Check Redis connectivity.
3. **DNS mismatch**: verify `RIFT_BASE_DOMAIN` matches actual domain. Custom domains must be registered via API.
4. **Manual invalidation**: redeploy the project or change its subdomain to force cache clear.

---

## 3. Runaway Tenant Resource Usage

### Symptoms
- Alert: `resource_violations_elevated`
- Worker processes consuming excessive CPU/memory
- OOM kills in worker cgroup (check `dmesg`)
- Other tenants experiencing latency

### Diagnosis
1. Check cgroup metrics: `cat /sys/fs/cgroup/rift_worker_*/memory.current`
2. Check `rift_resource_violation_total` for which violation types
3. Identify project: check active workers and their project assignments
4. Check enforcement mode: `RIFT_RESOURCE_ENFORCEMENT` (strict vs best-effort)

### Resolution
1. **Stop the project**: `POST /api/projects/{id}/stop`
2. **Tighten limits**: set per-project overrides or reduce global defaults
3. **Switch to strict mode**: set `RIFT_RESOURCE_ENFORCEMENT=strict` (restarts all workers)
4. **Verify cgroups**: ensure cgroup v2 is mounted and engine has write access to `/sys/fs/cgroup/`

### Prevention
- Set `RIFT_RESOURCE_ENFORCEMENT=strict` in production
- Review and test `RIFT_WORKER_MEMORY_LIMIT_MB` against expected workloads
- Monitor `rift_pool_active_workers` vs `max_active` capacity

---

## 4. Cold Start Latency Spikes

### Symptoms
- Alert: `cold_start_p95_high` or `cold_start_p95_critical`
- Users reporting slow first-request times
- `rift_cold_start_duration_seconds` p95 > 3s

### Diagnosis
1. Check `rift_cold_start_duration_seconds` histogram for latency distribution
2. Check pool stats: `rift_pool_warm_workers` — are warm workers available?
3. Check `rift_pool_suspended_deployments` — too many suspended?
4. Check build artifact sizes (large artifacts = slow wake)
5. Check host IO: `iostat` — disk bottleneck during wake?

### Resolution
1. **Increase warm pool**: raise `RIFT_WARM_POOL_SIZE` for more pre-warmed workers
2. **Increase idle timeout**: raise `RIFT_IDLE_TIMEOUT_SECS` to reduce suspensions
3. **Reduce artifact size**: enable immutable artifacts to trim unnecessary files
4. **Check IO**: move deploy_root to SSD/tmpfs if on spinning disk
5. **Preemptive wake**: consider periodic health checks to keep hot projects warm

### Thresholds
| Percentile | Target | Warning | Critical |
|------------|--------|---------|----------|
| p50 | < 500ms | - | - |
| p95 | < 3s | > 3s | > 10s |
| p99 | < 5s | - | > 15s |

---

## 5. Build Failures / Timeouts

### Symptoms
- Alert: `deploy_success_rate_low`
- `rift_deploy_outcome_total{outcome="failed"}` or `{outcome="timeout"}` rising
- `rift_build_queue_depth` staying high

### Diagnosis
1. Check deployment logs via WebSocket or API
2. Check `rift_deploy_stage_duration_seconds` — which stage is slow?
3. Check `rift_build_queue_depth` — builds queuing?
4. Check disk space on build root
5. Check network: can engine clone from GitHub?

### Resolution
1. **Timeout**: increase `RIFT_BUILD_TIMEOUT_SECS` if builds are legitimately large
2. **Queue depth**: increase `RIFT_BUILD_CONCURRENCY` if CPU/memory allows
3. **Disk full**: clean old deployments, increase disk, or move build root
4. **GitHub auth**: check `github_token` is valid for private repos
5. **OOM in build**: increase `RIFT_BUILD_MEMORY_LIMIT_MB`

---

## 6. Scheduler Placement Failures

### Symptoms
- Alert: `scheduler_placement_failures`
- `rift_scheduler_placement_total{outcome="failed"}` rising
- New deployments failing with "placement lease already held"

### Diagnosis
1. Check worker count: `rift_heartbeat_send_total{outcome="ok"}` — are workers sending heartbeats?
2. Check worker capacity: all workers at max active runtimes?
3. Check lease state in Redis: `redis-cli GET rift:placement:{project_id}`
4. Check for stale leases (TTL expired but not cleaned)

### Resolution
1. **Capacity exhausted**: add more worker nodes or increase `RIFT_MAX_ACTIVE_WORKERS`
2. **Stale leases**: leases have 300s TTL; wait for expiry or manually delete in Redis
3. **No workers**: ensure heartbeat task is running on all engine nodes
4. **Single-node**: scheduler falls back to self-placement; this error means the local node is also full
