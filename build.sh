#!/bin/bash
set -e

echo "=== Building run-python-sandbox Podman Container ==="
podman build -t run-python-sandbox -f host/Dockerfile host/
echo "=== Build Complete! ==="
