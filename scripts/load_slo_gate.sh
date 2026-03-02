#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SCENARIO="${ROOT_DIR}/load/k6/edge-abuse.js"

if ! command -v k6 >/dev/null 2>&1; then
  echo "k6 not installed. Install from https://k6.io/docs/get-started/installation/" >&2
  exit 1
fi

TARGET_URL="${TARGET_URL:-https://127.0.0.1}"
TARGET_HOST="${TARGET_HOST:-rift.atrainbots.com}"
OUT_JSON="${OUT_JSON:-${ROOT_DIR}/load/k6/last-summary.json}"

mkdir -p "$(dirname "$OUT_JSON")"

set -x
k6 run \
  -e TARGET_URL="$TARGET_URL" \
  -e TARGET_HOST="$TARGET_HOST" \
  --summary-export "$OUT_JSON" \
  "$SCENARIO"
set +x

echo "k6 summary written to $OUT_JSON"
