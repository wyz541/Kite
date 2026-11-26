# Kite v0.6.95

## Agent permission policy

- Enforce read-only Profile permissions for file writes, commands, and network access.
- Honor the Agent-provided permission option kind and disable unsupported authorization scopes instead of silently substituting another choice.
- Gate write confirmation on the Profile's declared capabilities and keep Agent-supplied secrets redacted from diagnostics and errors.
- Keep permission presentation and recovery behavior aligned between Windows and macOS.

## Agent working directory

- Resolve `SelectedItem`, `FileParent`, `RepositoryRoot`, `Fixed`, and `Ask` working-directory policies in the shared Rust Core.
- Use the same resolved directory for the UI preview, Core API sessions, and the actual Agent child-process working directory.
- Block invalid fixed and repository paths with an explicit recovery state instead of silently starting in the wrong directory.
- Keep the pre-send `Ask` directory picker and policy behavior aligned between Windows and macOS.

## Agent runtime reliability

- Preserve completed, failed, and cancelled states after the Agent process shuts down instead of replacing the terminal result with a generic closed state.

## Archive compatibility

- Restore legacy GBK/CP936 file and directory names when extracting ZIP archives that omit the UTF-8 filename flag.
- Preserve UTF-8 filenames, Unicode Path extra fields, ASCII names, and ordinary CP437 names.
- Apply the decoding in the shared Rust Core so Windows and macOS use the same archive behavior.

## Install

- Windows: use `Kite-v0.6.95-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.95-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
