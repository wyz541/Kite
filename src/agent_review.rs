//! Safe result review primitives for external Agent sessions.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU64, Ordering},
};

use atomicwrites::{AllowOverwrite, AtomicFile, DisallowOverwrite};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use thiserror::Error;

pub const AGENT_REVIEW_SCHEMA_VERSION: u32 = 1;
pub const MAX_AGENT_ANSWER_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_AGENT_PROPOSAL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_AGENT_DIFF_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_AGENT_RESULT_JSON_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_AGENT_CHANGES: usize = 64;
pub const MAX_AGENT_REFERENCES: usize = 256;
pub const MAX_AGENT_CONTEXT_PATHS: usize = 64;
pub const MAX_AGENT_PERMISSION_REQUESTS: usize = 64;
pub const MAX_AGENT_COMMANDS: usize = 64;
pub const MAX_AGENT_AUDIT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_AUDIT_LINE_BYTES: usize = 16 * 1024;
static AUDIT_LOCK: Mutex<()> = Mutex::new(());
static REVIEW_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    Acp,
    Cli,
}

impl AgentTransport {
    pub fn boundary_notice(self) -> &'static str {
        match self {
            Self::Acp => {
                "Kite 会在写入前展示完整 diff，并在文件发生外部修改时要求重新审核；拒绝的权限请求会返回给 Agent。"
            }
            Self::Cli => {
                "该 Agent 以普通 CLI 运行，权限由 Agent 自身管理。Kite 只能保护由 Kite 执行的写入，不能宣称该进程已被强制只读或沙箱化。"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentResultType {
    Answer,
    FileChanges,
    PermissionRequests,
    CommandSuggestions,
    Mixed,
}

impl AgentResultType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::FileChanges => "file_changes",
            Self::PermissionRequests => "permission_requests",
            Self::CommandSuggestions => "command_suggestions",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermissionKind {
    Read,
    Write,
    Command,
    Network,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReferenceDraft {
    pub path: PathBuf,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFileChangeDraft {
    pub path: PathBuf,
    pub proposed_text: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommandSuggestion {
    pub command: String,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPermissionRequestDraft {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: AgentPermissionKind,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub target_paths: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub can_allow_task: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReviewDraft {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub id: Option<String>,
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub transport: AgentTransport,
    pub working_directory: PathBuf,
    #[serde(default)]
    pub context_paths: Vec<PathBuf>,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub references: Vec<AgentReferenceDraft>,
    #[serde(default)]
    pub file_changes: Vec<AgentFileChangeDraft>,
    #[serde(default)]
    pub permission_requests: Vec<AgentPermissionRequestDraft>,
    #[serde(default)]
    pub commands: Vec<AgentCommandSuggestion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReference {
    pub path: PathBuf,
    pub label: String,
    pub line: Option<u32>,
    pub exists: bool,
    pub is_directory: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFileChange {
    pub working_directory: PathBuf,
    pub relative_path: String,
    pub target_path: PathBuf,
    pub proposed_text: String,
    pub proposed_sha256: String,
    pub target_exists: bool,
    pub overwrite_risk: bool,
    pub base_sha256: Option<String>,
    pub current_sha256: Option<String>,
    pub unified_diff: String,
    pub diff_truncated: bool,
    pub added_lines: u32,
    pub removed_lines: u32,
    pub byte_len: u64,
    pub requires_reconfirmation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPermissionRequest {
    pub id: String,
    pub kind: AgentPermissionKind,
    pub title: String,
    pub detail: String,
    pub target_paths: Vec<PathBuf>,
    pub can_allow_task: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReview {
    pub schema_version: u32,
    pub id: String,
    pub agent: String,
    pub model: Option<String>,
    pub transport: AgentTransport,
    pub working_directory: PathBuf,
    pub context_paths: Vec<PathBuf>,
    pub answer: String,
    pub references: Vec<AgentReference>,
    pub file_changes: Vec<AgentFileChange>,
    pub permission_requests: Vec<AgentPermissionRequest>,
    pub commands: Vec<AgentCommandSuggestion>,
    pub boundary_notice: String,
    pub created_at_unix_ms: i64,
}

impl AgentReview {
    pub fn result_type(&self) -> AgentResultType {
        let has_answer = !self.answer.trim().is_empty();
        let has_changes = !self.file_changes.is_empty();
        let has_permissions = !self.permission_requests.is_empty();
        let has_commands = !self.commands.is_empty();
        if has_changes || has_permissions || has_commands {
            if (has_answer as u8)
                + (has_changes as u8)
                + (has_permissions as u8)
                + (has_commands as u8)
                > 1
            {
                AgentResultType::Mixed
            } else if has_changes {
                AgentResultType::FileChanges
            } else if has_permissions {
                AgentResultType::PermissionRequests
            } else {
                AgentResultType::CommandSuggestions
            }
        } else {
            AgentResultType::Answer
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermissionDecision {
    AllowOnce,
    AllowTask,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPermissionResponse {
    pub request_id: String,
    pub decision: AgentPermissionDecision,
    pub allowed: bool,
    pub scope: String,
    pub message: String,
    pub response_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWriteOutcome {
    pub path: PathBuf,
    pub operation: String,
    pub created: bool,
    pub overwritten: bool,
    pub bytes_written: u64,
    pub content_sha256: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuditOutcome {
    Prepared,
    Applied,
    SavedAs,
    Discarded,
    PermissionAllowed,
    PermissionDenied,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAuditEvent {
    pub timestamp_unix_ms: i64,
    pub review_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub transport: AgentTransport,
    pub working_directory: PathBuf,
    pub context_count: u32,
    pub result_type: AgentResultType,
    pub change_count: u32,
    pub user_confirmed_write: bool,
    pub outcome: AgentAuditOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentReviewErrorKind {
    InvalidInput,
    InvalidPath,
    OutsideWorkingDirectory,
    SymlinkNotAllowed,
    FileChangedExternally,
    ConfirmationRequired,
    WriteFailed,
    ResponseTooLarge,
    UnsupportedSchema,
    PermissionNotAvailable,
    Cancelled,
}

impl AgentReviewErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidPath => "invalid_path",
            Self::OutsideWorkingDirectory => "outside_working_directory",
            Self::SymlinkNotAllowed => "symlink_not_allowed",
            Self::FileChangedExternally => "file_changed_externally",
            Self::ConfirmationRequired => "confirmation_required",
            Self::WriteFailed => "write_failed",
            Self::ResponseTooLarge => "response_too_large",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::PermissionNotAvailable => "permission_not_available",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::FileChangedExternally | Self::ConfirmationRequired | Self::WriteFailed
        )
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AgentReviewError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("目标路径无效：{0}")]
    InvalidPath(String),
    #[error("目标路径不在 Agent 工作目录内：{0}")]
    OutsideWorkingDirectory(String),
    #[error("不允许写入符号链接：{0}")]
    SymlinkNotAllowed(String),
    #[error("文件已在审核后被修改，请重新生成并确认 diff：{0}")]
    FileChangedExternally(String),
    #[error("写入需要明确确认：{0}")]
    ConfirmationRequired(String),
    #[error("写入失败：{0}")]
    WriteFailed(String),
    #[error("Agent 结果超过大小上限：{0}")]
    ResponseTooLarge(String),
    #[error("不支持的 Agent 结果 schema：{0}")]
    UnsupportedSchema(String),
    #[error("该 Agent 无法接收交互式权限回复：{0}")]
    PermissionNotAvailable(String),
    #[error("操作已取消")]
    Cancelled,
}

impl AgentReviewError {
    pub fn kind(&self) -> AgentReviewErrorKind {
        match self {
            Self::InvalidInput(_) => AgentReviewErrorKind::InvalidInput,
            Self::InvalidPath(_) => AgentReviewErrorKind::InvalidPath,
            Self::OutsideWorkingDirectory(_) => AgentReviewErrorKind::OutsideWorkingDirectory,
            Self::SymlinkNotAllowed(_) => AgentReviewErrorKind::SymlinkNotAllowed,
            Self::FileChangedExternally(_) => AgentReviewErrorKind::FileChangedExternally,
            Self::ConfirmationRequired(_) => AgentReviewErrorKind::ConfirmationRequired,
            Self::WriteFailed(_) => AgentReviewErrorKind::WriteFailed,
            Self::ResponseTooLarge(_) => AgentReviewErrorKind::ResponseTooLarge,
            Self::UnsupportedSchema(_) => AgentReviewErrorKind::UnsupportedSchema,
            Self::PermissionNotAvailable(_) => AgentReviewErrorKind::PermissionNotAvailable,
            Self::Cancelled => AgentReviewErrorKind::Cancelled,
        }
    }

    pub fn redacted_message(&self) -> String {
        redact_sensitive_text(&self.to_string())
    }
}

pub fn build_review(draft: AgentReviewDraft) -> Result<AgentReview, AgentReviewError> {
    if draft.schema_version != AGENT_REVIEW_SCHEMA_VERSION {
        return Err(AgentReviewError::UnsupportedSchema(format!(
            "期望 {}，收到 {}",
            AGENT_REVIEW_SCHEMA_VERSION, draft.schema_version
        )));
    }
    let agent = clean_label(&draft.agent, 160);
    if agent.is_empty() {
        return Err(AgentReviewError::InvalidInput("Agent 名称不能为空".into()));
    }
    if draft.answer.len() > MAX_AGENT_ANSWER_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "回答超过 {} MiB",
            MAX_AGENT_ANSWER_BYTES / 1024 / 1024
        )));
    }
    if draft.file_changes.len() > MAX_AGENT_CHANGES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "文件修改建议超过 {} 个",
            MAX_AGENT_CHANGES
        )));
    }
    if draft.references.len() > MAX_AGENT_REFERENCES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "文件引用超过 {} 个",
            MAX_AGENT_REFERENCES
        )));
    }
    if draft.permission_requests.len() > MAX_AGENT_PERMISSION_REQUESTS {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "权限请求超过 {} 个",
            MAX_AGENT_PERMISSION_REQUESTS
        )));
    }
    if draft.commands.len() > MAX_AGENT_COMMANDS {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "命令建议超过 {} 个",
            MAX_AGENT_COMMANDS
        )));
    }
    if draft.context_paths.len() > MAX_AGENT_CONTEXT_PATHS {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "上下文路径超过 {} 个",
            MAX_AGENT_CONTEXT_PATHS
        )));
    }
    let total_bytes = draft.answer.len()
        + draft
            .file_changes
            .iter()
            .map(|change| change.proposed_text.len())
            .sum::<usize>()
        + draft
            .commands
            .iter()
            .map(|command| command.command.len() + command.rationale.len())
            .sum::<usize>();
    if total_bytes > MAX_AGENT_RESULT_JSON_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "Agent 结果超过 {} MiB",
            MAX_AGENT_RESULT_JSON_BYTES / 1024 / 1024
        )));
    }

    let working_directory = canonical_directory(&draft.working_directory)?;
    let context_paths = draft
        .context_paths
        .iter()
        .map(|path| resolve_user_path(&working_directory, path))
        .collect::<Vec<_>>();
    let references = draft
        .references
        .into_iter()
        .map(|reference| build_reference(&working_directory, reference))
        .collect::<Result<Vec<_>, _>>()?;
    let file_changes = draft
        .file_changes
        .into_iter()
        .map(|change| build_file_change(&working_directory, change))
        .collect::<Result<Vec<_>, _>>()?;
    let permission_requests = draft
        .permission_requests
        .into_iter()
        .enumerate()
        .map(|(index, request)| build_permission_request(&working_directory, index, request))
        .collect::<Result<Vec<_>, _>>()?;
    let commands = draft
        .commands
        .into_iter()
        .map(normalize_command)
        .collect::<Result<Vec<_>, _>>()?;
    let id = draft
        .id
        .map(|value| clean_label(&value, 160))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            format!(
                "agent-review-{}-{}",
                Utc::now().timestamp_millis().max(0),
                REVIEW_COUNTER.fetch_add(1, Ordering::Relaxed)
            )
        });

    Ok(AgentReview {
        schema_version: AGENT_REVIEW_SCHEMA_VERSION,
        id,
        agent,
        model: draft
            .model
            .as_deref()
            .map(|value| clean_label(value, 160))
            .filter(|value| !value.is_empty()),
        transport: draft.transport,
        working_directory,
        context_paths,
        answer: draft.answer,
        references,
        file_changes,
        permission_requests,
        commands,
        boundary_notice: draft.transport.boundary_notice().to_string(),
        created_at_unix_ms: Utc::now().timestamp_millis(),
    })
}

pub fn parse_review_json(payload: &str) -> Result<AgentReview, AgentReviewError> {
    if payload.len() > MAX_AGENT_RESULT_JSON_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "结构化结果超过 {} MiB",
            MAX_AGENT_RESULT_JSON_BYTES / 1024 / 1024
        )));
    }
    let draft = serde_json::from_str::<AgentReviewDraft>(payload)
        .map_err(|error| AgentReviewError::InvalidInput(format!("JSON 无效：{error}")))?;
    build_review(draft)
}

pub fn reconfirm_file_change(
    change: &AgentFileChange,
) -> Result<AgentFileChange, AgentReviewError> {
    let root = canonical_directory(&change.working_directory)?;
    let existing = current_existing_text(&root, Path::new(&change.relative_path))?;
    build_change_from_text(
        &root,
        Path::new(&change.relative_path),
        change.proposed_text.clone(),
        existing.map(|(_, bytes, text)| (hash_bytes(&bytes), text)),
        false,
    )
}

pub fn apply_file_change(
    change: &AgentFileChange,
    write_confirmed: bool,
) -> Result<AgentWriteOutcome, AgentReviewError> {
    apply_review_changes(std::slice::from_ref(change), write_confirmed)?
        .into_iter()
        .next()
        .ok_or_else(|| AgentReviewError::InvalidInput("没有可应用的修改".into()))
}

pub fn apply_review_changes(
    changes: &[AgentFileChange],
    write_confirmed: bool,
) -> Result<Vec<AgentWriteOutcome>, AgentReviewError> {
    if !write_confirmed {
        return Err(AgentReviewError::ConfirmationRequired(
            "应用 Agent 修改".into(),
        ));
    }
    if changes.is_empty() {
        return Err(AgentReviewError::InvalidInput(
            "Agent 结果没有可应用的文件修改".into(),
        ));
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut prepared = Vec::with_capacity(changes.len());
    for change in changes {
        let target = preflight_change(change)?;
        if !seen.insert(target.to_string_lossy().to_ascii_lowercase()) {
            return Err(AgentReviewError::InvalidInput(format!(
                "重复的文件修改：{}",
                change.relative_path
            )));
        }
        let original = if change.base_sha256.is_some() {
            Some(
                fs::read(&target)
                    .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?,
            )
        } else {
            None
        };
        let encoded = encode_with_style(&change.proposed_text, original.as_deref());
        prepared.push((change, target, original, encoded));
    }

    let mut written = Vec::with_capacity(prepared.len());
    for (change, target, original, encoded) in &prepared {
        let overwrite = change.base_sha256.is_some();
        let result =
            write_atomic_checked(target, encoded, change.base_sha256.as_deref(), overwrite);
        if let Err(error) = result {
            rollback_writes(&written);
            if !overwrite && target.exists() {
                return Err(AgentReviewError::FileChangedExternally(
                    change.relative_path.clone(),
                ));
            }
            return Err(error);
        }
        written.push((target.clone(), original.clone(), hash_bytes(encoded)));
    }

    Ok(prepared
        .iter()
        .map(|(change, target, _, encoded)| AgentWriteOutcome {
            path: target.clone(),
            operation: "apply".into(),
            created: change.base_sha256.is_none(),
            overwritten: change.base_sha256.is_some(),
            bytes_written: encoded.len() as u64,
            content_sha256: hash_bytes(encoded),
            message: format!("已应用 Agent 修改：{}", change.relative_path),
        })
        .collect())
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn encode_with_style(text: &str, original: Option<&[u8]>) -> Vec<u8> {
    let has_bom = original.is_some_and(|bytes| bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    let uses_crlf = original.is_some_and(|bytes| bytes.windows(2).any(|pair| pair == b"\r\n"));
    let normalized = normalize_line_endings(text);
    let serialized = if uses_crlf {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    };
    if has_bom {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(serialized.as_bytes());
        bytes
    } else {
        serialized.into_bytes()
    }
}

fn preflight_change(change: &AgentFileChange) -> Result<PathBuf, AgentReviewError> {
    if change.requires_reconfirmation {
        return Err(AgentReviewError::FileChangedExternally(
            change.relative_path.clone(),
        ));
    }
    if hash_bytes(change.proposed_text.as_bytes()) != change.proposed_sha256 {
        return Err(AgentReviewError::InvalidInput(format!(
            "{} 的建议内容校验失败",
            change.relative_path
        )));
    }
    if change.proposed_text.len() > MAX_AGENT_PROPOSAL_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "{} 的建议内容超过 {} MiB",
            change.relative_path,
            MAX_AGENT_PROPOSAL_BYTES / 1024 / 1024
        )));
    }

    let root = canonical_directory(&change.working_directory)?;
    let target = resolve_inside(&root, Path::new(&change.relative_path))?;
    match (&change.base_sha256, fs::symlink_metadata(&target)) {
        (Some(expected), Ok(metadata)) => {
            ensure_regular_non_symlink(&target, &metadata)?;
            let current = fs::read(&target)
                .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
            if &hash_bytes(&current) != expected {
                return Err(AgentReviewError::FileChangedExternally(
                    change.relative_path.clone(),
                ));
            }
        }
        (None, Ok(_)) => {
            return Err(AgentReviewError::FileChangedExternally(format!(
                "{} 在审核后出现",
                change.relative_path
            )));
        }
        (Some(_), Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(AgentReviewError::FileChangedExternally(format!(
                "{} 在审核后被删除",
                change.relative_path
            )));
        }
        (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
        (_, Err(error)) => return Err(AgentReviewError::WriteFailed(error.to_string())),
    }
    Ok(target)
}

fn rollback_writes(written: &[(PathBuf, Option<Vec<u8>>, String)]) {
    for (path, original, applied_hash) in written.iter().rev() {
        let Ok(current) = fs::read(path) else {
            continue;
        };
        if hash_bytes(&current) != applied_hash.as_str() {
            continue;
        }
        match original {
            Some(bytes) => {
                let _ = AtomicFile::new(path, AllowOverwrite).write(|file| file.write_all(bytes));
            }
            None => {
                let _ = fs::remove_file(path);
            }
        }
    }
}

pub fn save_file_change_as(
    change: &AgentFileChange,
    destination: &Path,
    overwrite_confirmed: bool,
) -> Result<AgentWriteOutcome, AgentReviewError> {
    if hash_bytes(change.proposed_text.as_bytes()) != change.proposed_sha256 {
        return Err(AgentReviewError::InvalidInput("建议内容校验失败".into()));
    }
    save_text_as(
        &change.proposed_text,
        destination,
        overwrite_confirmed,
        "save_as_change",
        "已另存 Agent 修改",
    )
}

pub fn save_answer_as(
    answer: &str,
    destination: &Path,
    overwrite_confirmed: bool,
) -> Result<AgentWriteOutcome, AgentReviewError> {
    if answer.len() > MAX_AGENT_ANSWER_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "回答超过 {} MiB",
            MAX_AGENT_ANSWER_BYTES / 1024 / 1024
        )));
    }
    save_text_as(
        answer,
        destination,
        overwrite_confirmed,
        "save_answer",
        "已另存 Agent 回答",
    )
}

fn save_text_as(
    text: &str,
    destination: &Path,
    overwrite_confirmed: bool,
    operation: &str,
    message: &str,
) -> Result<AgentWriteOutcome, AgentReviewError> {
    let target = prepare_destination(destination)?;
    let exists = fs::symlink_metadata(&target).is_ok();
    if exists && !overwrite_confirmed {
        return Err(AgentReviewError::ConfirmationRequired(
            target.display().to_string(),
        ));
    }
    let bytes = text.as_bytes();
    write_atomic_checked(&target, bytes, None, exists && overwrite_confirmed)?;
    Ok(AgentWriteOutcome {
        path: target.clone(),
        operation: operation.into(),
        created: !exists,
        overwritten: exists,
        bytes_written: bytes.len() as u64,
        content_sha256: hash_bytes(bytes),
        message: format!("{message}：{}", target.display()),
    })
}

pub fn respond_to_permission(
    request: &AgentPermissionRequest,
    decision: AgentPermissionDecision,
    task_id: &str,
    transport: AgentTransport,
) -> Result<AgentPermissionResponse, AgentReviewError> {
    if request.id.trim().is_empty() {
        return Err(AgentReviewError::InvalidInput("权限请求缺少稳定 ID".into()));
    }
    let task_id = clean_label(task_id, 160);
    if decision == AgentPermissionDecision::AllowTask && task_id.is_empty() {
        return Err(AgentReviewError::InvalidInput(
            "“本任务允许”需要任务 ID".into(),
        ));
    }
    if decision == AgentPermissionDecision::AllowTask && !request.can_allow_task {
        return Err(AgentReviewError::PermissionNotAvailable(
            "该请求不支持任务级授权".into(),
        ));
    }
    if transport == AgentTransport::Cli {
        return Err(AgentReviewError::PermissionNotAvailable(
            "普通 CLI 的权限由外部 Agent 自身管理".into(),
        ));
    }

    let (allowed, scope, message) = match decision {
        AgentPermissionDecision::AllowOnce => (true, "once", "用户允许本次请求"),
        AgentPermissionDecision::AllowTask => (true, "task", "用户允许本任务内的同类请求"),
        AgentPermissionDecision::Deny => (false, "deny", "用户拒绝了该请求"),
    };
    let decision_value = match decision {
        AgentPermissionDecision::AllowOnce => "allow_once",
        AgentPermissionDecision::AllowTask => "allow_task",
        AgentPermissionDecision::Deny => "deny",
    };
    let response_json = serde_json::json!({
        "request_id": request.id,
        "decision": decision_value,
        "allowed": allowed,
        "scope": scope,
        "task_id": (!task_id.is_empty()).then_some(task_id),
        "message": message,
    })
    .to_string();
    Ok(AgentPermissionResponse {
        request_id: request.id.clone(),
        decision,
        allowed,
        scope: scope.into(),
        message: message.into(),
        response_json,
    })
}

pub fn materialize_text_draft(
    text: &str,
    directory: &Path,
    suggested_name: &str,
) -> Result<PathBuf, AgentReviewError> {
    if text.len() > MAX_AGENT_PROPOSAL_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "草稿超过 {} MiB",
            MAX_AGENT_PROPOSAL_BYTES / 1024 / 1024
        )));
    }
    fs::create_dir_all(directory)
        .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    let name = safe_file_name(suggested_name, "agent-result.md");
    let target = unique_path(directory, &name);
    AtomicFile::new(&target, DisallowOverwrite)
        .write(|file| file.write_all(text.as_bytes()))
        .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    Ok(target)
}

pub fn default_draft_directory() -> Result<PathBuf, AgentReviewError> {
    let config = dirs::data_local_dir()
        .or_else(dirs::config_dir)
        .ok_or_else(|| AgentReviewError::InvalidPath("无法确定本地数据目录".to_string()))?;
    Ok(config.join("Kite").join("agent").join("drafts"))
}

fn safe_file_name(value: &str, fallback: &str) -> String {
    let name = value
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        })
        .filter(|character| !character.is_control())
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string();
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.chars().take(160).collect()
    }
}

fn unique_path(directory: &Path, file_name: &str) -> PathBuf {
    let candidate = directory.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("agent");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 2..10_000u32 {
        let name = match extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!("{stem}-{}.txt", Utc::now().timestamp_millis()))
}

pub fn build_audit_event(
    review: &AgentReview,
    user_confirmed_write: bool,
    outcome: AgentAuditOutcome,
) -> AgentAuditEvent {
    AgentAuditEvent {
        timestamp_unix_ms: Utc::now().timestamp_millis(),
        review_id: clean_label(&review.id, 160),
        agent: redact_sensitive_text(&clean_label(&review.agent, 160)),
        model: review
            .model
            .as_deref()
            .map(|value| redact_sensitive_text(&clean_label(value, 160)))
            .filter(|value| !value.is_empty()),
        transport: review.transport,
        working_directory: review.working_directory.clone(),
        context_count: (review.context_paths.len() + review.references.len()).min(u32::MAX as usize)
            as u32,
        result_type: review.result_type(),
        change_count: review.file_changes.len().min(u32::MAX as usize) as u32,
        user_confirmed_write,
        outcome,
    }
}

pub fn default_audit_path() -> Result<PathBuf, AgentReviewError> {
    let config = dirs::config_dir()
        .ok_or_else(|| AgentReviewError::InvalidPath("无法确定系统配置目录".to_string()))?;
    Ok(config.join("Kite").join("agent-audit.jsonl"))
}

pub fn append_audit_event(
    event: &AgentAuditEvent,
    path: Option<&Path>,
) -> Result<PathBuf, AgentReviewError> {
    let path = path
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(default_audit_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    }
    let mut line = serde_json::to_vec(event)
        .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    if line.len() > MAX_AUDIT_LINE_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(
            "审计记录超过单行上限".into(),
        ));
    }
    line.push(b'\n');
    let _guard = AUDIT_LOCK
        .lock()
        .map_err(|_| AgentReviewError::WriteFailed("审计锁不可用".into()))?;
    rotate_audit_if_needed(&path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    file.write_all(&line)
        .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    file.sync_data()
        .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    Ok(path)
}

pub fn redact_sensitive_text(input: &str) -> String {
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        vec![
            Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+").expect("valid bearer regex"),
            Regex::new(r"(?i)((?:api[_-]?key|token|secret|password)\s*[=:]\s*)[^\s,;]+")
                .expect("valid assignment regex"),
            Regex::new(r"(https?://[^/\s:@]+:)[^@\s]+@").expect("valid URL credential regex"),
            Regex::new(r"\bsk-[A-Za-z0-9_-]{8,}\b").expect("valid API key regex"),
        ]
    });
    let mut value = input.to_string();
    value = PATTERNS[0].replace_all(&value, "$1[REDACTED]").into_owned();
    value = PATTERNS[1].replace_all(&value, "$1[REDACTED]").into_owned();
    value = PATTERNS[2]
        .replace_all(&value, "$1[REDACTED]@")
        .into_owned();
    value = PATTERNS[3].replace_all(&value, "[REDACTED]").into_owned();
    value
}

fn build_reference(
    root: &Path,
    reference: AgentReferenceDraft,
) -> Result<AgentReference, AgentReviewError> {
    let path = resolve_user_path(root, &reference.path);
    let metadata = fs::metadata(&path).ok();
    let label = if reference.label.trim().is_empty() {
        path.file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    } else {
        clean_label(&reference.label, 240)
    };
    Ok(AgentReference {
        path,
        label,
        line: reference.line,
        exists: metadata.is_some(),
        is_directory: metadata.is_some_and(|metadata| metadata.is_dir()),
    })
}

fn build_permission_request(
    root: &Path,
    index: usize,
    request: AgentPermissionRequestDraft,
) -> Result<AgentPermissionRequest, AgentReviewError> {
    let title = clean_label(&request.title, 240);
    if title.is_empty() {
        return Err(AgentReviewError::InvalidInput(
            "权限请求标题不能为空".into(),
        ));
    }
    let id = request
        .id
        .map(|value| clean_label(&value, 160))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("permission-{}", index + 1));
    Ok(AgentPermissionRequest {
        id,
        kind: request.kind,
        title,
        detail: request.detail,
        target_paths: request
            .target_paths
            .iter()
            .map(|path| resolve_user_path(root, path))
            .collect(),
        can_allow_task: request.can_allow_task,
    })
}

fn normalize_command(
    command: AgentCommandSuggestion,
) -> Result<AgentCommandSuggestion, AgentReviewError> {
    let command_text = single_line(&command.command, 8 * 1024);
    if command_text.is_empty() {
        return Err(AgentReviewError::InvalidInput("命令建议不能为空".into()));
    }
    Ok(AgentCommandSuggestion {
        command: command_text,
        rationale: single_line(&command.rationale, 8 * 1024),
    })
}

fn single_line(value: &str, max_bytes: usize) -> String {
    let flattened = value
        .chars()
        .map(|character| match character {
            '\r' | '\n' | '\t' => ' ',
            other => other,
        })
        .collect::<String>();
    let mut truncated = flattened;
    while truncated.len() > max_bytes {
        truncated.pop();
    }
    truncated.trim().to_string()
}

fn build_file_change(
    root: &Path,
    draft: AgentFileChangeDraft,
) -> Result<AgentFileChange, AgentReviewError> {
    if draft.proposed_text.len() > MAX_AGENT_PROPOSAL_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "{} 超过 {} MiB",
            draft.path.display(),
            MAX_AGENT_PROPOSAL_BYTES / 1024 / 1024
        )));
    }
    let (relative, target) = relative_target(root, &draft.path)?;
    let expected = match draft.expected_sha256 {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(AgentReviewError::InvalidInput(format!(
                    "{} 的 expected_sha256 无效",
                    relative.display()
                )));
            }
            Some(normalized)
        }
        None => None,
    };
    let existing = current_existing_text(root, &relative)?;
    let existing_for_diff = existing.map(|(current_hash, _, text)| (current_hash, text));
    let mut change = build_change_from_text(
        root,
        &relative,
        draft.proposed_text,
        existing_for_diff,
        false,
    )?;
    if let Some(expected) = expected {
        change.requires_reconfirmation =
            change.current_sha256.as_deref() != Some(expected.as_str());
        change.base_sha256 = Some(expected);
    }
    change.target_path = target;
    Ok(change)
}

fn build_change_from_text(
    root: &Path,
    relative_path: &Path,
    proposed_text: String,
    existing: Option<(String, String)>,
    force_reconfirmation: bool,
) -> Result<AgentFileChange, AgentReviewError> {
    if proposed_text.len() > MAX_AGENT_PROPOSAL_BYTES {
        return Err(AgentReviewError::ResponseTooLarge(format!(
            "{} 超过 {} MiB",
            relative_path.display(),
            MAX_AGENT_PROPOSAL_BYTES / 1024 / 1024
        )));
    }
    let relative = normalized_relative_path(relative_path)?;
    let target = resolve_inside(root, Path::new(&relative))?;
    let target_exists = fs::symlink_metadata(&target).is_ok();
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        ensure_regular_non_symlink(&target, &metadata)?;
    }
    let current_sha256 = if target_exists {
        Some(hash_bytes(&fs::read(&target).map_err(|error| {
            AgentReviewError::WriteFailed(error.to_string())
        })?))
    } else {
        None
    };
    let (base_sha256, current_text) = match existing {
        Some((expected_hash, text)) => (Some(expected_hash), text),
        None => {
            if target_exists {
                return Err(AgentReviewError::FileChangedExternally(format!(
                    "{relative} 在审核后出现"
                )));
            }
            (None, String::new())
        }
    };
    let current_text = normalize_line_endings(&current_text);
    let proposed_for_diff = normalize_line_endings(&proposed_text);
    if current_text == proposed_for_diff {
        return Err(AgentReviewError::InvalidInput(format!(
            "{relative} 没有实际改动"
        )));
    }
    let diff = TextDiff::from_lines(&current_text, &proposed_for_diff);
    let mut added_lines = 0u32;
    let mut removed_lines = 0u32;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added_lines = added_lines.saturating_add(1),
            ChangeTag::Delete => removed_lines = removed_lines.saturating_add(1),
            ChangeTag::Equal => {}
        }
    }
    let old_header = if target_exists {
        format!("a/{relative}")
    } else {
        "/dev/null".to_string()
    };
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(&old_header, &format!("b/{relative}"))
        .to_string();
    let (unified_diff, diff_truncated) = truncate_utf8(unified, MAX_AGENT_DIFF_BYTES);
    let requires_reconfirmation = force_reconfirmation
        || (target_exists && base_sha256.as_deref() != current_sha256.as_deref());
    Ok(AgentFileChange {
        working_directory: root.to_path_buf(),
        relative_path: relative,
        target_path: target,
        proposed_sha256: hash_bytes(proposed_text.as_bytes()),
        byte_len: proposed_text.len() as u64,
        proposed_text,
        target_exists,
        overwrite_risk: target_exists,
        base_sha256,
        current_sha256,
        unified_diff,
        diff_truncated,
        added_lines,
        removed_lines,
        requires_reconfirmation,
    })
}

fn current_existing_text(
    root: &Path,
    relative: &Path,
) -> Result<Option<(String, Vec<u8>, String)>, AgentReviewError> {
    let relative = normalized_relative_path(relative)?;
    let target = resolve_inside(root, Path::new(&relative))?;
    match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            ensure_regular_non_symlink(&target, &metadata)?;
            if metadata.len() > MAX_AGENT_PROPOSAL_BYTES as u64 {
                return Err(AgentReviewError::ResponseTooLarge(format!(
                    "{relative} 超过 {} MiB",
                    MAX_AGENT_PROPOSAL_BYTES / 1024 / 1024
                )));
            }
            let bytes = fs::read(&target)
                .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
            if bytes.contains(&0) {
                return Err(AgentReviewError::InvalidInput(format!(
                    "{relative} 不是文本文件"
                )));
            }
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| AgentReviewError::InvalidInput(format!("{relative} 不是 UTF-8")))?
                .trim_start_matches('\u{feff}')
                .to_string();
            Ok(Some((hash_bytes(&bytes), bytes, text)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AgentReviewError::WriteFailed(error.to_string())),
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, AgentReviewError> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| AgentReviewError::InvalidPath(format!("{}：{error}", path.display())))?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| AgentReviewError::InvalidPath(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(AgentReviewError::InvalidPath(format!(
            "{} 不是目录",
            path.display()
        )));
    }
    Ok(dunce::simplified(&canonical).to_path_buf())
}

fn resolve_user_path(root: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    dunce::simplified(&joined).to_path_buf()
}

fn relative_target(root: &Path, path: &Path) -> Result<(PathBuf, PathBuf), AgentReviewError> {
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| AgentReviewError::OutsideWorkingDirectory(path.display().to_string()))?
    } else {
        path
    };
    let normalized = normalized_relative_path(relative)?;
    let target = resolve_inside(root, Path::new(&normalized))?;
    Ok((PathBuf::from(normalized), target))
}

fn normalized_relative_path(path: &Path) -> Result<String, AgentReviewError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(AgentReviewError::InvalidPath(path.display().to_string()));
    }
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => {
                segments.push(value.to_string_lossy().into_owned())
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(AgentReviewError::InvalidPath(path.display().to_string()));
            }
        }
    }
    if segments.is_empty() {
        return Err(AgentReviewError::InvalidPath(path.display().to_string()));
    }
    Ok(segments.join("/"))
}

fn resolve_inside(root: &Path, relative: &Path) -> Result<PathBuf, AgentReviewError> {
    let relative = normalized_relative_path(relative)?;
    let candidate = root.join(&relative);
    let parent = candidate
        .parent()
        .ok_or_else(|| AgentReviewError::InvalidPath(relative.clone()))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| AgentReviewError::InvalidPath(format!("{}：{error}", parent.display())))?;
    let canonical_parent = dunce::simplified(&canonical_parent);
    let file_name = candidate
        .file_name()
        .ok_or_else(|| AgentReviewError::InvalidPath(relative.clone()))?;
    let target = canonical_parent.join(file_name);
    if !target.starts_with(root) {
        return Err(AgentReviewError::OutsideWorkingDirectory(
            target.display().to_string(),
        ));
    }
    Ok(dunce::simplified(&target).to_path_buf())
}

fn prepare_destination(path: &Path) -> Result<PathBuf, AgentReviewError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| AgentReviewError::InvalidPath(error.to_string()))?
            .join(path)
    };
    let file_name = absolute
        .file_name()
        .ok_or_else(|| AgentReviewError::InvalidPath(absolute.display().to_string()))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| AgentReviewError::InvalidPath(absolute.display().to_string()))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| AgentReviewError::InvalidPath(format!("{}：{error}", parent.display())))?;
    let target = canonical_parent.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        ensure_regular_non_symlink(&target, &metadata)?;
    }
    Ok(dunce::simplified(&target).to_path_buf())
}

fn ensure_regular_non_symlink(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), AgentReviewError> {
    if metadata.file_type().is_symlink() {
        return Err(AgentReviewError::SymlinkNotAllowed(
            path.display().to_string(),
        ));
    }
    if !metadata.is_file() {
        return Err(AgentReviewError::InvalidPath(format!(
            "{} 不是普通文件",
            path.display()
        )));
    }
    Ok(())
}

fn write_atomic_checked(
    target: &Path,
    bytes: &[u8],
    expected_sha256: Option<&str>,
    overwrite: bool,
) -> Result<(), AgentReviewError> {
    let write = |file: &mut File| -> io::Result<()> {
        if let Some(expected) = expected_sha256 {
            let current = match fs::read(target) {
                Ok(current) => current,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Err(io::Error::other("target changed before write"));
                }
                Err(error) => return Err(error),
            };
            if hash_bytes(&current) != expected {
                return Err(io::Error::other("target changed before write"));
            }
        }
        file.write_all(bytes)
    };
    let result = if overwrite {
        AtomicFile::new(target, AllowOverwrite).write(write)
    } else {
        AtomicFile::new(target, DisallowOverwrite).write(write)
    };
    result.map_err(|error| {
        let message = error.to_string();
        if message.contains("target changed") {
            AgentReviewError::FileChangedExternally(target.display().to_string())
        } else {
            AgentReviewError::WriteFailed(message)
        }
    })?;
    let written =
        fs::read(target).map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    if hash_bytes(&written) != hash_bytes(bytes) {
        return Err(AgentReviewError::WriteFailed(format!(
            "{} 写入校验失败",
            target.display()
        )));
    }
    Ok(())
}

fn rotate_audit_if_needed(path: &Path) -> Result<(), AgentReviewError> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() <= MAX_AGENT_AUDIT_BYTES {
        return Ok(());
    }
    let backup = path.with_file_name(format!(
        "{}.1",
        path.file_name()
            .map(|value| value.to_string_lossy())
            .unwrap_or_else(|| "agent-audit.jsonl".into())
    ));
    if backup.exists() {
        fs::remove_file(&backup)
            .map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    }
    fs::rename(path, &backup).map_err(|error| AgentReviewError::WriteFailed(error.to_string()))?;
    Ok(())
}

fn truncate_utf8(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (format!("{}\n… diff 已截断 …", &value[..boundary]), true)
}

fn clean_label(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn default_schema_version() -> u32 {
    AGENT_REVIEW_SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("kite-agent-review-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, name: &str, text: &str) {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).expect("write fixture");
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn draft(root: &Path) -> AgentReviewDraft {
        AgentReviewDraft {
            schema_version: AGENT_REVIEW_SCHEMA_VERSION,
            id: Some("review-1".into()),
            agent: "Test Agent".into(),
            model: Some("fixture".into()),
            transport: AgentTransport::Acp,
            working_directory: root.to_path_buf(),
            context_paths: Vec::new(),
            answer: "answer body".into(),
            references: Vec::new(),
            file_changes: Vec::new(),
            permission_requests: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn change(root: &Path, file: &str, proposed: &str) -> AgentFileChange {
        let review = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from(file),
                proposed_text: proposed.into(),
                expected_sha256: None,
            }],
            ..draft(root)
        })
        .expect("build review");
        review.file_changes.into_iter().next().expect("change")
    }

    #[test]
    fn builds_diff_and_stats_without_writing() {
        let dir = TestDir::new();
        dir.write("src/lib.rs", "one\ntwo\n");
        let change = change(dir.path(), "src/lib.rs", "one\nthree\n");
        assert_eq!(change.added_lines, 1);
        assert_eq!(change.removed_lines, 1);
        assert!(change.unified_diff.contains("+three"));
        assert!(change.overwrite_risk);
        assert_eq!(
            fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(change.relative_path, "src/lib.rs");
    }

    #[test]
    fn apply_requires_confirmation() {
        let dir = TestDir::new();
        dir.write("file.txt", "old\n");
        let change = change(dir.path(), "file.txt", "new\n");
        assert_eq!(
            apply_file_change(&change, false).unwrap_err().kind(),
            AgentReviewErrorKind::ConfirmationRequired
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn apply_rejects_external_modification() {
        let dir = TestDir::new();
        dir.write("file.txt", "old\n");
        let change = change(dir.path(), "file.txt", "agent\n");
        dir.write("file.txt", "external\n");
        assert_eq!(
            apply_file_change(&change, true).unwrap_err().kind(),
            AgentReviewErrorKind::FileChangedExternally
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "external\n"
        );
    }

    #[test]
    fn reconfirm_refreshes_the_latest_file() {
        let dir = TestDir::new();
        dir.write("file.txt", "old\n");
        let change = change(dir.path(), "file.txt", "agent\n");
        dir.write("file.txt", "old\nexternal\n");
        let refreshed = reconfirm_file_change(&change).expect("reconfirm");
        assert!(!refreshed.requires_reconfirmation);
        assert_eq!(refreshed.removed_lines, 2);
        assert_eq!(refreshed.added_lines, 1);
        apply_file_change(&refreshed, true).expect("apply refreshed");
        assert_eq!(
            fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "agent\n"
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = TestDir::new();
        let error = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from("../outside.txt"),
                proposed_text: "x".into(),
                expected_sha256: None,
            }],
            ..draft(dir.path())
        })
        .unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::InvalidPath);
    }

    #[test]
    fn save_as_does_not_overwrite_without_confirmation() {
        let dir = TestDir::new();
        let existing = dir.path().join("result.md");
        fs::write(&existing, "existing").unwrap();
        let error = save_answer_as("answer", &existing, false).unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::ConfirmationRequired);
        let outcome = save_answer_as("answer", &existing, true).unwrap();
        assert!(outcome.overwritten);
        assert_eq!(fs::read_to_string(existing).unwrap(), "answer");
    }
}

#[cfg(test)]
mod review_security_tests {
    use super::tests_support::*;
    use super::*;

    fn request(can_allow_task: bool) -> AgentPermissionRequest {
        AgentPermissionRequest {
            id: "perm-1".into(),
            kind: AgentPermissionKind::Write,
            title: "Write file".into(),
            detail: "Needs access".into(),
            target_paths: vec![PathBuf::from("README.md")],
            can_allow_task,
        }
    }

    #[test]
    fn permission_denial_is_structured() {
        let response = respond_to_permission(
            &request(true),
            AgentPermissionDecision::Deny,
            "task-1",
            AgentTransport::Acp,
        )
        .unwrap();
        assert!(!response.allowed);
        let json: serde_json::Value = serde_json::from_str(&response.response_json).unwrap();
        assert_eq!(json["request_id"], "perm-1");
        assert_eq!(json["decision"], "deny");
        assert_eq!(json["allowed"], false);
    }

    #[test]
    fn task_permission_requires_support() {
        let error = respond_to_permission(
            &request(false),
            AgentPermissionDecision::AllowTask,
            "task-1",
            AgentTransport::Acp,
        )
        .unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::PermissionNotAvailable);
    }

    #[test]
    fn cli_permissions_are_not_faked() {
        let error = respond_to_permission(
            &request(true),
            AgentPermissionDecision::AllowOnce,
            "task-1",
            AgentTransport::Cli,
        )
        .unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::PermissionNotAvailable);
        assert!(AgentTransport::Cli.boundary_notice().contains("不能宣称"));
    }

    #[test]
    fn audit_contains_metadata_but_not_answer_content() {
        let dir = temp_fixture();
        let review = build_review(base_draft(dir.path())).unwrap();
        let event = build_audit_event(&review, true, AgentAuditOutcome::Applied);
        let path = dir.path().join("audit.jsonl");
        append_audit_event(&event, Some(&path)).unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("Test Agent"));
        assert!(text.contains("applied"));
        assert!(!text.contains("answer body"));
    }

    #[test]
    fn redacts_bearer_url_and_api_keys() {
        let value = "Bearer abc.def token=secret https://user:pass@example.com sk-abcdefghijk";
        let redacted = redact_sensitive_text(value);
        for secret in ["abc.def", "secret", "pass", "sk-abcdefghijk"] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
    }

    #[test]
    fn parses_structured_payload() {
        let dir = temp_fixture();
        dir.write("a.txt", "before\n");
        let payload = serde_json::json!({
            "schema_version": 1,
            "id": "json-review",
            "agent": "Fixture",
            "transport": "acp",
            "working_directory": dir.path(),
            "answer": "hello",
            "file_changes": [{
                "path": "a.txt",
                "proposed_text": "after\n"
            }]
        })
        .to_string();
        let review = parse_review_json(&payload).unwrap();
        assert_eq!(review.result_type(), AgentResultType::Mixed);
        assert_eq!(review.file_changes.len(), 1);
    }

    #[test]
    fn rejects_unknown_schema() {
        let dir = temp_fixture();
        let error = build_review(AgentReviewDraft {
            schema_version: 99,
            ..base_draft(dir.path())
        })
        .unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::UnsupportedSchema);
    }

    #[test]
    fn new_file_apply_does_not_overwrite_race() {
        let dir = temp_fixture();
        let review = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from("new.txt"),
                proposed_text: "agent\n".into(),
                expected_sha256: None,
            }],
            ..base_draft(dir.path())
        })
        .unwrap();
        let change = &review.file_changes[0];
        dir.write("new.txt", "other\n");
        assert_eq!(
            apply_file_change(change, true).unwrap_err().kind(),
            AgentReviewErrorKind::FileChangedExternally
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "other\n"
        );
    }

    #[test]
    fn expected_hash_mismatch_requires_reconfirmation() {
        let dir = temp_fixture();
        dir.write("file.txt", "version one\n");
        let review = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from("file.txt"),
                proposed_text: "proposal\n".into(),
                expected_sha256: Some(hash_bytes(b"version zero\n")),
            }],
            ..base_draft(dir.path())
        })
        .unwrap();
        let change = &review.file_changes[0];
        assert!(change.requires_reconfirmation);
        assert_eq!(
            apply_file_change(change, true).unwrap_err().kind(),
            AgentReviewErrorKind::FileChangedExternally
        );
    }

    #[test]
    fn cancelled_review_leaves_file_untouched() {
        let dir = temp_fixture();
        dir.write("file.txt", "before\n");
        let error = AgentReviewError::Cancelled;
        assert_eq!(error.kind(), AgentReviewErrorKind::Cancelled);
        assert_eq!(
            fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "before\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_targets() {
        use std::os::unix::fs::symlink;
        let dir = temp_fixture();
        dir.write("real.txt", "old\n");
        symlink(dir.path().join("real.txt"), dir.path().join("link.txt")).unwrap();
        let error = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from("link.txt"),
                proposed_text: "new\n".into(),
                expected_sha256: None,
            }],
            ..base_draft(dir.path())
        })
        .unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::SymlinkNotAllowed);
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SUPPORT_ID: AtomicU64 = AtomicU64::new(10_000);

    pub(super) struct TempFixture(pub PathBuf);

    impl TempFixture {
        pub(super) fn path(&self) -> &Path {
            &self.0
        }

        pub(super) fn write(&self, name: &str, text: &str) {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for TempFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn temp_fixture() -> TempFixture {
        let id = SUPPORT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kite-agent-security-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempFixture(path)
    }

    pub(super) fn base_draft(root: &Path) -> AgentReviewDraft {
        AgentReviewDraft {
            schema_version: AGENT_REVIEW_SCHEMA_VERSION,
            id: Some("review-2".into()),
            agent: "Test Agent".into(),
            model: Some("fixture".into()),
            transport: AgentTransport::Acp,
            working_directory: root.to_path_buf(),
            context_paths: Vec::new(),
            answer: "answer body".into(),
            references: Vec::new(),
            file_changes: Vec::new(),
            permission_requests: Vec::new(),
            commands: Vec::new(),
        }
    }
}

#[cfg(test)]
mod review_batch_tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn batch_preflight_prevents_partial_writes() {
        let dir = temp_fixture();
        dir.write("one.txt", "one-before\n");
        dir.write("two.txt", "two-before\n");
        let review = build_review(AgentReviewDraft {
            file_changes: vec![
                AgentFileChangeDraft {
                    path: PathBuf::from("one.txt"),
                    proposed_text: "one-after\n".into(),
                    expected_sha256: None,
                },
                AgentFileChangeDraft {
                    path: PathBuf::from("two.txt"),
                    proposed_text: "two-after\n".into(),
                    expected_sha256: None,
                },
            ],
            ..base_draft(dir.path())
        })
        .unwrap();
        dir.write("two.txt", "two-external\n");
        let error = apply_review_changes(&review.file_changes, true).unwrap_err();
        assert_eq!(error.kind(), AgentReviewErrorKind::FileChangedExternally);
        assert_eq!(
            fs::read_to_string(dir.path().join("one.txt")).unwrap(),
            "one-before\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("two.txt")).unwrap(),
            "two-external\n"
        );
    }

    #[test]
    fn tampered_dto_target_is_ignored() {
        let dir = temp_fixture();
        dir.write("real.txt", "before\n");
        dir.write("other.txt", "other\n");
        let review = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from("real.txt"),
                proposed_text: "after\n".into(),
                expected_sha256: None,
            }],
            ..base_draft(dir.path())
        })
        .unwrap();
        let mut change = review.file_changes[0].clone();
        change.target_path = dir.path().join("other.txt");
        let outcome = apply_file_change(&change, true).unwrap();
        assert_eq!(
            outcome.path,
            dir.path().join("real.txt").canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("real.txt")).unwrap(),
            "after\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("other.txt")).unwrap(),
            "other\n"
        );
    }

    #[test]
    fn command_suggestions_are_one_line_and_not_executed() {
        let dir = temp_fixture();
        let review = build_review(AgentReviewDraft {
            commands: vec![AgentCommandSuggestion {
                command: "echo first\necho second".into(),
                rationale: "line one\nline two".into(),
            }],
            ..base_draft(dir.path())
        })
        .unwrap();
        assert_eq!(review.commands[0].command, "echo first echo second");
        assert_eq!(review.commands[0].rationale, "line one line two");
        assert_eq!(review.result_type(), AgentResultType::Mixed);
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn materializes_unique_editor_draft() {
        let dir = temp_fixture();
        let first = materialize_text_draft("answer", dir.path(), "result.md").unwrap();
        let second = materialize_text_draft("answer", dir.path(), "result.md").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), "answer");
        assert_eq!(fs::read_to_string(second).unwrap(), "answer");
    }
}

#[cfg(test)]
mod review_encoding_tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn apply_preserves_bom_and_crlf() {
        let dir = temp_fixture();
        let path = dir.path().join("file.txt");
        let mut original = vec![0xEF, 0xBB, 0xBF];
        original.extend_from_slice(b"before\r\n");
        fs::write(&path, original).unwrap();
        let review = build_review(AgentReviewDraft {
            file_changes: vec![AgentFileChangeDraft {
                path: PathBuf::from("file.txt"),
                proposed_text: "after\n".into(),
                expected_sha256: None,
            }],
            ..base_draft(dir.path())
        })
        .unwrap();
        apply_review_changes(&review.file_changes, true).unwrap();
        let written = fs::read(path).unwrap();
        assert_eq!(written, b"\xEF\xBB\xBFafter\r\n");
    }
}

#[cfg(test)]
mod review_audit_privacy_tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn audit_never_contains_command_or_rationale() {
        let dir = temp_fixture();
        let review = build_review(AgentReviewDraft {
            commands: vec![AgentCommandSuggestion {
                command: "echo API_TOKEN=secret-value".into(),
                rationale: "PRIVATE RATIONALE".into(),
            }],
            ..base_draft(dir.path())
        })
        .unwrap();
        let event = build_audit_event(&review, false, AgentAuditOutcome::Prepared);
        let path = dir.path().join("audit.jsonl");
        append_audit_event(&event, Some(&path)).unwrap();
        let audit = fs::read_to_string(path).unwrap();
        assert!(audit.contains("Test Agent"));
        assert!(!audit.contains("secret-value"));
        assert!(!audit.contains("PRIVATE RATIONALE"));
    }
}
