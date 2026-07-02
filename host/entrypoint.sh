#!/bin/bash
set -e

# Network isolation config
NETWORK_MODE=${NETWORK_MODE:-offline}

echo "[entrypoint.sh] Configuring network mode: $NETWORK_MODE"

# The sandbox python runs as UID 10001, but nested rootless Podman containers
# run under the mapped subuid range (20000..59999 per /etc/subuid). Egress rules
# that only match UID 10001 would leave nested containers unfiltered, so we apply
# the same policy to both owner specs. Some iptables backends do not support a
# uid range in the owner match; guard those rules so a failure cannot abort
# container startup (set -e is active).
OWNER_SPECS="10001 20000-59999"

block_all_egress() {
  local owner="$1"
  iptables -A OUTPUT -m owner --uid-owner "$owner" -j REJECT 2>/dev/null \
    || echo "[entrypoint.sh] WARN: could not add REJECT rule for owner $owner (unsupported by this iptables backend?)"
}

apply_isolated_egress() {
  local owner="$1"
  # Allow slirp4netns container gateway (10.0.2.0/24) for routing/dns
  iptables -A OUTPUT -m owner --uid-owner "$owner" -d 10.0.2.0/24 -j ACCEPT 2>/dev/null || true
  # Block RFC 1918, loopback, and link-local
  for net in 127.0.0.0/8 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16; do
    iptables -A OUTPUT -m owner --uid-owner "$owner" -d "$net" -j REJECT 2>/dev/null \
      || echo "[entrypoint.sh] WARN: could not add isolated REJECT rule ($net) for owner $owner"
  done
}

if [ "$NETWORK_MODE" = "offline" ]; then
  for spec in $OWNER_SPECS; do block_all_egress "$spec"; done
  echo "[entrypoint.sh] Offline mode: egress blocked for sandbox-user and nested-container subuids."
elif [ "$NETWORK_MODE" = "isolated" ]; then
  for spec in $OWNER_SPECS; do apply_isolated_egress "$spec"; done
  echo "[entrypoint.sh] Isolated network mode configured (RFC1918, Loopback, Link-Local blocked for sandbox-user and nested subuids)."
elif [ "$NETWORK_MODE" = "full" ]; then
  echo "[entrypoint.sh] Full network access granted to sandbox-user."
else
  echo "[entrypoint.sh] Unknown network mode '$NETWORK_MODE'. Defaulting to offline."
  for spec in $OWNER_SPECS; do block_all_egress "$spec"; done
fi

# Ensure /output is writable by sandbox-user
chown -R 10001:10001 /output

# Set XDG_RUNTIME_DIR for rootless podman inside the container
export XDG_RUNTIME_DIR=/run/user/10001
mkdir -p $XDG_RUNTIME_DIR
chown -R 10001:10001 /run/user/10001

# Execute the python script inside namespaces and drop privileges, capturing metrics
# unshare -p -m --fork: PID and Mount namespace isolation.
# --mount-proc: Mounts a private /proc for the process so it cannot see parent processes.
echo "[entrypoint.sh] Launching sandbox process as sandbox-user (UID 10001) with metrics..."
set +e
/usr/bin/time -o /tmp/metrics.json -f "{\"max_memory_kb\": %M, \"cpu_percentage\": \"%P\", \"user_time_sec\": %U, \"sys_time_sec\": %S, \"fs_inputs\": %I, \"fs_outputs\": %O, \"voluntary_context_switches\": %w, \"involuntary_context_switches\": %c}" \
  unshare -p -m --fork --mount-proc gosu sandbox-user python3 /sandbox/run.py
EXIT_CODE=$?
set -e

# Copy the metrics file to /output so the host worker can ingest it
if [ -f /tmp/metrics.json ]; then
  mv /tmp/metrics.json /output/metrics.json
  chown 10001:10001 /output/metrics.json
else
  echo '{"max_memory_kb": 0, "cpu_percentage": "0%", "user_time_sec": 0.0, "sys_time_sec": 0.0, "fs_inputs": 0, "fs_outputs": 0, "voluntary_context_switches": 0, "involuntary_context_switches": 0}' > /output/metrics.json
  chown 10001:10001 /output/metrics.json
fi

exit $EXIT_CODE
