# Kite v0.6.77

Kite v0.6.77 improves Windows information density by tightening the spacing around and inside the application's islands.

## Windows layout

- Reduce outer window and inter-island spacing from 8 px to 4 px.
- Compact the tab bar, toolbar, sidebar footer, and status bar heights.
- Reduce repeated island padding from 8 px to 6 px.
- Tighten column-strip gaps and padding from 8 px to 4 px.
- Narrow the sidebar, default columns, preview pane, and tabs.
- Keep file rows and primary controls at usable click-target sizes.
- Fix issue #97.

## Verification

- Rust formatting check.
- Rust Core tests.
- Windows Clippy check.
- Windows GPUI tests.
- Windows debug and release builds.
- Windows release package verification.
- Linux checks.
- Platform parity.
- Startup performance.
- macOS SwiftUI and AppKit release build.

## Install

- Windows: use `Kite-v0.6.77-windows-x64.zip`.
- Windows direct executable: use `Kite.exe`.
- macOS: use `Kite-v0.6.77-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

## Platform note

This release updates Windows UI density.
macOS receives the shared version bump and release package.
macOS parity: not required
