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
    cd "$SCRIPT_DIR/server/server_go"
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
    cd "$SCRIPT_DIR/server/server_rust"
    echo ""
    echo "=========================================================="
    echo " -> Rust Service started successfully!"
    echo " -> Open your browser and navigate to: http://localhost:8081"
    echo " -> Press Ctrl+C to stop the server."
    echo "=========================================================="
    echo ""
    PORT=8081 cargo run --release
}

# Start .NET
run_dotnet() {
    echo "[*] Starting .NET controller (compiling if needed)..."
    cd "$SCRIPT_DIR/server/server_dotnet"
    echo ""
    echo "=========================================================="
    echo " -> .NET Service started successfully!"
    echo " -> Open your browser and navigate to: http://localhost:8082"
    echo " -> Press Ctrl+C to stop the server."
    echo "=========================================================="
    echo ""
    PORT=8082 dotnet run --configuration Release
}

# Start all 3 servers concurrently
run_all() {
    echo "=========================================================="
    echo " -> Launching ALL 3 worker services concurrently:"
    echo "    1) Go Server on:      http://localhost:8080"
    echo "    2) Rust Server on:    http://localhost:8081"
    echo "    3) .NET Server on:    http://localhost:8082"
    echo " -> Open WFE on any of these ports and switch on-the-fly!"
    echo " -> Press Ctrl+C to stop all servers."
    echo "=========================================================="
    echo ""

    # Start Go Server
    cd "$SCRIPT_DIR/server/server_go"
    go build -o server_go_bin main.go
    PORT=8080 ./server_go_bin > go_server.log 2>&1 &
    GO_PID=$!

    # Start Rust Server
    cd "$SCRIPT_DIR/server/server_rust"
    PORT=8081 cargo run --release > rust_server.log 2>&1 &
    RUST_PID=$!

    # Start .NET Server
    cd "$SCRIPT_DIR/server/server_dotnet"
    PORT=8082 dotnet run --configuration Release > dotnet_server.log 2>&1 &
    DOTNET_PID=$!

    # Trap Ctrl+C to kill all child processes
    trap 'echo ""; echo "Stopping all services..."; kill $GO_PID $RUST_PID $DOTNET_PID 2>/dev/null; exit 0' SIGINT SIGTERM

    echo "[*] Go Server PID: $GO_PID (logs: server/server_go/go_server.log)"
    echo "[*] Rust Server PID: $RUST_PID (logs: server/server_rust/rust_server.log)"
    echo "[*] .NET Server PID: $DOTNET_PID (logs: server/server_dotnet/dotnet_server.log)"
    echo ""
    echo "[*] Web interface is active. Ready for execution..."

    # Keep script running to maintain trap
    wait
}

# Check availability
HAS_GO=false
HAS_RUST=false
HAS_DOTNET=false

if command -v go >/dev/null 2>&1; then HAS_GO=true; fi
if command -v cargo >/dev/null 2>&1; then HAS_RUST=true; fi
if command -v dotnet >/dev/null 2>&1; then HAS_DOTNET=true; fi

if [ "$MODE" = "go" ]; then
    run_go
elif [ "$MODE" = "rust" ]; then
    run_rust
elif [ "$MODE" = "dotnet" ]; then
    run_dotnet
elif [ "$MODE" = "all" ]; then
    run_all
else
    # Auto-detection mode
    if $HAS_GO && $HAS_RUST && $HAS_DOTNET; then
        run_all
    elif $HAS_GO; then
        run_go
    elif $HAS_RUST; then
        run_rust
    elif $HAS_DOTNET; then
        run_dotnet
    else
        echo "Error: Neither Go, Rust, nor .NET SDK was found on your host."
        exit 1
    fi
fi
