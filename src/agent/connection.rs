use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::process::build_agent_command;
use super::{AgentProfile, AgentTransport, PermissionPolicy, WorkingDirectoryPolicy, credentials};

const MAX_STDERR_BYTES: u64 = 64 * 1024;
const ACP_PROTOCOL_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum AgentConnectionStatus {
    #[default]
    Connected,
    AuthenticationRequired,
    NotInstalled,
    IncompatibleVersion,
    AuthenticationFailed,
    ProtocolError,
    PermissionDenied,
    Timeout,
    OutputTooLarge,
    InvalidProfile,
    CredentialStore,
    Io,
    Other,
}

impl AgentConnectionStatus {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Connected | Self::AuthenticationRequired)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "已连接",
            Self::AuthenticationRequired => "需要认证",
            Self::NotInstalled => "未安装",
            Self::IncompatibleVersion => "版本不兼容",
            Self::AuthenticationFailed => "认证失败",
            Self::ProtocolError => "协议错误",
            Self::PermissionDenied => "权限不足",
            Self::Timeout => "连接超时",
            Self::OutputTooLarge => "输出超过限制",
            Self::InvalidProfile => "配置无效",
            Self::CredentialStore => "钥匙串错误",
            Self::Io => "启动失败",
            Self::Other => "连接失败",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentConnectionResult {
    pub success: bool,
    pub status: AgentConnectionStatus,
    pub message: String,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub protocol_version: Option<u32>,
    pub capabilities: Vec<String>,
    pub auth_methods: Vec<String>,
    pub elapsed_ms: u64,
}

impl AgentConnectionResult {
    fn failure(
        status: AgentConnectionStatus,
        message: impl Into<String>,
        elapsed: Duration,
    ) -> Self {
        Self {
            success: false,
            status,
            message: message.into(),
            agent_name: None,
            agent_version: None,
            protocol_version: None,
            capabilities: Vec::new(),
            auth_methods: Vec::new(),
            elapsed_ms: elapsed.as_millis() as u64,
        }
    }
}

struct ConnectionFailure {
    status: AgentConnectionStatus,
    message: String,
}

impl ConnectionFailure {
    fn new(status: AgentConnectionStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

enum FrameRead {
    Line(Vec<u8>),
    Eof,
    TooLarge,
}

pub fn test_agent_connection(
    profile: AgentProfile,
    ephemeral_credential: Option<String>,
) -> AgentConnectionResult {
    let started = Instant::now();
    let profile = match super::prepare_loaded_profile(profile, true) {
        Ok(profile) => profile,
        Err(message) => {
            return AgentConnectionResult::failure(
                AgentConnectionStatus::InvalidProfile,
                message.to_string(),
                started.elapsed(),
            );
        }
    };
    let credential = match resolve_credential(&profile, ephemeral_credential) {
        Ok(credential) => credential,
        Err(message) => {
            return AgentConnectionResult::failure(
                AgentConnectionStatus::CredentialStore,
                message,
                started.elapsed(),
            );
        }
    };
    let result = match profile.transport {
        AgentTransport::AcpStdio => test_acp_stdio(&profile, credential.as_deref(), started),
        AgentTransport::AcpHttp => test_acp_http(&profile, credential.as_deref(), started),
        AgentTransport::Cli => test_generic_cli(&profile, credential.as_deref(), started),
    };
    match result {
        Ok(result) => result,
        Err(failure) => {
            AgentConnectionResult::failure(failure.status, failure.message, started.elapsed())
        }
    }
}

fn resolve_credential(
    profile: &AgentProfile,
    ephemeral_credential: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(credential) = ephemeral_credential {
        let credential = credential.trim().to_string();
        if !credential.is_empty() {
            return Ok(Some(credential));
        }
    }
    if profile.credential_ref.is_some() {
        match credentials::load_secret(&profile.id) {
            Ok(Some(secret)) => Ok(Some(secret)),
            Ok(None) => Err("Agent 配置引用了钥匙串凭据，但系统中找不到该条目".to_string()),
            Err(error) => Err(error),
        }
    } else {
        Ok(None)
    }
}

fn profile_working_directory(profile: &AgentProfile) -> Result<PathBuf, ConnectionFailure> {
    let directory = match &profile.working_directory {
        WorkingDirectoryPolicy::Fixed { path } => PathBuf::from(path),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
    };
    if !directory.is_dir() {
        return Err(ConnectionFailure::new(
            AgentConnectionStatus::PermissionDenied,
            format!("工作目录不可用：{}", directory.display()),
        ));
    }
    Ok(directory)
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "auth": { "terminal": false },
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false
            },
            "clientInfo": {
                "name": "kite",
                "title": "Kite",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn test_acp_http(
    profile: &AgentProfile,
    credential: Option<&str>,
    started: Instant,
) -> Result<AgentConnectionResult, ConnectionFailure> {
    let endpoint = profile.endpoint.as_deref().ok_or_else(|| {
        ConnectionFailure::new(AgentConnectionStatus::InvalidProfile, "缺少 ACP HTTP 端点")
    })?;
    let mut request = ureq::post(endpoint)
        .timeout(Duration::from_secs(u64::from(profile.timeout_seconds)))
        .set("content-type", "application/json")
        .set("accept", "application/json");
    if let Some(credential) = credential {
        request = request.set("authorization", &format!("Bearer {credential}"));
    }
    let encoded = serde_json::to_string(&initialize_request()).map_err(|error| {
        ConnectionFailure::new(
            AgentConnectionStatus::Io,
            format!("无法编码 ACP initialize：{error}"),
        )
    })?;
    let response = match request.send_string(&encoded) {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            let detail = redact_text(&body, credential);
            let status = if code == 401 {
                AgentConnectionStatus::AuthenticationFailed
            } else if code == 403 {
                AgentConnectionStatus::PermissionDenied
            } else {
                classify_failure_text(&detail)
            };
            return Err(ConnectionFailure::new(
                status,
                if detail.trim().is_empty() {
                    format!("ACP HTTP 返回状态码 {code}")
                } else {
                    format!("ACP HTTP 返回状态码 {code}：{detail}")
                },
            ));
        }
        Err(ureq::Error::Transport(error)) => {
            return Err(ConnectionFailure::new(
                if error.kind() == ureq::ErrorKind::ConnectionFailed {
                    AgentConnectionStatus::NotInstalled
                } else {
                    AgentConnectionStatus::Io
                },
                format!("无法连接 ACP HTTP 端点：{error}"),
            ));
        }
    };
    let (bytes, exceeded) = read_limited(response.into_reader(), profile.output_limit_bytes);
    if exceeded {
        return Err(ConnectionFailure::new(
            AgentConnectionStatus::OutputTooLarge,
            "Agent HTTP 响应超过配置的输出上限",
        ));
    }
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|_| {
        ConnectionFailure::new(
            AgentConnectionStatus::ProtocolError,
            "ACP HTTP 端点返回了非 JSON-RPC 内容",
        )
    })?;
    parse_initialize_response(value, profile, started)
}

fn test_acp_stdio(
    profile: &AgentProfile,
    credential: Option<&str>,
    started: Instant,
) -> Result<AgentConnectionResult, ConnectionFailure> {
    let working_directory = profile_working_directory(profile)?;
    let (mut command, _) = build_agent_command(profile, &working_directory)
        .map_err(|error| ConnectionFailure::new(AgentConnectionStatus::Io, error))?;
    command
        .current_dir(&working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_environment(&mut command, profile, credential);
    apply_no_window(&mut command);

    let mut child = command.spawn().map_err(classify_spawn_error)?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        ConnectionFailure::new(AgentConnectionStatus::Io, "无法写入 Agent 标准输入")
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ConnectionFailure::new(AgentConnectionStatus::Io, "无法读取 Agent 标准输出")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ConnectionFailure::new(AgentConnectionStatus::Io, "无法读取 Agent 错误输出")
    })?;
    let (frame_tx, frame_rx) = mpsc::channel();
    let stdout_thread = thread::spawn(move || read_frames(stdout, frame_tx));
    let (stderr_tx, stderr_rx) = mpsc::channel();
    let stderr_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = BufReader::new(stderr)
            .take(MAX_STDERR_BYTES)
            .read_to_end(&mut bytes);
        let _ = stderr_tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });

    let mut encoded = serde_json::to_vec(&initialize_request())
        .map_err(|error| ConnectionFailure::new(AgentConnectionStatus::Io, error.to_string()))?;
    encoded.push(b'\n');
    stdin.write_all(&encoded).map_err(|error| {
        ConnectionFailure::new(
            AgentConnectionStatus::Io,
            format!("无法发送 ACP initialize：{error}"),
        )
    })?;
    stdin.flush().map_err(|error| {
        ConnectionFailure::new(
            AgentConnectionStatus::Io,
            format!("无法刷新 ACP initialize：{error}"),
        )
    })?;

    let timeout = Duration::from_secs(u64::from(profile.timeout_seconds));
    let deadline = Instant::now() + timeout;
    let response = loop {
        let now = Instant::now();
        if now >= deadline {
            break Err(ConnectionFailure::new(
                AgentConnectionStatus::Timeout,
                "Agent 在超时前没有返回 initialize 响应",
            ));
        }
        match frame_rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(FrameRead::Line(bytes)) => {
                if bytes.len() as u64 > profile.output_limit_bytes {
                    break Err(ConnectionFailure::new(
                        AgentConnectionStatus::OutputTooLarge,
                        "Agent 响应超过配置的输出上限",
                    ));
                }
                let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                    break Err(ConnectionFailure::new(
                        AgentConnectionStatus::ProtocolError,
                        "Agent 在标准输出写入了非 JSON-RPC 内容",
                    ));
                };
                if value.get("id").and_then(Value::as_u64) != Some(1) {
                    continue;
                }
                break parse_initialize_response(value, profile, started);
            }
            Ok(FrameRead::Eof) | Err(RecvTimeoutError::Disconnected) => {
                let exit = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|status| status.code());
                let stderr_text = stderr_rx
                    .recv_timeout(Duration::from_millis(120))
                    .unwrap_or_default();
                let detail = redact_text(&stderr_text, credential);
                let suffix = if detail.trim().is_empty() {
                    String::new()
                } else {
                    format!("：{detail}")
                };
                let status = classify_failure_text(&detail);
                break Err(ConnectionFailure::new(
                    status,
                    format!(
                        "Agent 在响应前退出{}{}",
                        exit.map(|code| format!("（退出码 {code}）"))
                            .unwrap_or_default(),
                        suffix
                    ),
                ));
            }
            Ok(FrameRead::TooLarge) => {
                break Err(ConnectionFailure::new(
                    AgentConnectionStatus::OutputTooLarge,
                    "Agent 输出超过大小限制",
                ));
            }
            Err(RecvTimeoutError::Timeout) => {
                break Err(ConnectionFailure::new(
                    AgentConnectionStatus::Timeout,
                    "Agent 在超时前没有返回 initialize 响应",
                ));
            }
        }
    };

    drop(stdin);
    terminate_child(&mut child);
    let _ = stderr_thread.join();
    let _ = stdout_thread.join();
    response
}

fn parse_initialize_response(
    value: Value,
    profile: &AgentProfile,
    started: Instant,
) -> Result<AgentConnectionResult, ConnectionFailure> {
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Agent 返回了未说明的协议错误");
        let code = error.get("code").and_then(Value::as_i64);
        let status = classify_failure_text(message);
        return Err(ConnectionFailure::new(
            status,
            format!(
                "initialize 失败{}{}",
                code.map(|code| format!("（{code}）")).unwrap_or_default(),
                message
            ),
        ));
    }
    let Some(result) = value.get("result") else {
        return Err(ConnectionFailure::new(
            AgentConnectionStatus::ProtocolError,
            "initialize 响应缺少 result",
        ));
    };
    let protocol_version = result
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ConnectionFailure::new(
                AgentConnectionStatus::ProtocolError,
                "initialize 响应缺少 protocolVersion",
            )
        })?;
    if protocol_version != ACP_PROTOCOL_VERSION {
        return Err(ConnectionFailure::new(
            AgentConnectionStatus::IncompatibleVersion,
            format!(
                "Agent 使用 ACP 版本 {protocol_version}，Kite 仅支持版本 {ACP_PROTOCOL_VERSION}"
            ),
        ));
    }
    let agent_info = result.get("agentInfo");
    let agent_name = agent_info
        .and_then(|info| info.get("title").or_else(|| info.get("name")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let agent_version = agent_info
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let capabilities = capability_labels(result.get("agentCapabilities"));
    let auth_methods = auth_method_labels(result.get("authMethods"));
    let status = if auth_methods.is_empty() {
        AgentConnectionStatus::Connected
    } else {
        AgentConnectionStatus::AuthenticationRequired
    };
    let message = if auth_methods.is_empty() {
        "ACP 初始化成功，协议版本 1，未声明额外认证要求。".to_string()
    } else {
        format!(
            "ACP 初始化成功；Agent 声明了认证方式：{}",
            auth_methods.join("、")
        )
    };
    Ok(AgentConnectionResult {
        success: true,
        status,
        message: if profile.permission == PermissionPolicy::AgentManaged {
            format!("{message} 权限由 Agent 自身管理。")
        } else {
            message
        },
        agent_name,
        agent_version,
        protocol_version: Some(protocol_version as u32),
        capabilities,
        auth_methods,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn test_generic_cli(
    profile: &AgentProfile,
    credential: Option<&str>,
    started: Instant,
) -> Result<AgentConnectionResult, ConnectionFailure> {
    let working_directory = profile_working_directory(profile)?;
    let (mut command, _) = build_agent_command(profile, &working_directory)
        .map_err(|error| ConnectionFailure::new(AgentConnectionStatus::Io, error))?;
    command
        .arg("--version")
        .current_dir(&working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_environment(&mut command, profile, credential);
    apply_no_window(&mut command);
    let mut child = command.spawn().map_err(classify_spawn_error)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let output_limit = profile.output_limit_bytes;
    let stdout_thread = stdout.map(|stream| {
        thread::spawn(move || {
            let (bytes, exceeded) = read_limited(stream, output_limit);
            (bytes, exceeded)
        })
    });
    let stderr_thread =
        stderr.map(|stream| thread::spawn(move || read_limited(stream, MAX_STDERR_BYTES).0));
    let deadline = Instant::now() + Duration::from_secs(u64::from(profile.timeout_seconds));
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            ConnectionFailure::new(
                AgentConnectionStatus::Io,
                format!("无法读取 CLI 状态：{error}"),
            )
        })? {
            break status;
        }
        if Instant::now() >= deadline {
            terminate_child(&mut child);
            if let Some(handle) = stdout_thread {
                let _ = handle.join();
            }
            if let Some(handle) = stderr_thread {
                let _ = handle.join();
            }
            return Err(ConnectionFailure::new(
                AgentConnectionStatus::Timeout,
                "CLI 在超时前没有退出",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let (stdout, output_exceeded) = stdout_thread
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr_thread
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    if output_exceeded {
        return Err(ConnectionFailure::new(
            AgentConnectionStatus::OutputTooLarge,
            "CLI 版本输出超过配置的输出上限",
        ));
    }
    let stdout = String::from_utf8_lossy(&stdout).trim().to_string();
    let stderr = redact_text(&String::from_utf8_lossy(&stderr), credential);
    let version_text = if stdout.is_empty() { stderr } else { stdout };
    let first_line = version_text.lines().next().unwrap_or("").trim();
    let version = extract_semver(first_line)
        .or_else(|| (!first_line.is_empty()).then(|| first_line.to_string()));
    Ok(AgentConnectionResult {
        success: true,
        status: AgentConnectionStatus::Connected,
        message: if status.success() {
            "可执行文件已启动。通用 CLI 的权限由 Agent 自身管理。".to_string()
        } else {
            "可执行文件已启动，但 --version 返回非零。通用 CLI 的权限由 Agent 自身管理。"
                .to_string()
        },
        agent_name: profile.name.clone().into(),
        agent_version: version,
        protocol_version: None,
        capabilities: Vec::new(),
        auth_methods: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn apply_environment(command: &mut Command, profile: &AgentProfile, credential: Option<&str>) {
    for entry in &profile.environment {
        command.env(&entry.name, &entry.value);
    }
    if let Some(credential) = credential {
        let key = profile
            .credential_env_var
            .as_deref()
            .unwrap_or("KITE_AGENT_TOKEN");
        command.env(key, credential);
    }
}

#[cfg(windows)]
fn apply_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn apply_no_window(_command: &mut Command) {}

fn read_limited<R: Read>(mut stream: R, limit: u64) -> (Vec<u8>, bool) {
    let keep = limit.saturating_add(1) as usize;
    let mut output = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let total = output.len().saturating_add(read);
                if total as u64 > limit {
                    exceeded = true;
                }
                if output.len() < keep {
                    let available = keep - output.len();
                    output.extend_from_slice(&buffer[..read.min(available)]);
                }
            }
            Err(_) => break,
        }
    }
    (output, exceeded)
}

fn read_frames<R: Read>(stream: R, sender: mpsc::Sender<FrameRead>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => {
                let _ = sender.send(FrameRead::Eof);
                return;
            }
            Ok(_) => {
                if line.len() > 16 * 1024 * 1024 {
                    let _ = sender.send(FrameRead::TooLarge);
                    return;
                }
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                if sender.send(FrameRead::Line(line.clone())).is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = sender.send(FrameRead::Eof);
                return;
            }
        }
    }
}

fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn classify_spawn_error(error: std::io::Error) -> ConnectionFailure {
    use std::io::ErrorKind;
    let status = match error.kind() {
        ErrorKind::NotFound => AgentConnectionStatus::NotInstalled,
        ErrorKind::PermissionDenied => AgentConnectionStatus::PermissionDenied,
        _ => AgentConnectionStatus::Io,
    };
    ConnectionFailure::new(status, format!("无法启动 Agent：{error}"))
}

fn classify_failure_text(text: &str) -> AgentConnectionStatus {
    let normalized = text.to_ascii_lowercase();
    if normalized.contains("unauthorized")
        || normalized.contains("authentication")
        || normalized.contains("not authenticated")
        || normalized.contains("login")
        || normalized.contains("credential")
        || normalized.contains("token")
        || normalized.contains("认证")
    {
        AgentConnectionStatus::AuthenticationFailed
    } else if normalized.contains("permission")
        || normalized.contains("forbidden")
        || normalized.contains("access denied")
        || normalized.contains("权限")
    {
        AgentConnectionStatus::PermissionDenied
    } else if normalized.contains("protocol version")
        || normalized.contains("incompatible")
        || normalized.contains("unsupported version")
        || normalized.contains("版本")
    {
        AgentConnectionStatus::IncompatibleVersion
    } else {
        AgentConnectionStatus::ProtocolError
    }
}

fn redact_text(text: &str, credential: Option<&str>) -> String {
    let mut redacted = text.to_string();
    if let Some(credential) = credential.filter(|value| !value.is_empty()) {
        redacted = redacted.replace(credential, "[REDACTED]");
    }
    redact_url_credentials(&redacted)
}

fn redact_url_credentials(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let Some(prefix) = token.find("://") else {
            output.push_str(token);
            continue;
        };
        let authority_start = prefix + 3;
        let rest = &token[authority_start..];
        let authority_end = rest.find(['/', '?', '#', '\n', '\r']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if let Some((userinfo, host)) = authority.rsplit_once('@')
            && userinfo.contains(':')
        {
            output.push_str(&token[..authority_start]);
            output.push_str("[REDACTED]@");
            output.push_str(host);
            output.push_str(&rest[authority_end..]);
            continue;
        }
        output.push_str(token);
    }
    output
}

fn capability_labels(capabilities: Option<&Value>) -> Vec<String> {
    let Some(capabilities) = capabilities else {
        return Vec::new();
    };
    let mut labels = Vec::new();
    if capabilities.get("loadSession").and_then(Value::as_bool) == Some(true) {
        labels.push("恢复会话".to_string());
    }
    if let Some(prompt) = capabilities.get("promptCapabilities") {
        for (key, label) in [
            ("image", "图片上下文"),
            ("audio", "音频上下文"),
            ("embeddedContext", "嵌入文件上下文"),
        ] {
            if prompt.get(key).and_then(Value::as_bool) == Some(true) {
                labels.push(label.to_string());
            }
        }
    }
    if let Some(mcp) = capabilities.get("mcpCapabilities") {
        for (key, label) in [("http", "MCP HTTP"), ("sse", "MCP SSE")] {
            if mcp.get(key).and_then(Value::as_bool) == Some(true) {
                labels.push(label.to_string());
            }
        }
    }
    if capabilities
        .get("auth")
        .and_then(|value| value.get("logout"))
        .is_some()
    {
        labels.push("登出".to_string());
    }
    if capabilities
        .get("session")
        .and_then(|value| value.get("additionalDirectories"))
        .is_some()
    {
        labels.push("附加工作目录".to_string());
    }
    labels
}

fn auth_method_labels(auth_methods: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(methods)) = auth_methods else {
        return Vec::new();
    };
    methods
        .iter()
        .filter_map(|method| {
            if let Some(method) = method.as_str() {
                return Some(method.to_string());
            }
            method
                .get("name")
                .or_else(|| method.get("title"))
                .or_else(|| method.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn extract_semver(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_ascii_digit() && character != '.')
        })
        .find(|word| {
            let mut parts = word.split('.');
            parts
                .next()
                .is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
                && parts.next().is_some_and(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())
                })
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_labels_are_flattened_for_the_ui() {
        let value = json!({
            "loadSession": true,
            "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
            "mcpCapabilities": { "http": true }
        });
        let labels = capability_labels(Some(&value));
        assert!(labels.contains(&"恢复会话".to_string()));
        assert!(labels.contains(&"图片上下文".to_string()));
        assert!(!labels.contains(&"音频上下文".to_string()));
        assert!(labels.contains(&"MCP HTTP".to_string()));
    }

    #[test]
    fn failure_text_distinguishes_auth_permission_and_version() {
        assert_eq!(
            classify_failure_text("authentication required"),
            AgentConnectionStatus::AuthenticationFailed
        );
        assert_eq!(
            classify_failure_text("permission denied"),
            AgentConnectionStatus::PermissionDenied
        );
        assert_eq!(
            classify_failure_text("unsupported protocol version"),
            AgentConnectionStatus::IncompatibleVersion
        );
    }

    #[test]
    fn redacts_credentials_from_errors() {
        let text = "token secret-value failed at https://user:pass@example.com/path";
        let redacted = redact_text(text, Some("secret-value"));
        assert!(!redacted.contains("secret-value"));
        assert!(!redacted.contains("user:pass"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn extracts_a_version_from_cli_output() {
        assert_eq!(extract_semver("agent 1.2.3"), Some("1.2.3".to_string()));
    }
    #[test]
    fn probes_a_local_acp_http_endpoint() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fixture request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"agentInfo":{"name":"http-fixture","title":"HTTP Fixture","version":"1.0.0"},"authMethods":[]}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture response");
        });
        let profile = AgentProfile {
            name: "HTTP fixture".to_string(),
            transport: AgentTransport::AcpHttp,
            endpoint: Some(format!("http://{address}/acp")),
            executable: None,
            timeout_seconds: 5,
            ..AgentProfile::default()
        };
        let result = test_agent_connection(profile, None);
        server.join().expect("join fixture server");
        assert!(result.success, "{}", result.message);
        assert_eq!(result.agent_name.as_deref(), Some("HTTP Fixture"));
        assert_eq!(result.protocol_version, Some(1));
    }

    #[test]
    #[ignore = "requires the local ACP fixture interpreter"]
    fn probes_an_acp_fixture_without_saving_credentials() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("agent-fixtures")
            .join("acp_probe.py");
        let profile = AgentProfile {
            name: "ACP probe fixture".to_string(),
            executable: Some(if cfg!(windows) { "python" } else { "python3" }.to_string()),
            arguments: vec![fixture.to_string_lossy().into_owned()],
            timeout_seconds: 10,
            ..AgentProfile::default()
        };
        let result = test_agent_connection(profile, None);
        assert!(result.success, "{}", result.message);
        assert_eq!(result.protocol_version, Some(1));
        assert_eq!(result.agent_name.as_deref(), Some("Kite ACP Probe Fixture"));
        assert_eq!(result.agent_version.as_deref(), Some("1.0.0"));
        assert!(result.capabilities.contains(&"恢复会话".to_string()));
    }
}
