#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> Abuse unit/integration tests"
cargo test -p rift-engine services::abuse::tests::challenge_then_block_flow
cargo test -p rift-engine services::abuse::tests::allowlist_and_bypass_token_are_trusted
cargo test -p rift-engine services::abuse::tests::challenge_cookie_round_trip_validates
cargo test -p rift-engine services::abuse::tests::redis_fallback_works_for_unreachable_backend

if [[ -n "${TEST_REDIS_URL:-}" ]]; then
  echo "==> Redis persistence test"
  cargo test -p rift-engine services::abuse::tests::redis_backend_persists_ban_when_configured -- --nocapture
else
  echo "==> Skipping Redis persistence test (set TEST_REDIS_URL to enable)"
fi

if [[ "${RUN_LOAD_GATES:-0}" == "1" ]]; then
  echo "==> Running load SLO gate"
  "${ROOT_DIR}/scripts/load_slo_gate.sh"
else
  echo "==> Skipping load gate (set RUN_LOAD_GATES=1 to enable)"
fi
