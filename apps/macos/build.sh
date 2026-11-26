#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP_DIR="$ROOT/apps/macos/build/Kite.app"
CONTENTS="$APP_DIR/Contents"
GEN_DIR="$ROOT/apps/macos/Generated"
PROFILE="${KITE_BUILD_PROFILE:-release}"
APP_EXECUTABLE="Kite"

case "$PROFILE" in
    release)
        TARGET_DIR="$ROOT/target/release"
        ;;
    debug)
        TARGET_DIR="$ROOT/target/debug"
        APP_EXECUTABLE="KiteMindmapDev"
        ;;
    *)
        echo "Unsupported KITE_BUILD_PROFILE: $PROFILE" >&2
        exit 2
        ;;
esac

cd "$ROOT"
if [[ "$PROFILE" == "release" ]]; then
    cargo build --release --locked --lib
else
    cargo build --locked --lib
fi

rm -rf "$GEN_DIR" "$APP_DIR"
mkdir -p "$GEN_DIR" "$CONTENTS/MacOS" "$CONTENTS/Frameworks" "$CONTENTS/Resources"

if [[ "$PROFILE" == "release" ]]; then
    cargo run --release --locked --features uniffi-bindgen --bin uniffi-bindgen -- \
        generate \
        --library "$TARGET_DIR/libkite_core.dylib" \
        --language swift \
        --out-dir "$GEN_DIR"
else
    cargo run --locked --features uniffi-bindgen --bin uniffi-bindgen -- \
        generate \
        --library "$TARGET_DIR/libkite_core.dylib" \
        --language swift \
        --out-dir "$GEN_DIR"
fi

xcrun swiftc \
    -O \
    -target arm64-apple-macos13.0 \
    -parse-as-library \
    -I "$GEN_DIR" \
    -Xcc -fmodule-map-file="$GEN_DIR/kite_coreFFI.modulemap" \
    -Xcc -I"$GEN_DIR" \
    -L "$TARGET_DIR" \
    -lkite_core \
    -framework AppKit \
    -framework QuickLookUI \
    -framework WebKit \
    -Xlinker -rpath \
    -Xlinker @executable_path/../Frameworks \
    "$ROOT"/apps/macos/Sources/*.swift \
    "$GEN_DIR"/kite_core.swift \
    -o "$CONTENTS/MacOS/$APP_EXECUTABLE"

cp "$ROOT/apps/macos/Info.plist" "$CONTENTS/Info.plist"
if [[ "$PROFILE" == "debug" ]]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier com.wyz541.kite.mindmapdev" "$CONTENTS/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleName Kite Mindmap Dev" "$CONTENTS/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleExecutable $APP_EXECUTABLE" "$CONTENTS/Info.plist"
fi
cp "$ROOT/assets/kite.icns" "$CONTENTS/Resources/AppIcon.icns"
cp "$ROOT/tests/fixtures/mindmap/kite-rust-gpui-capability.xmind" "$CONTENTS/Resources/"
cp "$TARGET_DIR/libkite_core.dylib" "$CONTENTS/Frameworks/"
install_name_tool -id @rpath/libkite_core.dylib "$CONTENTS/Frameworks/libkite_core.dylib"

CORE_LINK="$(otool -L "$CONTENTS/MacOS/$APP_EXECUTABLE" | awk '/libkite_core\.dylib/ { print $1; exit }')"
if [ -n "$CORE_LINK" ] && [ "$CORE_LINK" != "@rpath/libkite_core.dylib" ]; then
    install_name_tool -change "$CORE_LINK" @rpath/libkite_core.dylib "$CONTENTS/MacOS/$APP_EXECUTABLE"
fi

codesign --force --deep --sign - "$APP_DIR"

echo "$APP_DIR"
