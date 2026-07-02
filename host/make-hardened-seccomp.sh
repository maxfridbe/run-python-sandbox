#!/bin/bash
# Regenerates host/seccomp-hardened.json from the container runtime's default
# seccomp profile.
#
# Why this exists: the outer sandbox container is launched with
# --cap-add=SYS_ADMIN because nested rootless Podman (newuidmap + crun mounting
# a private /proc/sysfs for the inner container) genuinely requires it — dropping
# SYS_ADMIN breaks nested containers (newuidmap "write to uid_map failed") and the
# private /proc remount (locked masked-mounts). See HARDENING.md.
#
# seccomp filters cannot inspect capabilities, so Podman *compiles in* the
# CAP_SYS_ADMIN-gated ALLOW rules only because SYS_ADMIN is present. That is what
# re-enables bpf, perf_event_open, and friends. This script derives a profile that
# removes those dangerous syscalls from every ALLOW rule while leaving the
# mount/namespace syscalls nested Podman needs untouched, shrinking what SYS_ADMIN
# actually unlocks.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE="${IMAGE:-localhost/run-python-sandbox:latest}"
OUT="${1:-$SCRIPT_DIR/seccomp-hardened.json}"

# Syscalls to deny even though CAP_SYS_ADMIN would normally re-enable them. None
# are needed by the Python sandbox or by nested rootless Podman.
DANGER='["bpf","perf_event_open","lookup_dcookie","fanotify_init","quotactl","kexec_load","kexec_file_load"]'

# 1. Obtain a base profile: prefer the host's containers default, else extract
#    the one shipped inside the image.
BASE=""
if [ -f /usr/share/containers/seccomp.json ]; then
  BASE=/usr/share/containers/seccomp.json
else
  TMP_BASE="$(mktemp)"
  cid="$(podman create "$IMAGE")"
  podman cp "$cid":/usr/share/containers/seccomp.json "$TMP_BASE"
  podman rm "$cid" >/dev/null
  BASE="$TMP_BASE"
fi

# 2. Strip the dangerous syscalls from every ALLOW rule and prune emptied rules.
jq --argjson danger "$DANGER" '
  .syscalls |= map(
    if (.action=="SCMP_ACT_ALLOW") then
      .names |= map(select(. as $n | ($danger | index($n)) | not))
    else . end
  )
  | .syscalls |= map(select((.names|length) > 0))
' "$BASE" > "$OUT"

echo "Wrote hardened seccomp profile: $OUT ($(wc -c < "$OUT") bytes)"
echo "Denied (SYS_ADMIN-gated) syscalls: $(echo "$DANGER" | jq -r 'join(", ")')"
