# Kite v0.6.79

Kite v0.6.79 makes update downloads survive temporary GitHub connection failures and interrupted transfers.

## Update downloads

- Use environment proxies, the Windows system proxy, and direct access when downloading updates.
- Support HTTP and SOCKS5 proxies without changing the trusted GitHub release URL.
- Retry transient HTTP and transport failures.
- Resume interrupted `.part` downloads with HTTP Range instead of starting over.
- Keep raw connection and TLS diagnostics out of the update UI.
- Preserve mandatory SHA-256 verification before installation.

## Verification

- Rust formatting check.
- Rust Core tests.
- Windows Clippy check.
- Windows GPUI build.
- Linux checks.
- Platform parity.
- Startup performance.
- Live GitHub update download and SHA-256 verification.
- Dead-proxy fallback verification.
- Windows and macOS release builds.

## Install

- Windows: use `Kite-v0.6.79-windows-x64.zip`.
- Windows direct executable: use `Kite.exe`.
- macOS: use `Kite-v0.6.79-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.

## Platform note

This release updates shared updater networking on Windows and macOS.
macOS parity: not required
