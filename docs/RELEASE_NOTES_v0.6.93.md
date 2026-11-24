# Kite v0.6.93

## Windows updater repair

- Fix Windows auto-update from older Kite builds by extracting the downloaded `Kite-v*-windows-x64.zip` package instead of copying the ZIP file directly to `Kite.exe`.
- Install `kensaku-cli.exe`, `kensaku-daemon.exe`, their license, and backend documentation from the same verified package.
- Verify the downloaded ZIP and the installed `Kite.exe` with the built-in .NET SHA-256 implementation, avoiding PowerShell module availability problems.
- Wait for existing Kite and Kensaku processes to exit before replacing files.
- Retry starting the updated executable after transient Windows executable-image, antivirus, or file-system locks instead of rolling back immediately.
- Roll back the complete managed package only after all startup retries fail.

If an older build cannot start its updater once, download `Kite-v0.6.93-windows-x64.zip` or the standalone `Kite.exe` manually and run it once. Later updates use the repaired package flow.

## Included improvements

- Real file promises for macOS drag-to-web uploads, delivered in v0.6.91.
- Ranked column search, grouped keyboard navigation, and floating previews, delivered in v0.6.92.

## Install

- Windows: use `Kite-v0.6.93-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.93-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
