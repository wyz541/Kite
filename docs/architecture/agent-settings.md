# Agent settings and credential boundary

- Status: implemented for Windows and macOS
- Tracking: GitHub #152
- Scope: Agent profile CRUD, default selection, connection testing, credential references

## Product boundary

Kite is an Agent host. The settings page stores only the minimum information needed to launch or connect to an external Agent. It does not ingest the Agent's native configuration, manage model credentials for the Agent, or claim that general CLI processes are sandboxed by Kite.

The initial connection modes are:

| Mode | Settings behavior | Connection test |
| --- | --- | --- |
| ACP / JSON-RPC over stdio | Saves executable, arguments, environment, model and bounds | Starts the process, sends ACP `initialize`, validates protocol version 1 and reports declared capabilities/auth methods |
| General CLI preset | Hides unsupported ACP-only fields | Starts the executable with `--version`; always labels permissions as managed by the Agent |

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

`AgentTransport`, `WorkingDirectoryPolicy` and `PermissionPolicy` are platform-neutral enums. Both platforms render the same labels and error semantics from the shared enums.

## Persistence

The configuration file is `agent.json` under the platform config directory (`Kite/agent.json` by default). `KITE_AGENT_CONFIG_PATH` can override it for tests or diagnostics.

- schema version: `1`
- maximum file size: 1 MiB
- maximum profiles: 64
- writes are atomic
- the previous valid file is copied to `agent.json.bak`
- a corrupt primary falls back to the backup, then to an empty configuration with a visible warning
- Unix builds set user-only file permissions

Name uniqueness and all field validation are enforced in Rust so the two views cannot drift.

## Credentials

Tokens and passwords are intentionally absent from `AgentProfile` and `agent.json`.

When a user enters a credential, Core writes it to the platform keychain under service `com.kite.agent` and account `<profile-id>`. Only the reference is persisted. Deleting a profile removes its keychain entry on a best-effort basis. Connection tests can use an unsaved form credential without storing it, or resolve an existing saved credential by reference.

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

The ACP probe sends the required `initialize` request with protocol version 1 and a Kite client identity. Messages are newline-delimited JSON-RPC. The process is started without a console window on Windows, has a bounded response size, and is terminated after the probe. Error text is redacted before it reaches the UI.

## UI behavior

Both native settings pages provide:

- a same-level Agent pane
- an empty state with add and preset entry points
- default-Agent summary, permission summary and effective working-directory label
- add, edit, copy, delete and set-default actions
- a test action for saved profiles and the current unsaved form
- unsaved-change protection when leaving the form or closing settings
- a replacement selector before deleting the default profile
- native keyboard focus and accessibility labels

The right-click menu and Agent sidebar are delivered by later issues. This page is the shared source of truth those surfaces will read.
