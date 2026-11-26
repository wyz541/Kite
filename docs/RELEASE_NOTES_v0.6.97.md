# Kite v0.6.97

## Agent integration

- Add a GUI-independent Agent acceptance path through `kite-cli agent run`, `kite-cli batch`, and the `kite-mcp` stdio server.
- Verify real Codex ACP and Claude Code ACP sessions end to end, including streaming output, permission rejection, cancellation, structured errors, session close, and process-tree cleanup.
- Replace the deprecated `@zed-industries/codex-acp` preset with `@agentclientprotocol/codex-acp 2.12.1`.
- Harden streamed credential redaction, including secrets split across chunks and terminal-state flushing.
- Record Windows and macOS real-Agent acceptance evidence, including the Windows Codex ACP completion state.

The CLI/MCP acceptance does not replace the remaining UI-only checks for Windows auto-follow/manual-scroll pause and the complete Windows/macOS keyboard focus sequence.

## XMind and mind maps

- Add a lossless Rust core for reading and atomically saving modern XMind packages while preserving unknown ZIP entries, unknown JSON fields, topic order, and unchanged fixture bytes.
- Open XMind files in an independent macOS editor window with a topic-tree canvas, selection, pan, zoom, fit, title editing, and basic structure actions.
- Add keyboard editing and outline navigation, `.xmind` file association, `Command+Shift+M`, and a bundled capability fixture.
- Keep edits behind source-hash validation so external file changes are not overwritten silently.

## Platform parity

- Keep Agent profile, permission, working-directory, cancellation, recovery, and transport semantics shared through the Rust Core.
- Keep XMind package parsing and save validation in the shared Rust Core; native Windows XMind editing parity remains tracked by the feature issue.

## Verification

- Windows GPUI release build and package checks.
- macOS SwiftUI and AppKit release build and package checks.
- Shared Rust tests, Clippy, feature matrix, platform parity, and release parity checks.

## Known limitations

- The macOS XMind editor is a first usable editing slice, not full XMind parity; it currently displays the first sheet and supports title plus basic tree-structure edits.
- Rich text, notes, markers, relationships, boundaries, and multi-sheet editing are not yet available.
- Windows native XMind editor parity is not included in this release.
- External Agent authentication, quota, and process permissions remain owned by the Agent runtime and the operating system.

## Install

- Windows: use `Kite-v0.6.97-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.97-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
