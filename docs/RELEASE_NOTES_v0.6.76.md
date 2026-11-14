# Kite v0.6.76

Kite v0.6.76 fixes automatic update checks for releases returned by the GitHub API.

## Automatic updates

- Accept GitHub Release asset URLs without case-sensitive owner or repository names.
- Keep host, owner, repository path, asset name, and SHA-256 checks.
- Restore automatic update resolution when GitHub returns `/wyz541/Kite/` in asset URLs.
- Add regression coverage for mixed-case Release URLs.
- Validate the fix against the live GitHub Release API.

## Verification

- Rust formatting check.
- Rust Core tests.
- Windows Clippy check.
- Windows GPUI tests.
- Live GitHub Release API test.
- Windows debug and release builds.
- macOS SwiftUI and AppKit release build.
- Linux checks.
- Platform parity.

## Install

- Windows: use `Kite-v0.6.76-windows-x64.zip`.
- Windows direct executable: use `Kite.exe`.
- macOS: use `Kite-v0.6.76-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

## Platform note

This release fixes shared updater URL validation.
macOS receives the shared version bump and release package.
macOS parity: not required
