# Kite v0.6.95

## Agent permission policy

- Enforce read-only Profile permissions for file writes, commands, and network access.
- Honor the Agent-provided permission option kind and disable unsupported authorization scopes instead of silently substituting another choice.
- Gate write confirmation on the Profile's declared capabilities and keep Agent-supplied secrets redacted from diagnostics and errors.
- Keep permission presentation and recovery behavior aligned between Windows and macOS.

## Archive compatibility

- Restore legacy GBK/CP936 file and directory names when extracting ZIP archives that omit the UTF-8 filename flag.
- Preserve UTF-8 filenames, Unicode Path extra fields, ASCII names, and ordinary CP437 names.
- Apply the decoding in the shared Rust Core so Windows and macOS use the same archive behavior.

## Install

- Windows: use `Kite-v0.6.95-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.95-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
