# Kite v0.6.94

## Agent integration

- Add a native Agent settings page on Windows and macOS with profile management, default Agent selection, connection tests, permission summaries, and verified Claude Code / Codex ACP presets.
- Open an Agent from the file or folder context menu with a fixed context snapshot, explicit working directory, and a prefilled prompt that is not sent automatically.
- Add a right-side Agent panel and an independent Agent window with model, workspace, permission, connection, and context status.
- Support ACP / JSON-RPC over stdio handshake, session creation, streamed Markdown, stop, retry, copy, long-output protection, and structured error recovery.
- Show permission requests with explicit allow-once, allow-for-task, and reject choices. A rejection is returned to the Agent instead of leaving the task blocked.
- Optionally restore the Agent, model, working directory, context snapshot, conversation, draft, and independent window size. Missing paths expose a repair path and never block Kite startup.
- Add a diagnostics view for command resolution, version, ACP protocol, capabilities, recent errors, configuration paths, and redacted logs.
- Add cross-platform ACP fixtures and automated coverage for streaming output, permission requests, cancellation, timeout, oversized output, retry, process-tree cleanup, and credential redaction.

## Security boundary

- P0 is read-only by default. Kite does not automatically apply Agent edits, execute suggested shell commands, or suppress permission requests.
- Secrets are not stored in `agent.json`; profiles keep only credential references and rely on the Agent's native authentication where applicable.
- External Agents are not sandboxed by Kite. The UI states when an Agent's own permission system remains authoritative.
- Diagnostics redact Bearer tokens, API keys, passwords, URL credentials, and common `sk-*` secrets.

## Performance and parity

- Add startup latency, Agent process memory, streamed-output throughput, and long-conversation preparation baselines.
- Keep configuration, context, permission, cancellation, error, and recovery semantics aligned between Windows and macOS while retaining native UI and process cleanup.

## Known limitations

- Structured diff review and in-app file application are not part of this P0 release; Kite remains read-only and only copies or locates Agent results.
- Kite does not bundle, download, or log in an Agent. Users must install and authenticate the external Agent first.
- The built-in preset list is intentionally limited to verified ACP adapters. Other integrations remain custom profiles.
- Multi-Agent parallel comparison, cloud sync, team-shared profiles, and automatic Agent upgrades are out of scope.
- Shell commands proposed by an Agent are displayed as text for manual review and are never executed automatically.

## Install

- Windows: use `Kite-v0.6.94-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.94-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
