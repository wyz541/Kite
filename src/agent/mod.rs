mod acp;
mod config;
mod events;
pub mod model;
mod redact;
mod session;
mod transport;

pub use events::AgentSessionEventSubscription;
pub use model::*;
pub use session::AgentSession;
pub mod capstone_diagnostics;
pub mod capstone_diagnostics_adapter;
pub mod connection;
mod context;
pub mod credentials;
mod diagnostics;
pub mod preset_adapter;
pub mod presets;
mod process;
mod restore;
mod runtime;
mod store;
mod ui_compat;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use connection::{AgentConnectionResult, AgentConnectionStatus, test_agent_connection};
pub use context::*;
pub use store::AgentProfileStore;

pub const AGENT_PROFILE_SCHEMA_VERSION: u32 = 1;
pub const MAX_AGENT_PROFILE_FILE_BYTES: u64 = 256 * 1024;
pub const MAX_AGENT_PROFILES: usize = 64;

const MAX_PROFILE_NAME_BYTES: usize = 80;
const MAX_PROFILE_ID_BYTES: usize = 128;
const MAX_EXECUTABLE_BYTES: usize = 2_048;
const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 4_096;
const MAX_ENVIRONMENT_VARIABLES: usize = 64;
const MAX_ENVIRONMENT_NAME_BYTES: usize = 128;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 4_096;
const MAX_MODEL_BYTES: usize = 200;
const MAX_CREDENTIAL_REF_BYTES: usize = 256;
const MAX_WORKING_DIRECTORY_BYTES: usize = 4_096;
const MIN_TIMEOUT_SECONDS: u32 = 1;
const MAX_TIMEOUT_SECONDS: u32 = 86_400;
const MIN_OUTPUT_LIMIT_BYTES: u64 = 1_024;
const MAX_OUTPUT_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

static NEXT_PROFILE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    AcpStdio,
    AcpHttp,
    Cli,
}

impl AgentTransport {
    pub const fn label(self) -> &'static str {
        match self {
            Self::AcpStdio => "ACP / JSON-RPC",
            Self::AcpHttp => "ACP HTTP",
            Self::Cli => "通用 CLI",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum WorkingDirectoryPolicy {
    #[default]
    SelectedItem,
    FileParent,
    RepositoryRoot,
    Fixed {
        path: String,
    },
    Ask,
}

impl WorkingDirectoryPolicy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::SelectedItem => "所选项目",
            Self::FileParent => "文件所在目录",
            Self::RepositoryRoot => "Git 仓库根目录",
            Self::Fixed { .. } => "固定目录",
            Self::Ask => "每次询问",
        }
    }

    pub fn fixed_path(&self) -> Option<&str> {
        match self {
            Self::Fixed { path } => Some(path),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    #[default]
    ReadOnly,
    ConfirmWrites,
    AgentManaged,
}

impl PermissionPolicy {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "只读",
            Self::ConfirmWrites => "写入前确认",
            Self::AgentManaged => "由 Agent 自身管理",
        }
    }

    pub const fn summary(self) -> &'static str {
        match self {
            Self::ReadOnly => "Kite 仅提供文件上下文；不自动写入或执行命令。",
            Self::ConfirmWrites => "写入和命令执行需要用户确认。",
            Self::AgentManaged => "权限由 Agent 自身管理；Kite 不声称已沙箱化。",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentCapabilities {
    pub read_selection: bool,
    pub read_workspace: bool,
    pub write_files: bool,
    pub execute_commands: bool,
    pub network: bool,
    pub permission_requests: bool,
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        Self {
            read_selection: true,
            read_workspace: false,
            write_files: false,
            execute_commands: false,
            network: false,
            permission_requests: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentEnvironmentVariable {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub transport: AgentTransport,
    #[serde(alias = "command")]
    pub executable: Option<String>,
    pub endpoint: Option<String>,
    #[serde(alias = "args")]
    pub arguments: Vec<String>,
    #[serde(alias = "env")]
    pub environment: Vec<AgentEnvironmentVariable>,
    pub model: Option<String>,
    pub working_directory: WorkingDirectoryPolicy,
    pub permission: PermissionPolicy,
    pub timeout_seconds: u32,
    pub output_limit_bytes: u64,
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub credential_env_var: Option<String>,
    pub capabilities: AgentCapabilities,
}

impl Default for AgentProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            transport: AgentTransport::default(),
            executable: None,
            endpoint: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            model: None,
            working_directory: WorkingDirectoryPolicy::default(),
            permission: PermissionPolicy::default(),
            timeout_seconds: 600,
            output_limit_bytes: 8 * 1024 * 1024,
            credential_ref: None,
            credential_env_var: None,
            capabilities: AgentCapabilities::default(),
        }
    }
}

impl AgentProfile {
    pub fn has_stored_credential(&self) -> bool {
        self.credential_ref.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, uniffi::Record)]
pub struct AgentProfileSnapshot {
    pub schema_version: u32,
    pub profiles: Vec<AgentProfile>,
    pub default_profile_id: Option<String>,
    pub warning: Option<AgentConfigWarning>,
}

impl AgentProfileSnapshot {
    pub fn default_profile(&self) -> Option<&AgentProfile> {
        self.default_profile_id
            .as_deref()
            .and_then(|id| self.profiles.iter().find(|profile| profile.id == id))
            .or_else(|| self.profiles.first())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentConfigWarning {
    CorruptConfig { message: String },
    UnknownSchemaVersion { found: u32, supported: u32 },
    ConfigTooLarge { bytes: u64, max_bytes: u64 },
    InvalidProfile { field: String, message: String },
    StorageUnavailable { message: String },
    Io { message: String },
    MigrationApplied { from_version: u32, to_version: u32 },
}

impl AgentConfigWarning {
    pub fn message(&self) -> String {
        match self {
            Self::CorruptConfig { message } => format!("Agent 配置损坏：{message}"),
            Self::UnknownSchemaVersion { found, supported } => {
                format!("不支持的 Agent 配置版本：{found}（当前为 {supported}）")
            }
            Self::ConfigTooLarge { bytes, max_bytes } => {
                format!("Agent 配置过大：{bytes} 字节（上限 {max_bytes} 字节）")
            }
            Self::InvalidProfile { field, message } => {
                format!("Agent 配置字段 {field} 无效：{message}")
            }
            Self::StorageUnavailable { message } => {
                format!("Agent 配置存储不可用：{message}")
            }
            Self::Io { message } => format!("Agent 配置读写失败：{message}"),
            Self::MigrationApplied {
                from_version,
                to_version,
            } => format!("Agent 配置已从版本 {from_version} 迁移到 {to_version}"),
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum AgentConfigError {
    #[error("invalid profile field `{field}`: {message}")]
    InvalidProfile { field: String, message: String },
    #[error("profile name `{name}` already exists")]
    DuplicateProfileName { name: String },
    #[error("profile id `{profile_id}` already exists")]
    DuplicateProfileId { profile_id: String },
    #[error("profile `{profile_id}` was not found")]
    ProfileNotFound { profile_id: String },
    #[error("at most {limit} agent profiles are allowed")]
    ProfileLimitReached { limit: u32 },
    #[error("unknown agent config schema version {found}; supported version is {supported}")]
    UnknownSchemaVersion { found: u32, supported: u32 },
    #[error("agent config exceeds the {max_bytes}-byte limit ({bytes} bytes)")]
    ConfigTooLarge { bytes: u64, max_bytes: u64 },
    #[error("agent config is corrupt: {message}")]
    CorruptConfig { message: String },
    #[error("agent config storage is unavailable: {message}")]
    StorageUnavailable { message: String },
    #[error("agent config I/O failed: {message}")]
    Io { message: String },
}

impl AgentConfigError {
    pub(crate) fn warning(&self) -> AgentConfigWarning {
        match self {
            Self::UnknownSchemaVersion { found, supported } => {
                AgentConfigWarning::UnknownSchemaVersion {
                    found: *found,
                    supported: *supported,
                }
            }
            Self::ConfigTooLarge { bytes, max_bytes } => AgentConfigWarning::ConfigTooLarge {
                bytes: *bytes,
                max_bytes: *max_bytes,
            },
            Self::CorruptConfig { message } => AgentConfigWarning::CorruptConfig {
                message: message.clone(),
            },
            Self::StorageUnavailable { message } => AgentConfigWarning::StorageUnavailable {
                message: message.clone(),
            },
            Self::Io { message } => AgentConfigWarning::Io {
                message: message.clone(),
            },
            Self::InvalidProfile { field, message } => AgentConfigWarning::InvalidProfile {
                field: field.clone(),
                message: message.clone(),
            },
            Self::DuplicateProfileName { name } => AgentConfigWarning::InvalidProfile {
                field: "name".to_string(),
                message: format!("profile name `{name}` is duplicated"),
            },
            Self::DuplicateProfileId { profile_id } => AgentConfigWarning::InvalidProfile {
                field: "id".to_string(),
                message: format!("profile id `{profile_id}` is duplicated"),
            },
            Self::ProfileNotFound { profile_id } => AgentConfigWarning::InvalidProfile {
                field: "profiles".to_string(),
                message: format!("profile `{profile_id}` was not found"),
            },
            Self::ProfileLimitReached { limit } => AgentConfigWarning::InvalidProfile {
                field: "profiles".to_string(),
                message: format!("at most {limit} profiles are allowed"),
            },
        }
    }
}

pub(crate) fn prepare_new_profile(
    mut profile: AgentProfile,
) -> Result<AgentProfile, AgentConfigError> {
    profile.id = profile.id.trim().to_string();
    if profile.id.is_empty() {
        profile.id = generate_profile_id();
    }
    validate_and_normalize_profile(&mut profile)?;
    Ok(profile)
}

pub(crate) fn prepare_loaded_profile(
    mut profile: AgentProfile,
    allow_generated_id: bool,
) -> Result<AgentProfile, AgentConfigError> {
    profile.id = profile.id.trim().to_string();
    if profile.id.is_empty() {
        if !allow_generated_id {
            return Err(invalid("id", "must not be empty"));
        }
        profile.id = generate_profile_id();
    }
    validate_and_normalize_profile(&mut profile)?;
    Ok(profile)
}

pub(crate) fn validate_and_normalize_profile(
    profile: &mut AgentProfile,
) -> Result<(), AgentConfigError> {
    profile.id = profile.id.trim().to_string();
    validate_identifier("id", &profile.id, MAX_PROFILE_ID_BYTES)?;

    profile.name = profile.name.trim().to_string();
    validate_text("name", &profile.name, MAX_PROFILE_NAME_BYTES, false)?;

    profile.executable = normalize_optional_text(
        profile.executable.take(),
        "executable",
        MAX_EXECUTABLE_BYTES,
    )?;
    profile.endpoint =
        normalize_optional_text(profile.endpoint.take(), "endpoint", MAX_ENDPOINT_BYTES)?;
    profile.model = normalize_optional_text(profile.model.take(), "model", MAX_MODEL_BYTES)?;

    match profile.transport {
        AgentTransport::AcpStdio | AgentTransport::Cli => {
            let executable = profile
                .executable
                .as_deref()
                .ok_or_else(|| invalid("executable", "is required for process-based transports"))?;
            reject_secret_literal("executable", executable)?;
            if profile.endpoint.is_some() {
                return Err(invalid(
                    "endpoint",
                    "must be empty for process-based transports",
                ));
            }
        }
        AgentTransport::AcpHttp => {
            let endpoint = profile
                .endpoint
                .as_deref()
                .ok_or_else(|| invalid("endpoint", "is required for HTTP transport"))?;
            validate_endpoint(endpoint)?;
            if profile.executable.is_some() {
                return Err(invalid("executable", "must be empty for HTTP transport"));
            }
        }
    }

    if profile.arguments.len() > MAX_ARGUMENTS {
        return Err(invalid(
            "arguments",
            format!("must contain at most {MAX_ARGUMENTS} entries"),
        ));
    }
    for (index, argument) in profile.arguments.iter().enumerate() {
        validate_text(
            &format!("arguments[{index}]"),
            argument,
            MAX_ARGUMENT_BYTES,
            true,
        )?;
        reject_secret_literal(&format!("arguments[{index}]"), argument)?;
    }

    if profile.environment.len() > MAX_ENVIRONMENT_VARIABLES {
        return Err(invalid(
            "environment",
            format!("must contain at most {MAX_ENVIRONMENT_VARIABLES} entries"),
        ));
    }
    let mut environment_names = HashSet::new();
    for (index, variable) in profile.environment.iter_mut().enumerate() {
        variable.name = variable.name.trim().to_string();
        validate_environment_name(index, &variable.name)?;
        validate_text(
            &format!("environment[{index}].value"),
            &variable.value,
            MAX_ENVIRONMENT_VALUE_BYTES,
            true,
        )?;
        if !environment_names.insert(variable.name.to_ascii_uppercase()) {
            return Err(invalid(
                format!("environment[{index}].name"),
                "duplicates another environment variable",
            ));
        }
        if sensitive_environment_name(&variable.name) {
            return Err(invalid(
                format!("environment[{index}].name"),
                "sensitive values must be supplied through credential_ref",
            ));
        }
        reject_secret_literal(&format!("environment[{index}].value"), &variable.value)?;
        reject_embedded_url_credentials(&format!("environment[{index}].value"), &variable.value)?;
    }

    if let WorkingDirectoryPolicy::Fixed { path } = &mut profile.working_directory {
        *path = path.trim().to_string();
        validate_text(
            "working_directory.path",
            path,
            MAX_WORKING_DIRECTORY_BYTES,
            false,
        )?;
    }

    if !(MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&profile.timeout_seconds) {
        return Err(invalid(
            "timeout_seconds",
            format!("must be between {MIN_TIMEOUT_SECONDS} and {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    if !(MIN_OUTPUT_LIMIT_BYTES..=MAX_OUTPUT_LIMIT_BYTES).contains(&profile.output_limit_bytes) {
        return Err(invalid(
            "output_limit_bytes",
            format!("must be between {MIN_OUTPUT_LIMIT_BYTES} and {MAX_OUTPUT_LIMIT_BYTES} bytes"),
        ));
    }

    if let Some(model) = profile.model.as_deref() {
        validate_text("model", model, MAX_MODEL_BYTES, false)?;
        reject_secret_literal("model", model)?;
    }
    profile.credential_ref = normalize_optional_text(
        profile.credential_ref.take(),
        "credential_ref",
        MAX_CREDENTIAL_REF_BYTES,
    )?;
    if let Some(credential_ref) = profile.credential_ref.as_deref() {
        validate_credential_ref(credential_ref)?;
    }
    profile.credential_env_var = normalize_optional_text(
        profile.credential_env_var.take(),
        "credential_env_var",
        MAX_ENVIRONMENT_NAME_BYTES,
    )?;
    if let Some(environment_name) = profile.credential_env_var.as_deref() {
        validate_environment_name(0, environment_name)?;
        if profile
            .environment
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(environment_name))
        {
            return Err(invalid(
                "credential_env_var",
                "must not duplicate a normal environment variable",
            ));
        }
    }

    Ok(())
}

fn validate_identifier(field: &str, value: &str, max_bytes: usize) -> Result<(), AgentConfigError> {
    validate_text(field, value, max_bytes, false)?;
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
    }) {
        return Err(invalid(
            field,
            "may contain only ASCII letters, numbers, '.', '-', '_' and ':'",
        ));
    }
    Ok(())
}

fn validate_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), AgentConfigError> {
    if !allow_empty && value.trim().is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(invalid(field, format!("must not exceed {max_bytes} bytes")));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

fn normalize_optional_text(
    value: Option<String>,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>, AgentConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }
    validate_text(field, &value, max_bytes, false)?;
    Ok(Some(value))
}

fn validate_endpoint(endpoint: &str) -> Result<(), AgentConfigError> {
    let parsed = url::Url::parse(endpoint)
        .map_err(|_| invalid("endpoint", "must be a valid absolute HTTP or HTTPS URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid(
            "endpoint",
            "must be a valid absolute HTTP or HTTPS URL",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(
            "endpoint",
            "must not contain embedded credentials; use credential_ref",
        ));
    }
    reject_secret_literal("endpoint", endpoint)
}

fn validate_environment_name(index: usize, name: &str) -> Result<(), AgentConfigError> {
    validate_text(
        &format!("environment[{index}].name"),
        name,
        MAX_ENVIRONMENT_NAME_BYTES,
        false,
    )?;
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return Err(invalid(
            format!("environment[{index}].name"),
            "must not be empty",
        ));
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(invalid(
            format!("environment[{index}].name"),
            "must match [A-Za-z_][A-Za-z0-9_]*",
        ));
    }
    Ok(())
}

fn validate_credential_ref(credential_ref: &str) -> Result<(), AgentConfigError> {
    let (scheme, reference) = credential_ref.split_once(':').ok_or_else(|| {
        invalid(
            "credential_ref",
            "must use an opaque scheme prefix such as `keychain:`",
        )
    })?;
    if !matches!(scheme, "keychain" | "credential" | "env") {
        return Err(invalid(
            "credential_ref",
            "must use one of the schemes `keychain:`, `credential:` or `env:`",
        ));
    }
    if reference.is_empty()
        || !reference.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '/' | ':')
        })
    {
        return Err(invalid(
            "credential_ref",
            "must contain only ASCII letters, numbers, '.', '_', '-', '/' or ':'",
        ));
    }
    reject_secret_literal("credential_ref", credential_ref)
}

fn reject_secret_literal(field: &str, value: &str) -> Result<(), AgentConfigError> {
    if looks_like_secret(value) {
        return Err(invalid(
            field,
            "must not contain plaintext secrets; use credential_ref",
        ));
    }
    Ok(())
}

fn reject_embedded_url_credentials(field: &str, value: &str) -> Result<(), AgentConfigError> {
    if let Ok(parsed) = url::Url::parse(value.trim())
        && (!parsed.username().is_empty() || parsed.password().is_some())
    {
        return Err(invalid(
            field,
            "must not contain embedded credentials; use credential_ref",
        ));
    }
    Ok(())
}

fn looks_like_secret(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return false;
    }
    if value.starts_with("sk-")
        || value.starts_with("sk_")
        || value.starts_with("ghp_")
        || value.starts_with("github_pat_")
        || value.starts_with("xox")
        || value.starts_with("akia")
        || value.starts_with("bearer ")
    {
        return true;
    }
    if value.contains("-----begin") && value.contains("private key-----") {
        return true;
    }
    [
        "token=",
        "token:",
        "secret=",
        "secret:",
        "password=",
        "password:",
        "passwd=",
        "api_key=",
        "api-key=",
        "apikey=",
        "access_key=",
        "access-key=",
        "authorization=",
        "authorization:",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

fn sensitive_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTH",
        "AUTHORIZATION",
    ]
    .iter()
    .any(|marker| {
        name == *marker
            || name.starts_with(&format!("{marker}_"))
            || name.ends_with(&format!("_{marker}"))
            || name.contains(&format!("_{marker}_"))
    })
}

fn invalid(field: impl Into<String>, message: impl Into<String>) -> AgentConfigError {
    AgentConfigError::InvalidProfile {
        field: field.into(),
        message: message.into(),
    }
}

pub fn new_profile_id() -> String {
    generate_profile_id()
}

fn generate_profile_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed);
    format!(
        "agent-{:x}-{:x}-{:x}",
        std::process::id(),
        timestamp,
        sequence
    )
}

pub use diagnostics::{
    AgentConnectionInfo, AgentDiagnosticEntry, AgentDiagnostics, AgentProfileDiagnostic,
    agent_log_path, clear_agent_diagnostics, diagnose_agents, probe_agent_connection,
    record_agent_event, redact_agent_text,
};
pub use preset_adapter::{apply_preset_to_existing, preset_profile};
pub use presets::{
    AgentPreset, AgentPresetPermission, AgentPresetSpec, AgentPresetTransport, agent_presets,
    preset_spec, preset_specs,
};
pub use restore::{
    AgentContextRepair, AgentConversation, AgentMessage, AgentMessageRole, AgentSessionRestore,
    AgentSessionStore, agent_session_path, delete_agent_conversation, load_agent_conversation,
    load_agent_sessions, load_agent_sessions_at, new_agent_conversation_id, repair_agent_context,
    save_agent_conversation, save_agent_sessions_at,
};
pub use runtime::{
    AgentEvent, AgentEventKind, AgentPermissionDecision, AgentPermissionOption,
    AgentPermissionRequest, AgentRuntime, AgentRuntimeSnapshot, AgentRuntimeState,
};
pub use ui_compat::{
    AgentStoreLoad, agent_config_path, default_agent_profile_id, load_agent_profiles,
    save_agent_profiles,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> AgentProfile {
        AgentProfile {
            id: "profile-1".to_string(),
            name: "Local Agent".to_string(),
            executable: Some("agent".to_string()),
            ..AgentProfile::default()
        }
    }

    #[test]
    fn snapshot_resolves_the_default_profile_with_a_first_profile_fallback() {
        let first = profile();
        let mut second = profile();
        second.id = "profile-2".to_string();
        second.name = "Review Agent".to_string();

        let snapshot = AgentProfileSnapshot {
            schema_version: AGENT_PROFILE_SCHEMA_VERSION,
            profiles: vec![first, second.clone()],
            default_profile_id: Some(second.id.clone()),
            warning: None,
        };
        assert_eq!(
            snapshot
                .default_profile()
                .map(|profile| profile.id.as_str()),
            Some("profile-2")
        );

        let snapshot = AgentProfileSnapshot {
            default_profile_id: None,
            ..snapshot
        };
        assert_eq!(
            snapshot
                .default_profile()
                .map(|profile| profile.id.as_str()),
            Some("profile-1")
        );
    }

    #[test]
    fn validates_process_and_http_transport_requirements() {
        let mut missing_command = profile();
        missing_command.executable = None;
        assert!(matches!(
            validate_and_normalize_profile(&mut missing_command),
            Err(AgentConfigError::InvalidProfile { field, .. }) if field == "executable"
        ));

        let mut http = profile();
        http.transport = AgentTransport::AcpHttp;
        http.executable = None;
        http.endpoint = Some("https://localhost:7777/acp".to_string());
        validate_and_normalize_profile(&mut http).expect("valid HTTP profile");

        let mut credentialed = http.clone();
        credentialed.endpoint = Some("https://user:secret@localhost/acp".to_string());
        assert!(validate_and_normalize_profile(&mut credentialed).is_err());
    }

    #[test]
    fn rejects_plaintext_secrets_and_sensitive_environment_names() {
        let mut environment_secret = profile();
        environment_secret.environment = vec![AgentEnvironmentVariable {
            name: "OPENAI_API_KEY".to_string(),
            value: "not-stored-secret".to_string(),
        }];
        let error = validate_and_normalize_profile(&mut environment_secret).unwrap_err();
        assert!(!error.to_string().contains("not-stored-secret"));

        let mut argument_secret = profile();
        argument_secret.arguments = vec!["--token=not-stored-secret".to_string()];
        assert!(validate_and_normalize_profile(&mut argument_secret).is_err());

        let mut raw_reference = profile();
        raw_reference.credential_ref = Some("not-stored-secret".to_string());
        assert!(validate_and_normalize_profile(&mut raw_reference).is_err());

        let mut url_reference = profile();
        url_reference.credential_ref = Some("https://example.test/credential".to_string());
        assert!(validate_and_normalize_profile(&mut url_reference).is_err());

        let mut control_name = profile();
        control_name.name = "Local\nAgent".to_string();
        assert!(validate_and_normalize_profile(&mut control_name).is_err());
    }

    #[test]
    fn normalizes_safe_profiles() {
        let mut profile = profile();
        profile.id = "  profile-1  ".to_string();
        profile.name = "  Local Agent  ".to_string();
        profile.executable = Some("  agent  ".to_string());
        profile.credential_ref = Some(" keychain:kite/agent/local ".to_string());
        profile.environment = vec![AgentEnvironmentVariable {
            name: " LANG ".to_string(),
            value: "en_US.UTF-8".to_string(),
        }];

        validate_and_normalize_profile(&mut profile).expect("normalize profile");

        assert_eq!(profile.id, "profile-1");
        assert_eq!(profile.name, "Local Agent");
        assert_eq!(profile.executable.as_deref(), Some("agent"));
        assert_eq!(
            profile.credential_ref.as_deref(),
            Some("keychain:kite/agent/local")
        );
        assert_eq!(profile.environment[0].name, "LANG");
    }
}
