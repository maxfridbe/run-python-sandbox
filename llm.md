# LLM Context: run-python-sandbox

This document describes the design, architecture, security boundary constraints, and implementation details of the `run-python-sandbox` project. It is intended to help future LLM agents understand how this system operates.

## System Intent
To run untrusted Python scripts that may perform dynamic operations, execute nested Podman containers ("offensive containers" such as security scanning, pen-testing tools, or simulated payloads), and to do so safely on a host running rootless Podman.

## Architecture: The UID-in-Container Sandbox Model
Instead of launching a separate container on the host for every task (which leaks Docker/Podman host socket permissions and has high overhead), this project uses a single parent container that implements Unix-level isolation internally.

### 1. Nested Rootless Podman
* The parent container is built with `podman` installed.
* To allow nested rootless containers inside the parent container without mounting `/dev/fuse` on the host, the nested Podman utilizes the `vfs` storage driver.
* In `/etc/subuid` and `/etc/subgid` inside the parent container, we define user mapping ranges. When an unprivileged process inside the container runs a nested container, the Linux kernel uses these mapping ranges to isolate the nested container's root user.

### 2. Privilege Dropping via `gosu` and Namespaces via `unshare`
* The parent container's orchestrator/worker runs as `root` (within the container user namespace).
* When executing a user-submitted script:
  1. A low-privileged UNIX user is dynamically allocated (or selected from a pool) in the range `10000–65000`.
  2. The worker mounts the target script and the `/output` folder.
  3. The worker spawns the task using Linux namespaces via `unshare -p -m --fork` (isolating the PID and Mount tables of the execution run).
  4. The executor drops privileges using `gosu` to the assigned UID.
  5. The script executes under this restricted user. It can spawn rootless containers inside its allocated namespace, but cannot affect other UIDs or the parent container root.

### 3. Network Isolation Options
Network access is controlled by passing configurations to the parent container:
* **`offline`**: The network is disabled or completely blocked. Egress packets from UIDs `10000-65000` are rejected by `iptables`.
* **`isolated`**: The sandboxed UIDs can communicate with the external public internet but are blocked via `iptables` owner-matching from reaching:
  * Local host interfaces (`127.0.0.0/8`).
  * Private RFC 1918 networks (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`).
  * Link-Local addresses (`169.254.169.254` cloud metadata).
* **`full`**: Unrestricted network access for the sandbox user.

## Files Structure
* `Dockerfile`: Container image build instructions containing Podman, gosu, iptables, and Python dependencies.
* `entrypoint.sh`: Initializer script. Runs as root inside the container, configures `iptables` based on parameters, and delegates to the worker.
* `worker.py`: Python CLI/Daemon wrapper. Allocates UID namespaces, prepares directories, starts execution, maps `/output`, captures stdout/stderr, and returns.
* `containers.conf` & `storage.conf`: Configures internal rootless Podman to run in VFS mode.
* `build.sh`: Script to build the container rootlessly on the host.
* `test.sh`: Automated integration test suite.
* `server_go/`: Directory containing Go web service implementation (`main.go`, `go.mod`) for HTTP-based on-demand sandbox orchestration.
* `server_rust/`: Directory containing Rust web service implementation (`src/main.rs`, `Cargo.toml`) complying with universal guidelines (e.g. `mimalloc` memory allocator, structured handling).
