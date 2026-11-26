# Kite v0.6.96

macOS parity: not required - this hotfix changes only Windows update extraction, shared updater tests, and release tooling.

## Windows updater repair

- Replace Windows PowerShell `Expand-Archive` with the stable two-argument `System.IO.Compression.ZipFile.ExtractToDirectory` API.
- Fix the `找不到“ExtractToDirectory”的重载，参数计数为“3”` update failure seen on some Windows .NET Framework installations.
- Add a Windows PowerShell extraction regression test and run updater tests in Windows CI.

If Kite v0.6.93–v0.6.95 currently shows this error, download `Kite-v0.6.96-windows-x64.zip` or `Kite.exe` manually and run it once. Later updates use the repaired extraction path.

## Included improvements

- Legacy GBK/CP936 ZIP filename recovery, Agent working-directory policies, stricter Agent permission handling, and terminal-state preservation from v0.6.95.

## Install

- Windows: use `Kite-v0.6.96-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.96-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
