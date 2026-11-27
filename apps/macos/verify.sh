#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

echo "Checking Rust formatting..."
cargo fmt --all -- --check

echo "Running core tests..."
cargo test --locked --lib

echo "Running Clippy..."
cargo clippy --locked --all-targets -- -D warnings

echo "Testing directory watcher..."
bash apps/macos/test-directory-watcher.sh

echo "Testing file drag provider..."
bash apps/macos/test-file-drag.sh

echo "Testing Agent permission presentation..."
bash apps/macos/Tests/run-agent-permission-presentation-smoke.sh

echo "Testing canonical context-menu plan..."
bash apps/macos/Tests/run-context-menu-plan-smoke.sh

echo "Testing Open With catalog..."
bash apps/macos/Tests/run-open-with-smoke.sh

echo "Testing tool registry..."
bash apps/macos/Tests/run-tool-registry-smoke.sh

echo "Building release app..."
bash apps/macos/build.sh

echo "macOS verification passed."
