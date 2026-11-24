use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent::config::ValidatedSessionConfig;
use crate::agent::events::EventDispatcher;
use crate::agent::model::{
    ACP_PROTOCOL_VERSION, AcpCapabilities, AgentDiagnosticLevel, AgentError, AgentErrorKind,
    AgentEvent, AgentFailure, AgentPermissionOption, AgentPermissionRequest, AgentPlanEntry,
    AgentToolCall,
};
use crate::agent::process::{ProcessGuard, ProcessHandle};
use crate::agent::redact::Redactor;
use crate::agent::transport::{AgentHandshake, AgentPrompt, AgentTransport, TransportEvent};

const DIAGNOSTIC_DETAIL_CHARS: usize = 2048;
const PROTOCOL_PREVIEW_CHARS: usize = 240;

#[derive(Clone)]
struct PendingPermission {
    wire_id: Value,
    option_ids: HashSet<String>,
}

enum Incoming {
    Json(Value),
    Stderr(String),
    StdoutClosed,
    StderrClosed,
    Fatal(AgentFailure),
}

pub(crate) struct AcpStdioTransport {
    process: Option<ProcessGuard>,
    stdin: Option<ChildStdin>,
    incoming: Option<Receiver<Incoming>>,
    events: Option<Arc<EventDispatcher>>,
    redactor: Option<Arc<Redactor>>,
    next_id: u64,
    session_id: Option<String>,
    pending_prompt: Option<u64>,
    pending_permissions: HashMap<String, PendingPermission>,
    output_bytes: Arc<AtomicU64>,
    max_output_bytes: u64,
    shutdown_timeout: Duration,
    reader_threads: Vec<JoinHandle<()>>,
    process_handle: Option<Arc<ProcessHandle>>,
}

impl AcpStdioTransport {
    pub(crate) fn new() -> Self {
        Self {
            process: None,
            stdin: None,
            incoming: None,
            events: None,
            redactor: None,
            next_id: 1,
            session_id: None,
            pending_prompt: None,
            pending_permissions: HashMap::new(),
            output_bytes: Arc::new(AtomicU64::new(0)),
            max_output_bytes: 0,
            shutdown_timeout: Duration::from_secs(2),
            reader_threads: Vec::new(),
            process_handle: None,
        }
    }

    fn start_process(
        &mut self,
        config: &ValidatedSessionConfig,
        events: Arc<EventDispatcher>,
        process_slot: &OnceLock<Arc<ProcessHandle>>,
        timeout: Duration,
    ) -> Result<AgentHandshake, AgentError> {
        if self.process.is_some() {
            return Err(AgentError::new(
                AgentErrorKind::InvalidConfiguration,
                "Agent 会话已经启动。",
            ));
        }

        let spawned = ProcessGuard::spawn(config.command())?;
        let pid = spawned.handle.pid();
        if process_slot.set(spawned.handle.clone()).is_err() {
            return Err(AgentError::new(
                AgentErrorKind::InvalidConfiguration,
                "Agent 进程句柄已经初始化。",
            ));
        }

        self.process = Some(spawned.guard);
        self.stdin = Some(spawned.stdin);
        self.events = Some(events.clone());
        self.redactor = Some(config.redactor.clone());
        self.max_output_bytes = config.max_output_bytes;
        self.shutdown_timeout = config.shutdown_timeout;
        self.output_bytes.store(0, Ordering::Release);
        self.process_handle = Some(spawned.handle);

        let (sender, receiver) = mpsc::sync_channel(64);
        self.incoming = Some(receiver);
        let fatal = Arc::new(AtomicBool::new(false));
        let output_bytes = self.output_bytes.clone();
        self.reader_threads.push(spawn_stdout_reader(
            spawned.stdout,
            sender.clone(),
            fatal.clone(),
            config.redactor.clone(),
            config.max_frame_bytes,
            config.max_output_bytes,
            output_bytes.clone(),
        ));
        self.reader_threads.push(spawn_stderr_reader(
            spawned.stderr,
            sender,
            fatal,
            config.redactor.clone(),
            config.max_frame_bytes,
            config.max_output_bytes,
            output_bytes,
        ));

        events.emit(AgentEvent::ProcessStarted { pid });

        let deadline = Instant::now() + timeout;
        match self.initialize(deadline) {
            Ok(handshake) => Ok(handshake),
            Err(error) => {
                self.shutdown();
                Err(error)
            }
        }
    }

    fn initialize(&mut self, deadline: Instant) -> Result<AgentHandshake, AgentError> {
        let params = json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": {
                    "readTextFile": false,
                    "writeTextFile": false
                },
                "terminal": false,
                "auth": {
                    "terminal": false
                }
            },
            "clientInfo": {
                "name": "Kite",
                "title": "Kite",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let result = self.request("initialize", params, deadline)?;
        self.parse_handshake(&result)
    }

    fn parse_handshake(&self, result: &Value) -> Result<AgentHandshake, AgentError> {
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AgentError::new(
                    AgentErrorKind::Protocol,
                    "Agent 未返回有效的 ACP 协议版本。",
                )
            })?;
        if protocol_version != u64::from(ACP_PROTOCOL_VERSION) {
            return Err(AgentError::new(
                AgentErrorKind::UnsupportedVersion,
                format!(
                    "Agent 使用的 ACP 协议版本为 {protocol_version}，Kite 支持版本 {ACP_PROTOCOL_VERSION}。"
                ),
            ));
        }

        let capabilities_value = result
            .get("agentCapabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !capabilities_value.is_object() {
            return Err(AgentError::new(
                AgentErrorKind::Protocol,
                "Agent 返回了无效的能力声明。",
            ));
        }
        let prompt_caps = &capabilities_value["promptCapabilities"];
        let mcp_caps = &capabilities_value["mcpCapabilities"];
        let session_caps = &capabilities_value["sessionCapabilities"];
        let capabilities = AcpCapabilities {
            load_session: bool_field(&capabilities_value, "loadSession"),
            prompt_image: bool_field(prompt_caps, "image"),
            prompt_audio: bool_field(prompt_caps, "audio"),
            prompt_embedded_context: bool_field(prompt_caps, "embeddedContext"),
            mcp_http: bool_field(mcp_caps, "http"),
            mcp_sse: bool_field(mcp_caps, "sse"),
            session_close: bool_field(session_caps, "close"),
        };

        let info = &result["agentInfo"];
        let agent_name = info
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("ACP Agent")
            .to_string();
        let agent_version = info
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        Ok(AgentHandshake {
            protocol_version: ACP_PROTOCOL_VERSION,
            agent_name,
            agent_version,
            capabilities,
        })
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, AgentError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write_request(id, method, params)?;

        loop {
            let incoming = self.recv_until(deadline)?;
            match incoming {
                Incoming::Json(message) => {
                    if response_id(&message) == Some(id) {
                        if let Some(error) = message.get("error") {
                            return Err(self.classify_rpc_error(method, error));
                        }
                        return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                    }
                    let _ = self.handle_async_message(message, false)?;
                }
                Incoming::Stderr(line) => self.emit_stderr(&line),
                Incoming::Fatal(failure) => return Err(AgentError::from(failure)),
                Incoming::StdoutClosed => return Err(self.process_exit_error()),
                Incoming::StderrClosed => {}
            }
        }
    }

    fn create_session_request(
        &mut self,
        cwd: &Path,
        additional_directories: &[PathBuf],
        deadline: Instant,
    ) -> Result<String, AgentError> {
        let additional = additional_directories
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut params = json!({
            "cwd": cwd.to_string_lossy(),
            "mcpServers": []
        });
        if !additional.is_empty() {
            params["additionalDirectories"] =
                Value::Array(additional.into_iter().map(Value::String).collect());
        }
        let result = self.request("session/new", params, deadline)?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AgentError::new(AgentErrorKind::Protocol, "Agent 未返回 ACP 会话标识。")
            })?
            .to_string();
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    fn write_request(&mut self, id: u64, method: &str, params: Value) -> Result<(), AgentError> {
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
    }

    fn write_notification(&mut self, method: &str, params: Value) -> Result<(), AgentError> {
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
    }

    fn write_response(&mut self, id: Value, result: Value) -> Result<(), AgentError> {
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }))
    }

    fn write_error_response(
        &mut self,
        id: Value,
        code: i64,
        message: &str,
    ) -> Result<(), AgentError> {
        self.write_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message
            }
        }))
    }

    fn write_value(&mut self, value: &Value) -> Result<(), AgentError> {
        let bytes = serde_json::to_vec(value).map_err(|error| {
            AgentError::new(AgentErrorKind::Protocol, "无法编码 ACP 消息。")
                .with_detail(error.to_string())
        })?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Closed, "Agent 标准输入已经关闭。"))?;
        stdin
            .write_all(&bytes)
            .map_err(|error| write_error(&error))?;
        stdin
            .write_all(b"\n")
            .map_err(|error| write_error(&error))?;
        stdin.flush().map_err(|error| write_error(&error))
    }

    fn recv_until(&self, deadline: Instant) -> Result<Incoming, AgentError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timeout_error("等待 Agent 响应超时。"));
        }
        let incoming = self
            .incoming
            .as_ref()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Closed, "Agent 输出通道已经关闭。"))?;
        match incoming.recv_timeout(remaining) {
            Ok(message) => Ok(message),
            Err(RecvTimeoutError::Timeout) => Err(timeout_error("等待 Agent 响应超时。")),
            Err(RecvTimeoutError::Disconnected) => Err(self.process_exit_error()),
        }
    }

    fn emit_stderr(&self, line: &str) {
        if let Some(events) = &self.events
            && let Some(redactor) = &self.redactor
        {
            events.emit(AgentEvent::Diagnostic {
                level: AgentDiagnosticLevel::Error,
                message: redactor.truncate(line, DIAGNOSTIC_DETAIL_CHARS),
            });
        }
    }

    fn classify_rpc_error(&self, _method: &str, error: &Value) -> AgentError {
        let code = error.get("code").and_then(Value::as_i64);
        let raw_message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Agent 返回了协议错误。");
        let detail = self
            .redactor
            .as_ref()
            .map(|redactor| redactor.truncate(raw_message, DIAGNOSTIC_DETAIL_CHARS))
            .unwrap_or_else(|| raw_message.to_string());
        let lower = raw_message.to_ascii_lowercase();
        let kind = if code == Some(-32000)
            || lower.contains("auth_required")
            || lower.contains("authentication required")
            || lower.contains("unauthorized")
        {
            AgentErrorKind::AuthenticationRequired
        } else if code == Some(-32602) {
            AgentErrorKind::InvalidConfiguration
        } else if code == Some(-32601) {
            AgentErrorKind::Protocol
        } else if lower.contains("protocol version") || lower.contains("unsupported version") {
            AgentErrorKind::UnsupportedVersion
        } else {
            AgentErrorKind::Other
        };
        let message = match kind {
            AgentErrorKind::AuthenticationRequired => "Agent 认证失败或需要重新登录。",
            AgentErrorKind::InvalidConfiguration => "Agent 拒绝了会话配置。",
            AgentErrorKind::Protocol => "Agent 不支持当前 ACP 请求。",
            AgentErrorKind::UnsupportedVersion => "Agent 与 Kite 的协议版本不兼容。",
            _ => "Agent 返回了错误。",
        };
        AgentError::new(kind, message).with_detail(format!(
            "{}: {}",
            code.map(|value| value.to_string())
                .unwrap_or_else(|| "RPC".to_string()),
            detail
        ))
    }

    fn process_exit_error(&self) -> AgentError {
        AgentError::new(AgentErrorKind::ProcessExited, "Agent 进程已退出。")
    }
}

impl AgentTransport for AcpStdioTransport {
    fn start(
        &mut self,
        config: &ValidatedSessionConfig,
        events: Arc<EventDispatcher>,
        process_slot: &OnceLock<Arc<ProcessHandle>>,
        timeout: Duration,
    ) -> Result<AgentHandshake, AgentError> {
        self.start_process(config, events, process_slot, timeout)
    }

    fn create_session(
        &mut self,
        cwd: &Path,
        additional_directories: &[PathBuf],
        timeout: Duration,
    ) -> Result<String, AgentError> {
        self.create_session_request(cwd, additional_directories, Instant::now() + timeout)
    }

    fn send_prompt(&mut self, session_id: &str, prompt: &AgentPrompt) -> Result<(), AgentError> {
        self.send_prompt_request(session_id, prompt)
    }

    fn wait_for_event(&mut self, timeout: Duration) -> Result<Option<TransportEvent>, AgentError> {
        self.wait_for_transport_event(timeout)
    }

    fn respond_permission(
        &mut self,
        request_id: &str,
        option_id: Option<&str>,
    ) -> Result<bool, AgentError> {
        self.respond_to_permission(request_id, option_id)
    }

    fn cancel(&mut self, session_id: &str) -> Result<(), AgentError> {
        self.cancel_turn(session_id)
    }

    fn shutdown(&mut self) {
        self.shutdown_transport();
    }

    fn try_wait(&mut self) -> Option<ExitStatus> {
        self.process.as_mut().and_then(ProcessGuard::try_wait)
    }
}

impl Drop for AcpStdioTransport {
    fn drop(&mut self) {
        self.shutdown_transport();
    }
}
impl AcpStdioTransport {
    fn send_prompt_request(
        &mut self,
        session_id: &str,
        prompt: &AgentPrompt,
    ) -> Result<(), AgentError> {
        if prompt.text.trim().is_empty() && prompt.context_paths.is_empty() {
            return Err(AgentError::invalid_configuration("Agent 提示词不能为空。"));
        }
        self.output_bytes.store(0, Ordering::Release);

        let mut prompt_blocks = Vec::new();
        if !prompt.text.is_empty() {
            prompt_blocks.push(json!({
                "type": "text",
                "text": prompt.text
            }));
        }
        for path in &prompt.context_paths {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("resource")
                .to_string();
            let size = std::fs::metadata(path).ok().map(|metadata| metadata.len());
            prompt_blocks.push(json!({
                "type": "resource_link",
                "uri": path_to_file_uri(path),
                "name": name,
                "title": name,
                "size": size
            }));
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.pending_prompt = Some(id);
        let params = json!({
            "sessionId": session_id,
            "prompt": prompt_blocks
        });
        if let Err(error) = self.write_request(id, "session/prompt", params) {
            self.pending_prompt = None;
            return Err(error);
        }
        Ok(())
    }

    fn wait_for_transport_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<TransportEvent>, AgentError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let incoming = match self.receive_optional(remaining) {
                Ok(Some(incoming)) => incoming,
                Ok(None) => return Ok(None),
                Err(error) => return Err(error),
            };
            match incoming {
                Incoming::Json(message) => {
                    if let Some(event) = self.handle_async_message(message, true)? {
                        return Ok(Some(event));
                    }
                }
                Incoming::Stderr(line) => self.emit_stderr(&line),
                Incoming::Fatal(failure) => return Err(AgentError::from(failure)),
                Incoming::StdoutClosed => {
                    let exit_code = self
                        .process
                        .as_mut()
                        .and_then(ProcessGuard::try_wait)
                        .and_then(|status| status.code());
                    return Ok(Some(TransportEvent::ProcessExited(exit_code)));
                }
                Incoming::StderrClosed => {}
            }
        }
    }

    fn receive_optional(&self, timeout: Duration) -> Result<Option<Incoming>, AgentError> {
        let incoming = self
            .incoming
            .as_ref()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Closed, "Agent 输出通道已经关闭。"))?;
        match incoming.recv_timeout(timeout) {
            Ok(incoming) => Ok(Some(incoming)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(AgentError::new(
                AgentErrorKind::ProcessExited,
                "Agent 已关闭输出通道。",
            )),
        }
    }

    fn handle_async_message(
        &mut self,
        message: Value,
        allow_permission: bool,
    ) -> Result<Option<TransportEvent>, AgentError> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(id) = message.get("id").cloned() {
                if method == "session/request_permission" && allow_permission {
                    return self
                        .handle_permission_request(id, &message["params"])
                        .map(Some);
                }
                self.write_error_response(
                    id,
                    -32601,
                    "Kite P0 does not expose this ACP client method.",
                )?;
                return Ok(Some(TransportEvent::Activity));
            }

            if method == "session/update" {
                self.handle_session_update(&message["params"])?;
                return Ok(Some(TransportEvent::Activity));
            }
            return Ok(Some(TransportEvent::Activity));
        }

        let Some(id) = response_id(&message) else {
            return Err(AgentError::new(
                AgentErrorKind::Protocol,
                "Agent 发送了无法识别的 JSON-RPC 消息。",
            ));
        };
        if Some(id) != self.pending_prompt {
            if let Some(error) = message.get("error") {
                return Err(self.classify_rpc_error("unknown", error));
            }
            return Ok(Some(TransportEvent::Activity));
        }

        if let Some(error) = message.get("error") {
            self.pending_prompt = None;
            return Err(self.classify_rpc_error("session/prompt", error));
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        let reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AgentError::new(AgentErrorKind::Protocol, "Agent 未返回提示词停止原因。")
            })?;
        let reason = crate::agent::model::AgentStopReason::from_wire(reason);
        self.pending_prompt = None;
        Ok(Some(TransportEvent::PromptCompleted(reason)))
    }

    fn handle_permission_request(
        &mut self,
        wire_id: Value,
        params: &Value,
    ) -> Result<TransportEvent, AgentError> {
        if let Some(expected) = self.session_id.as_deref()
            && params.get("sessionId").and_then(Value::as_str) != Some(expected)
        {
            return Err(AgentError::new(
                AgentErrorKind::Protocol,
                "Agent 权限请求不属于当前会话。",
            ));
        }
        let tool_call = params.get("toolCall").ok_or_else(|| {
            AgentError::new(AgentErrorKind::Protocol, "Agent 权限请求缺少工具调用。")
        })?;
        let tool_call_id = tool_call
            .get("toolCallId")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::new(AgentErrorKind::Protocol, "Agent 工具调用缺少标识。"))?
            .to_string();
        let title = tool_call
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string);
        let kind = tool_call
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string);

        let mut options = Vec::new();
        let mut option_ids = HashSet::new();
        for option in params
            .get("options")
            .and_then(Value::as_array)
            .ok_or_else(|| AgentError::new(AgentErrorKind::Protocol, "Agent 权限请求缺少选项。"))?
        {
            let option_id = option
                .get("optionId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AgentError::new(AgentErrorKind::Protocol, "Agent 权限选项缺少标识。")
                })?;
            let name = option
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(option_id);
            let kind = option
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            option_ids.insert(option_id.to_string());
            options.push(AgentPermissionOption {
                option_id: option_id.to_string(),
                name: name.to_string(),
                kind: kind.to_string(),
            });
        }
        if options.is_empty() {
            return Err(AgentError::new(
                AgentErrorKind::Protocol,
                "Agent 权限请求没有可用选项。",
            ));
        }

        let public_id = request_id_string(&wire_id);
        self.pending_permissions.insert(
            public_id.clone(),
            PendingPermission {
                wire_id,
                option_ids,
            },
        );
        Ok(TransportEvent::PermissionRequested(
            AgentPermissionRequest {
                request_id: public_id,
                tool_call_id,
                title,
                kind,
                options,
            },
        ))
    }

    fn respond_to_permission(
        &mut self,
        request_id: &str,
        option_id: Option<&str>,
    ) -> Result<bool, AgentError> {
        let Some(pending) = self.pending_permissions.remove(request_id) else {
            return Ok(false);
        };
        if let Some(option_id) = option_id
            && !pending.option_ids.contains(option_id)
        {
            self.pending_permissions
                .insert(request_id.to_string(), pending);
            return Err(AgentError::invalid_configuration("Agent 权限选项不存在。"));
        }
        let outcome = if let Some(option_id) = option_id {
            json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id
                }
            })
        } else {
            json!({ "outcome": { "outcome": "cancelled" } })
        };
        self.write_response(pending.wire_id, outcome)?;
        Ok(true)
    }

    fn cancel_turn(&mut self, session_id: &str) -> Result<(), AgentError> {
        self.cancel_pending_permissions()?;
        self.write_notification("session/cancel", json!({ "sessionId": session_id }))
    }

    fn cancel_pending_permissions(&mut self) -> Result<(), AgentError> {
        let pending = std::mem::take(&mut self.pending_permissions);
        for permission in pending.into_values() {
            self.write_response(
                permission.wire_id,
                json!({ "outcome": { "outcome": "cancelled" } }),
            )?;
        }
        Ok(())
    }

    fn handle_session_update(&mut self, params: &Value) -> Result<(), AgentError> {
        if let Some(expected) = self.session_id.as_deref()
            && params.get("sessionId").and_then(Value::as_str) != Some(expected)
        {
            return Ok(());
        }
        let update = params
            .get("update")
            .ok_or_else(|| AgentError::new(AgentErrorKind::Protocol, "Agent 会话更新缺少内容。"))?;
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::new(AgentErrorKind::Protocol, "Agent 会话更新缺少类型。"))?;
        let events = self
            .events
            .as_ref()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Closed, "Agent 事件通道已经关闭。"))?;
        let message_id = update
            .get("messageId")
            .and_then(Value::as_str)
            .map(str::to_string);

        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let text = update
                    .get("content")
                    .and_then(|content| content.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if kind == "agent_message_chunk" {
                    events.emit(AgentEvent::MessageChunk { text, message_id });
                } else {
                    events.emit(AgentEvent::ThoughtChunk { text, message_id });
                }
            }
            "tool_call" | "tool_call_update" => {
                let tool_call = parse_tool_call(update)?;
                events.emit(AgentEvent::ToolCallUpdated { tool_call });
            }
            "plan" => {
                let entries = update
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| {
                                Some(AgentPlanEntry {
                                    content: entry.get("content")?.as_str()?.to_string(),
                                    status: entry
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                    priority: entry
                                        .get("priority")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                events.emit(AgentEvent::PlanUpdated { entries });
            }
            _ => {}
        }
        Ok(())
    }
}
impl AcpStdioTransport {
    fn shutdown_transport(&mut self) {
        let _ = self.cancel_pending_permissions();
        self.stdin.take();
        if let Some(mut process) = self.process.take() {
            process.terminate_tree(self.shutdown_timeout);
        }
        self.incoming.take();
        for reader in self.reader_threads.drain(..) {
            if reader.is_finished() {
                let _ = reader.join();
            } else {
                // The OS pipes will close when the job/process group is terminated.
                // Detaching here keeps timeout and shutdown events off the caller's
                // critical path while the reader exits.
                drop(reader);
            }
        }
    }
}

impl From<AgentFailure> for AgentError {
    fn from(failure: AgentFailure) -> Self {
        AgentError::Failure {
            kind: failure.kind,
            message: failure.message,
            detail: failure.detail,
            retryable: failure.retryable,
        }
    }
}

fn bool_field(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool).unwrap_or(false)
}

fn response_id(message: &Value) -> Option<u64> {
    message.get("id").and_then(Value::as_u64)
}

fn request_id_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

fn timeout_error(message: &str) -> AgentError {
    AgentError::new(AgentErrorKind::Timeout, message)
}

fn write_error(error: &std::io::Error) -> AgentError {
    let kind = match error.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected => {
            AgentErrorKind::ProcessExited
        }
        _ => AgentErrorKind::Io,
    };
    AgentError::new(kind, "无法写入 Agent 标准输入。").with_detail(error.to_string())
}

fn parse_tool_call(value: &Value) -> Result<AgentToolCall, AgentError> {
    let tool_call_id = value
        .get("toolCallId")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::new(AgentErrorKind::Protocol, "Agent 工具调用缺少标识。"))?
        .to_string();
    Ok(AgentToolCall {
        tool_call_id,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string),
        kind: value
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
        content: content_text(value.get("content")),
    })
}

fn content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

fn path_to_file_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        let normalized = text.replace('\\', "/");
        let prefix = if normalized.starts_with("//") {
            "file:"
        } else {
            "file:///"
        };
        format!("{prefix}{}", percent_encode_uri_path(&normalized))
    }
    #[cfg(not(windows))]
    {
        format!("file://{}", percent_encode_uri_path(&text))
    }
}

fn percent_encode_uri_path(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        let allowed =
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':');
        if allowed {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn spawn_stdout_reader(
    stdout: ChildStdout,
    sender: SyncSender<Incoming>,
    fatal: Arc<AtomicBool>,
    redactor: Arc<Redactor>,
    max_frame_bytes: usize,
    max_output_bytes: u64,
    output_bytes: Arc<AtomicU64>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader, max_frame_bytes) {
                Ok(FrameRead::Eof) => {
                    let _ = sender.send(Incoming::StdoutClosed);
                    return;
                }
                Ok(FrameRead::TooLarge) => {
                    send_fatal(
                        &sender,
                        &fatal,
                        AgentFailure::new(AgentErrorKind::OutputLimit, "Agent 单帧输出超过上限。"),
                    );
                    return;
                }
                Ok(FrameRead::Line(mut line)) => {
                    let bytes = line.len() as u64;
                    if !count_output(&output_bytes, bytes, max_output_bytes) {
                        send_fatal(
                            &sender,
                            &fatal,
                            AgentFailure::new(
                                AgentErrorKind::OutputLimit,
                                "Agent 输出超过会话上限。",
                            ),
                        );
                        return;
                    }
                    trim_line_ending(&mut line);
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_slice::<Value>(&line) {
                        Ok(value) => {
                            if sender.send(Incoming::Json(value)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let preview = redactor
                                .truncate(&String::from_utf8_lossy(&line), PROTOCOL_PREVIEW_CHARS);
                            send_fatal(
                                &sender,
                                &fatal,
                                AgentFailure {
                                    kind: AgentErrorKind::Protocol,
                                    message: "Agent 输出了无效的 JSON-RPC 消息。".to_string(),
                                    detail: Some(format!("{error}: {preview}")),
                                    retryable: false,
                                },
                            );
                            return;
                        }
                    }
                }
                Err(error) => {
                    send_fatal(
                        &sender,
                        &fatal,
                        AgentFailure {
                            kind: AgentErrorKind::Io,
                            message: "读取 Agent 标准输出失败。".to_string(),
                            detail: Some(redactor.truncate(&error.to_string(), 500)),
                            retryable: AgentErrorKind::Io.default_retryable(),
                        },
                    );
                    return;
                }
            }
        }
    })
}

fn spawn_stderr_reader(
    stderr: ChildStderr,
    sender: SyncSender<Incoming>,
    fatal: Arc<AtomicBool>,
    redactor: Arc<Redactor>,
    max_frame_bytes: usize,
    max_output_bytes: u64,
    output_bytes: Arc<AtomicU64>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        loop {
            match read_frame(&mut reader, max_frame_bytes) {
                Ok(FrameRead::Eof) => {
                    let _ = sender.send(Incoming::StderrClosed);
                    return;
                }
                Ok(FrameRead::TooLarge) => {
                    send_fatal(
                        &sender,
                        &fatal,
                        AgentFailure::new(
                            AgentErrorKind::OutputLimit,
                            "Agent 标准错误单帧超过上限。",
                        ),
                    );
                    return;
                }
                Ok(FrameRead::Line(mut line)) => {
                    let bytes = line.len() as u64;
                    if !count_output(&output_bytes, bytes, max_output_bytes) {
                        send_fatal(
                            &sender,
                            &fatal,
                            AgentFailure::new(
                                AgentErrorKind::OutputLimit,
                                "Agent 输出超过会话上限。",
                            ),
                        );
                        return;
                    }
                    trim_line_ending(&mut line);
                    if line.is_empty() {
                        continue;
                    }
                    let line = String::from_utf8_lossy(&line).into_owned();
                    if sender.send(Incoming::Stderr(line)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    send_fatal(
                        &sender,
                        &fatal,
                        AgentFailure {
                            kind: AgentErrorKind::Io,
                            message: "读取 Agent 标准错误失败。".to_string(),
                            detail: Some(redactor.truncate(&error.to_string(), 500)),
                            retryable: AgentErrorKind::Io.default_retryable(),
                        },
                    );
                    return;
                }
            }
        }
    })
}

fn send_fatal(sender: &SyncSender<Incoming>, fatal: &AtomicBool, failure: AgentFailure) {
    if !fatal.swap(true, Ordering::AcqRel) {
        let _ = sender.send(Incoming::Fatal(failure));
    }
}

fn count_output(counter: &AtomicU64, bytes: u64, limit: u64) -> bool {
    let previous = counter.fetch_add(bytes, Ordering::AcqRel);
    previous.saturating_add(bytes) <= limit
}

fn trim_line_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

enum FrameRead {
    Eof,
    Line(Vec<u8>),
    TooLarge,
}

fn read_frame<R: BufRead>(reader: &mut R, limit: usize) -> std::io::Result<FrameRead> {
    let mut line = Vec::new();
    loop {
        let (chunk, ended, chunk_len) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(if line.is_empty() {
                    FrameRead::Eof
                } else {
                    FrameRead::Line(line)
                });
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let remaining_with_guard = limit.saturating_sub(line.len()).saturating_add(1);
            let take = newline
                .map(|position| position + 1)
                .unwrap_or(available.len())
                .min(remaining_with_guard);
            if take == 0 {
                return Ok(FrameRead::TooLarge);
            }
            (
                available[..take].to_vec(),
                newline.is_some_and(|position| take == position + 1),
                take,
            )
        };
        reader.consume(chunk_len);
        line.extend_from_slice(&chunk);
        if line.len() > limit {
            return Ok(FrameRead::TooLarge);
        }
        if ended {
            return Ok(FrameRead::Line(line));
        }
    }
}
