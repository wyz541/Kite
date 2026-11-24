//! Redacted Agent diagnostics shared by settings and the Agent window.
//!
//! Runtime and connection testing stay in the dependency modules. This module
//! only defines the cross-platform presentation records, redaction, and bounded
//! JSONL log storage so both frontends continue to agree on error semantics.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_DIAGNOSTIC_LOG_BYTES: u64 = 256 * 1024;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;
const MAX_RECENT_DIAGNOSTIC_ENTRIES: usize = 200;

static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentCapstoneDiagnosticEntry {
    pub unix_ms: u64,
    pub level: String,
    pub event: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentCapstoneProfileDiagnostic {
    pub profile_id: String,
    pub profile_name: String,
    pub status: String,
    pub message: String,
    pub command_path: Option<String>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub protocol_version: Option<u32>,
    pub capabilities: Vec<String>,
    pub auth_methods: Vec<String>,
    pub latency_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentCapstoneDiagnostics {
    pub generated_unix_ms: u64,
    pub config_path: String,
    pub session_path: String,
    pub log_path: String,
    pub config_warning: Option<String>,
    pub recovered_from_backup: bool,
    pub profiles: Vec<AgentCapstoneProfileDiagnostic>,
    pub recent_entries: Vec<AgentCapstoneDiagnosticEntry>,
    pub session_missing_paths: Vec<String>,
    pub session_warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentCapstoneDiagnosticsInput {
    pub config_path: String,
    pub session_path: String,
    pub log_path: String,
    pub config_warning: Option<String>,
    pub recovered_from_backup: bool,
    pub profiles: Vec<AgentCapstoneProfileDiagnostic>,
    pub session_missing_paths: Vec<String>,
    pub session_warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentCapstoneConnectionDiagnostic {
    pub status: String,
    pub message: String,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub protocol_version: Option<u32>,
    pub capabilities: Vec<String>,
    pub auth_methods: Vec<String>,
    pub elapsed_ms: u64,
}

pub fn build_agent_diagnostics(input: AgentCapstoneDiagnosticsInput) -> AgentCapstoneDiagnostics {
    let log_path = input.log_path;
    AgentCapstoneDiagnostics {
        generated_unix_ms: unix_ms(),
        config_path: input.config_path,
        session_path: input.session_path,
        recent_entries: read_recent_diagnostic_entries(Path::new(&log_path), 100),
        log_path,
        config_warning: input.config_warning.map(|value| redact_agent_text(&value)),
        recovered_from_backup: input.recovered_from_backup,
        profiles: input
            .profiles
            .into_iter()
            .map(|profile| AgentCapstoneProfileDiagnostic {
                message: redact_agent_text(&profile.message),
                ..profile
            })
            .collect(),
        session_missing_paths: input.session_missing_paths,
        session_warning: input.session_warning.map(|value| redact_agent_text(&value)),
    }
}

pub fn sanitize_connection_diagnostic(
    mut diagnostic: AgentCapstoneConnectionDiagnostic,
) -> AgentCapstoneConnectionDiagnostic {
    diagnostic.message = redact_agent_text(&diagnostic.message);
    diagnostic
}

pub fn append_agent_diagnostic(
    log_path: &Path,
    level: &str,
    event: &str,
    message: impl AsRef<str>,
) -> Result<(), String> {
    let message = redact_agent_text(message.as_ref());
    let message = truncate_utf8(&message, MAX_DIAGNOSTIC_LINE_BYTES);
    let entry = AgentCapstoneDiagnosticEntry {
        unix_ms: unix_ms(),
        level: level.to_string(),
        event: event.to_string(),
        message,
    };
    let _guard = LOG_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Agent 诊断日志锁不可用".to_string())?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 Agent 日志目录：{error}"))?;
    }
    if fs::metadata(log_path)
        .map(|metadata| metadata.len() > MAX_DIAGNOSTIC_LOG_BYTES)
        .unwrap_or(false)
    {
        rotate_log(log_path)?;
    }
    let line =
        serde_json::to_string(&entry).map_err(|error| format!("无法编码 Agent 日志：{error}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("无法打开 Agent 日志：{error}"))?;
    writeln!(file, "{line}").map_err(|error| format!("无法写入 Agent 日志：{error}"))
}

pub fn read_recent_diagnostic_entries(
    log_path: &Path,
    limit: usize,
) -> Vec<AgentCapstoneDiagnosticEntry> {
    let Ok(bytes) = fs::read(log_path) else {
        return Vec::new();
    };
    let limit = limit.min(MAX_RECENT_DIAGNOSTIC_ENTRIES);
    let mut entries = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<AgentCapstoneDiagnosticEntry>(line).ok())
        .collect::<Vec<_>>();
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    entries
}

pub fn clear_agent_diagnostic_log(log_path: &Path) -> Result<(), String> {
    let _guard = LOG_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Agent 诊断日志锁不可用".to_string())?;
    match fs::remove_file(log_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法清除 Agent 日志：{error}")),
    }
}

pub fn redact_agent_text(text: &str) -> String {
    let mut output = text.to_string();
    for prefix in [
        "Authorization: Bearer ",
        "authorization: bearer ",
        "Bearer ",
        "bearer ",
        "OPENAI_API_KEY=",
        "ANTHROPIC_API_KEY=",
        "GEMINI_API_KEY=",
        "API_KEY=",
        "api_key=",
        "access_token=",
        "TOKEN=",
        "token=",
        "SECRET=",
        "secret=",
        "PASSWORD=",
        "password=",
    ] {
        output = redact_after_prefix(&output, prefix);
    }
    output = redact_url_credentials(&output);
    redact_known_key_fragments(&output)
}

fn redact_after_prefix(text: &str, prefix: &str) -> String {
    let mut output = text.to_string();
    let mut offset = 0;
    while let Some(index) = output[offset..].find(prefix) {
        let value_start = offset + index + prefix.len();
        let value_end = output[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | ',' | ';')
            })
            .map(|end| value_start + end)
            .unwrap_or(output.len());
        if value_end > value_start {
            output.replace_range(value_start..value_end, "[REDACTED]");
        }
        offset = value_start + "[REDACTED]".len();
        if offset >= output.len() {
            break;
        }
    }
    output
}

fn redact_url_credentials(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme) = rest.find("://") {
        let after_scheme = scheme + 3;
        output.push_str(&rest[..after_scheme]);
        let candidate = &rest[after_scheme..];
        let authority_end = candidate.find('/').unwrap_or(candidate.len());
        let authority = &candidate[..authority_end];
        if let Some(at) = authority.rfind('@') {
            let userinfo = &authority[..at];
            if userinfo.contains(':') {
                output.push_str("[REDACTED]@");
                output.push_str(&authority[at + 1..]);
                rest = &candidate[authority_end..];
                continue;
            }
        }
        output.push_str(authority);
        rest = &candidate[authority_end..];
    }
    output.push_str(rest);
    output
}

fn redact_known_key_fragments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim_end();
        if trimmed.starts_with("sk-ant-")
            || trimmed.starts_with("sk-proj-")
            || trimmed.starts_with("sk-") && trimmed.len() > 20
        {
            output.push_str("[REDACTED]");
            output.push_str(&token[trimmed.len()..]);
        } else {
            output.push_str(token);
        }
    }
    output
}

fn rotate_log(log_path: &Path) -> Result<(), String> {
    let bytes = fs::read(log_path).map_err(|error| format!("无法读取 Agent 日志：{error}"))?;
    let keep_from = bytes
        .len()
        .saturating_sub((MAX_DIAGNOSTIC_LOG_BYTES / 2) as usize);
    let tail = &bytes[keep_from..];
    let tail_start = tail
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    fs::write(log_path, &tail[tail_start..])
        .map_err(|error| format!("无法轮换 Agent 日志：{error}"))
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
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
    use std::path::PathBuf;

    fn temp_log_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "kite-agent-diagnostics-{label}-{}-{unique}",
                std::process::id()
            ))
            .join("agent.log")
    }

    #[test]
    fn redacts_tokens_url_credentials_and_common_keys() {
        let text = "Authorization: Bearer secret-token\nhttps://alice:hunter2@example.test/x\nOPENAI_API_KEY=sk-proj-12345678901234567890";
        let redacted = redact_agent_text(text);
        assert!(!redacted.contains("secret-token"));
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("sk-proj-12345678901234567890"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn diagnostic_log_is_bounded_and_redacted() {
        let path = temp_log_path("roundtrip");
        append_agent_diagnostic(&path, "error", "connection", "token=super-secret")
            .expect("append log");
        let entries = read_recent_diagnostic_entries(&path, 10);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].message.contains("super-secret"));
        clear_agent_diagnostic_log(&path).expect("clear");
        assert!(read_recent_diagnostic_entries(&path, 10).is_empty());
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn snapshot_redacts_warnings_and_profile_messages() {
        let snapshot = build_agent_diagnostics(AgentCapstoneDiagnosticsInput {
            config_path: "agent.json".to_string(),
            session_path: "agent-session.json".to_string(),
            log_path: "agent.log".to_string(),
            config_warning: Some("Bearer secret".to_string()),
            recovered_from_backup: false,
            profiles: vec![AgentCapstoneProfileDiagnostic {
                profile_id: "p1".to_string(),
                profile_name: "Agent".to_string(),
                status: "error".to_string(),
                message: "TOKEN=secret".to_string(),
                command_path: None,
                agent_name: None,
                agent_version: None,
                protocol_version: None,
                capabilities: Vec::new(),
                auth_methods: Vec::new(),
                latency_ms: 0,
            }],
            session_missing_paths: Vec::new(),
            session_warning: None,
        });
        assert_eq!(
            snapshot.config_warning.as_deref(),
            Some("Bearer [REDACTED]")
        );
        assert_eq!(snapshot.profiles[0].message, "TOKEN=[REDACTED]");
    }
}
