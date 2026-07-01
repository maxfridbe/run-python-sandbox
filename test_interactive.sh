#!/bin/bash
set -e

# Interactive test runner for run-python-sandbox Web Front End

echo "=========================================================="
echo "      run-python-sandbox Interactive Dashboard Service"
echo "=========================================================="

# Check for Go toolchain (default, starts instantly)
if command -v go >/dev/null 2>&1; then
    echo "[*] Go toolchain detected. Compiling Go controller..."
    cd server_go
    go build -o server_go_bin main.go
    
    echo ""
    echo "=========================================================="
    echo " -> Go Service started successfully!"
    echo " -> Open your browser and navigate to: http://localhost:8080"
    echo " -> Press Ctrl+C to stop the server."
    echo "=========================================================="
    echo ""
    
    PORT=8080 ./server_go_bin
else
    # Fallback to Rust
    if command -v cargo >/dev/null 2>&1; then
        echo "[*] Cargo detected. Starting Rust controller (compiling if needed)..."
        cd server_rust
        
        echo ""
        echo "=========================================================="
        echo " -> Rust Service started successfully!"
        echo " -> Open your browser and navigate to: http://localhost:8080"
        echo " -> Press Ctrl+C to stop the server."
        echo "=========================================================="
        echo ""
        
        PORT=8080 cargo run --release
    else
        echo "Error: Neither Go nor Rust toolchains were found on your host."
        exit 1
    fi
fi
