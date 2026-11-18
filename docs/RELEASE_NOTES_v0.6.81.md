# Kite v0.6.81

Kite v0.6.81 improves feedback for file operations and keeps extracted files together.

## Changes

- On Windows, Copy To uses the native Windows copy dialog for progress and details.
- On Windows, ZIP and TAR.GZ compression and extraction show a separate window with the source, destination, bytes processed, and final result.
- On Windows and macOS, Extract Here creates a folder named after the archive. If that name is taken, Kite uses a numbered name without merging into the existing folder. Extract To still uses the chosen folder directly.

## Install

- Windows: use `Kite-v0.6.81-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.81-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

## Platform note

The extraction layout is shared across platforms. The Windows progress windows are platform-specific.
