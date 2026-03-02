#!/usr/bin/env bash
set -euo pipefail

mkdir -p /var/rift/builds /var/rift/deployments /var/rift/ssl /var/rift/cache

# ─── Kernel network hardening (sysctl) ───────────────────────────────
# These are best-effort — some may fail in unprivileged containers
apply_sysctl() {
  local key="$1" val="$2"
  local path="/proc/sys/${key//./\/}"
  if [ -w "$path" ]; then
    printf '%s' "$val" > "$path" 2>/dev/null || true
  fi
}

# Disable IP forwarding (workers should not route traffic)
apply_sysctl net.ipv4.ip_forward 0

# Ignore ICMP redirects (prevent MITM attacks)
apply_sysctl net.ipv4.conf.all.accept_redirects 0
apply_sysctl net.ipv4.conf.default.accept_redirects 0
apply_sysctl net.ipv4.conf.all.send_redirects 0

# Enable SYN cookies (SYN flood protection)
apply_sysctl net.ipv4.tcp_syncookies 1

# Ignore source-routed packets
apply_sysctl net.ipv4.conf.all.accept_source_route 0
apply_sysctl net.ipv4.conf.default.accept_source_route 0

# Enable reverse path filtering (anti-spoofing)
apply_sysctl net.ipv4.conf.all.rp_filter 1
apply_sysctl net.ipv4.conf.default.rp_filter 1

# Log martian packets (unexpected source addresses)
apply_sysctl net.ipv4.conf.all.log_martians 1

# Ignore ICMP broadcast requests (Smurf attack prevention)
apply_sysctl net.ipv4.icmp_echo_ignore_broadcasts 1

# Reduce TIME_WAIT duration for faster port reuse
apply_sysctl net.ipv4.tcp_fin_timeout 15

# Increase connection tracking limits
apply_sysctl net.netfilter.nf_conntrack_max 131072 2>/dev/null || true

echo "kernel network hardening applied"

# ─── iptables firewall rules ──────────────────────────────────────────
# Only apply if iptables is available and we have NET_ADMIN capability
if command -v iptables >/dev/null 2>&1; then
  # Flush existing rules
  iptables -F 2>/dev/null || true
  iptables -X 2>/dev/null || true

  # Default policies: allow outbound, drop unsolicited inbound
  iptables -P INPUT DROP 2>/dev/null || true
  iptables -P FORWARD DROP 2>/dev/null || true
  iptables -P OUTPUT ACCEPT 2>/dev/null || true

  # Allow loopback traffic (engine <-> workers)
  iptables -A INPUT -i lo -j ACCEPT 2>/dev/null || true

  # Allow established/related connections
  iptables -A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true

  # Allow API port (3001) from anywhere (Docker will handle external routing)
  iptables -A INPUT -p tcp --dport 3001 -j ACCEPT 2>/dev/null || true

  # Allow HTTP proxy port (8080) from anywhere
  iptables -A INPUT -p tcp --dport 8080 -j ACCEPT 2>/dev/null || true

  # Allow HTTPS proxy port (8443)
  iptables -A INPUT -p tcp --dport 8443 -j ACCEPT 2>/dev/null || true

  # Drop obvious L4 floods before app-layer rate limits.
  iptables -A INPUT -p tcp --syn --dport 8080 -m connlimit --connlimit-above 200 --connlimit-mask 32 -j DROP 2>/dev/null || true
  iptables -A INPUT -p tcp --syn --dport 8443 -m connlimit --connlimit-above 200 --connlimit-mask 32 -j DROP 2>/dev/null || true
  iptables -A INPUT -p tcp --dport 8080 -m hashlimit --hashlimit-name rift_http --hashlimit-above 1000/second --hashlimit-burst 2000 --hashlimit-mode srcip --hashlimit-srcmask 32 -j DROP 2>/dev/null || true
  iptables -A INPUT -p tcp --dport 8443 -m hashlimit --hashlimit-name rift_https --hashlimit-above 1000/second --hashlimit-burst 2000 --hashlimit-mode srcip --hashlimit-srcmask 32 -j DROP 2>/dev/null || true

  # Allow worker port range (10000-10100) only from localhost
  iptables -A INPUT -p tcp --dport 10000:10100 -s 127.0.0.0/8 -j ACCEPT 2>/dev/null || true

  # Drop everything else with rate-limited logging
  iptables -A INPUT -m limit --limit 5/min -j LOG --log-prefix "rift-dropped: " 2>/dev/null || true
  iptables -A INPUT -j DROP 2>/dev/null || true

  # Worker output restrictions: block workers from accessing internal services
  # Workers (port range 10000-10100) should not reach the API port
  iptables -A OUTPUT -p tcp -s 127.0.0.1 --sport 10000:10100 --dport 3001 -j DROP 2>/dev/null || true

  echo "iptables firewall rules applied"
else
  echo "iptables not available, skipping host firewall rules"
fi

# ─── cgroup v2 initialization ─────────────────────────────────────────
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
