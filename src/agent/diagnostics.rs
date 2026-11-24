use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::config::{AgentProfile, agent_config_path, load_agent_profiles};
use super::process::{resolve_agent_command, spawn_agent_process, terminate_agent_process};

const MAX_LOG_READ_BYTES: u64 = 256 * 1024;
const MAX_LOG_ENTRIES: usize = 100;
const MAX_LOG_FILE_BYTES: u64 = 1024 * 1024;

static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentConnectionInfo {
    pub success: bool,
    pub protocol_version: u32,
    pub agent_name: String,
    pub agent_version: String,
    pub capabilities: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentDiagnosticEntry {
    pub unix_ms: u64,
    pub level: String,
    pub event: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentProfileDiagnostic {
    pub profile_id: String,
    pub profile_name: String,
    pub status: String,
    pub command: String,
    pub resolved_command: Option<String>,
    pub installed: bool,
    pub version: Option<String>,
    pub protocol: String,
    pub capabilities: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentDiagnostics {
    pub generated_unix_ms: u64,
    pub config_path: String,
    pub log_path: String,
    pub profiles: Vec<AgentProfileDiagnostic>,
    pub recent_entries: Vec<AgentDiagnosticEntry>,
}

pub fn agent_log_path() -> PathBuf {
    if let Some(path) = std::env::var_os("KITE_AGENT_LOG_PATH") {
        return PathBuf::from(path);
    }
    agent_config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("agent.log")
}

pub fn diagnose_agents() -> AgentDiagnostics {
    let loaded = load_agent_profiles();
    let profiles = loaded
        .store
        .profiles
        .iter()
        .map(diagnose_profile)
        .collect::<Vec<_>>();
    AgentDiagnostics {
        generated_unix_ms: unix_ms(),
        config_path: loaded.config_path,
        log_path: agent_log_path().to_string_lossy().into_owned(),
        profiles,
        recent_entries: read_recent_entries(),
    }
}

pub fn probe_agent_connection(profile: AgentProfile) -> AgentConnectionInfo {
    let started = Instant::now();
    match probe_profile(&profile) {
        Ok((protocol_version, agent_name, agent_version, capabilities)) => AgentConnectionInfo {
            success: true,
            protocol_version,
            agent_name,
            agent_version,
            capabilities,
            latency_ms: started.elapsed().as_millis() as u64,
            error: None,
        },
        Err(error) => {
            record_agent_event("error", "probe", &error);
            AgentConnectionInfo {
                success: false,
                protocol_version: 0,
                agent_name: profile.name,
                agent_version: String::new(),
                capabilities: Vec::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(error),
            }
        }
    }
}

pub fn clear_agent_diagnostics() -> Result<(), String> {
    let guard = LOG_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = guard.lock().map_err(|_| "Agent 日志锁不可用".to_string())?;
    let path = agent_log_path();
    if path.exists() {
        fs::remove_file(&path).map_err(|error| format!("无法清除 Agent 日志：{error}"))?;
    }
    Ok(())
}

pub fn record_agent_event(level: &str, event: &str, message: impl AsRef<str>) {
    let guard = LOG_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = guard.lock() else {
        return;
    };
    let path = agent_log_path();
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_FILE_BYTES) {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(&path, rotated);
    }
    let entry = AgentDiagnosticEntry {
        unix_ms: unix_ms(),
        level: normalize_level(level).to_string(),
        event: sanitize_short(event, 120),
        message: redact_agent_text(&sanitize_short(message.as_ref(), 16_000)),
    };
    if let Ok(line) = serde_json::to_string(&entry)
        && let Ok(mut output) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(output, "{line}");
    }
}

pub fn redact_agent_text(value: &str) -> String {
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static URL: OnceLock<Regex> = OnceLock::new();
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(r"(?i)(token|secret|password|passwd|api[_-]?key|authorization)\s*[:=]\s*[^\s,]+")
            .expect("valid secret assignment regex")
    });
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)bearer\s+[A-Za-z0-9._~+/\-=]+").expect("valid bearer regex")
    });
    let url = URL.get_or_init(|| {
        Regex::new(r"(?i)([a-z][a-z0-9+.-]*://)[^/\s@]+@").expect("valid URL credential regex")
    });
    let token =
        TOKEN.get_or_init(|| Regex::new(r"\bsk-[A-Za-z0-9_-]{8,}\b").expect("valid token regex"));
    let value = bearer.replace_all(value, "Bearer [REDACTED]");
    let value = url.replace_all(&value, "$1[REDACTED]@");
    let value = token.replace_all(&value, "[REDACTED]");
    assignment.replace_all(&value, "$1=[REDACTED]").into_owned()
}

fn diagnose_profile(profile: &AgentProfile) -> AgentProfileDiagnostic {
    let resolved = resolve_agent_command(profile, Path::new(".")).ok();
    AgentProfileDiagnostic {
        profile_id: profile.id.clone(),
        profile_name: profile.name.clone(),
        status: if !profile.enabled {
            "disabled".to_string()
        } else if resolved.is_some() {
            "installed".to_string()
        } else if profile.command.trim().is_empty() {
            "invalid".to_string()
        } else {
            "not_installed".to_string()
        },
        command: profile.command.clone(),
        resolved_command: resolved
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        installed: resolved.is_some(),
        version: None,
        protocol: format!("{:?}", profile.transport),
        capabilities: Vec::new(),
        error: None,
    }
}

fn probe_profile(profile: &AgentProfile) -> Result<(u32, String, String, Vec<String>), String> {
    if !profile.enabled {
        return Err("Agent 已禁用".to_string());
    }
    let mut child = spawn_agent_process(profile, Path::new("."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Agent stdout 不可用".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Agent stderr 不可用".to_string())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Agent stdin 不可用".to_string())?;
    let (sender, receiver) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_buffer = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let stderr_for_thread = stderr_buffer.clone();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(mut buffer) = stderr_for_thread.lock() {
                if buffer.len() >= 20 {
                    buffer.remove(0);
                }
                buffer.push(redact_agent_text(&line));
            }
        }
    });
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "terminal": false,
                "fs": { "readTextFile": false, "writeTextFile": false }
            },
            "clientInfo": { "name": "Kite", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    writeln!(stdin, "{initialize}").map_err(|error| format!("无法写入 Agent 握手：{error}"))?;
    let timeout = Duration::from_millis(profile.startup_timeout_ms.max(1) as u64);
    let deadline = Instant::now() + timeout;
    #[allow(unused_assignments)]
    let mut failure: Option<String> = None;
    loop {
        if Instant::now() >= deadline {
            failure = Some("Agent 握手超时".to_string());
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if value.get("id").and_then(Value::as_u64) != Some(1) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    failure = Some(
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Agent 返回握手错误")
                            .to_string(),
                    );
                } else {
                    terminate_agent_process(&mut child);
                    let result = value.get("result").cloned().unwrap_or(Value::Null);
                    let protocol_version = result
                        .get("protocolVersion")
                        .and_then(Value::as_u64)
                        .unwrap_or(1) as u32;
                    let agent_name = result
                        .pointer("/agentInfo/name")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown Agent")
                        .to_string();
                    let agent_version = result
                        .pointer("/agentInfo/version")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let capabilities = flatten_capabilities(
                        result
                            .get("agentCapabilities")
                            .cloned()
                            .unwrap_or(Value::Null),
                        "agent",
                    );
                    record_agent_event(
                        "info",
                        "probe",
                        &format!("{agent_name} {agent_version} protocol={protocol_version}"),
                    );
                    return Ok((protocol_version, agent_name, agent_version, capabilities));
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                failure = Some("Agent 在握手前退出".to_string());
                break;
            }
        }
    }
    terminate_agent_process(&mut child);
    let stderr = stderr_buffer
        .lock()
        .map(|buffer| buffer.join("\n"))
        .unwrap_or_default();
    let message = match failure {
        Some(message) if stderr.trim().is_empty() => message,
        Some(message) => format!("{message}\n{stderr}"),
        None => "Agent 握手失败".to_string(),
    };
    Err(redact_agent_text(&message))
}

fn flatten_capabilities(value: Value, prefix: &str) -> Vec<String> {
    let mut result = Vec::new();
    if let Value::Object(map) = value {
        for (key, value) in map {
            let name = format!("{prefix}.{key}");
            match value {
                Value::Bool(true) => result.push(name),
                Value::Object(_) => result.extend(flatten_capabilities(value, &name)),
                _ => {}
            }
        }
    }
    result
}

fn read_recent_entries() -> Vec<AgentDiagnosticEntry> {
    let path = agent_log_path();
    let Ok(metadata) = fs::metadata(&path) else {
        return Vec::new();
    };
    if !metadata.is_file() {
        return Vec::new();
    }
    let Ok(mut file) = fs::File::open(&path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_LOG_READ_BYTES {
        let _ = file.seek(SeekFrom::End(-(MAX_LOG_READ_BYTES as i64)));
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let mut entries = String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<AgentDiagnosticEntry>(line).ok())
        .collect::<Vec<_>>();
    if entries.len() > MAX_LOG_ENTRIES {
        entries = entries.split_off(entries.len() - MAX_LOG_ENTRIES);
    }
    entries
}

fn normalize_level(level: &str) -> &'static str {
    match level.to_ascii_lowercase().as_str() {
        "error" => "error",
        "warn" | "warning" => "warning",
        "info" => "info",
        _ => "debug",
    }
}

fn sanitize_short(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_secret_shapes() {
        let redacted = redact_agent_text(
            "Authorization: Bearer abc.def token=secret-value https://user:pass@example.test sk-abcdefgh12345678",
        );
        assert!(!redacted.contains("abc.def"));
        assert!(!redacted.contains("secret-value"));
        assert!(!redacted.contains("user:pass"));
        assert!(!redacted.contains("sk-abcdefgh12345678"));
    }

    #[test]
    fn diagnostics_do_not_block_on_corrupt_profile_store() {
        let diagnostics = diagnose_agents();
        assert!(!diagnostics.config_path.is_empty());
        assert!(!diagnostics.log_path.is_empty());
    }
}
