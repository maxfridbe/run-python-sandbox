#!/bin/bash
set -e

echo "=== Building run-python-sandbox Podman Container ==="
podman build -t run-python-sandbox .
echo "=== Build Complete! ==="
