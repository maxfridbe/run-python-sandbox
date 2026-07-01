import os
import sys

def main():
    print("=== Running Isolation Test ===")
    uid = os.getuid()
    gid = os.getgid()
    print(f"[test_isolation] Running as UID={uid}, GID={gid}")

    if uid != 10001 or gid != 10001:
        print(f"FAIL: Expected UID=10001 and GID=10001, got UID={uid}, GID={gid}", file=sys.stderr)
        sys.exit(1)
    print("SUCCESS: Running under restricted sandbox-user.")

    # Check process space (should only see itself and system tasks in /proc)
    pids = [int(p) for p in os.listdir('/proc') if p.isdigit()]
    print(f"[test_isolation] Visible PIDs in /proc: {pids}")
    # Under --mount-proc and PID namespace, we should only see a very small number of PIDs (typically 1 for python or similar, and no host PIDs)
    if len(pids) > 10:
        print(f"FAIL: Process namespace leak! Visible PIDs in /proc: {len(pids)} (too many).", file=sys.stderr)
        sys.exit(1)
    print("SUCCESS: Process namespace is isolated.")

    # Test file writing to /output
    try:
        output_file = "/output/test_result.txt"
        with open(output_file, "w") as f:
            f.write("sandbox_isolation_success")
        print(f"SUCCESS: Wrote file to {output_file}")
    except Exception as e:
        print(f"FAIL: Cannot write to /output: {e}", file=sys.stderr)
        sys.exit(1)

if __name__ == "__main__":
    main()
