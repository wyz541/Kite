# Kite v0.6.78

Kite v0.6.78 keeps update checks, release notes, downloads, and installation in the existing Settings > Updates page instead of opening a second update dialog.

## Update flow

- Remove the extra Windows update dialog shown after checking for updates.
- Remove macOS update alerts and keep the result in the existing settings window.
- Show the available version, release notes, release link, download action, progress, errors, and retry controls in Settings > Updates.
- Keep update status visible in the application status bar.
- Preserve Windows SHA-256 verification, automatic replacement, and rollback behavior.

## Verification

- Rust formatting check.
- Rust Core tests.
- Windows Clippy check.
- Windows GPUI tests.
- Windows release build and package verification.
- Linux checks.
- Platform parity.
- Startup performance.
- macOS SwiftUI and AppKit release build.

## Install

- Windows: use `Kite-v0.6.78-windows-x64.zip`.
- Windows direct executable: use `Kite.exe`.
- macOS: use `Kite-v0.6.78-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

## Platform note

This release updates the update flow on Windows and macOS.
