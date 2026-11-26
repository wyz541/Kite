# Kite v0.6.94

## Agent integration

- Add a native Agent settings page on Windows and macOS with profile management, default Agent selection, connection tests, permission summaries, and verified Claude Code / Codex ACP presets.
- Open an Agent from the file or folder context menu with a fixed context snapshot, explicit working directory, and a prefilled prompt that is not sent automatically.
- Add a right-side Agent panel and an independent Agent window with model, workspace, permission, connection, and context status.
- Support ACP / JSON-RPC over stdio handshake, session creation, streamed Markdown, stop, retry, copy, long-output protection, and structured error recovery.
- Show permission requests with the Agent-provided option labels and exact allow-once, allow-always, reject-once, and reject-always choices. Missing option kinds are disabled instead of falling back to a different authorization scope.
- Review structured file changes as a unified diff before applying or saving them. Files changed after review are not overwritten without a new confirmation.
- Optionally restore the Agent, model, working directory, context snapshot, conversation, draft, and independent window size. Missing paths expose a repair path and never block Kite startup.
- Add a diagnostics view for command resolution, version, ACP protocol, capabilities, recent errors, configuration paths, and redacted logs.
- Add cross-platform ACP fixtures and automated coverage for streaming output, permission requests, cancellation, timeout, oversized output, retry, process-tree cleanup, and credential redaction.
- Windows acceptance fixes: submit the session prompt after ACP readiness, keep terminal status text aligned with completed/cancelled/failed/closed states, and defer opening the shared Agent popout until the panel update has finished so close-and-reopen no longer double-leases the panel.
- Add headless Agent acceptance through `kite-cli agent run` / `kite-cli batch` and `kite-mcp`, covering real ACP runs, permission rejection, cancellation, structured errors, and process cleanup without GUI automation.
- Replace the deprecated Zed Codex adapter preset with `@agentclientprotocol/codex-acp 2.12.1`.

## Security boundary

- P0 is read-only by default. Kite denies write, command, and network allow decisions for read-only Profiles, gates confirm-write decisions by declared capabilities, applies file edits only after explicit diff review, never executes suggested shell commands, and never suppresses permission requests.
- Secrets are not stored in `agent.json`; profiles keep only credential references and rely on the Agent's native authentication where applicable.
- External Agents are not sandboxed by Kite. The UI states when an Agent's own permission system remains authoritative.
- Diagnostics redact Bearer tokens, API keys, passwords, URL credentials, and common `sk-*` secrets.

## Performance and parity

- Add startup latency, Agent process memory, streamed-output throughput, and long-conversation preparation baselines.
- Keep configuration, context, permission, cancellation, error, and recovery semantics aligned between Windows and macOS while retaining native UI and process cleanup.

## Update reliability

- Make macOS update downloads use the configured system HTTPS, HTTP, or SOCKS5 proxy before falling back to a direct connection.
- Retry transient GitHub asset `403` responses with a fresh release download redirect.
- Record download failures in the updater diagnostic log so Settings can expose actionable troubleshooting information.

## Archive compatibility

- Restore legacy GBK/CP936 file and directory names when extracting ZIP archives that omit the UTF-8 filename flag, while preserving UTF-8, Unicode Path extra-field, ASCII, and ordinary CP437 names.

## Known limitations

- Kite does not bundle, download, or log in an Agent. Users must install and authenticate the external Agent first.
- The built-in preset list is intentionally limited to verified ACP adapters. Other integrations remain custom profiles.
- Multi-Agent parallel comparison, cloud sync, team-shared profiles, and automatic Agent upgrades are out of scope.
- Shell commands proposed by an Agent are displayed as text for manual review and are never executed automatically.

## Install

- Windows: use `Kite-v0.6.94-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.94-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
