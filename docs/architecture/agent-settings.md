# Agent settings and credential boundary

- Status: implemented for Windows and macOS
- Tracking: GitHub #152, #175
- Scope: Agent profile CRUD, default selection, connection testing, credential references

## Product boundary

Kite is an Agent host. The settings page stores only the minimum information needed to launch or connect to an external Agent. It does not ingest the Agent's native configuration, manage model credentials for the Agent, or claim that general CLI processes are sandboxed by Kite.

The initial connection modes and their support matrix are:

| Mode | Save / default | Session runtime | Connection test |
| --- | --- | --- | --- |
| ACP / JSON-RPC over stdio | Supported | Supported | Starts the process, sends ACP `initialize`, validates protocol version 1 and reports declared capabilities/auth methods |
| ACP HTTP | Connection-test-only | Not available in this release | Validates status, JSON-RPC protocol version and declared capabilities; saved credentials are sent as a bearer token |
| General CLI | Connection-test-only | Not available in this release | Starts the executable with `--version`; always labels permissions as managed by the Agent |

`AgentTransport::capabilities()` is the single source of truth for these flags.
The Rust runtime, Core API, Windows settings, macOS settings, context menu and
Agent panel all read the same matrix. A successful connection probe never
implies that a transport can start a session.

Connection-test-only modes are available from the settings form, but the form
disables Save and explains how to migrate to ACP stdio. Profiles created by an
older build with a connection-test-only transport are retained, visibly marked
`仅测试`, excluded from default selection and never exposed as runnable context
actions.

## Shared profile model

Windows and macOS use the Rust Core types exported through UniFFI. A profile contains:

- stable ID and unique display name
- transport, executable and optional fields reserved for future transports
- arguments and non-sensitive environment variables
- optional model name
- working-directory policy and fixed directory when applicable
- permission policy and visible summary
- timeout and output limit
- optional credential environment-variable name
- optional `keychain://com.kite.agent/<profile-id>` reference
- explicit capability flags for selection/workspace reads, writes, commands, network and permission requests

`AgentTransport`, `WorkingDirectoryPolicy` and `PermissionPolicy` are platform-neutral enums. Both platforms render the same labels and error semantics from the shared enums. `KiteCore.agentTransportCapabilities()` exposes the transport matrix to the macOS UI, while the Windows UI calls the same Rust methods directly.

## Persistence

The configuration file is `agent.json` under the platform config directory (`Kite/agent.json` by default). `KITE_AGENT_PROFILE_PATH` can override it for tests or diagnostics.

- schema version: `1`
- maximum file size: 256 KiB
- maximum profiles: 64
- writes use a same-directory temporary file and atomic replacement
- process-local reads and writes are serialized across store instances
- corrupt, oversized or future-schema files yield an empty snapshot with a typed warning instead of blocking startup
- strict CRUD operations return a typed error rather than overwriting an unreadable file

Name uniqueness, field validation, the runnable-transport save guard and default selection are enforced in Rust so the two views cannot drift. A default is selected only from session-capable profiles; if none exist, `default_profile_id` is `None`.

## Credentials

Tokens and passwords are intentionally absent from `AgentProfile` and `agent.json`.

When a user enters a credential, Core writes it to the platform keychain under service `com.kite.agent` and account `<profile-id>`. Only the reference is persisted. Deleting a profile removes its keychain entry on a best-effort basis. Connection tests can use an unsaved form credential without storing it, or resolve an existing saved credential by reference. Saving with a new credential replaces the old secret; leaving the credential field blank preserves the saved entry.

Validation rejects:

- environment-variable names that look like secrets (`TOKEN`, `PASSWORD`, `SECRET`, `API_KEY`, and similar)
- command arguments such as `--token=...`, `--password=...` and `--secret=...`
- URLs containing userinfo credentials

## Connection test semantics

The connection test never persists form values. It returns structured statuses that both platforms display:

- connected
- authentication required
- not installed
- incompatible ACP version
- authentication failed
- protocol error
- permission denied
- timeout
- output limit exceeded
- invalid profile
- credential-store error
- process startup failure
- other

The ACP probe sends the required `initialize` request with protocol version 1 and a Kite client identity. Stdio messages are newline-delimited JSON-RPC; HTTP uses a standard JSON request. The process is started without a console window on Windows, has a bounded response size, and is terminated after the probe. Error text is redacted before it reaches the UI.

## UI behavior

Both native settings pages provide:

- a same-level Agent pane
- an empty state with add and connection-test entry points
- default-Agent summary, permission summary and effective working-directory label
- add, edit, copy, delete and set-default actions for runnable profiles
- a test-only action for ACP HTTP and general CLI without offering a saveable runnable profile
- explicit `仅测试` markers and remediation text for connection-test-only modes
- unsaved-change protection when leaving the form or closing settings
- a replacement selector before deleting a default when another runnable profile exists
- native keyboard focus and accessibility labels

The right-click menu and Agent surface read the same shared store, so profile and default changes are immediately visible to those entry points. Until a runnable default profile exists, context actions stay disabled while “Configure Agent…” remains available; saving or changing the default enables them without reopening the app. Core CLI and MCP return `unsupported_transport` with the same recovery text when a caller explicitly selects a connection-test-only profile for a session.
