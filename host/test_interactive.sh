#!/bin/bash
set -e

# Interactive test runner for run-python-sandbox Web Front End

echo "=========================================================="
echo "      run-python-sandbox Interactive Dashboard Service"
echo "=========================================================="

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

MODE=${1:-"auto"}

# Start Go
run_go() {
    echo "[*] Compiling and starting Go controller..."
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
}

# Start Rust
run_rust() {
    echo "[*] Starting Rust controller (compiling if needed)..."
    cd server_rust
    echo ""
    echo "=========================================================="
    echo " -> Rust Service started successfully!"
    echo " -> Open your browser and navigate to: http://localhost:8080"
    echo " -> Press Ctrl+C to stop the server."
    echo "=========================================================="
    echo ""
    PORT=8080 cargo run --release
}

# Start .NET
run_dotnet() {
    echo "[*] Starting .NET controller (compiling if needed)..."
    cd server_dotnet
    echo ""
    echo "=========================================================="
    echo " -> .NET Service started successfully!"
    echo " -> Open your browser and navigate to: http://localhost:8080"
    echo " -> Press Ctrl+C to stop the server."
    echo "=========================================================="
    echo ""
    PORT=8080 dotnet run --configuration Release
}

if [ "$MODE" = "go" ]; then
    run_go
elif [ "$MODE" = "rust" ]; then
    run_rust
elif [ "$MODE" = "dotnet" ]; then
    run_dotnet
else
    # Auto detection fallback
    if command -v go >/dev/null 2>&1; then
        run_go
    elif command -v cargo >/dev/null 2>&1; then
        run_rust
    elif command -v dotnet >/dev/null 2>&1; then
        run_dotnet
    else
        echo "Error: Neither Go, Rust, nor .NET SDK was found on your host."
        exit 1
    fi
fi
