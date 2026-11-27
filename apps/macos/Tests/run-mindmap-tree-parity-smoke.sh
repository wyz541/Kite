#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

xcrun swiftc \
    -parse-as-library \
    -framework CoreFoundation \
    -framework CryptoKit \
    "$ROOT/apps/macos/Sources/MindmapTreeCanonicalizer.swift" \
    "$ROOT/apps/macos/Tests/MindmapTreeParitySmoke.swift" \
    -o "$WORK_DIR/mindmap-tree-parity-smoke"

"$WORK_DIR/mindmap-tree-parity-smoke" "$ROOT"
