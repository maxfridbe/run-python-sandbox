# TODO List: Rootless run-python-sandbox Container

- [x] Write documentation files:
  - [x] `todo.md` (This file)
  - [x] `llm.md` (Documentation optimized for LLMs)
  - [x] `README.md` (Standard user documentation)
- [x] Create configuration files:
  - [x] `storage.conf` (Podman storage config using `vfs`)
  - [x] `containers.conf` (Podman runtime configuration for rootless-in-rootless)
- [x] Write scripts for parent container:
  - [x] `entrypoint.sh` (Initializes iptables firewall and starts worker or runs daemon)
  - [x] `worker.py` (Core sandboxing runner: user allocations, namespaces, execution tracking, file ingestion)
- [x] Write host helper scripts:
  - [x] `build.sh` (Commands to build the podman container)
  - [x] `test.sh` (Tests for nested podman, process isolation, network isolation, output capture)
- [x] Implement Dockerfile:
  - [x] Standard tools, Python 3, Podman CLI, `gosu`, `iptables`, `uidmap` tools
  - [x] Mapping `/etc/subuid` and `/etc/subgid` range for dynamically mapped UIDs
- [x] Verify execution and correctness:
  - [x] Run test scripts on host
  - [x] Debug container nesting bugs (e.g. namespaces, iptables permissions, storage paths)
- [x] Create on-demand HTTP Web Services:
  - [x] Implement HTTP web service in Go (`server_go/`)
  - [x] Implement HTTP web service in Rust (`server_rust/`) complying with universal guidelines
  - [x] Validate compilation of both services on host
- [x] Create Monaco Web Front End (WFE) & Interactive Testing:
  - [x] Write `index.html` featuring Monaco editor, run trigger, and image/text output previews
  - [x] Connect `index.html` to both Go and Rust server routers
  - [x] Implement `test_interactive.sh` script to run the interactive server dashboard


