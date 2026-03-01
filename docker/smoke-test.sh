#!/usr/bin/env bash
# Docker Compose smoke test for Rift platform.
#
# Usage: ./docker/smoke-test.sh
#
# Prerequisites:
#   - Docker and Docker Compose installed
#   - .env file configured with required secrets (see Infrastructure.md)
#
# This script:
#   1. Builds and starts all services via docker-compose
#   2. Waits for health checks
#   3. Verifies core API endpoints respond
#   4. Tears down on exit (pass --keep to skip teardown)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.yml"
KEEP_RUNNING=false

for arg in "$@"; do
  case $arg in
    --keep) KEEP_RUNNING=true ;;
  esac
done

cleanup() {
  if [ "$KEEP_RUNNING" = false ]; then
    echo "==> Tearing down..."
    docker compose -f "$COMPOSE_FILE" down -v --remove-orphans 2>/dev/null || true
  else
    echo "==> Keeping services running (--keep flag set)"
  fi
}
trap cleanup EXIT

echo "==> Building and starting Rift platform..."
cd "$PROJECT_DIR"
docker compose -f "$COMPOSE_FILE" up --build -d

echo "==> Waiting for database health check..."
for i in $(seq 1 30); do
  if docker compose -f "$COMPOSE_FILE" exec -T db pg_isready -U rift -d rift >/dev/null 2>&1; then
    echo "    Database ready"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "FAIL: Database did not become ready in time"
    docker compose -f "$COMPOSE_FILE" logs db
    exit 1
  fi
  sleep 1
done

echo "==> Waiting for engine API..."
ENGINE_URL="http://localhost:3001"
for i in $(seq 1 60); do
  if curl -sf "$ENGINE_URL/api/health" >/dev/null 2>&1 || \
     curl -sf -o /dev/null -w '%{http_code}' "$ENGINE_URL/api/users/me" 2>/dev/null | grep -qE '401|200'; then
    echo "    Engine API responding"
    break
  fi
  if [ "$i" -eq 60 ]; then
    echo "FAIL: Engine API did not become ready in time"
    docker compose -f "$COMPOSE_FILE" logs engine
    exit 1
  fi
  sleep 2
done

echo "==> Verifying API endpoints..."

# Test 1: Register a user
REGISTER_RESP=$(curl -sf -w '\n%{http_code}' -X POST "$ENGINE_URL/api/users/register" \
  -H 'Content-Type: application/json' \
  -d "{\"email\":\"smoke-test-$(date +%s)@example.com\",\"password\":\"smoketestpassword123\"}" 2>/dev/null || echo "FAILED 000")
REGISTER_STATUS=$(echo "$REGISTER_RESP" | tail -1)
if [ "$REGISTER_STATUS" = "201" ]; then
  echo "    [PASS] User registration"
  ACCESS_TOKEN=$(echo "$REGISTER_RESP" | head -1 | grep -o '"access_token":"[^"]*"' | cut -d'"' -f4)
else
  echo "    [FAIL] User registration (HTTP $REGISTER_STATUS)"
  echo "$REGISTER_RESP"
  exit 1
fi

# Test 2: Get current user
ME_STATUS=$(curl -sf -o /dev/null -w '%{http_code}' "$ENGINE_URL/api/users/me" \
  -H "Authorization: Bearer $ACCESS_TOKEN" 2>/dev/null || echo "000")
if [ "$ME_STATUS" = "200" ]; then
  echo "    [PASS] Get current user"
else
  echo "    [FAIL] Get current user (HTTP $ME_STATUS)"
  exit 1
fi

# Test 3: List projects (should be empty)
PROJECTS_STATUS=$(curl -sf -o /dev/null -w '%{http_code}' "$ENGINE_URL/api/projects/" \
  -H "Authorization: Bearer $ACCESS_TOKEN" 2>/dev/null || echo "000")
if [ "$PROJECTS_STATUS" = "200" ]; then
  echo "    [PASS] List projects"
else
  echo "    [FAIL] List projects (HTTP $PROJECTS_STATUS)"
  exit 1
fi

# Test 4: Proxy responds (should return 404 for unknown host)
PROXY_STATUS=$(curl -sf -o /dev/null -w '%{http_code}' "http://localhost:80" \
  -H "Host: nonexistent.localhost" 2>/dev/null || echo "000")
if echo "$PROXY_STATUS" | grep -qE '404|502|503'; then
  echo "    [PASS] Proxy responds to unknown host"
else
  echo "    [FAIL] Proxy responds to unknown host (HTTP $PROXY_STATUS)"
  exit 1
fi

# Test 5: Check engine security hardening
echo "==> Verifying security hardening..."
SECCOMP_CHECK=$(docker compose -f "$COMPOSE_FILE" exec -T engine cat /var/rift/deployments/rift-worker-seccomp.json 2>/dev/null | head -1 || echo "NOT FOUND")
if echo "$SECCOMP_CHECK" | grep -q "defaultAction"; then
  echo "    [PASS] Seccomp profile deployed"
else
  echo "    [WARN] Seccomp profile not found (may be normal if engine hasn't started pool mode)"
fi

echo ""
echo "=== SMOKE TEST PASSED ==="
echo "All core API endpoints are responding correctly."
