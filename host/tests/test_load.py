import json
import urllib.request
import urllib.error
import asyncio
import time
import sys

SERVER_URL = "http://127.0.0.1:8080/run"

NORMAL_CODE = """
import time
time.sleep(1)
print("Normal job completed successfully.")
"""

OOM_CODE = """
import time
print("Starting memory allocation bomb...")
blocks = []
try:
    for i in range(45):
        # Allocate 100MB blocks and write to them to force physical page mapping
        blocks.append(bytearray(100 * 1024 * 1024))
        print(f"Allocated {i+1}00 MB successfully.")
        time.sleep(0.05)
    print("Warning: Memory bomb did not get OOM killed!")
except Exception as e:
    print("Caught allocation exception:", e)
"""

async def run_worker(name, code, cpus, memory_mb):
    payload = {
        "code": code,
        "network": "offline",
        "cpus": cpus,
        "memory_mb": memory_mb
    }
    
    print(f"[{name}] Dispatching worker (CPUs={cpus}, Mem={memory_mb}MB)...")
    data = json.dumps(payload).encode('utf-8')
    req = urllib.request.Request(
        SERVER_URL,
        data=data,
        headers={'Content-Type': 'application/json'}
    )
    
    start_time = time.time()
    try:
        # Run request in separate thread to prevent blocking the asyncio loop
        response = await asyncio.to_thread(urllib.request.urlopen, req, timeout=20)
        res_data = json.loads(response.read().decode('utf-8'))
        elapsed = time.time() - start_time
        print(f"[{name}] Finished in {elapsed:.2f}s. Exit Code: {res_data.get('exit_code')}")
        return res_data
    except Exception as e:
        elapsed = time.time() - start_time
        print(f"[{name}] HTTP/Request Error after {elapsed:.2f}s: {e}")
        return {"error": str(e), "exit_code": -99}

async def main():
    print("=== Sandbox Concurrency & Resource Limit Load Test ===")
    print(f"Targeting server: {SERVER_URL}")
    print("Starting 4 concurrent sandboxes...")
    
    tasks = [
        run_worker("Worker-1 (Normal)", NORMAL_CODE, cpus=1.0, memory_mb=1024),
        run_worker("Worker-2 (Normal)", NORMAL_CODE, cpus=1.0, memory_mb=1024),
        run_worker("Worker-3 (Memory-Bomb)", OOM_CODE, cpus=1.0, memory_mb=1024),
        run_worker("Worker-4 (Normal)", NORMAL_CODE, cpus=1.0, memory_mb=1024),
    ]
    
    start_total = time.time()
    results = await asyncio.gather(*tasks)
    total_elapsed = time.time() - start_total
    
    print("\n=== Load Test Summary ===")
    print(f"Total time for all sandboxes to complete: {total_elapsed:.2f} seconds")
    
    # Assertions
    failures = 0
    passed = 0
    oom_failed_as_expected = False
    
    for i, res in enumerate(results):
        name = f"Worker-{i+1}"
        if "error" in res:
            print(f" - {name} failed with critical error: {res['error']}")
            failures += 1
        else:
            exit_code = res.get("exit_code")
            stdout = res.get("stdout", "").strip()
            stderr = res.get("stderr", "").strip()
            metrics = res.get("metrics", {})
            
            if i == 2: # Memory Bomb Worker
                # We expect exit_code != 0 due to OOM kill (exit code 137 or non-zero status)
                if exit_code != 0:
                    print(f" - {name} (Memory-Bomb) failed as expected. Exit Code: {exit_code}")
                    oom_failed_as_expected = True
                else:
                    print(f" - WARNING: {name} (Memory-Bomb) completed with exit code 0! Output: {stdout}")
            else: # Normal Workers
                if exit_code == 0:
                    print(f" - {name} (Normal) passed. Exit Code: 0, Wall Time: {metrics.get('wall_time_ms')}ms")
                    passed += 1
                else:
                    print(f" - {name} (Normal) failed unexpectedly! Exit Code: {exit_code}. Stderr: {stderr}")
                    failures += 1
                    
    print("-" * 30)
    if passed == 3 and oom_failed_as_expected:
        print("SUCCESS: Concurrency load test passed perfectly!")
        sys.exit(0)
    else:
        print("FAIL: Concurrency load test did not match expectations.")
        sys.exit(1)

if __name__ == "__main__":
    asyncio.run(main())
