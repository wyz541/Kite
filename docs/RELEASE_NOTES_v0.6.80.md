# Kite v0.6.80

Kite v0.6.80 makes file previews faster and keeps navigation responsive while previews load.

## Preview performance

- Decode only the first frame of animated GIF and WebP previews instead of loading every frame.
- Downscale image previews before handing them to the renderer.
- Cache up to 12 recent image previews with file size and modification time as the cache key.
- Debounce rapid selection changes by 45 ms so keyboard navigation starts only the useful preview load.
- Load image previews asynchronously in both the side pane and the detached preview window.
- Read text previews from a bounded prefix and large logs from a bounded tail.
- Avoid duplicate full-string scans while constructing bounded text excerpts.

## Verification

- Rust formatting check.
- Rust Core and Windows UI tests.
- Windows Clippy check.
- Windows GPUI release package build.
- Preview smoke benchmark for 100 MiB text, 100 MiB logs, and a 3200x2000 image.
- Windows and macOS release builds through the release workflow.

## Install

- Windows: use `Kite-v0.6.80-windows-x64.zip`.
- Windows direct executable: use `Kite.exe`.
- macOS: use `Kite-v0.6.80-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

## Platform note

This release changes the Windows preview UI and shared preview preparation.
macOS parity: not required
