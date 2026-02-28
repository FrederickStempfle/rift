#!/usr/bin/env bash
set -euo pipefail

mkdir -p /var/rift/builds /var/rift/deployments /var/rift/ssl

# Migrations run during engine startup in db::connect_and_migrate.
exec /usr/local/bin/rift-engine
