# Abuse Gates

## CI Gate Command

Run the abuse protection checks from repository root:

```bash
./scripts/ci_abuse_gates.sh
```

Optional environment variables:

- `TEST_REDIS_URL`: enables Redis persistence test.
- `RUN_LOAD_GATES=1`: enables `k6` load SLO gate.
- `TARGET_URL`: load target URL for `k6` (default `http://127.0.0.1`).
- `TARGET_HOST`: host header used in load tests.
- `BYPASS_TOKEN`: optional trusted bypass token for synthetic load traffic.
- `BYPASS_HEADER`: optional bypass header name (default `x-rift-abuse-bypass`).

## External Load Gate

```bash
TARGET_URL="https://rift.atrainbots.com" TARGET_HOST="rift.atrainbots.com" ./scripts/load_slo_gate.sh
```

The gate fails on SLO threshold violations defined in `load/k6/edge-abuse.js`.
