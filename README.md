# run-python-sandbox

A secure, rootless Podman-in-Podman container designed to execute untrusted Python code and run "offensive" nested Podman containers safely. It implements the **Unix UID-in-container** security model to provide double-sandboxing, process-level isolation, and configurable resource allocation limits. Pre-installed tools inside the container include `reportlab` (PDF generation), `Pillow` (image editing), `pypdf`, and `poppler-utils` (`pdftoppm` CLI utility for high-fidelity PDF-to-TIFF page rendering).

This implementation is based on the security models and concepts explored in the article: [Unix solved this satisfactorily in 1971. We took two days to figure that out.](https://www.ikangai.com/unix-solved-this-satisfactorily-in-1971-we-took-two-days-to-figure-that-out/)

![Interactive Web Dashboard - Execution Run](media/screenshot.jpg)
![Interactive Web Dashboard - Main Interface](media/dashboard.jpg)

---

## Key Features
* **Rootless Host Run**: Fully unprivileged execution on the host machine.
* **Unix UID Isolation**: Executes untrusted code under dynamically provisioned low-privileged UIDs (`10000–65000`) inside the container.
* **Namespace Sandboxing**: Isolates Mount and PID namespaces per execution using `unshare`.
* **Optional Hardened Seccomp**: A generated seccomp profile denies the dangerous syscalls (`bpf`, `perf_event_open`, etc.) that `--cap-add=SYS_ADMIN` would otherwise re-enable, without breaking nested rootless Podman.
* **Safe Nested Podman**: Spawns nested rootless containers inside the sandbox using Podman's VFS storage driver.
* **Locked Egress Control**: Hardcoded network-disabled (`offline`) policies on API workers and UI for ultimate security compliance.
* **Input Directory Mounting**: Upload files in the UI or pass them in the API to mount them as a read-only volume inside the container at `/input/`.
* **Standard Core Windows/macOS Fonts**: Equips the container with Microsoft core fonts (Arial, Times, Courier, etc.), ChromeOS core/extra fonts (Arimo, Tinos, Cousine, Carlito, Caladea), and DejaVu/URW-base35 fonts for high-fidelity text rendering in PDFs and images.
* **Request GUID Tracing**: Assigns a unique trace GUID to every script run, standardizing temporary host directories to `/tmp/sandbox-out-[guid]` and `/tmp/sandbox-in-[guid]`.
* **Hardware-Independent Performance Metrics**: Captures scheduler context switches and filesystem I/O metrics for node-independent codebase comparisons.
* **Interactive Monaco Web UI**: Pastable Python editor with dynamic module autocomplete, file upload input zone, live output previews (images, text, PDF rendering via PDF.js, and TIFF rendering via LibTiff WebAssembly), and configurable CPU and RAM limit controls.

---

## Quick Start

### 1. Build the Container
Run the build script to compile the container image:
```bash
./build.sh
```

### 2. Run the Interactive Web Frontend
Launch the interactive web interface (defaults to the Go backend on port 8080):
```bash
./test_interactive.sh
```
Open your browser at [http://localhost:8080](http://localhost:8080) to paste scripts, trigger sandbox runs, customize resource limits, and preview generated files.

### 3. Run the Worker via CLI
You can execute Python files using the host-side worker script directly:
```bash
python3 worker.py --run_py path/to/script.py --network offline --output_dir ./my_outputs --cpus 1.0 --memory_mb 2048 --timeout 60 --hardened
```
`--hardened` applies the bundled seccomp profile (`host/seccomp-hardened.json`); `--timeout` and `--pids_limit` bound runtime and process count. Use `--seccomp <path>` to supply your own profile.

---

## Isolation Strategy

### The whole picture

Untrusted Python is treated as hostile. No single mechanism is trusted to contain it; instead the design is **defense in depth** — several independent layers, each of which must be defeated for an escape to matter. The threats we care about are:

1. **Escape to the host** — breaking out of the container to run code as the host user or root.
2. **Privilege escalation inside the container** — becoming a more capable user/process than the sandbox account.
3. **Lateral / network abuse** — reaching the host's localhost services, private networks, or cloud metadata endpoints.
4. **Noisy neighbor / denial of service** — exhausting CPU, memory, PIDs, disk, or the request-handling server itself.
5. **Information disclosure** — reading host files or other executions' data through shared paths.

The layers, from outermost to innermost:

| Layer | Mechanism | Primary threat it addresses |
| --- | --- | --- |
| 1. Rootless host Podman | outer container run by an unprivileged host user | Escape to host root |
| 2. In-container UID drop | `gosu` to `sandbox-user` (UID 10001) | Privilege escalation |
| 3. Namespaces | `unshare -p -m --fork --mount-proc` | Process/mount visibility |
| 4. Capabilities | only `NET_ADMIN`, `NET_RAW`, `SYS_ADMIN` added | Kernel attack surface |
| 5. Seccomp | default profile, plus optional hardened profile | Kernel attack surface |
| 6. Nested rootless Podman (VFS) | inner containers with no host devices | Escape via storage/devices |
| 7. Egress control | `iptables` owner-match on UID + subuid range | Network abuse |
| 8. Filesystem scoping | read-only script/input, per-run `/output` | Information disclosure |
| 9. Resource & availability guards | cpu/mem/pids/timeout/concurrency caps | Noisy neighbor / DoS |

The load-bearing boundary is **Layer 1**: because the whole stack runs inside a *rootless* Podman container, "root" anywhere inside it is only ever the unprivileged host user mapped through a user namespace. Every other layer exists to raise the cost of getting even that far, and to contain the blast radius if a layer fails.

### Layer by layer

**1. Rootless host Podman (the outer sandbox).**
The `podman run` that launches everything is executed by an ordinary, unprivileged host user — never root. The kernel maps the container's UIDs to a range of unprivileged host subuids via a user namespace. *Why it's necessary:* it makes a full container escape land the attacker on a powerless host account instead of root. Do **not** run the host process as root; that would collapse this layer. The servers additionally default to binding `127.0.0.1` so the endpoint is not exposed to the network without an explicit choice.

**2. Unprivileged in-container UID (`sandbox-user`, UID 10001).**
`entrypoint.sh` starts as the container's root only long enough to configure `iptables`, then uses `gosu` to drop to UID 10001 before running any untrusted code. *Why it's necessary:* untrusted code should never execute with the container's capabilities or as UID 0, even inside the namespace. The image pre-creates this low, fixed UID and owns `/sandbox` and `/output` to it.

**3. PID and mount namespaces with a private `/proc` (`unshare -p -m --fork --mount-proc`).**
The sandboxed process is launched in a fresh PID namespace and mount namespace, with a freshly mounted `/proc`. *Why it's necessary:* without a private `/proc`, the untrusted process could enumerate and `/proc`-inspect the entrypoint and metrics helper processes; the new PID namespace means it sees essentially only itself. This is what `test_isolation.py` asserts (a very small visible PID count).

**4. Linux capabilities (only what nesting requires).**
The container drops all capabilities except three that are explicitly added:
* `NET_ADMIN` — lets `entrypoint.sh` install the `iptables` egress rules inside the container's network namespace.
* `NET_RAW` — allows raw sockets so tools like `ping`/`nmap` work in the "offensive nested container" use case.
* `SYS_ADMIN` — required for nested rootless Podman and the inner `unshare -m`.

*Why `SYS_ADMIN` is necessary (and can't simply be dropped):* it was verified empirically. Removing it breaks the sandbox in two independent ways — the inner `unshare -m --mount-proc` fails with `Operation not permitted` (mounting a private `/proc` is refused from an unprivileged nested user namespace because the runtime's masked `/proc` mounts are *locked*), and nested Podman fails at `newuidmap: write to uid_map failed: Operation not permitted`. `SYS_ADMIN` is broad, so Layer 5 narrows what it actually unlocks.

**5. Seccomp system-call filtering.**
By default the container keeps Podman's built-in seccomp profile (it is **not** run `unconfined`). An optional **hardened profile** (`host/seccomp-hardened.json`, generated by `host/make-hardened-seccomp.sh`) goes further. *Why it's necessary:* seccomp cannot inspect capabilities, so Podman *compiles in* the `CAP_SYS_ADMIN`-gated ALLOW rules purely because `SYS_ADMIN` is present — which silently re-enables `bpf`, `perf_event_open`, `lookup_dcookie`, `fanotify_init`, and `quotactl`. The hardened profile removes those from every ALLOW rule (they fall through to the default `ENOSYS` deny) while leaving the `mount`/`unshare`/`clone`/`pivot_root`/`setns`/`open_tree`/`move_mount`/`fsopen` syscalls that nested rootless Podman genuinely needs. Enable it with `worker.py --hardened`, or `SANDBOX_HARDENED=1` / `SANDBOX_SECCOMP=<path>` on the HTTP services. It is verified by `host/tests/test_seccomp.py`, which confirms `bpf` is denied under the profile but the sandbox still runs.

**6. Nested rootless Podman with VFS storage.**
Inside the sandbox, `sandbox-user` runs further rootless Podman containers mapped to subuids (`/etc/subuid` → `20000:40000`). Storage uses the `vfs` driver (`storage.conf`). *Why it's necessary:* VFS keeps all inner-container storage in user-space directories, so nesting works **without** mounting dangerous host devices such as `/dev/fuse` and without overlay mounts that would need extra privileges. It trades disk efficiency for a smaller device/host attack surface.

**7. Network egress control (`iptables` owner-match).**
`entrypoint.sh` applies egress policy keyed on the process owner, for both the sandbox UID (`10001`) **and** the nested-container subuid range (`20000–59999`):
* `offline` (default, and forced by the Go/Rust/.NET services) — all egress rejected.
* `isolated` — public internet allowed, but loopback, RFC 1918 private ranges, and link-local/metadata (`169.254.0.0/16`) rejected.
* `full` — unrestricted (local, trusted testing only, via `worker.py`).

*Why the subuid range matters:* nested containers run as mapped subuids, not UID 10001, so a rule matching only 10001 would let nested containers bypass the policy. Matching the subuid range closes that gap. *Why loopback/metadata are blocked in `isolated`:* to stop sandboxed code from reaching host-local services or a cloud instance's credential metadata endpoint.

**8. Filesystem scoping.**
The script is mounted read-only at `/sandbox/run.py:ro`, uploaded inputs read-only at `/input:ro`, and each run gets its own `/output` directory under `/tmp` named with an unguessable GUID. Input/output filenames are reduced to their basename to prevent path traversal, and output collection reads only regular files — **symlinks are skipped** so sandboxed code can't plant a symlink to exfiltrate arbitrary host files readable by the server. *Why it's necessary:* it prevents the untrusted code from modifying its own driver, reading other runs' data, or using the result-collection step as a file-read primitive.

**9. Resource and availability guards.**
See [Resource & Access Guards](#resource--access-guards-http-services) below — server-enforced clamps on CPU, memory, PID count, wall-clock timeout, request-body size, output size, and concurrency. *Why it's necessary:* the isolation layers above contain *what* untrusted code can touch; these bound *how much* it can consume, so one submission cannot starve the host or the service.

### Residual trade-offs
`SYS_ADMIN` and `--security-opt label=disable` remain necessary for nested rootless Podman (they were validated as load-bearing above), so the outer rootless user namespace stays the primary boundary — keep the host process unprivileged. The hardened seccomp profile is the recommended way to shrink the extra kernel surface that `SYS_ADMIN` exposes.

---

## Resource & Access Guards (HTTP services)

The Go, Rust, and .NET services apply the same server-enforced guards so a single request cannot exhaust or monopolize the host. All are configurable via environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `SANDBOX_BIND` | `127.0.0.1` | Interface to bind. Loopback by default; set to `0.0.0.0` only behind auth/a proxy. |
| `SANDBOX_TOKEN` | *(unset)* | If set, `POST /run` requires `Authorization: Bearer <token>`. A warning is logged if unset while binding to a non-loopback address. |
| `SANDBOX_TIMEOUT_SEC` | `60` | Max container runtime; enforced via podman `--timeout` plus a client-side backstop. On expiry the run is killed (`exit_code` 124, `timed_out: true`). |
| `SANDBOX_MAX_CONCURRENCY` | `4` | Max simultaneous executions. Excess requests get HTTP `429` immediately (no unbounded queue). |
| `SANDBOX_MAX_CPUS` / `SANDBOX_DEFAULT_CPUS` | `2.0` / `1.0` | Ceiling and default for the clamped `--cpus`. |
| `SANDBOX_MAX_MEMORY_MB` / `SANDBOX_DEFAULT_MEMORY_MB` | `2048` / `1024` | Ceiling and default for the clamped `--memory`. |
| `SANDBOX_PIDS_LIMIT` | `256` | `--pids-limit` fork-bomb guard. |
| `SANDBOX_MAX_BODY_MB` | `10` | Max request body size. |
| `SANDBOX_MAX_LOG_MB` | `4` | Cap on captured stdout/stderr per stream. |
| `SANDBOX_MAX_OUTPUT_MB` | `32` | Total byte budget for returned `/output` files. |
| `SANDBOX_HARDENED` | *(unset)* | If truthy (`1`/`true`/`yes`), applies the bundled `host/seccomp-hardened.json` profile. |
| `SANDBOX_SECCOMP` | *(unset)* | Explicit path to a seccomp profile (takes precedence over `SANDBOX_HARDENED`). |

Output-file collection reads only regular files (symlinks are skipped) so sandboxed code cannot use a planted symlink to exfiltrate arbitrary host files readable by the server process.

### Residual risks / known trade-offs
* The outer container is launched with `--cap-add=SYS_ADMIN` and `--security-opt label=disable` because nested rootless Podman (VFS storage, `unshare -m`) requires them. This widens the blast radius of any container-escape bug; the primary boundary is the host's **rootless** Podman user namespace. Do not run the host process as root.
* `/output` is created world-writable (`0777`) under `/tmp` with an unguessable GUID name. On a shared host, prefer a dedicated per-service tmp root.
* The `full` network mode grants unrestricted egress and should only be used for trusted local testing via `worker.py`.

---

## API Contract: On-Demand HTTP Services (Go, Rust, & .NET)

All three services expose a `POST /run` endpoint:

**Request Payload:**
```json
{
  "code": "import os; print(os.listdir('/input'))",
  "network": "offline",
  "cpus": 1.0,
  "memory_mb": 4096,
  "input_files": {
    "data.txt": "SGVsbG8gd29ybGQ="
  }
}
```

* **`cpus`** (float): Requests a CPU allocation (e.g. `0.25`, `0.5`, `1.0`, `2.0`). Server-side this is **clamped** to `(0, SANDBOX_MAX_CPUS]`; a value `<= 0` is replaced by `SANDBOX_DEFAULT_CPUS` (never unlimited).
* **`memory_mb`** (integer): Requests a RAM allocation in MB (e.g. `256`, `512`, `1024`, `4096`). Server-side this is **clamped** to `(0, SANDBOX_MAX_MEMORY_MB]`; a value `<= 0` is replaced by `SANDBOX_DEFAULT_MEMORY_MB` (never unlimited).
* **`input_files`** (object): Dictionary mapping filenames to their Base64-encoded file contents. These files are mounted read-only to `/input/` inside the container.

> Note: the HTTP services always clamp resource requests to the operator-configured ceilings, enforce an execution timeout, a fork-bomb `--pids-limit`, and a bounded number of concurrent runs. A container that exceeds the timeout is killed and the response carries `"timed_out": true` with `exit_code` `124`.

**Response Payload:**
```json
{
  "stdout": "['data.txt']\n",
  "stderr": "",
  "exit_code": 0,
  "metrics": {
    "wall_time_ms": 280,
    "max_memory_kb": 20480,
    "cpu_percentage": "94%",
    "user_time_sec": 0.04,
    "sys_time_sec": 0.01,
    "fs_inputs": 0,
    "fs_outputs": 8,
    "voluntary_context_switches": 31,
    "involuntary_context_switches": 5
  },
  "output_files": {},
  "run_id": "97e68c07-b280-4dfa-b108-a53b519bfb8d",
  "timed_out": false
}
```
*Note: Any output files written by the sandboxed python execution to `/output` are base64-encoded and returned in the `output_files` map. The `run_id` contains the unique request trace GUID.*

### Node-Independent Metrics
* **`fs_inputs` & `fs_outputs`**: The number of filesystem blocks read and written.
* **`voluntary_context_switches`**: Indicates how many times the code yielded execution control.
* These metrics remain constant for the same algorithm across different host machines, serving as hardware-independent indicators for program profile analysis.

---

## Running the Services

### Running the Go Service
Change into the Go server directory, build, and run:
```bash
cd server/server_go
go build -o server_go_bin main.go
PORT=8080 ./server_go_bin
```

### Running the Rust Service
Change into the Rust server directory, build, and run:
```bash
cd server/server_rust
cargo run --release
```
*Note: The Rust service utilizes fully asynchronous Axum and Tokio subprocess handling for low latency.*

### Running the .NET Service
Change into the .NET server directory, build, and run:
```bash
cd server/server_dotnet
dotnet run --configuration Release
```
*Note: The .NET service uses a high-performance Minimal API backend with asynchronous process spawning.*

---

## Test Suite
We provide a comprehensive testing framework in `test.sh` to verify security boundaries:
```bash
./test.sh
```

Tests validate user isolation, process visibility restrictions, nested container runs, egress policy matching, and resource performance capturing.
