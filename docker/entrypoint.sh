#!/usr/bin/env bash
set -euo pipefail

mkdir -p /var/rift/builds /var/rift/deployments /var/rift/ssl

# Set up cgroup v2 directory for worker pool resource limits (if available)
if [ -d /sys/fs/cgroup ] && [ -f /sys/fs/cgroup/cgroup.controllers ]; then
  mkdir -p /sys/fs/cgroup/rift/workers 2>/dev/null || true
  # Enable memory, cpu, and pids controllers for worker cgroups
  if [ -f /sys/fs/cgroup/rift/cgroup.subtree_control ]; then
    echo "+memory +cpu +pids" > /sys/fs/cgroup/rift/cgroup.subtree_control 2>/dev/null || true
  fi
  echo "cgroup v2 initialized for worker pool"
else
  echo "cgroup v2 not available, worker resource limits will not be enforced"
fi

# Migrations run during engine startup in db::connect_and_migrate.
exec /usr/local/bin/rift-engine
