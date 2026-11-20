# Kite v0.6.86

Kite v0.6.86 fixes update detection when a release tag is newer than the version declared on the default branch.

## Changes

- Always check the published GitHub Release before reporting that Kite is up to date.
- Detect `v0.6.85` correctly from older installed builds such as `v0.6.84`.
- Keep release assets and SHA-256 verification unchanged.

## Install

- Windows: use `Kite-v0.6.86-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.86-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

macOS parity: not required