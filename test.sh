#!/bin/bash
set -e

# Run the test suite for run-python-sandbox

echo "=== Starting run-python-sandbox Test Suite ==="

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Clean up any old test outputs
rm -rf ./test_out_isolation ./test_out_net_offline ./test_out_net_isolated ./test_out_nested

# 1. Run Isolation Test
echo ""
echo "[test.sh] 1. Running process & user isolation test..."
python3 host/worker.py --run_py host/tests/test_isolation.py --output_dir ./test_out_isolation --network offline

# Check if the output file was successfully sucked up
if [ -f "./test_out_isolation/test_result.txt" ]; then
    echo "SUCCESS: Output file test_result.txt successfully captured from sandbox."
    echo "Content: $(cat ./test_out_isolation/test_result.txt)"
else
    echo "FAIL: Output file test_result.txt was not captured."
    exit 1
fi
echo "--------------------------------------------------"

# 2. Run Network offline test
echo ""
echo "[test.sh] 2. Running offline network egress test..."
# Run and capture output
output_net_offline=$(python3 host/worker.py --run_py host/tests/test_network.py --output_dir ./test_out_net_offline --network offline 2>&1)
echo "$output_net_offline"

# For offline mode, loopback, private, metadata, and internet should ALL be False (or connection fail)
if echo "$output_net_offline" | grep -q "RESULT_LOCALHOST=True" || \
   echo "$output_net_offline" | grep -q "RESULT_PRIVATE=True" || \
   echo "$output_net_offline" | grep -q "RESULT_METADATA=True" || \
   echo "$output_net_offline" | grep -q "RESULT_INTERNET=True"; then
    echo "FAIL: Network traffic allowed in offline mode!"
    exit 1
fi
echo "SUCCESS: Egress fully blocked in offline mode."
echo "--------------------------------------------------"

# 3. Run Network isolated test
echo ""
echo "[test.sh] 3. Running isolated network egress test..."
# Run and capture output
output_net_isolated=$(python3 host/worker.py --run_py host/tests/test_network.py --output_dir ./test_out_net_isolated --network isolated 2>&1)
echo "$output_net_isolated"

# For isolated mode, loopback, private, and metadata should be False (blocked)
if echo "$output_net_isolated" | grep -q "RESULT_LOCALHOST=True" || \
   echo "$output_net_isolated" | grep -q "RESULT_PRIVATE=True" || \
   echo "$output_net_isolated" | grep -q "RESULT_METADATA=True"; then
    echo "FAIL: Loopback or private range accessible in isolated mode!"
    exit 1
fi

echo "SUCCESS: Local loopback and private networks blocked in isolated mode."
echo "--------------------------------------------------"

# 4. Run Nested Podman Test
echo ""
echo "[test.sh] 4. Running nested rootless Podman execution test..."
python3 host/worker.py --run_py host/tests/test_nested_podman.py --output_dir ./test_out_nested --network isolated

# Check if nested container ran successfully
if [ -f "./test_out_nested/nested_podman_success.txt" ]; then
    echo "SUCCESS: Nested Podman container ran successfully inside sandbox and output captured."
    echo "Content: $(cat ./test_out_nested/nested_podman_success.txt)"
else
    echo "FAIL: Nested Podman test failed or output was not captured."
    exit 1
fi

echo ""
echo "============================================="
echo "=== All Sandbox Isolation Tests Passed! ==="
echo "============================================="
