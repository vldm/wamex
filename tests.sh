#!/usr/bin/env bash

set -euo pipefail
export RUST_LOG=error
directory="$(cd "$(dirname "$0")" && pwd)"
cd "$directory"

cargo test --all

echo "Running e2e tests..."
source "$directory/crates/example/e2e/run_tests.sh"