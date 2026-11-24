use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::agent::config::{AgentProfile, AgentTransport, validate_agent_profile};
use crate::agent::diagnostics::{record_agent_event, redact_agent_text};
use crate::agent::process::{spawn_agent_process, terminate_agent_process};
use crate::agent::restore::{AgentMessage, AgentMessageRole};

const MAX_ACP_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RUNTIME_EVENTS: usize = 4_096;
const MAX_RESTORED_CONTEXT_BYTES: usize = 128 * 1024;
const CANCEL_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum AgentRuntimeState {
    #[default]
    Starting,
    Initializing,
    Ready,
    Running,
    WaitingPermission,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentEventKind {
    Status,
    Output,
    Permission,
    Diagnostic,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentEvent {
    pub sequence: u64,
    pub kind: AgentEventKind,
    pub text: Option<String>,
    pub request_id: Option<String>,
    pub state: Option<AgentRuntimeState>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentPermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentPermissionRequest {
    pub request_id: String,
    pub title: String,
    pub description: String,
    pub options: Vec<AgentPermissionOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentPermissionDecision {
    AllowOnce,
    AllowTask,
    RejectOnce,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentRuntimeSnapshot {
    pub state: AgentRuntimeState,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub protocol_version: Option<u32>,
    pub capabilities: Vec<String>,
    pub output: String,
    pub error: Option<String>,
    pub output_truncated: bool,
    pub permission_requests: Vec<AgentPermissionRequest>,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    pub closed: bool,
}

impl Default for AgentRuntimeSnapshot {
    fn default() -> Self {
        Self {
            state: AgentRuntimeState::Starting,
            agent_name: None,
            agent_version: None,
            protocol_version: None,
            capabilities: Vec::new(),
            output: String::new(),
            error: None,
            output_truncated: false,
            permission_requests: Vec::new(),
            started_unix_ms: unix_ms(),
            elapsed_ms: 0,
            closed: false,
        }
    }
}

#[derive(Debug)]
struct RuntimeShared {
    commands: Mutex<Option<mpsc::Sender<RuntimeSignal>>>,
    snapshot: Mutex<AgentRuntimeSnapshot>,
    events: Mutex<VecDeque<AgentEvent>>,
    next_event_sequence: AtomicU64,
    started: Instant,
}

impl RuntimeShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            commands: Mutex::new(None),
            snapshot: Mutex::new(AgentRuntimeSnapshot::default()),
            events: Mutex::new(VecDeque::new()),
            next_event_sequence: AtomicU64::new(1),
            started: Instant::now(),
        })
    }

    fn send(&self, signal: RuntimeSignal) -> Result<(), String> {
        self.commands
            .lock()
            .map_err(|_| "Agent 命令通道不可用".to_string())?
            .as_ref()
            .cloned()
            .ok_or_else(|| "Agent 已关闭".to_string())?
            .send(signal)
            .map_err(|_| "Agent 已关闭".to_string())
    }

    fn emit(&self, kind: AgentEventKind, text: Option<String>, request_id: Option<String>) {
        let sequence = self.next_event_sequence.fetch_add(1, Ordering::Relaxed);
        let state = self.snapshot.lock().ok().map(|snapshot| snapshot.state);
        if let Ok(mut events) = self.events.lock() {
            if events.len() >= MAX_RUNTIME_EVENTS {
                events.pop_front();
            }
            events.push_back(AgentEvent {
                sequence,
                kind,
                text: text.map(|text| redact_agent_text(&text)),
                request_id,
                state,
            });
        }
    }

    fn set_state(&self, state: AgentRuntimeState) {
        if let Ok(mut snapshot) = self.snapshot.lock()
            && snapshot.state != state
        {
            snapshot.state = state;
            snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        self.emit(AgentEventKind::Status, None, None);
    }

    fn set_error(&self, error: impl Into<String>) {
        let error = redact_agent_text(&error.into());
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.error = Some(error.clone());
            snapshot.state = AgentRuntimeState::Failed;
            snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        record_agent_event("error", "runtime", &error);
        self.emit(AgentEventKind::Failed, Some(error), None);
    }

    fn append_output(&self, text: &str, max_output_bytes: u64) -> bool {
        if text.is_empty() {
            return true;
        }
        if let Ok(mut snapshot) = self.snapshot.lock() {
            if snapshot.output.len() + text.len() > max_output_bytes as usize {
                snapshot.output_truncated = true;
                return false;
            }
            snapshot.output.push_str(text);
        }
        self.emit(AgentEventKind::Output, Some(text.to_string()), None);
        true
    }

    fn add_permission(&self, request: AgentPermissionRequest) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot
                .permission_requests
                .retain(|existing| existing.request_id != request.request_id);
            snapshot.permission_requests.push(request.clone());
            snapshot.state = AgentRuntimeState::WaitingPermission;
        }
        self.emit(
            AgentEventKind::Permission,
            Some(request.description.clone()),
            Some(request.request_id),
        );
    }

    fn remove_permission(&self, request_id: &str) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot
                .permission_requests
                .retain(|request| request.request_id != request_id);
            if snapshot.permission_requests.is_empty()
                && snapshot.state == AgentRuntimeState::WaitingPermission
            {
                snapshot.state = AgentRuntimeState::Running;
            }
        }
    }

    fn close(&self) {
        if let Ok(mut commands) = self.commands.lock() {
            *commands = None;
        }
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.closed = true;
            snapshot.state = AgentRuntimeState::Closed;
            snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        self.emit(AgentEventKind::Status, None, None);
    }
}

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum AgentRuntimeError {
    #[error("{message}")]
    Failure { message: String },
}

impl From<String> for AgentRuntimeError {
    fn from(message: String) -> Self {
        Self::Failure { message }
    }
}

impl From<&str> for AgentRuntimeError {
    fn from(message: &str) -> Self {
        Self::Failure {
            message: message.to_string(),
        }
    }
}

#[derive(Debug, uniffi::Object)]
pub struct AgentRuntime {
    shared: Arc<RuntimeShared>,
}

#[uniffi::export]
impl AgentRuntime {
    #[uniffi::constructor]
    pub fn start(
        profile: AgentProfile,
        working_directory: String,
        context_paths: Vec<String>,
        restored_messages: Vec<AgentMessage>,
    ) -> Result<Arc<Self>, AgentRuntimeError> {
        validate_agent_profile(&profile).map_err(AgentRuntimeError::from)?;
        if profile.transport != AgentTransport::AcpStdio {
            return Err(AgentRuntimeError::from(
                "P0 Agent 运行时仅支持 ACP / JSON-RPC stdio",
            ));
        }
        let shared = RuntimeShared::new();
        let (sender, receiver) = mpsc::channel();
        if let Ok(mut commands) = shared.commands.lock() {
            *commands = Some(sender);
        }
        let worker_shared = shared.clone();
        let cwd = PathBuf::from(working_directory);
        std::thread::Builder::new()
            .name(format!("kite-agent-{}", profile.id))
            .spawn(move || {
                let mut worker = RuntimeWorker::new(
                    worker_shared,
                    profile,
                    cwd,
                    context_paths,
                    restored_messages,
                    receiver,
                );
                worker.run();
            })
            .map_err(|error| {
                AgentRuntimeError::from(format!("无法启动 Agent 工作线程：{error}"))
            })?;
        Ok(Arc::new(Self { shared }))
    }

    pub fn send_prompt(
        &self,
        prompt: String,
        context_paths: Vec<String>,
    ) -> Result<(), AgentRuntimeError> {
        if prompt.trim().is_empty() {
            return Err(AgentRuntimeError::from("提示不能为空"));
        }
        self.shared
            .send(RuntimeSignal::Prompt {
                prompt,
                context_paths,
            })
            .map_err(AgentRuntimeError::from)
    }

    pub fn cancel(&self) -> Result<(), AgentRuntimeError> {
        self.shared
            .send(RuntimeSignal::Cancel)
            .map_err(AgentRuntimeError::from)
    }

    pub fn respond_permission(
        &self,
        request_id: String,
        decision: AgentPermissionDecision,
    ) -> Result<(), AgentRuntimeError> {
        self.shared
            .send(RuntimeSignal::Permission {
                request_id,
                decision,
            })
            .map_err(AgentRuntimeError::from)
    }

    pub fn snapshot(&self) -> AgentRuntimeSnapshot {
        self.shared
            .snapshot
            .lock()
            .map(|snapshot| AgentRuntimeSnapshot {
                elapsed_ms: self.shared.started.elapsed().as_millis() as u64,
                ..snapshot.clone()
            })
            .unwrap_or_default()
    }

    pub fn drain_events(&self) -> Vec<AgentEvent> {
        self.shared
            .events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn shutdown(&self) {
        let _ = self.shared.send(RuntimeSignal::Shutdown);
    }

    pub fn is_closed(&self) -> bool {
        self.shared
            .snapshot
            .lock()
            .map(|snapshot| snapshot.closed)
            .unwrap_or(true)
    }
}

impl Drop for AgentRuntime {
    fn drop(&mut self) {
        let _ = self.shared.send(RuntimeSignal::Shutdown);
    }
}

#[derive(Debug)]
enum RuntimeSignal {
    Prompt {
        prompt: String,
        context_paths: Vec<String>,
    },
    Cancel,
    Permission {
        request_id: String,
        decision: AgentPermissionDecision,
    },
    Shutdown,
    StdoutLine(String),
    StdoutTooLarge,
    StderrLine(String),
    ReaderClosed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingRequest {
    Initialize,
    SessionNew,
    Prompt,
}

struct PendingPermission {
    request_id: Value,
    options: Vec<AgentPermissionOption>,
}

struct RuntimeWorker {
    shared: Arc<RuntimeShared>,
    profile: AgentProfile,
    cwd: PathBuf,
    context_paths: Vec<String>,
    restored_messages: Vec<AgentMessage>,
    receiver: mpsc::Receiver<RuntimeSignal>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    session_id: Option<String>,
    pending_request: Option<(u64, PendingRequest)>,
    pending_permissions: HashMap<String, PendingPermission>,
    next_request_id: u64,
    started: Instant,
    last_activity: Instant,
    prompt_started: Option<Instant>,
    cancel_started: Option<Instant>,
    total_prompt_timeout: Duration,
    output_limit_sent: bool,
    received_any_output: bool,
    shutdown_requested: bool,
}

impl RuntimeWorker {
    fn new(
        shared: Arc<RuntimeShared>,
        profile: AgentProfile,
        cwd: PathBuf,
        context_paths: Vec<String>,
        restored_messages: Vec<AgentMessage>,
        receiver: mpsc::Receiver<RuntimeSignal>,
    ) -> Self {
        let total_prompt_timeout = Duration::from_millis(u64::from(profile.idle_timeout_ms) * 10)
            .clamp(Duration::from_secs(60), Duration::from_secs(30 * 60));
        Self {
            shared,
            profile,
            cwd,
            context_paths,
            restored_messages,
            receiver,
            child: None,
            stdin: None,
            session_id: None,
            pending_request: None,
            pending_permissions: HashMap::new(),
            next_request_id: 0,
            started: Instant::now(),
            last_activity: Instant::now(),
            prompt_started: None,
            cancel_started: None,
            total_prompt_timeout,
            output_limit_sent: false,
            received_any_output: false,
            shutdown_requested: false,
        }
    }

    fn run(&mut self) {
        self.shared.set_state(AgentRuntimeState::Starting);
        if let Err(error) = self.start_process() {
            self.fail(error);
            self.finish();
            return;
        }
        self.shared.set_state(AgentRuntimeState::Initializing);
        self.shared.emit(
            AgentEventKind::Diagnostic,
            Some(format!("正在启动 {}", self.profile.name)),
            None,
        );
        if let Err(error) = self.send_initialize() {
            self.fail(error);
            self.finish();
            return;
        }
        self.event_loop();
        self.finish();
    }

    fn event_loop(&mut self) {
        loop {
            if self.shutdown_requested {
                self.cancel_pending_permissions("cancelled");
                self.shutdown_child();
                return;
            }
            match self.receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(signal) => {
                    self.last_activity = Instant::now();
                    if let Err(error) = self.handle_signal(signal) {
                        self.fail(error);
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.shutdown_requested = true;
                    continue;
                }
            }
            self.check_process_exit();
            if self.check_timeout() {
                return;
            }
            if self
                .shared
                .snapshot
                .lock()
                .is_ok_and(|snapshot| snapshot.state == AgentRuntimeState::Failed)
            {
                return;
            }
        }
    }

    fn start_process(&mut self) -> Result<(), String> {
        let mut child = spawn_agent_process(&self.profile, &self.cwd)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Agent stdin 不可用".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Agent stdout 不可用".to_string())?;
        let stderr = child.stderr.take();
        let sender = self
            .shared
            .commands
            .lock()
            .map_err(|_| "Agent 命令通道不可用".to_string())?
            .as_ref()
            .cloned()
            .ok_or_else(|| "Agent 已关闭".to_string())?;
        let stdout_sender = sender.clone();
        std::thread::Builder::new()
            .name("kite-agent-stdout".to_string())
            .spawn(move || read_stdout(stdout, stdout_sender))
            .map_err(|error| format!("无法启动 Agent stdout 读取线程：{error}"))?;
        if let Some(stderr) = stderr {
            std::thread::Builder::new()
                .name("kite-agent-stderr".to_string())
                .spawn(move || {
                    let mut reader = BufReader::new(stderr);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let text = line.trim_end().to_string();
                                if sender.send(RuntimeSignal::StderrLine(text)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                })
                .map_err(|error| format!("无法启动 Agent stderr 读取线程：{error}"))?;
        }
        self.child = Some(child);
        self.stdin = Some(stdin);
        record_agent_event(
            "info",
            "stream_start",
            format!("profile={} cwd={}", self.profile.id, self.cwd.display()),
        );
        Ok(())
    }

    fn send_initialize(&mut self) -> Result<(), String> {
        let id = self.next_id();
        self.pending_request = Some((id, PendingRequest::Initialize));
        self.write_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {
                    "auth": { "terminal": true },
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "kite",
                    "title": "Kite",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }))
    }

    fn send_session_new(&mut self) -> Result<(), String> {
        let id = self.next_id();
        self.pending_request = Some((id, PendingRequest::SessionNew));
        self.write_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {
                "cwd": self.cwd.to_string_lossy(),
                "mcpServers": []
            }
        }))
    }

    fn send_prompt(&mut self, prompt: String, context_paths: Vec<String>) -> Result<(), String> {
        let session_id = self
            .session_id
            .clone()
            .ok_or_else(|| "Agent 会话尚未就绪".to_string())?;
        let id = self.next_id();
        self.pending_request = Some((id, PendingRequest::Prompt));
        self.prompt_started = Some(Instant::now());
        self.last_activity = Instant::now();
        self.output_limit_sent = false;
        self.received_any_output = false;
        if let Ok(mut snapshot) = self.shared.snapshot.lock() {
            snapshot.output.clear();
            snapshot.error = None;
            snapshot.output_truncated = false;
            snapshot.permission_requests.clear();
        }
        self.context_paths = context_paths;
        let mut prompt_blocks = Vec::new();
        if let Some(history) = self.restored_history_context() {
            prompt_blocks.push(json!({
                "type": "text",
                "text": format!(
                    "以下是 Kite 恢复的先前对话，仅作为上下文：\n\n{history}\n\n当前用户请求：\n"
                )
            }));
        }
        prompt_blocks.push(json!({ "type": "text", "text": prompt }));
        for path in &self.context_paths {
            let path_buf = PathBuf::from(path);
            prompt_blocks.push(json!({
                "type": "resource_link",
                "uri": path_to_uri(&path_buf),
                "name": path_buf.file_name().and_then(|name| name.to_str()).unwrap_or(path),
                "title": path,
                "description": if path_buf.is_dir() { "Kite 选中的目录" } else { "Kite 选中的文件" }
            }));
        }
        self.restored_messages.clear();
        self.shared.set_state(AgentRuntimeState::Running);
        self.write_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": prompt_blocks
            }
        }))
    }

    fn handle_signal(&mut self, signal: RuntimeSignal) -> Result<(), String> {
        match signal {
            RuntimeSignal::Prompt {
                prompt,
                context_paths,
            } => {
                if !matches!(
                    self.shared.snapshot.lock().map(|snapshot| snapshot.state),
                    Ok(AgentRuntimeState::Ready
                        | AgentRuntimeState::Completed
                        | AgentRuntimeState::Failed
                        | AgentRuntimeState::Cancelled)
                ) {
                    self.shared.emit(
                        AgentEventKind::Diagnostic,
                        Some("Agent 尚未就绪，提示未发送".to_string()),
                        None,
                    );
                    return Ok(());
                }
                self.send_prompt(prompt, context_paths)
            }
            RuntimeSignal::Cancel => self.cancel_running(),
            RuntimeSignal::Permission {
                request_id,
                decision,
            } => self.respond_permission(&request_id, decision),
            RuntimeSignal::Shutdown => {
                self.shutdown_requested = true;
                Ok(())
            }
            RuntimeSignal::StdoutLine(line) => self.handle_stdout_line(&line),
            RuntimeSignal::StdoutTooLarge => Err(format!(
                "Agent 单帧输出超过 {} MiB",
                MAX_ACP_LINE_BYTES / 1024 / 1024
            )),
            RuntimeSignal::StderrLine(line) => {
                if !line.trim().is_empty() {
                    record_agent_event("debug", "agent_stderr", line);
                }
                Ok(())
            }
            RuntimeSignal::ReaderClosed => {
                if !self.shutdown_requested
                    && !matches!(
                        self.shared.snapshot.lock().map(|snapshot| snapshot.state),
                        Ok(AgentRuntimeState::Completed
                            | AgentRuntimeState::Failed
                            | AgentRuntimeState::Cancelled
                            | AgentRuntimeState::Closed)
                    )
                {
                    Err("Agent 关闭了 stdout".to_string())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn handle_stdout_line(&mut self, line: &str) -> Result<(), String> {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            self.shared.emit(
                AgentEventKind::Diagnostic,
                Some(format!(
                    "忽略非 JSON Agent 输出：{}",
                    truncate_text(line, 200)
                )),
                None,
            );
            return Ok(());
        };
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let id = value.get("id").cloned();
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            match method {
                "session/update" => {
                    self.handle_session_update(&params);
                    Ok(())
                }
                "session/request_permission" => {
                    self.handle_permission_request(id, &params);
                    Ok(())
                }
                _ => {
                    if let Some(id) = id {
                        self.respond_jsonrpc_error(id, -32601, "Kite 不支持该客户端方法")?;
                    }
                    Ok(())
                }
            }
        } else if let Some(id) = value.get("id").and_then(Value::as_u64) {
            self.handle_response(id, &value)
        } else if let Some(error) = value.get("error") {
            Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Agent 协议错误")
                .to_string())
        } else {
            Ok(())
        }
    }

    fn handle_response(&mut self, id: u64, value: &Value) -> Result<(), String> {
        let Some((expected_id, kind)) = self.pending_request else {
            return Ok(());
        };
        if id != expected_id {
            return Ok(());
        }
        self.pending_request = None;
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Agent 协议错误")
                .to_string();
            if kind == PendingRequest::Prompt && code == -32800 {
                self.complete(AgentRuntimeState::Cancelled, "提示已取消".to_string());
                return Ok(());
            }
            return Err(message);
        }
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        match kind {
            PendingRequest::Initialize => {
                let protocol_version = result
                    .get("protocolVersion")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as u32;
                if protocol_version != 1 {
                    return Err(format!(
                        "ACP 协议版本不兼容：期望 1，收到 {protocol_version}"
                    ));
                }
                let info = result.get("agentInfo").cloned().unwrap_or_default();
                let capabilities = collect_capabilities(
                    result.get("agentCapabilities").cloned().unwrap_or_default(),
                );
                if let Ok(mut snapshot) = self.shared.snapshot.lock() {
                    snapshot.protocol_version = Some(protocol_version);
                    snapshot.agent_name = info
                        .get("title")
                        .and_then(Value::as_str)
                        .or_else(|| info.get("name").and_then(Value::as_str))
                        .map(str::to_string)
                        .or_else(|| Some(self.profile.name.clone()));
                    snapshot.agent_version = info
                        .get("version")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    snapshot.capabilities = capabilities;
                }
                self.send_session_new()
            }
            PendingRequest::SessionNew => {
                let session_id = result
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Agent session/new 响应缺少 sessionId".to_string())?;
                self.session_id = Some(session_id.to_string());
                if let Ok(mut snapshot) = self.shared.snapshot.lock() {
                    snapshot.state = AgentRuntimeState::Ready;
                    snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
                }
                self.shared.emit(
                    AgentEventKind::Diagnostic,
                    Some("Agent 已就绪".to_string()),
                    None,
                );
                Ok(())
            }
            PendingRequest::Prompt => {
                let stop_reason = result
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn");
                if stop_reason == "cancelled" {
                    self.complete(AgentRuntimeState::Cancelled, "提示已取消".to_string());
                } else if self
                    .shared
                    .snapshot
                    .lock()
                    .is_ok_and(|snapshot| snapshot.output_truncated)
                {
                    self.complete(
                        AgentRuntimeState::Completed,
                        "输出已达到上限，内容已截断".to_string(),
                    );
                } else {
                    self.complete(AgentRuntimeState::Completed, "回答完成".to_string());
                }
                Ok(())
            }
        }
    }

    fn handle_session_update(&mut self, params: &Value) {
        let update = params.get("update").unwrap_or(&Value::Null);
        let update_type = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match update_type {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let text = content_text(update.get("content").unwrap_or(&Value::Null));
                if update_type == "agent_message_chunk" && !text.is_empty() {
                    self.received_any_output = true;
                    let accepted = self
                        .shared
                        .append_output(&text, self.profile.max_output_bytes);
                    if !accepted && !self.output_limit_sent {
                        self.output_limit_sent = true;
                        let _ = self.write_cancel_notification();
                        self.shared.emit(
                            AgentEventKind::Diagnostic,
                            Some("Agent 输出超过上限，已请求停止".to_string()),
                            None,
                        );
                    }
                }
            }
            "tool_call" | "tool_call_update" => {
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        update
                            .get("toolCall")
                            .and_then(|tool| tool.get("title"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("Agent 正在使用工具");
                let status = update.get("status").and_then(Value::as_str).unwrap_or("");
                self.shared.emit(
                    AgentEventKind::Diagnostic,
                    Some(format!(
                        "{title}{}",
                        if status.is_empty() {
                            String::new()
                        } else {
                            format!(" · {status}")
                        }
                    )),
                    None,
                );
            }
            "agent_plan" | "plan" => {
                self.shared.emit(
                    AgentEventKind::Diagnostic,
                    Some("Agent 已更新任务计划".to_string()),
                    None,
                );
            }
            "notice" => {
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Agent 通知");
                let description = update
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.shared.emit(
                    AgentEventKind::Diagnostic,
                    Some(format!("{title} {description}").trim().to_string()),
                    None,
                );
            }
            _ => {}
        }
    }

    fn handle_permission_request(&mut self, id: Option<Value>, params: &Value) {
        let Some(request_id) = id else { return };
        let string_id = match &request_id {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        };
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        Some(AgentPermissionOption {
                            option_id: option.get("optionId")?.as_str()?.to_string(),
                            name: option.get("name")?.as_str()?.to_string(),
                            kind: option
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("allow_once")
                                .to_string(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
        let title = tool_call
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Agent 请求权限")
            .to_string();
        let description = tool_call
            .get("kind")
            .and_then(Value::as_str)
            .map(|kind| format!("权限类型：{kind}"))
            .unwrap_or_else(|| "请确认是否允许 Agent 执行该操作".to_string());
        self.pending_permissions.insert(
            string_id.clone(),
            PendingPermission {
                request_id,
                options: options.clone(),
            },
        );
        self.shared.add_permission(AgentPermissionRequest {
            request_id: string_id,
            title,
            description,
            options,
        });
    }

    fn respond_permission(
        &mut self,
        request_id: &str,
        decision: AgentPermissionDecision,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_permissions.remove(request_id) else {
            return Err("权限请求已失效".to_string());
        };
        self.shared.remove_permission(request_id);
        let outcome = match decision {
            AgentPermissionDecision::AllowOnce => {
                select_permission(&pending.options, &["allow_once", "allow_always"])
            }
            AgentPermissionDecision::AllowTask => {
                select_permission(&pending.options, &["allow_always", "allow_once"])
            }
            AgentPermissionDecision::RejectOnce => {
                select_permission(&pending.options, &["reject_once", "reject_always"])
            }
            AgentPermissionDecision::Cancel => None,
        };
        let result = match outcome {
            Some(option_id) if decision != AgentPermissionDecision::Cancel => json!({
                "outcome": { "outcome": "selected", "optionId": option_id }
            }),
            _ => json!({ "outcome": { "outcome": "cancelled" } }),
        };
        self.write_json(json!({
            "jsonrpc": "2.0",
            "id": pending.request_id,
            "result": result
        }))
    }

    fn cancel_running(&mut self) -> Result<(), String> {
        let state = self
            .shared
            .snapshot
            .lock()
            .map(|snapshot| snapshot.state)
            .unwrap_or(AgentRuntimeState::Failed);
        if !matches!(
            state,
            AgentRuntimeState::Running
                | AgentRuntimeState::WaitingPermission
                | AgentRuntimeState::Initializing
                | AgentRuntimeState::Starting
        ) {
            return Ok(());
        }
        self.shared.set_state(AgentRuntimeState::Cancelling);
        self.cancel_started = Some(Instant::now());
        self.cancel_pending_permissions("cancelled");
        if state == AgentRuntimeState::Initializing || state == AgentRuntimeState::Starting {
            self.shared.emit(
                AgentEventKind::Diagnostic,
                Some("正在终止 Agent 启动".to_string()),
                None,
            );
            self.shutdown_child();
            self.complete(AgentRuntimeState::Cancelled, "Agent 启动已取消".to_string());
            return Ok(());
        }
        self.write_cancel_notification()
    }

    fn write_cancel_notification(&mut self) -> Result<(), String> {
        let Some(session_id) = self.session_id.clone() else {
            return Ok(());
        };
        self.write_json(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": { "sessionId": session_id }
        }))
    }

    fn cancel_pending_permissions(&mut self, outcome: &str) {
        let pending = self.pending_permissions.drain().collect::<Vec<_>>();
        for (_, pending) in pending {
            let result = if outcome == "cancelled" {
                json!({ "outcome": { "outcome": "cancelled" } })
            } else {
                json!({ "outcome": { "outcome": outcome } })
            };
            let _ = self.write_json(json!({
                "jsonrpc": "2.0",
                "id": pending.request_id,
                "result": result
            }));
        }
        if let Ok(mut snapshot) = self.shared.snapshot.lock() {
            snapshot.permission_requests.clear();
        }
    }

    fn complete(&mut self, state: AgentRuntimeState, message: String) {
        self.prompt_started = None;
        if let Ok(mut snapshot) = self.shared.snapshot.lock() {
            snapshot.state = state;
            snapshot.error = if state == AgentRuntimeState::Failed {
                Some(message.clone())
            } else {
                None
            };
            snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        let kind = match state {
            AgentRuntimeState::Completed => AgentEventKind::Completed,
            AgentRuntimeState::Cancelled => AgentEventKind::Cancelled,
            AgentRuntimeState::Failed => AgentEventKind::Failed,
            _ => AgentEventKind::Status,
        };
        self.shared.emit(kind, Some(message), None);
        if state == AgentRuntimeState::Failed {
            record_agent_event("error", "runtime_complete", "Agent 运行失败");
        }
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.shared.set_error(error);
        self.prompt_started = None;
    }

    fn check_process_exit(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let Ok(Some(status)) = child.try_wait() else {
            return;
        };
        let state = self
            .shared
            .snapshot
            .lock()
            .map(|snapshot| snapshot.state)
            .unwrap_or(AgentRuntimeState::Failed);
        if matches!(
            state,
            AgentRuntimeState::Completed
                | AgentRuntimeState::Failed
                | AgentRuntimeState::Cancelled
                | AgentRuntimeState::Closed
        ) {
            return;
        }
        self.fail(format!("Agent 进程已退出：{status}"));
    }

    fn check_timeout(&mut self) -> bool {
        let state = self
            .shared
            .snapshot
            .lock()
            .map(|snapshot| snapshot.state)
            .unwrap_or(AgentRuntimeState::Failed);
        if state == AgentRuntimeState::Cancelling
            && self
                .cancel_started
                .is_some_and(|started| started.elapsed() >= CANCEL_GRACE)
        {
            self.shutdown_child();
            self.complete(
                AgentRuntimeState::Cancelled,
                "Agent 未在宽限期内结束，已终止进程".to_string(),
            );
            return false;
        }
        if matches!(
            state,
            AgentRuntimeState::Starting | AgentRuntimeState::Initializing
        ) && self.started.elapsed()
            >= Duration::from_millis(u64::from(self.profile.startup_timeout_ms))
        {
            self.shutdown_child();
            self.fail("Agent 启动或 ACP 初始化超时");
            return true;
        }
        if matches!(
            state,
            AgentRuntimeState::Running | AgentRuntimeState::WaitingPermission
        ) {
            if self.last_activity.elapsed()
                >= Duration::from_millis(u64::from(self.profile.idle_timeout_ms))
            {
                self.shutdown_child();
                self.fail(format!(
                    "Agent 空闲超时（{} 秒）",
                    self.profile.idle_timeout_ms / 1_000
                ));
                return true;
            }
            if self
                .prompt_started
                .is_some_and(|started| started.elapsed() >= self.total_prompt_timeout)
            {
                self.shutdown_child();
                self.fail("Agent 提示总超时");
                return true;
            }
        }
        false
    }

    fn next_id(&mut self) -> u64 {
        self.next_request_id += 1;
        self.next_request_id
    }

    fn write_json(&mut self, value: Value) -> Result<(), String> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err("Agent stdin 已关闭".to_string());
        };
        serde_json::to_writer(&mut *stdin, &value)
            .map_err(|error| format!("无法写入 Agent 协议帧：{error}"))?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("无法发送 Agent 协议帧：{error}"))
    }

    fn respond_jsonrpc_error(&mut self, id: Value, code: i64, message: &str) -> Result<(), String> {
        self.write_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }))
    }

    fn restored_history_context(&self) -> Option<String> {
        let mut history = String::new();
        for message in &self.restored_messages {
            let role = match message.role {
                AgentMessageRole::User => "用户",
                AgentMessageRole::Assistant => "Agent",
                AgentMessageRole::System => "系统",
            };
            let remaining = MAX_RESTORED_CONTEXT_BYTES.saturating_sub(history.len());
            if remaining == 0 {
                break;
            }
            let content = truncate_text(&message.content, remaining);
            history.push_str(role);
            history.push_str(": ");
            history.push_str(&content);
            history.push_str("\n\n");
        }
        (!history.trim().is_empty()).then_some(history)
    }

    fn shutdown_child(&mut self) {
        self.cancel_pending_permissions("cancelled");
        if let Some(mut child) = self.child.take() {
            terminate_agent_process(&mut child);
        }
        self.stdin.take();
    }

    fn finish(&mut self) {
        self.shutdown_child();
        self.shared.close();
    }
}

fn read_stdout(stdout: std::process::ChildStdout, sender: mpsc::Sender<RuntimeSignal>) {
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => {
                let _ = sender.send(RuntimeSignal::ReaderClosed);
                break;
            }
            Ok(_) if bytes.len() > MAX_ACP_LINE_BYTES => {
                let _ = sender.send(RuntimeSignal::StdoutTooLarge);
                break;
            }
            Ok(_) => {
                let line = String::from_utf8_lossy(&bytes).trim_end().to_string();
                if sender.send(RuntimeSignal::StdoutLine(line)).is_err() {
                    break;
                }
            }
        }
    }
}

fn content_text(content: &Value) -> String {
    match content.get("type").and_then(Value::as_str) {
        Some("text") => content
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Some("resource_link") => {
            let name = content
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("resource");
            format!("[{name}]")
        }
        _ => String::new(),
    }
}

fn collect_capabilities(value: Value) -> Vec<String> {
    let mut capabilities = Vec::new();
    if value
        .get("loadSession")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        capabilities.push("load_session".to_string());
    }
    if let Some(prompt) = value.get("promptCapabilities") {
        for (key, label) in [
            ("image", "prompt_image"),
            ("audio", "prompt_audio"),
            ("embeddedContext", "prompt_embedded_context"),
        ] {
            if prompt.get(key).and_then(Value::as_bool).unwrap_or(false) {
                capabilities.push(label.to_string());
            }
        }
    }
    if let Some(mcp) = value.get("mcpCapabilities") {
        for (key, label) in [("http", "mcp_http"), ("sse", "mcp_sse")] {
            if mcp.get(key).and_then(Value::as_bool).unwrap_or(false) {
                capabilities.push(label.to_string());
            }
        }
    }
    capabilities
}

fn select_permission(options: &[AgentPermissionOption], preferred: &[&str]) -> Option<String> {
    for kind in preferred {
        if let Some(option) = options.iter().find(|option| option.kind == *kind) {
            return Some(option.option_id.clone());
        }
    }
    options
        .iter()
        .find(|option| !option.kind.starts_with("reject"))
        .map(|option| option.option_id.clone())
}

fn path_to_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && !normalized.starts_with('/') {
        format!("file:///{normalized}")
    } else {
        format!("file://{normalized}")
    }
}

fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
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
    fn permission_selection_prefers_requested_kind() {
        let options = vec![
            AgentPermissionOption {
                option_id: "reject".to_string(),
                name: "Reject".to_string(),
                kind: "reject_once".to_string(),
            },
            AgentPermissionOption {
                option_id: "allow".to_string(),
                name: "Allow".to_string(),
                kind: "allow_once".to_string(),
            },
        ];
        assert_eq!(
            select_permission(&options, &["allow_once"]),
            Some("allow".to_string())
        );
    }

    #[test]
    fn file_uris_are_well_formed() {
        let uri = path_to_uri(Path::new("/tmp/example.txt"));
        assert_eq!(uri, "file:///tmp/example.txt");
    }
}
