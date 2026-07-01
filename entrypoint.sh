#!/bin/bash
set -e

# Network isolation config
NETWORK_MODE=${NETWORK_MODE:-offline}

echo "[entrypoint.sh] Configuring network mode: $NETWORK_MODE"

if [ "$NETWORK_MODE" = "offline" ]; then
  # Block all egress for sandbox-user (UID 10001)
  iptables -A OUTPUT -m owner --uid-owner 10001 -j REJECT
elif [ "$NETWORK_MODE" = "isolated" ]; then
  # Allow slirp4netns container gateway (10.0.2.0/24) for routing/dns
  iptables -A OUTPUT -m owner --uid-owner 10001 -d 10.0.2.0/24 -j ACCEPT
  # Block RFC 1918, loopback, and link-local for sandbox-user
  iptables -A OUTPUT -m owner --uid-owner 10001 -d 127.0.0.0/8 -j REJECT
  iptables -A OUTPUT -m owner --uid-owner 10001 -d 10.0.0.0/8 -j REJECT
  iptables -A OUTPUT -m owner --uid-owner 10001 -d 172.16.0.0/12 -j REJECT
  iptables -A OUTPUT -m owner --uid-owner 10001 -d 192.168.0.0/16 -j REJECT
  iptables -A OUTPUT -m owner --uid-owner 10001 -d 169.254.0.0/16 -j REJECT
  echo "[entrypoint.sh] Isolated network mode configured (RFC1918, Loopback, Link-Local blocked)."
elif [ "$NETWORK_MODE" = "full" ]; then
  echo "[entrypoint.sh] Full network access granted to sandbox-user."
else
  echo "[entrypoint.sh] Unknown network mode '$NETWORK_MODE'. Defaulting to offline."
  iptables -A OUTPUT -m owner --uid-owner 10001 -j REJECT
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
