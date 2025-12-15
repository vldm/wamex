#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Step 1: Building and splitting WASM modules..."
"$SCRIPT_DIR/build.sh"

echo ""
echo "Step 2: Starting Selenium Docker container..."
cd "$SCRIPT_DIR"
docker-compose up -d

cleanup() {
    echo "Stopping services..."
    kill $SERVER_PID 2>/dev/null || true
    docker-compose down 2>/dev/null || true
}
trap cleanup EXIT

echo "Waiting for Selenium to be ready..."
timeout=30
while ! curl -s http://localhost:4444/status > /dev/null 2>&1; do
    timeout=$((timeout - 1))
    if [ $timeout -le 0 ]; then
        echo "Error: Selenium failed to start"
        exit 1
    fi
    sleep 1
done
echo "Selenium is ready!"

echo ""
echo "Step 3: Starting web server..."
BINDGEN_DIR="$SCRIPT_DIR/dist/bindgen"
cd "$BINDGEN_DIR"
python3 -m http.server 8080 &
SERVER_PID=$!

echo "Waiting for server to start..."
sleep 2

echo ""
echo "Step 4: Running tests..."
cd "$SCRIPT_DIR"
cargo test

echo ""
echo "Tests complete!"
