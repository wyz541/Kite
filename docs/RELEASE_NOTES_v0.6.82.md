# Kite v0.6.82

Kite v0.6.82 brings archive progress windows to macOS.

## Changes

- On macOS, Compress, Extract Here, and Extract To show a separate progress window for ZIP and TAR.GZ archives.
- The window shows the source, destination, bytes processed, elapsed time, and the final result. A percentage appears when the total is known. Closing the window does not cancel the operation.
- Extract Here still creates a new folder named after the archive. Extract To still uses the selected folder directly. Windows archive behavior is unchanged.

## Install

- macOS: use `Kite-v0.6.82-macOS-arm64.zip` on Apple Silicon.
- Windows: use `Kite-v0.6.82-windows-x64.zip` or the standalone `Kite.exe`.
- Verify downloads with `SHA256SUMS.txt`.
