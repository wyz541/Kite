#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

cd "$ROOT"
cargo build --locked --lib
cargo run --locked --features uniffi-bindgen --bin uniffi-bindgen -- \
    generate \
    --library "$ROOT/target/debug/libkite_core.dylib" \
    --language swift \
    --out-dir "$WORK_DIR/Generated"

xcrun swiftc \
    -parse-as-library \
    -I "$WORK_DIR/Generated" \
    -Xcc -fmodule-map-file="$WORK_DIR/Generated/kite_coreFFI.modulemap" \
    -Xcc -I"$WORK_DIR/Generated" \
    -L "$ROOT/target/debug" \
    -lkite_core \
    -framework AppKit \
    -Xlinker -rpath \
    -Xlinker "$ROOT/target/debug" \
    "$ROOT/apps/macos/Sources/KiteToolRegistry.swift" \
    "$ROOT/apps/macos/Tests/ToolRegistrySmoke.swift" \
    "$WORK_DIR/Generated/kite_core.swift" \
    -o "$WORK_DIR/tool-registry-smoke"

"$WORK_DIR/tool-registry-smoke"
