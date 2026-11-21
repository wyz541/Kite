# Kite v0.6.88

## Fixes

- Windows: update checks now select the complete `Kite-vX.Y.Z-windows-x64.zip` package, including Kensaku sidecars.
- Windows: the standalone `Kite.exe` keeps a runnable executable payload and carries the complete package for legacy `0.6.84` updaters.
- Windows and macOS: published GitHub Releases are checked directly instead of being suppressed by the main branch manifest version.
- macOS parity: not required; this release changes the Windows updater and package bootstrap only.

## Install

- Windows: use `Kite-v0.6.88-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.88-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.