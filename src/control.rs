//! Headless control plane shared by the terminal CLI and MCP server.
//!
//! UI adapters, the CLI, and MCP must call the same tool registry. This module
//! owns argument validation, agent session lifecycles, JSON result shapes, and
//! read-only inspection commands. It deliberately has no GPUI or AppKit
//! dependency.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::agent::{
    AgentPermissionDecision, AgentProfile, AgentProfileStore, AgentRuntime, AgentRuntimeSnapshot,
    AgentRuntimeState, diagnose_agents, new_agent_conversation_id, test_agent_connection,
};
use crate::fs::{EntryKind, SortKey, SortSpec, load_directory};
use crate::session::load_session;
use crate::ui_preferences::load_ui_preferences;

const DEFAULT_READY_TIMEOUT_MS: u64 = 20_000;
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 120_000;
const MAX_WAIT_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const DEFAULT_MAX_READ_BYTES: u64 = 1024 * 1024;
const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const CLOSE_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ControlError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ControlError {}

struct ControlSession {
    runtime: Arc<AgentRuntime>,
    profile_id: String,
    working_directory: String,
    context_paths: Vec<String>,
}

pub struct ControlPlane {
    profiles: Arc<AgentProfileStore>,
    sessions: HashMap<String, ControlSession>,
}

impl Default for ControlPlane {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlPlane {
    pub fn new() -> Self {
        Self {
            profiles: AgentProfileStore::new(),
            sessions: HashMap::new(),
        }
    }

    pub fn with_profile_store(profiles: Arc<AgentProfileStore>) -> Self {
        Self {
            profiles,
            sessions: HashMap::new(),
        }
    }

    pub fn execute(&mut self, tool: &str, arguments: Value) -> Result<Value, ControlError> {
        let arguments = normalize_arguments(arguments)?;
        match tool {
            "agent_profiles_list" => self.agent_profiles_list(),
            "agent_connection_test" => self.agent_connection_test(&arguments),
            "agent_session_start" => self.agent_session_start(&arguments),
            "agent_session_prompt" => self.agent_session_prompt(&arguments),
            "agent_session_status" => self.agent_session_status(&arguments),
            "agent_session_events" => self.agent_session_events(&arguments),
            "agent_session_wait" => self.agent_session_wait(&arguments),
            "agent_session_cancel" => self.agent_session_cancel(&arguments),
            "agent_session_permission" => self.agent_session_permission(&arguments),
            "agent_session_close" => self.agent_session_close(&arguments),
            "agent_run" => self.agent_run(&arguments),
            "agent_diagnostics" => self.agent_diagnostics(),
            "files_list" => self.files_list(&arguments),
            "files_stat" => self.files_stat(&arguments),
            "files_read_text" => self.files_read_text(&arguments),
            "app_state" => self.app_state(),
            _ => Err(ControlError::new(
                "unknown_tool",
                format!("unknown control tool: {tool}"),
            )),
        }
    }

    fn agent_profiles_list(&self) -> Result<Value, ControlError> {
        let snapshot = self.profiles.snapshot();
        serde_json::to_value(snapshot)
            .map_err(|error| ControlError::new("serialization", error.to_string()))
    }

    fn agent_connection_test(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let profile = self.select_profile(optional_string(arguments, "profile_id")?)?;
        let result = test_agent_connection(profile, None);
        Ok(json!({
            "success": result.success,
            "status": format!("{:?}", result.status),
            "message": result.message,
            "agent_name": result.agent_name,
            "agent_version": result.agent_version,
            "protocol_version": result.protocol_version,
            "capabilities": result.capabilities,
            "auth_methods": result.auth_methods,
            "elapsed_ms": result.elapsed_ms,
        }))
    }

    fn agent_session_start(
        &mut self,
        arguments: &Map<String, Value>,
    ) -> Result<Value, ControlError> {
        let profile = self.select_profile(optional_string(arguments, "profile_id")?)?;
        let working_directory = match optional_string(arguments, "working_directory")? {
            Some(path) => path.to_string(),
            None => std::env::current_dir()
                .map_err(|error| ControlError::new("cwd", error.to_string()))?
                .to_string_lossy()
                .into_owned(),
        };
        ensure_directory(&working_directory)?;
        let context_paths = string_array(arguments, "context_paths")?;
        let runtime = AgentRuntime::start(
            profile.clone(),
            working_directory.clone(),
            context_paths.clone(),
            Vec::new(),
        )
        .map_err(|error| ControlError::new("agent_start", error.to_string()))?;
        let session_id = new_agent_conversation_id();
        let snapshot = runtime.snapshot();
        self.sessions.insert(
            session_id.clone(),
            ControlSession {
                runtime,
                profile_id: profile.id.clone(),
                working_directory: working_directory.clone(),
                context_paths: context_paths.clone(),
            },
        );
        Ok(json!({
            "session_id": session_id,
            "profile_id": profile.id,
            "profile_name": profile.name,
            "working_directory": working_directory,
            "context_paths": context_paths,
            "snapshot": snapshot,
        }))
    }

    fn agent_session_prompt(
        &mut self,
        arguments: &Map<String, Value>,
    ) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let prompt = required_string(arguments, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(ControlError::invalid_args("prompt must not be empty"));
        }
        let context_paths = match optional_string_array(arguments, "context_paths")? {
            Some(paths) => paths,
            None => self.session(session_id)?.context_paths.clone(),
        };
        let wait_ready_ms =
            optional_u64(arguments, "wait_ready_ms")?.unwrap_or(DEFAULT_READY_TIMEOUT_MS);
        let session = self.session(session_id)?;
        let runtime = session.runtime.clone();
        ensure_ready(&runtime, wait_ready_ms)?;
        runtime
            .send_prompt(prompt.to_string(), context_paths)
            .map_err(|error| ControlError::new("agent_prompt", error.to_string()))?;
        thread::sleep(Duration::from_millis(80));
        Ok(json!({
            "session_id": session_id,
            "sent": true,
            "snapshot": runtime.snapshot(),
        }))
    }

    fn agent_session_status(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let session = self.session(session_id)?;
        Ok(json!({
            "session_id": session_id,
            "profile_id": session.profile_id,
            "working_directory": session.working_directory,
            "context_paths": session.context_paths,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_events(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let session = self.session(session_id)?;
        let events = session.runtime.drain_events();
        Ok(json!({
            "session_id": session_id,
            "events": events,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_wait(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let timeout_ms = optional_u64(arguments, "timeout_ms")?
            .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
            .min(MAX_WAIT_TIMEOUT_MS);
        let session = self.session(session_id)?;
        let snapshot = wait_for_terminal(&session.runtime, timeout_ms)?;
        let events = session.runtime.drain_events();
        Ok(json!({
            "session_id": session_id,
            "snapshot": snapshot,
            "events": events,
        }))
    }

    fn agent_session_cancel(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let session = self.session(session_id)?;
        session
            .runtime
            .cancel()
            .map_err(|error| ControlError::new("agent_cancel", error.to_string()))?;
        Ok(json!({
            "session_id": session_id,
            "cancel_requested": true,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_permission(
        &self,
        arguments: &Map<String, Value>,
    ) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let request_id = required_string(arguments, "request_id")?;
        let decision_text = required_string(arguments, "decision")?;
        let decision = match decision_text {
            "allow_once" => AgentPermissionDecision::AllowOnce,
            "allow_task" => AgentPermissionDecision::AllowTask,
            "reject_once" => AgentPermissionDecision::RejectOnce,
            "cancel" => AgentPermissionDecision::Cancel,
            other => {
                return Err(ControlError::invalid_args(format!(
                    "decision must be allow_once, allow_task, reject_once, or cancel; got {other}"
                )));
            }
        };
        let session = self.session(session_id)?;
        session
            .runtime
            .respond_permission(request_id.to_string(), decision)
            .map_err(|error| ControlError::new("agent_permission", error.to_string()))?;
        Ok(json!({
            "session_id": session_id,
            "request_id": request_id,
            "decision": decision_text,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_close(
        &mut self,
        arguments: &Map<String, Value>,
    ) -> Result<Value, ControlError> {
        let session_id = required_string(arguments, "session_id")?;
        let Some(session) = self.sessions.remove(session_id) else {
            return Err(session_not_found(session_id));
        };
        session.runtime.shutdown();
        let closed = wait_for_closed(&session.runtime, CLOSE_TIMEOUT_MS);
        Ok(json!({
            "session_id": session_id,
            "closed": closed,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_run(&mut self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let profile = self.select_profile(optional_string(arguments, "profile_id")?)?;
        let working_directory = match optional_string(arguments, "working_directory")? {
            Some(path) => path.to_string(),
            None => std::env::current_dir()
                .map_err(|error| ControlError::new("cwd", error.to_string()))?
                .to_string_lossy()
                .into_owned(),
        };
        ensure_directory(&working_directory)?;
        let context_paths = string_array(arguments, "context_paths")?;
        let prompt = required_string(arguments, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(ControlError::invalid_args("prompt must not be empty"));
        }
        let timeout_ms = optional_u64(arguments, "timeout_ms")?
            .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
            .min(MAX_WAIT_TIMEOUT_MS);
        let session_id = new_agent_conversation_id();
        let runtime = Arc::new(
            AgentRuntime::start(
                profile.clone(),
                working_directory.clone(),
                context_paths.clone(),
                Vec::new(),
            )
            .map_err(|error| ControlError::new("agent_start", error.to_string()))?,
        );

        let result = (|| {
            ensure_ready(&runtime, timeout_ms.clamp(1_000, DEFAULT_READY_TIMEOUT_MS))?;
            runtime
                .send_prompt(prompt.to_string(), context_paths.clone())
                .map_err(|error| ControlError::new("agent_prompt", error.to_string()))?;
            let snapshot = wait_for_terminal(&runtime, timeout_ms)?;
            let events = runtime.drain_events();
            if snapshot.state == AgentRuntimeState::Failed {
                return Err(ControlError::new(
                    "agent_failed",
                    snapshot
                        .error
                        .clone()
                        .unwrap_or_else(|| "agent run failed".to_string()),
                )
                .with_details(json!({
                    "session_id": session_id,
                    "snapshot": snapshot,
                    "events": events,
                })));
            }
            Ok((snapshot, events))
        })();

        runtime.shutdown();
        let closed = wait_for_closed(&runtime, CLOSE_TIMEOUT_MS);
        match result {
            Ok((mut snapshot, events)) => {
                snapshot.closed = closed;
                Ok(json!({
                    "session_id": session_id,
                    "profile_id": profile.id,
                    "working_directory": working_directory,
                    "context_paths": context_paths,
                    "snapshot": snapshot,
                    "events": events,
                }))
            }
            Err(error) => Err(error),
        }
    }

    fn agent_diagnostics(&self) -> Result<Value, ControlError> {
        serde_json::to_value(diagnose_agents())
            .map_err(|error| ControlError::new("serialization", error.to_string()))
    }

    fn files_list(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let path = match optional_string(arguments, "path")? {
            Some(path) => PathBuf::from(path),
            None => std::env::current_dir()
                .map_err(|error| ControlError::new("cwd", error.to_string()))?,
        };
        let sort = SortSpec {
            key: match optional_string(arguments, "sort_by")?.unwrap_or("name") {
                "name" => SortKey::Name,
                "modified" => SortKey::Modified,
                "kind" => SortKey::Kind,
                "size" => SortKey::Size,
                other => {
                    return Err(ControlError::invalid_args(format!(
                        "sort_by must be name, modified, kind, or size; got {other}"
                    )));
                }
            },
            ascending: optional_bool(arguments, "ascending")?.unwrap_or(true),
        };
        let snapshot =
            load_directory(&path, sort).map_err(|error| ControlError::new("files_list", error))?;
        let entries = snapshot
            .entries
            .iter()
            .map(file_entry_value)
            .collect::<Vec<_>>();
        Ok(json!({
            "path": snapshot.path,
            "entries": entries,
            "skipped": snapshot.skipped,
        }))
    }

    fn files_stat(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let path = PathBuf::from(required_string(arguments, "path")?);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| ControlError::new("files_stat", error.to_string()))?;
        Ok(json!({
            "path": path,
            "exists": true,
            "is_file": metadata.is_file(),
            "is_dir": metadata.is_dir(),
            "is_symlink": metadata.file_type().is_symlink(),
            "len": metadata.len(),
            "readonly": metadata.permissions().readonly(),
            "modified_unix_ms": metadata.modified().ok().and_then(system_time_ms),
        }))
    }

    fn files_read_text(&self, arguments: &Map<String, Value>) -> Result<Value, ControlError> {
        let path = PathBuf::from(required_string(arguments, "path")?);
        let max_bytes = optional_u64(arguments, "max_bytes")?
            .unwrap_or(DEFAULT_MAX_READ_BYTES)
            .min(MAX_READ_BYTES);
        let truncate = optional_bool(arguments, "truncate")?.unwrap_or(false);
        let metadata = fs::metadata(&path)
            .map_err(|error| ControlError::new("files_read_text", error.to_string()))?;
        if !metadata.is_file() {
            return Err(ControlError::new(
                "not_a_file",
                format!("{} is not a regular file", path.display()),
            ));
        }
        let file = fs::File::open(&path)
            .map_err(|error| ControlError::new("files_read_text", error.to_string()))?;
        let read_limit = max_bytes.saturating_add(1);
        let mut bytes = Vec::new();
        file.take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|error| ControlError::new("files_read_text", error.to_string()))?;
        let exceeded = bytes.len() as u64 > max_bytes;
        if exceeded {
            if !truncate {
                return Err(ControlError::new(
                    "file_too_large",
                    format!(
                        "{} is larger than the {} byte read limit",
                        path.display(),
                        max_bytes
                    ),
                )
                .with_details(json!({
                    "path": path,
                    "len": metadata.len(),
                    "max_bytes": max_bytes,
                })));
            }
            bytes.truncate(max_bytes as usize);
            while std::str::from_utf8(&bytes).is_err() && !bytes.is_empty() {
                bytes.pop();
            }
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            ControlError::new(
                "not_utf8",
                format!("{} does not contain valid UTF-8 text", path.display()),
            )
        })?;
        Ok(json!({
            "path": path,
            "text": text,
            "truncated": exceeded,
            "bytes_read": text.len(),
            "len": metadata.len(),
        }))
    }

    fn app_state(&self) -> Result<Value, ControlError> {
        let preferences = load_ui_preferences();
        Ok(json!({
            "session": load_session(),
            "ui_preferences": {
                "restore_session": preferences.restore_session,
                "appearance": preferences.appearance,
                "sidebar_visible": preferences.sidebar_visible,
                "preview_visible": preferences.preview_visible,
                "column_details_visible": preferences.column_details_visible,
                "date_column_visible": preferences.date_column_visible,
                "kind_column_visible": preferences.kind_column_visible,
                "size_column_visible": preferences.size_column_visible,
                "column_width": preferences.column_width,
                "git_auto_fetch": preferences.git_auto_fetch,
                "git_auto_fetch_minutes": preferences.git_auto_fetch_minutes,
                "default_column_rule": preferences.default_column_rule,
                "column_rules": preferences.column_rules,
                "git_branch_favorites": preferences.git_branch_favorites,
                "git_protected_patterns": preferences.git_protected_patterns,
            }
        }))
    }

    fn select_profile(&self, profile_id: Option<&str>) -> Result<AgentProfile, ControlError> {
        if let Some(profile_id) = profile_id {
            return self
                .profiles
                .get_profile(profile_id.to_string())
                .map_err(|error| ControlError::new("profile_store", error.to_string()))?
                .ok_or_else(|| {
                    ControlError::new(
                        "profile_not_found",
                        format!("profile not found: {profile_id}"),
                    )
                });
        }
        let snapshot = self.profiles.snapshot();
        snapshot.default_profile().cloned().ok_or_else(|| {
            ControlError::new("profile_not_configured", "no Agent profile is configured")
        })
    }

    fn session(&self, session_id: &str) -> Result<&ControlSession, ControlError> {
        self.sessions
            .get(session_id)
            .ok_or_else(|| session_not_found(session_id))
    }
}

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        spec(
            "agent_profiles_list",
            "List Agent profiles loaded by Kite, including the resolved default profile.",
            schema(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "agent_connection_test",
            "Run the ACP initialize handshake for a profile without starting a prompt.",
            schema(
                json!({
                    "profile_id": { "type": "string", "description": "Profile id; uses the default when omitted." }
                }),
                &[],
            ),
            true,
            false,
            true,
        ),
        spec(
            "agent_session_start",
            "Start a persistent ACP Agent session. Sessions live in the MCP server or CLI batch process.",
            schema(
                json!({
                    "profile_id": { "type": "string" },
                    "working_directory": { "type": "string" },
                    "context_paths": { "type": "array", "items": { "type": "string" } }
                }),
                &[],
            ),
            false,
            false,
            true,
        ),
        spec(
            "agent_session_prompt",
            "Wait for an Agent session to become ready, then send a prompt and optional context paths.",
            schema(
                json!({
                    "session_id": { "type": "string" },
                    "prompt": { "type": "string" },
                    "context_paths": { "type": "array", "items": { "type": "string" } },
                    "wait_ready_ms": { "type": "integer", "minimum": 1 }
                }),
                &["session_id", "prompt"],
            ),
            false,
            false,
            true,
        ),
        spec(
            "agent_session_status",
            "Read the current Agent runtime snapshot and session metadata.",
            schema(
                json!({ "session_id": { "type": "string" } }),
                &["session_id"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "agent_session_events",
            "Drain incremental Agent events since the previous poll.",
            schema(
                json!({ "session_id": { "type": "string" } }),
                &["session_id"],
            ),
            false,
            false,
            false,
        ),
        spec(
            "agent_session_wait",
            "Wait for an Agent session to reach a terminal state and return the remaining events.",
            schema(
                json!({
                    "session_id": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                }),
                &["session_id"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "agent_session_cancel",
            "Request cancellation of the active Agent prompt.",
            schema(
                json!({ "session_id": { "type": "string" } }),
                &["session_id"],
            ),
            false,
            true,
            false,
        ),
        spec(
            "agent_session_permission",
            "Respond to an Agent permission request with allow_once, allow_task, reject_once, or cancel.",
            schema(
                json!({
                    "session_id": { "type": "string" },
                    "request_id": { "type": "string" },
                    "decision": { "type": "string", "enum": ["allow_once", "allow_task", "reject_once", "cancel"] }
                }),
                &["session_id", "request_id", "decision"],
            ),
            false,
            true,
            true,
        ),
        spec(
            "agent_session_close",
            "Close an Agent session and terminate its complete child process tree.",
            schema(
                json!({ "session_id": { "type": "string" } }),
                &["session_id"],
            ),
            false,
            true,
            false,
        ),
        spec(
            "agent_run",
            "Start, prompt, wait for completion, and close an Agent session in one call. Best for CLI end-to-end debugging.",
            schema(
                json!({
                    "profile_id": { "type": "string" },
                    "working_directory": { "type": "string" },
                    "context_paths": { "type": "array", "items": { "type": "string" } },
                    "prompt": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                }),
                &["prompt"],
            ),
            false,
            false,
            true,
        ),
        spec(
            "agent_diagnostics",
            "Return profile installation state, redacted recent logs, and Agent diagnostic records.",
            schema(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "files_list",
            "List a directory using the same core reader as the Kite file browser.",
            schema(
                json!({
                    "path": { "type": "string" },
                    "sort_by": { "type": "string", "enum": ["name", "modified", "kind", "size"] },
                    "ascending": { "type": "boolean" }
                }),
                &[],
            ),
            true,
            false,
            false,
        ),
        spec(
            "files_stat",
            "Read filesystem metadata for one path.",
            schema(json!({ "path": { "type": "string" } }), &["path"]),
            true,
            false,
            false,
        ),
        spec(
            "files_read_text",
            "Read a bounded UTF-8 text file for Agent context or binary-free diagnostics.",
            schema(
                json!({
                    "path": { "type": "string" },
                    "max_bytes": { "type": "integer", "minimum": 1 },
                    "truncate": { "type": "boolean" }
                }),
                &["path"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "app_state",
            "Read persisted Kite tabs and UI preferences without opening a window.",
            schema(json!({}), &[]),
            true,
            false,
            false,
        ),
    ]
}

fn spec(
    name: &str,
    description: &str,
    input_schema: Value,
    read_only: bool,
    destructive: bool,
    open_world: bool,
) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: json!({
            "title": name,
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": read_only,
            "openWorldHint": open_world,
        }),
    }
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn normalize_arguments(arguments: Value) -> Result<Map<String, Value>, ControlError> {
    match arguments {
        Value::Null => Ok(Map::new()),
        Value::Object(arguments) => Ok(arguments),
        _ => Err(ControlError::invalid_args(
            "tool arguments must be a JSON object",
        )),
    }
}

fn required_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, ControlError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ControlError::invalid_args(format!("{key} must be a non-empty string")))
}

fn optional_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, ControlError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim())),
        Some(_) => Err(ControlError::invalid_args(format!(
            "{key} must be a string"
        ))),
    }
}

fn optional_bool(arguments: &Map<String, Value>, key: &str) -> Result<Option<bool>, ControlError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ControlError::invalid_args(format!(
            "{key} must be a boolean"
        ))),
    }
}

fn optional_u64(arguments: &Map<String, Value>, key: &str) -> Result<Option<u64>, ControlError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| ControlError::invalid_args(format!("{key} must be a positive integer"))),
    }
}

fn string_array(arguments: &Map<String, Value>, key: &str) -> Result<Vec<String>, ControlError> {
    optional_string_array(arguments, key).map(Option::unwrap_or_default)
}

fn optional_string_array(
    arguments: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, ControlError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    ControlError::invalid_args(format!("{key} must contain only strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(ControlError::invalid_args(format!(
            "{key} must be an array of strings"
        ))),
    }
}

fn ensure_directory(path: &str) -> Result<(), ControlError> {
    if Path::new(path).is_dir() {
        Ok(())
    } else {
        Err(ControlError::new(
            "working_directory_unavailable",
            format!("working directory is unavailable: {path}"),
        ))
    }
}

fn ensure_ready(runtime: &AgentRuntime, timeout_ms: u64) -> Result<(), ControlError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(MAX_WAIT_TIMEOUT_MS));
    loop {
        let snapshot = runtime.snapshot();
        match snapshot.state {
            AgentRuntimeState::Ready
            | AgentRuntimeState::Completed
            | AgentRuntimeState::Cancelled => return Ok(()),
            AgentRuntimeState::Failed => {
                return Err(ControlError::new(
                    "agent_failed",
                    snapshot
                        .error
                        .clone()
                        .unwrap_or_else(|| "Agent failed before it became ready".to_string()),
                )
                .with_details(json!(snapshot)));
            }
            AgentRuntimeState::Running | AgentRuntimeState::WaitingPermission => {
                return Err(ControlError::new(
                    "agent_busy",
                    "Agent session is already running a prompt",
                )
                .with_details(json!(snapshot)));
            }
            AgentRuntimeState::Starting
            | AgentRuntimeState::Initializing
            | AgentRuntimeState::Cancelling
            | AgentRuntimeState::Closed => {}
        }
        if Instant::now() >= deadline {
            return Err(ControlError::new(
                "agent_ready_timeout",
                format!("Agent did not become ready within {timeout_ms} ms"),
            )
            .with_details(json!(snapshot)));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_terminal(
    runtime: &AgentRuntime,
    timeout_ms: u64,
) -> Result<AgentRuntimeSnapshot, ControlError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(MAX_WAIT_TIMEOUT_MS));
    loop {
        let snapshot = runtime.snapshot();
        if matches!(
            snapshot.state,
            AgentRuntimeState::Completed
                | AgentRuntimeState::Failed
                | AgentRuntimeState::Cancelled
                | AgentRuntimeState::Closed
        ) {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(ControlError::new(
                "agent_wait_timeout",
                format!("Agent did not finish within {timeout_ms} ms"),
            )
            .with_details(json!(snapshot)));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_closed(runtime: &AgentRuntime, timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if runtime.is_closed() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn session_not_found(session_id: &str) -> ControlError {
    ControlError::new(
        "session_not_found",
        format!("Agent session not found: {session_id}"),
    )
}

fn file_entry_value(entry: &crate::fs::FileEntry) -> Value {
    json!({
        "path": entry.path,
        "name": entry.name,
        "is_dir": entry.kind == EntryKind::Directory,
        "type": entry.type_label(),
        "size": entry.size,
        "modified_unix_ms": entry.modified.and_then(system_time_ms),
        "extension": entry.extension,
        "git_status": entry.git_status.map(|status| status.code()),
        "svn_status": entry.svn_status.map(|status| status.code()),
    })
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_are_unique_and_have_object_schemas() {
        let specs = tool_specs();
        let mut names = std::collections::HashSet::new();
        for spec in &specs {
            assert!(names.insert(spec.name.clone()), "duplicate {}", spec.name);
            assert_eq!(spec.input_schema["type"], "object");
        }
        assert!(names.contains("agent_run"));
        assert!(names.contains("agent_session_prompt"));
        assert!(names.contains("files_read_text"));
    }
}
