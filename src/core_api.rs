//! UI-independent Core API shared by the GUI, CLI, and MCP adapters.
//!
//! UI shells, the CLI, and MCP must call the same business API. This module
//! owns execution context, typed errors, idempotency, tool contracts, and
//! Agent session lifecycles. It deliberately has no GPUI or AppKit dependency.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::agent::{
    AgentPermissionDecision, AgentProfile, AgentProfileStore, AgentRuntime, AgentRuntimeSnapshot,
    AgentRuntimeState, AgentWorkingDirectoryStatus, WorkingDirectoryPolicy,
    agent_transport_capabilities, diagnose_agents, new_agent_conversation_id,
    resolve_profile_working_directory, resolve_working_directory_for_context,
    test_agent_connection,
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
static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpec {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreErrorKind {
    #[default]
    Internal,
    InvalidInput,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    PreconditionFailed,
    External,
    Timeout,
    Cancelled,
}

impl CoreErrorKind {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::InvalidInput => 2,
            Self::Unauthorized | Self::Forbidden => 3,
            Self::NotFound => 4,
            Self::Conflict | Self::PreconditionFailed => 5,
            Self::External => 6,
            Self::Timeout => 7,
            Self::Cancelled => 130,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CoreError {
    pub kind: CoreErrorKind,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<Value>>,
}

impl CoreError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let kind = infer_error_kind(&code);
        Self {
            kind,
            code,
            message: message.into(),
            retryable: matches!(
                kind,
                CoreErrorKind::Conflict | CoreErrorKind::External | CoreErrorKind::Timeout
            ),
            recovery: None,
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    pub fn with_recovery(mut self, recovery: impl Into<String>) -> Self {
        self.recovery = Some(Box::from(recovery.into()));
        self
    }

    pub fn with_kind(mut self, kind: CoreErrorKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message).with_kind(CoreErrorKind::InvalidInput)
    }

    pub fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message).with_kind(CoreErrorKind::Forbidden)
    }

    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message).with_kind(CoreErrorKind::NotFound)
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message).with_kind(CoreErrorKind::Conflict)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new("timeout", message)
            .with_kind(CoreErrorKind::Timeout)
            .retryable(true)
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}/{}: {}", self.kind, self.code, self.message)
    }
}

impl std::error::Error for CoreError {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExecutionContext {
    pub actor: Option<String>,
    pub trace_id: String,
    pub deadline_unix_ms: Option<u64>,
    pub idempotency_key: Option<String>,
    pub read_only: bool,
    pub confirmed: bool,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self {
            actor: std::env::var("KITE_ACTOR")
                .ok()
                .filter(|value| !value.is_empty()),
            trace_id: std::env::var("KITE_TRACE_ID")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(new_trace_id),
            deadline_unix_ms: None,
            idempotency_key: None,
            read_only: std::env::var("KITE_READ_ONLY")
                .ok()
                .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes")),
            confirmed: std::env::var("KITE_CONFIRM")
                .ok()
                .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes")),
        }
    }
}

impl ExecutionContext {
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.deadline_unix_ms = Some(unix_ms().saturating_add(timeout_ms));
        self
    }

    pub fn remaining_ms(&self, default_ms: u64) -> Result<u64, CoreError> {
        let Some(deadline) = self.deadline_unix_ms else {
            return Ok(default_ms);
        };
        let remaining = deadline.saturating_sub(unix_ms());
        if remaining == 0 {
            return Err(CoreError::timeout("execution deadline has expired"));
        }
        Ok(default_ms.min(remaining))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CoreResult {
    pub trace_id: String,
    pub data: Value,
    pub state: Option<Value>,
    pub changed: bool,
    pub idempotent_replay: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_point: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "tool", content = "arguments", rename_all = "snake_case")]
pub enum CoreRequest {
    AgentProfilesList,
    AgentConnectionTest {
        #[serde(default)]
        profile_id: Option<String>,
    },
    AgentWorkingDirectoryResolve {
        #[serde(default)]
        profile_id: Option<String>,
        primary_path: String,
        #[serde(default)]
        selected_working_directory: Option<String>,
    },
    AgentSessionStart {
        #[serde(default)]
        profile_id: Option<String>,
        #[serde(default)]
        working_directory: Option<String>,
        #[serde(default)]
        context_paths: Vec<String>,
        #[serde(default)]
        restored_messages: Vec<crate::agent::AgentMessage>,
    },
    AgentSessionPrompt {
        session_id: String,
        prompt: String,
        #[serde(default)]
        context_paths: Option<Vec<String>>,
        #[serde(default)]
        wait_ready_ms: Option<u64>,
    },
    AgentSessionStatus {
        session_id: String,
    },
    AgentSessionEvents {
        session_id: String,
    },
    AgentSessionWait {
        session_id: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    AgentSessionCancel {
        session_id: String,
    },
    AgentSessionPermission {
        session_id: String,
        request_id: String,
        decision: String,
    },
    AgentSessionClose {
        session_id: String,
    },
    AgentRun {
        #[serde(default)]
        profile_id: Option<String>,
        #[serde(default)]
        working_directory: Option<String>,
        #[serde(default)]
        context_paths: Vec<String>,
        prompt: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    AgentDiagnostics,
    FilesList {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        sort_by: Option<String>,
        #[serde(default)]
        ascending: Option<bool>,
    },
    FilesStat {
        path: String,
    },
    FilesReadText {
        path: String,
        #[serde(default)]
        max_bytes: Option<u64>,
        #[serde(default)]
        truncate: Option<bool>,
    },
    AppState,
}

impl CoreRequest {
    pub fn into_parts(self) -> (&'static str, Value) {
        match self {
            Self::AgentProfilesList => ("agent_profiles_list", json!({})),
            Self::AgentConnectionTest { profile_id } => {
                ("agent_connection_test", json!({ "profile_id": profile_id }))
            }
            Self::AgentWorkingDirectoryResolve {
                profile_id,
                primary_path,
                selected_working_directory,
            } => (
                "agent_working_directory_resolve",
                json!({
                    "profile_id": profile_id,
                    "primary_path": primary_path,
                    "selected_working_directory": selected_working_directory,
                }),
            ),
            Self::AgentSessionStart {
                profile_id,
                working_directory,
                context_paths,
                restored_messages,
            } => (
                "agent_session_start",
                json!({
                    "profile_id": profile_id,
                    "working_directory": working_directory,
                    "context_paths": context_paths,
                    "restored_messages": restored_messages,
                }),
            ),
            Self::AgentSessionPrompt {
                session_id,
                prompt,
                context_paths,
                wait_ready_ms,
            } => (
                "agent_session_prompt",
                json!({
                    "session_id": session_id,
                    "prompt": prompt,
                    "context_paths": context_paths,
                    "wait_ready_ms": wait_ready_ms,
                }),
            ),
            Self::AgentSessionStatus { session_id } => {
                ("agent_session_status", json!({ "session_id": session_id }))
            }
            Self::AgentSessionEvents { session_id } => {
                ("agent_session_events", json!({ "session_id": session_id }))
            }
            Self::AgentSessionWait {
                session_id,
                timeout_ms,
            } => (
                "agent_session_wait",
                json!({ "session_id": session_id, "timeout_ms": timeout_ms }),
            ),
            Self::AgentSessionCancel { session_id } => {
                ("agent_session_cancel", json!({ "session_id": session_id }))
            }
            Self::AgentSessionPermission {
                session_id,
                request_id,
                decision,
            } => (
                "agent_session_permission",
                json!({
                    "session_id": session_id,
                    "request_id": request_id,
                    "decision": decision,
                }),
            ),
            Self::AgentSessionClose { session_id } => {
                ("agent_session_close", json!({ "session_id": session_id }))
            }
            Self::AgentRun {
                profile_id,
                working_directory,
                context_paths,
                prompt,
                timeout_ms,
            } => (
                "agent_run",
                json!({
                    "profile_id": profile_id,
                    "working_directory": working_directory,
                    "context_paths": context_paths,
                    "prompt": prompt,
                    "timeout_ms": timeout_ms,
                }),
            ),
            Self::AgentDiagnostics => ("agent_diagnostics", json!({})),
            Self::FilesList {
                path,
                sort_by,
                ascending,
            } => (
                "files_list",
                json!({ "path": path, "sort_by": sort_by, "ascending": ascending }),
            ),
            Self::FilesStat { path } => ("files_stat", json!({ "path": path })),
            Self::FilesReadText {
                path,
                max_bytes,
                truncate,
            } => (
                "files_read_text",
                json!({ "path": path, "max_bytes": max_bytes, "truncate": truncate }),
            ),
            Self::AppState => ("app_state", json!({})),
        }
    }
}

struct CoreSession {
    runtime: Arc<AgentRuntime>,
    profile_id: String,
    working_directory: String,
    context_paths: Vec<String>,
    actor: Option<String>,
    trace_id: String,
}

#[derive(Clone)]
struct IdempotentEntry {
    tool: String,
    result: CoreResult,
}

pub struct CoreApi {
    profiles: Arc<AgentProfileStore>,
    sessions: HashMap<String, CoreSession>,
    idempotency: HashMap<String, IdempotentEntry>,
}

impl Default for CoreApi {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreApi {
    pub fn new() -> Self {
        Self {
            profiles: AgentProfileStore::new(),
            sessions: HashMap::new(),
            idempotency: HashMap::new(),
        }
    }

    pub fn with_profile_store(profiles: Arc<AgentProfileStore>) -> Self {
        Self {
            profiles,
            sessions: HashMap::new(),
            idempotency: HashMap::new(),
        }
    }

    /// Compatibility entry point for in-process trusted callers.
    pub fn execute(&mut self, tool: &str, arguments: Value) -> Result<Value, CoreError> {
        self.invoke(
            tool,
            arguments,
            ExecutionContext {
                confirmed: true,
                ..ExecutionContext::default()
            },
        )
        .map(|result| result.data)
    }

    /// Invoke one business operation through the stable Core API contract.
    pub fn invoke(
        &mut self,
        tool: &str,
        arguments: Value,
        context: ExecutionContext,
    ) -> Result<CoreResult, CoreError> {
        let spec = tool_spec(tool)?;
        let read_only = spec
            .annotations
            .get("readOnlyHint")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let destructive = spec
            .annotations
            .get("destructiveHint")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if context.read_only && !read_only {
            return Err(CoreError::forbidden(
                "read_only_mode",
                format!("{tool} changes state and is unavailable in read-only mode"),
            )
            .with_recovery("Retry without read-only mode after obtaining write authorization."));
        }
        if destructive && !context.confirmed {
            return Err(CoreError::forbidden(
                "confirmation_required",
                format!("{tool} requires explicit confirmation"),
            )
            .with_recovery("Repeat the call with a confirmed execution context."));
        }

        context.remaining_ms(MAX_WAIT_TIMEOUT_MS)?;
        if let Some(key) = context.idempotency_key.as_deref()
            && let Some(entry) = self.idempotency.get(key)
        {
            if entry.tool == tool {
                let mut result = entry.result.clone();
                result.idempotent_replay = true;
                return Ok(result);
            }
            return Err(CoreError::conflict(
                "idempotency_conflict",
                format!("idempotency key {key} was already used by {}", entry.tool),
            ));
        }

        let mut arguments = normalize_arguments(arguments)?;
        apply_context_timeout(tool, &mut arguments, &context)?;
        let data = self.dispatch(tool, &arguments, &context)?;
        let state = data
            .get("snapshot")
            .cloned()
            .or_else(|| data.get("state").cloned());
        let recovery_point = data
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let result = CoreResult {
            trace_id: context.trace_id.clone(),
            data,
            state,
            changed: !read_only,
            idempotent_replay: false,
            recovery_point,
        };
        if let Some(key) = context.idempotency_key {
            self.idempotency.insert(
                key,
                IdempotentEntry {
                    tool: tool.to_string(),
                    result: result.clone(),
                },
            );
        }
        Ok(result)
    }

    pub fn invoke_request(
        &mut self,
        request: CoreRequest,
        context: ExecutionContext,
    ) -> Result<CoreResult, CoreError> {
        let (tool, arguments) = request.into_parts();
        self.invoke(tool, arguments, context)
    }

    fn dispatch(
        &mut self,
        tool: &str,
        arguments: &Map<String, Value>,
        context: &ExecutionContext,
    ) -> Result<Value, CoreError> {
        match tool {
            "agent_profiles_list" => self.agent_profiles_list(),
            "agent_connection_test" => self.agent_connection_test(arguments),
            "agent_working_directory_resolve" => self.agent_working_directory_resolve(arguments),
            "agent_session_start" => self.agent_session_start(arguments, context),
            "agent_session_prompt" => self.agent_session_prompt(arguments),
            "agent_session_status" => self.agent_session_status(arguments),
            "agent_session_events" => self.agent_session_events(arguments),
            "agent_session_wait" => self.agent_session_wait(arguments),
            "agent_session_cancel" => self.agent_session_cancel(arguments),
            "agent_session_permission" => self.agent_session_permission(arguments),
            "agent_session_close" => self.agent_session_close(arguments),
            "agent_run" => self.agent_run(arguments),
            "agent_diagnostics" => self.agent_diagnostics(),
            "files_list" => self.files_list(arguments),
            "files_stat" => self.files_stat(arguments),
            "files_read_text" => self.files_read_text(arguments),
            "app_state" => self.app_state(),
            _ => Err(CoreError::not_found(
                "unknown_tool",
                format!("unknown control tool: {tool}"),
            )),
        }
    }

    pub fn read_resource(
        &mut self,
        uri: &str,
        context: ExecutionContext,
    ) -> Result<(ResourceSpec, String), CoreError> {
        let resource = resource_specs()
            .into_iter()
            .find(|resource| resource.uri == uri)
            .ok_or_else(|| {
                CoreError::not_found("resource_not_found", format!("unknown resource: {uri}"))
            })?;
        let tool = resource_tool(uri).ok_or_else(|| {
            CoreError::not_found(
                "resource_not_found",
                format!("resource has no reader: {uri}"),
            )
        })?;
        let result = self.invoke(tool, json!({}), context)?;
        let text = serde_json::to_string_pretty(&result.data)
            .map_err(|error| CoreError::new("serialization", error.to_string()))?;
        Ok((resource, text))
    }

    fn agent_profiles_list(&self) -> Result<Value, CoreError> {
        let snapshot = self.profiles.snapshot();
        let mut value = serde_json::to_value(snapshot)
            .map_err(|error| CoreError::new("serialization", error.to_string()))?;
        let capabilities = serde_json::to_value(agent_transport_capabilities())
            .map_err(|error| CoreError::new("serialization", error.to_string()))?;
        if let Some(object) = value.as_object_mut() {
            object.insert("transport_capabilities".to_string(), capabilities);
        }
        Ok(value)
    }

    fn agent_connection_test(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
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
            "session_supported": result.session_supported,
            "profile_save_supported": result.profile_save_supported,
        }))
    }

    fn agent_working_directory_resolve(
        &self,
        arguments: &Map<String, Value>,
    ) -> Result<Value, CoreError> {
        let profile = self.select_profile(optional_string(arguments, "profile_id")?)?;
        let primary_path = required_string(arguments, "primary_path")?;
        let selected_working_directory =
            optional_string(arguments, "selected_working_directory")?.map(PathBuf::from);
        let resolution = resolve_profile_working_directory(
            &profile,
            Path::new(primary_path),
            selected_working_directory.as_deref(),
        );
        serde_json::to_value(resolution)
            .map_err(|error| CoreError::new("serialization", error.to_string()))
    }

    fn agent_session_start(
        &mut self,
        arguments: &Map<String, Value>,
        execution: &ExecutionContext,
    ) -> Result<Value, CoreError> {
        let profile = self.select_profile(optional_string(arguments, "profile_id")?)?;
        ensure_profile_can_start_session(&profile)?;
        let mut requested_working_directory =
            optional_string(arguments, "working_directory")?.map(PathBuf::from);
        let context_paths = string_array(arguments, "context_paths")?;
        if requested_working_directory.is_none()
            && context_paths.is_empty()
            && !matches!(&profile.working_directory, WorkingDirectoryPolicy::Ask)
        {
            requested_working_directory = Some(
                std::env::current_dir()
                    .map_err(|error| CoreError::new("cwd", error.to_string()))?,
            );
        }
        let resolution = resolve_working_directory_for_context(
            &profile,
            &context_paths,
            requested_working_directory.as_deref(),
        );
        if resolution.status != AgentWorkingDirectoryStatus::Ready {
            return Err(CoreError::invalid_args(
                resolution
                    .message
                    .unwrap_or_else(|| "Agent 工作目录不可用".to_string()),
            )
            .with_details(json!({
                "status": resolution.status,
                "policy": resolution.policy,
            })));
        }
        let working_directory = resolution
            .path
            .ok_or_else(|| CoreError::invalid_args("Agent 工作目录解析失败"))?;
        let restored_messages = optional_agent_messages(arguments, "restored_messages")?;
        let runtime = AgentRuntime::start(
            profile.clone(),
            working_directory.clone(),
            context_paths.clone(),
            restored_messages,
        )
        .map_err(|error| CoreError::new("agent_start", error.to_string()))?;
        let session_id = new_agent_conversation_id();
        let snapshot = runtime.snapshot();
        self.sessions.insert(
            session_id.clone(),
            CoreSession {
                runtime,
                profile_id: profile.id.clone(),
                working_directory: working_directory.clone(),
                context_paths: context_paths.clone(),
                actor: execution.actor.clone(),
                trace_id: execution.trace_id.clone(),
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

    fn agent_session_prompt(&mut self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let session_id = required_string(arguments, "session_id")?;
        let prompt = required_string(arguments, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(CoreError::invalid_args("prompt must not be empty"));
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
            .map_err(|error| CoreError::new("agent_prompt", error.to_string()))?;
        thread::sleep(Duration::from_millis(80));
        Ok(json!({
            "session_id": session_id,
            "sent": true,
            "snapshot": runtime.snapshot(),
        }))
    }

    fn agent_session_status(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let session_id = required_string(arguments, "session_id")?;
        let session = self.session(session_id)?;
        Ok(json!({
            "session_id": session_id,
            "profile_id": session.profile_id,
            "working_directory": session.working_directory,
            "context_paths": session.context_paths,
            "actor": session.actor,
            "trace_id": session.trace_id,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_events(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let session_id = required_string(arguments, "session_id")?;
        let session = self.session(session_id)?;
        let events = session.runtime.drain_events();
        Ok(json!({
            "session_id": session_id,
            "events": events,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_wait(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
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

    fn agent_session_cancel(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let session_id = required_string(arguments, "session_id")?;
        let session = self.session(session_id)?;
        session
            .runtime
            .cancel()
            .map_err(|error| CoreError::new("agent_cancel", error.to_string()))?;
        Ok(json!({
            "session_id": session_id,
            "cancel_requested": true,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_permission(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let session_id = required_string(arguments, "session_id")?;
        let request_id = required_string(arguments, "request_id")?;
        let decision_text = required_string(arguments, "decision")?;
        let decision = match decision_text {
            "allow_once" => AgentPermissionDecision::AllowOnce,
            "allow_task" => AgentPermissionDecision::AllowTask,
            "reject_once" => AgentPermissionDecision::RejectOnce,
            "cancel" => AgentPermissionDecision::Cancel,
            other => {
                return Err(CoreError::invalid_args(format!(
                    "decision must be allow_once, allow_task, reject_once, or cancel; got {other}"
                )));
            }
        };
        let session = self.session(session_id)?;
        session
            .runtime
            .respond_permission(request_id.to_string(), decision)
            .map_err(|error| CoreError::new("agent_permission", error.to_string()))?;
        Ok(json!({
            "session_id": session_id,
            "request_id": request_id,
            "decision": decision_text,
            "snapshot": session.runtime.snapshot(),
        }))
    }

    fn agent_session_close(&mut self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
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

    fn agent_run(&mut self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let profile = self.select_profile(optional_string(arguments, "profile_id")?)?;
        ensure_profile_can_start_session(&profile)?;
        let mut requested_working_directory =
            optional_string(arguments, "working_directory")?.map(PathBuf::from);
        let context_paths = string_array(arguments, "context_paths")?;
        if requested_working_directory.is_none()
            && context_paths.is_empty()
            && !matches!(&profile.working_directory, WorkingDirectoryPolicy::Ask)
        {
            requested_working_directory = Some(
                std::env::current_dir()
                    .map_err(|error| CoreError::new("cwd", error.to_string()))?,
            );
        }
        let resolution = resolve_working_directory_for_context(
            &profile,
            &context_paths,
            requested_working_directory.as_deref(),
        );
        if resolution.status != AgentWorkingDirectoryStatus::Ready {
            return Err(CoreError::invalid_args(
                resolution
                    .message
                    .unwrap_or_else(|| "Agent 工作目录不可用".to_string()),
            )
            .with_details(json!({
                "status": resolution.status,
                "policy": resolution.policy,
            })));
        }
        let working_directory = resolution
            .path
            .ok_or_else(|| CoreError::invalid_args("Agent 工作目录解析失败"))?;
        let prompt = required_string(arguments, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(CoreError::invalid_args("prompt must not be empty"));
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
            .map_err(|error| CoreError::new("agent_start", error.to_string()))?,
        );

        let result = (|| {
            ensure_ready(&runtime, timeout_ms.clamp(1_000, DEFAULT_READY_TIMEOUT_MS))?;
            runtime
                .send_prompt(prompt.to_string(), context_paths.clone())
                .map_err(|error| CoreError::new("agent_prompt", error.to_string()))?;
            let snapshot = wait_for_terminal(&runtime, timeout_ms)?;
            let events = runtime.drain_events();
            if snapshot.state == AgentRuntimeState::Failed {
                return Err(CoreError::new(
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

    fn agent_diagnostics(&self) -> Result<Value, CoreError> {
        serde_json::to_value(diagnose_agents())
            .map_err(|error| CoreError::new("serialization", error.to_string()))
    }

    fn files_list(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let path = match optional_string(arguments, "path")? {
            Some(path) => PathBuf::from(path),
            None => {
                std::env::current_dir().map_err(|error| CoreError::new("cwd", error.to_string()))?
            }
        };
        let sort = SortSpec {
            key: match optional_string(arguments, "sort_by")?.unwrap_or("name") {
                "name" => SortKey::Name,
                "modified" => SortKey::Modified,
                "kind" => SortKey::Kind,
                "size" => SortKey::Size,
                other => {
                    return Err(CoreError::invalid_args(format!(
                        "sort_by must be name, modified, kind, or size; got {other}"
                    )));
                }
            },
            ascending: optional_bool(arguments, "ascending")?.unwrap_or(true),
        };
        let snapshot =
            load_directory(&path, sort).map_err(|error| CoreError::new("files_list", error))?;
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

    fn files_stat(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let path = PathBuf::from(required_string(arguments, "path")?);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| CoreError::new("files_stat", error.to_string()))?;
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

    fn files_read_text(&self, arguments: &Map<String, Value>) -> Result<Value, CoreError> {
        let path = PathBuf::from(required_string(arguments, "path")?);
        let max_bytes = optional_u64(arguments, "max_bytes")?
            .unwrap_or(DEFAULT_MAX_READ_BYTES)
            .min(MAX_READ_BYTES);
        let truncate = optional_bool(arguments, "truncate")?.unwrap_or(false);
        let metadata = fs::metadata(&path)
            .map_err(|error| CoreError::new("files_read_text", error.to_string()))?;
        if !metadata.is_file() {
            return Err(CoreError::new(
                "not_a_file",
                format!("{} is not a regular file", path.display()),
            ));
        }
        let file = fs::File::open(&path)
            .map_err(|error| CoreError::new("files_read_text", error.to_string()))?;
        let read_limit = max_bytes.saturating_add(1);
        let mut bytes = Vec::new();
        file.take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|error| CoreError::new("files_read_text", error.to_string()))?;
        let exceeded = bytes.len() as u64 > max_bytes;
        if exceeded {
            if !truncate {
                return Err(CoreError::new(
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
            CoreError::new(
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

    fn app_state(&self) -> Result<Value, CoreError> {
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

    fn select_profile(&self, profile_id: Option<&str>) -> Result<AgentProfile, CoreError> {
        if let Some(profile_id) = profile_id {
            return self
                .profiles
                .get_profile(profile_id.to_string())
                .map_err(|error| CoreError::new("profile_store", error.to_string()))?
                .ok_or_else(|| {
                    CoreError::new(
                        "profile_not_found",
                        format!("profile not found: {profile_id}"),
                    )
                });
        }
        let snapshot = self.profiles.snapshot();
        snapshot.default_profile().cloned().ok_or_else(|| {
            CoreError::new(
                "profile_not_configured",
                "no runnable ACP / JSON-RPC stdio Agent profile is configured",
            )
            .with_recovery("Open Agent settings and save an ACP / JSON-RPC stdio profile.")
        })
    }

    pub fn session_snapshot(&self, session_id: &str) -> Result<AgentRuntimeSnapshot, CoreError> {
        Ok(self.session(session_id)?.runtime.snapshot())
    }

    fn session(&self, session_id: &str) -> Result<&CoreSession, CoreError> {
        self.sessions
            .get(session_id)
            .ok_or_else(|| session_not_found(session_id))
    }
}

#[derive(Clone)]
pub struct CoreClient {
    api: Arc<Mutex<CoreApi>>,
    context: ExecutionContext,
}

impl Default for CoreClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreClient {
    pub fn new() -> Self {
        Self {
            api: Arc::new(Mutex::new(CoreApi::new())),
            context: ExecutionContext {
                confirmed: true,
                ..ExecutionContext::default()
            },
        }
    }

    pub fn with_context(context: ExecutionContext) -> Self {
        Self {
            api: Arc::new(Mutex::new(CoreApi::new())),
            context,
        }
    }

    pub fn invoke(&self, tool: &str, arguments: Value) -> Result<CoreResult, CoreError> {
        let mut api = self.api.lock().map_err(|_| {
            CoreError::new("core_client_poisoned", "Core API client lock is poisoned")
        })?;
        api.invoke(tool, arguments, self.context.clone())
    }

    pub fn invoke_request(&self, request: CoreRequest) -> Result<CoreResult, CoreError> {
        let mut api = self.api.lock().map_err(|_| {
            CoreError::new("core_client_poisoned", "Core API client lock is poisoned")
        })?;
        api.invoke_request(request, self.context.clone())
    }

    pub fn session_snapshot(&self, session_id: &str) -> Result<AgentRuntimeSnapshot, CoreError> {
        let api = self.api.lock().map_err(|_| {
            CoreError::new("core_client_poisoned", "Core API client lock is poisoned")
        })?;
        api.session_snapshot(session_id)
    }
}

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        spec(
            "agent_profiles_list",
            "List Agent profiles, the runnable default, and the shared transport capability matrix.",
            schema(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec(
            "agent_connection_test",
            "Probe a profile without starting a prompt. A successful result may be diagnostic-only when session_supported is false.",
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
            "agent_working_directory_resolve",
            "Resolve a profile's WorkingDirectoryPolicy for a primary path without starting an Agent.",
            schema(
                json!({
                    "profile_id": { "type": "string" },
                    "primary_path": { "type": "string" },
                    "selected_working_directory": { "type": "string" }
                }),
                &["primary_path"],
            ),
            true,
            false,
            false,
        ),
        spec(
            "agent_session_start",
            "Start a persistent ACP stdio Agent session. Connection-test-only transports return unsupported_transport.",
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

fn tool_spec(name: &str) -> Result<ToolSpec, CoreError> {
    tool_specs()
        .into_iter()
        .find(|spec| spec.name == name)
        .ok_or_else(|| {
            CoreError::not_found("unknown_tool", format!("unknown core tool: {name}"))
                .with_recovery("Call list-tools or inspect the MCP tools/list response.")
        })
}

pub fn resource_specs() -> Vec<ResourceSpec> {
    vec![
        ResourceSpec {
            uri: "kite://agent/profiles".to_string(),
            name: "Agent profiles".to_string(),
            description: "Configured Agent profiles and the resolved default profile.".to_string(),
            mime_type: "application/json".to_string(),
        },
        ResourceSpec {
            uri: "kite://agent/diagnostics".to_string(),
            name: "Agent diagnostics".to_string(),
            description: "Redacted Agent installation, connection, log, and session diagnostics."
                .to_string(),
            mime_type: "application/json".to_string(),
        },
        ResourceSpec {
            uri: "kite://app/state".to_string(),
            name: "Kite app state".to_string(),
            description: "Persisted tabs and UI preferences without opening a UI shell."
                .to_string(),
            mime_type: "application/json".to_string(),
        },
    ]
}

fn resource_tool(uri: &str) -> Option<&'static str> {
    match uri {
        "kite://agent/profiles" => Some("agent_profiles_list"),
        "kite://agent/diagnostics" => Some("agent_diagnostics"),
        "kite://app/state" => Some("app_state"),
        _ => None,
    }
}

fn apply_context_timeout(
    tool: &str,
    arguments: &mut Map<String, Value>,
    context: &ExecutionContext,
) -> Result<(), CoreError> {
    let Some(_) = context.deadline_unix_ms else {
        return Ok(());
    };
    let key = match tool {
        "agent_session_prompt" => "wait_ready_ms",
        "agent_session_wait" | "agent_run" => "timeout_ms",
        _ => return Ok(()),
    };
    let remaining = context.remaining_ms(MAX_WAIT_TIMEOUT_MS)?;
    let existing = arguments
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(remaining);
    arguments.insert(key.to_string(), json!(existing.min(remaining)));
    Ok(())
}

fn ensure_profile_can_start_session(profile: &AgentProfile) -> Result<(), CoreError> {
    if profile.transport.supports_session() {
        return Ok(());
    }
    let capability = profile.transport.capabilities();
    Err(CoreError::new(
        "unsupported_transport",
        format!(
            "transport {label} is connection-test-only in this release; use ACP / JSON-RPC stdio to start an Agent session",
            label = profile.transport.label()
        ),
    )
    .with_kind(CoreErrorKind::PreconditionFailed)
    .with_recovery(
        capability
            .limitation
            .unwrap_or_else(|| "Choose ACP / JSON-RPC stdio in Agent settings.".to_string()),
    )
    .with_details(json!({
        "transport": profile.transport,
        "session_supported": capability.session_supported,
        "supported_transports": ["acp_stdio"],
    })))
}

fn infer_error_kind(code: &str) -> CoreErrorKind {
    let code = code.to_ascii_lowercase();
    if code.contains("invalid") || code.contains("unknown") || code == "not_utf8" {
        CoreErrorKind::InvalidInput
    } else if code.contains("not_found") || code.contains("not_configured") {
        CoreErrorKind::NotFound
    } else if code.contains("forbidden")
        || code.contains("read_only")
        || code.contains("confirmation")
        || code.contains("unauthorized")
    {
        CoreErrorKind::Forbidden
    } else if code.contains("timeout") {
        CoreErrorKind::Timeout
    } else if code.contains("busy")
        || code.contains("conflict")
        || code.contains("precondition")
        || code.contains("already")
    {
        CoreErrorKind::Conflict
    } else if code.contains("external")
        || code.contains("io")
        || code.contains("agent_")
        || code.contains("cwd")
        || code.contains("files_")
    {
        CoreErrorKind::External
    } else {
        CoreErrorKind::Internal
    }
}

fn new_trace_id() -> String {
    let sequence = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
    format!(
        "kite-{:x}-{:x}-{:x}",
        std::process::id(),
        unix_ms(),
        sequence
    )
}

pub type ControlPlane = CoreApi;
pub type ControlError = CoreError;

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

fn normalize_arguments(arguments: Value) -> Result<Map<String, Value>, CoreError> {
    match arguments {
        Value::Null => Ok(Map::new()),
        Value::Object(arguments) => Ok(arguments),
        _ => Err(CoreError::invalid_args(
            "tool arguments must be a JSON object",
        )),
    }
}

fn required_string<'a>(arguments: &'a Map<String, Value>, key: &str) -> Result<&'a str, CoreError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::invalid_args(format!("{key} must be a non-empty string")))
}

fn optional_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, CoreError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim())),
        Some(_) => Err(CoreError::invalid_args(format!("{key} must be a string"))),
    }
}

fn optional_bool(arguments: &Map<String, Value>, key: &str) -> Result<Option<bool>, CoreError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(CoreError::invalid_args(format!("{key} must be a boolean"))),
    }
}

fn optional_u64(arguments: &Map<String, Value>, key: &str) -> Result<Option<u64>, CoreError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| CoreError::invalid_args(format!("{key} must be a positive integer"))),
    }
}

fn optional_agent_messages(
    arguments: &Map<String, Value>,
    key: &str,
) -> Result<Vec<crate::agent::AgentMessage>, CoreError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            CoreError::invalid_args(format!(
                "{key} must contain valid AgentMessage records: {error}"
            ))
        }),
    }
}

fn string_array(arguments: &Map<String, Value>, key: &str) -> Result<Vec<String>, CoreError> {
    optional_string_array(arguments, key).map(Option::unwrap_or_default)
}

fn optional_string_array(
    arguments: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, CoreError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    CoreError::invalid_args(format!("{key} must contain only strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(CoreError::invalid_args(format!(
            "{key} must be an array of strings"
        ))),
    }
}

fn ensure_ready(runtime: &AgentRuntime, timeout_ms: u64) -> Result<(), CoreError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(MAX_WAIT_TIMEOUT_MS));
    loop {
        let snapshot = runtime.snapshot();
        match snapshot.state {
            AgentRuntimeState::Ready
            | AgentRuntimeState::Completed
            | AgentRuntimeState::Cancelled => return Ok(()),
            AgentRuntimeState::Failed => {
                return Err(CoreError::new(
                    "agent_failed",
                    snapshot
                        .error
                        .clone()
                        .unwrap_or_else(|| "Agent failed before it became ready".to_string()),
                )
                .with_details(json!(snapshot)));
            }
            AgentRuntimeState::Running | AgentRuntimeState::WaitingPermission => {
                return Err(CoreError::new(
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
            return Err(CoreError::new(
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
) -> Result<AgentRuntimeSnapshot, CoreError> {
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
            return Err(CoreError::new(
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

fn session_not_found(session_id: &str) -> CoreError {
    CoreError::not_found(
        "session_not_found",
        format!("Agent session not found: {session_id}"),
    )
    .with_recovery("List status for a live session id or start a new session.")
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

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        assert!(!resource_specs().is_empty());
    }

    #[test]
    fn read_only_context_rejects_mutating_tools() {
        let error = CoreApi::new()
            .invoke(
                "agent_session_start",
                json!({}),
                ExecutionContext {
                    read_only: true,
                    ..ExecutionContext::default()
                },
            )
            .expect_err("read-only mode must reject writes");
        assert_eq!(error.kind, CoreErrorKind::Forbidden);
        assert_eq!(error.code, "read_only_mode");
    }

    #[test]
    fn destructive_tools_require_confirmation() {
        let error = CoreApi::new()
            .invoke(
                "agent_session_close",
                json!({ "session_id": "missing" }),
                ExecutionContext::default(),
            )
            .expect_err("destructive tool must require confirmation");
        assert_eq!(error.code, "confirmation_required");
        assert!(error.recovery.is_some());
    }

    #[test]
    fn repeated_idempotency_key_replays_the_original_result() {
        let mut api = CoreApi::new();
        let context = ExecutionContext {
            idempotency_key: Some("core-api-test-read".to_string()),
            read_only: true,
            ..ExecutionContext::default()
        };
        let first = api
            .invoke(
                "files_stat",
                json!({ "path": "Cargo.toml" }),
                context.clone(),
            )
            .expect("first call");
        let second = api
            .invoke("files_stat", json!({ "path": "Cargo.toml" }), context)
            .expect("replay");
        assert!(!first.idempotent_replay);
        assert!(second.idempotent_replay);
        assert_eq!(first.data, second.data);
        assert_eq!(first.trace_id, second.trace_id);
    }

    #[test]
    fn typed_request_uses_the_same_core_contract() {
        let result = CoreApi::new()
            .invoke_request(
                CoreRequest::FilesStat {
                    path: "Cargo.toml".to_string(),
                },
                ExecutionContext {
                    trace_id: "typed-request".to_string(),
                    read_only: true,
                    ..ExecutionContext::default()
                },
            )
            .expect("typed request");
        assert_eq!(result.trace_id, "typed-request");
        assert_eq!(result.data["path"], "Cargo.toml");
        assert!(!result.changed);
    }

    #[test]
    fn expired_execution_deadline_fails_before_dispatch() {
        let error = CoreApi::new()
            .invoke(
                "files_stat",
                json!({ "path": "Cargo.toml" }),
                ExecutionContext {
                    deadline_unix_ms: Some(1),
                    ..ExecutionContext::default()
                },
            )
            .expect_err("expired context must fail");
        assert_eq!(error.kind, CoreErrorKind::Timeout);
    }
}
