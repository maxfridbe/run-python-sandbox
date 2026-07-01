#!/usr/bin/env python3
import argparse
import os
import sys
import subprocess
import time
import json

def main():
    parser = argparse.ArgumentParser(
        description="run-python-sandbox Host Worker CLI: Launches sandboxed Python and nested Podman containers rootlessly."
    )
    parser.add_argument(
        "--run_py",
        required=True,
        help="Path to the Python script to execute inside the sandbox."
    )
    parser.add_argument(
        "--network",
        choices=["offline", "isolated", "full"],
        default="offline",
        help="Network egress policy: 'offline' (fully blocked), 'isolated' (public internet only, local network/metadata blocked), or 'full' (unrestricted)."
    )
    parser.add_argument(
        "--output_dir",
        default="./output",
        help="Host directory to collect files written to /output by the sandbox script. Defaults to './output'."
    )
    parser.add_argument(
        "--image",
        default="run-python-sandbox",
        help="Name of the built podman image to run. Defaults to 'run-python-sandbox'."
    )
    parser.add_argument(
        "--cpus",
        type=float,
        default=0.0,
        help="CPU core limit (e.g. 0.5, 1.0, 2.0). 0.0 means unlimited."
    )
    parser.add_argument(
        "--memory_mb",
        type=int,
        default=0,
        help="Memory limit in megabytes. 0 means unlimited."
    )

    args = parser.parse_args()

    # 1. Resolve absolute paths
    run_py_path = os.path.abspath(args.run_py)
    if not os.path.isfile(run_py_path):
        print(f"Error: Python script file '{args.run_py}' does not exist.", file=sys.stderr)
        sys.exit(1)

    output_dir_path = os.path.abspath(args.output_dir)
    os.makedirs(output_dir_path, exist_ok=True)

    # Ensure permissions for the host output dir:
    # Since sandbox-user (UID 10001) in the container will write files to /output,
    # and container UIDs map to host subuids, we need to ensure the host directory is writable.
    # We can chmod it to 777 or set permissions so that the mapped subuid can write.
    # To keep it simple, we grant read/write/execute permissions to all for the output folder.
    try:
        os.chmod(output_dir_path, 0o777)
    except Exception as e:
        print(f"Warning: Failed to set permissions on output directory: {e}", file=sys.stderr)

    print(f"[*] Preparing execution for: {args.run_py}")
    print(f"[*] Network mode: {args.network}")
    print(f"[*] Mapping output directory: {output_dir_path} -> /output")

    # 2. Build Podman run command
    # - --rm: Clean up container after exit
    # - --cap-add=NET_ADMIN: Allows configuring iptables inside the network namespace
    # - --cap-add=NET_RAW: Allows raw sockets (ping, nmap)
    # - --cap-add=SYS_ADMIN: Allows mounting namespaces (unshare -m) and nested container storage
    # - --device /dev/net/tun: Allows nested rootless containers to use slirp4netns
    # - --security-opt label=disable: Disable SELinux restrictions to allow nested user namespaces
    podman_cmd = [
        "podman", "run", "--rm",
        "--cap-add=NET_ADMIN",
        "--cap-add=NET_RAW",
        "--cap-add=SYS_ADMIN",
        "--device", "/dev/net/tun",
        "--security-opt", "label=disable",
    ]

    if args.cpus > 0.0:
        podman_cmd.append(f"--cpus={args.cpus}")
    if args.memory_mb > 0:
        podman_cmd.append(f"--memory={args.memory_mb}m")

    podman_cmd.extend([
        "-e", f"NETWORK_MODE={args.network}",
        "-v", f"{run_py_path}:/sandbox/run.py:ro",
        "-v", f"{output_dir_path}:/output:rw",
        args.image
    ])

    # 3. Execute Podman run
    print("[*] Launching sandbox container...")
    start_time = time.time()
    try:
        result = subprocess.run(
            podman_cmd,
            stdout=sys.stdout,
            stderr=sys.stderr,
            text=True
        )
        elapsed_ms = int((time.time() - start_time) * 1000)
        
        # Ingest metrics
        metrics_file = os.path.join(output_dir_path, "metrics.json")
        metrics = {
            "wall_time_ms": elapsed_ms,
            "max_memory_kb": 0,
            "cpu_percentage": "0%",
            "user_time_sec": 0.0,
            "sys_time_sec": 0.0,
            "fs_inputs": 0,
            "fs_outputs": 0,
            "voluntary_context_switches": 0,
            "involuntary_context_switches": 0
        }
        
        if os.path.exists(metrics_file):
            try:
                with open(metrics_file, "r") as f:
                    inner_metrics = json.load(f)
                    metrics.update(inner_metrics)
                os.remove(metrics_file) # Clean up so it doesn't pollute user output
            except Exception as e:
                print(f"Warning: Failed to read metrics file: {e}", file=sys.stderr)
        
        print("\n=== Sandbox Execution Metrics ===")
        print(f"  Wall Time (Host):  {metrics['wall_time_ms']} ms")
        print(f"  Max RSS Memory:    {metrics['max_memory_kb']} KB")
        print(f"  CPU Percentage:    {metrics['cpu_percentage']}")
        print(f"  User CPU Time:     {metrics['user_time_sec']} sec")
        print(f"  System CPU Time:   {metrics['sys_time_sec']} sec")
        print(f"  FS Read Operations: {metrics['fs_inputs']}")
        print(f"  FS Write Operations:{metrics['fs_outputs']}")
        print(f"  Voluntary CS:      {metrics['voluntary_context_switches']}")
        print(f"  Involuntary CS:    {metrics['involuntary_context_switches']}")
        print("=================================\n")
        
        sys.exit(result.returncode)
    except FileNotFoundError:
        print("Error: 'podman' executable not found on host. Is Podman installed?", file=sys.stderr)
        sys.exit(127)
    except Exception as e:
        print(f"Error executing podman: {e}", file=sys.stderr)
        sys.exit(1)

if __name__ == "__main__":
    main()
