import subprocess
import sys

def main():
    print("=== Running Nested Podman Test ===")
    
    # Run a simple nested rootless podman command
    cmd = ["podman", "run", "--rm", "--network", "slirp4netns:mtu=1300", "public.ecr.aws/docker/library/alpine:latest", "echo", "hello from nested podman"]
    print(f"[test_nested_podman] Executing: {' '.join(cmd)}")
    
    try:
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=300  # Pulling might take a bit first time (e.g. slow registry)
        )
        
        print(f"[test_nested_podman] Exit code: {result.returncode}")
        print(f"[test_nested_podman] Stdout:\n{result.stdout.strip()}")
        print(f"[test_nested_podman] Stderr:\n{result.stderr.strip()}")
        
        if result.returncode != 0:
            print("FAIL: Nested podman execution failed.", file=sys.stderr)
            sys.exit(result.returncode)
            
        if "hello from nested podman" in result.stdout:
            print("SUCCESS: Nested rootless podman successfully ran inside the sandbox container!")
            
            # Write success file to /output for the worker to collect
            with open("/output/nested_podman_success.txt", "w") as f:
                f.write(result.stdout.strip())
        else:
            print("FAIL: Expected output not found in nested podman run stdout.", file=sys.stderr)
            sys.exit(1)
            
    except subprocess.TimeoutExpired:
        print("FAIL: Nested podman execution timed out (exceeded 120 seconds).", file=sys.stderr)
        sys.exit(1)
    except Exception as e:
        print(f"FAIL: Exception occurred while running nested podman: {e}", file=sys.stderr)
        sys.exit(1)

if __name__ == "__main__":
    main()
