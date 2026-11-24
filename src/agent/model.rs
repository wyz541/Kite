use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const AGENT_CONFIG_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_AGENT_TIMEOUT_SECONDS: u32 = 30;
pub const DEFAULT_AGENT_OUTPUT_LIMIT_BYTES: u64 = 512 * 1024;
pub const MAX_AGENT_PROFILES: usize = 64;
pub const MAX_AGENT_TIMEOUT_SECONDS: u32 = 3_600;
pub const MIN_AGENT_OUTPUT_LIMIT_BYTES: u64 = 16 * 1024;
pub const MAX_AGENT_OUTPUT_LIMIT_BYTES: u64 = 16 * 1024 * 1024;

static NEXT_PROFILE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    AcpStdio,
    GenericCli,
}

impl AgentTransport {
    pub fn label(self) -> &'static str {
        match self {
            Self::AcpStdio => "ACP / JSON-RPC",
            Self::GenericCli => "通用 CLI",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum WorkingDirectoryPolicy {
    SelectedItem,
    FileParent,
    RepositoryRoot,
    Fixed,
    #[default]
    Ask,
}

impl WorkingDirectoryPolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::SelectedItem => "所选项目",
            Self::FileParent => "文件所在目录",
            Self::RepositoryRoot => "Git 仓库根目录",
            Self::Fixed => "固定目录",
            Self::Ask => "每次询问",
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
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "只读",
            Self::ConfirmWrites => "写入前确认",
            Self::AgentManaged => "由 Agent 自身管理",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            Self::ReadOnly => "Kite 仅提供文件上下文；不自动写入或执行命令。",
            Self::ConfirmWrites => "写入和命令执行需要用户确认。",
            Self::AgentManaged => "权限由 Agent 自身管理；Kite 不声称已沙箱化。",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentEnvironmentVariable {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub transport: AgentTransport,
    pub executable: String,
    pub endpoint: Option<String>,
    pub arguments: Vec<String>,
    pub environment: Vec<AgentEnvironmentVariable>,
    pub model: Option<String>,
    pub working_directory_policy: WorkingDirectoryPolicy,
    pub working_directory: Option<String>,
    pub permission_policy: PermissionPolicy,
    pub timeout_seconds: u32,
    pub output_limit_bytes: u64,
    pub credential_env_var: Option<String>,
    pub credential_ref: Option<String>,
}

impl Default for AgentProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            transport: AgentTransport::AcpStdio,
            executable: String::new(),
            endpoint: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            model: None,
            working_directory_policy: WorkingDirectoryPolicy::Ask,
            working_directory: None,
            permission_policy: PermissionPolicy::ReadOnly,
            timeout_seconds: DEFAULT_AGENT_TIMEOUT_SECONDS,
            output_limit_bytes: DEFAULT_AGENT_OUTPUT_LIMIT_BYTES,
            credential_env_var: None,
            credential_ref: None,
        }
    }
}

impl AgentProfile {
    pub fn normalized(mut self) -> Result<Self, String> {
        if self.id.trim().is_empty() {
            self.id = new_profile_id();
        }
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.executable = self.executable.trim().to_string();
        self.endpoint = normalize_optional(self.endpoint);
        self.model = normalize_optional(self.model);
        self.working_directory = normalize_optional(self.working_directory);
        self.credential_env_var = normalize_optional(self.credential_env_var);
        self.credential_ref = normalize_optional(self.credential_ref);
        self.arguments = self
            .arguments
            .into_iter()
            .map(|argument| argument.trim_end_matches('\r').to_string())
            .filter(|argument| !argument.is_empty())
            .collect();
        self.environment = self
            .environment
            .into_iter()
            .filter(|entry| !entry.key.trim().is_empty())
            .map(|entry| AgentEnvironmentVariable {
                key: entry.key.trim().to_string(),
                value: entry.value,
            })
            .collect();
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.len() < 8 || self.id.len() > 128 {
            return Err("Agent 标识长度无效".to_string());
        }
        if self.name.is_empty() {
            return Err("请输入 Agent 名称".to_string());
        }
        if self.name.chars().count() > 80 {
            return Err("Agent 名称不能超过 80 个字符".to_string());
        }
        if self.executable.is_empty() {
            return Err("请输入可执行文件路径或命令名称".to_string());
        }
        if self.executable.chars().count() > 4096 {
            return Err("可执行文件路径过长".to_string());
        }
        if self
            .endpoint
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            return Err("当前接入方式不支持远程端点".to_string());
        }
        if self.arguments.len() > 64 {
            return Err("启动参数不能超过 64 项".to_string());
        }
        for argument in &self.arguments {
            if argument.len() > 4096 {
                return Err("单个启动参数过长".to_string());
            }
            if argument_contains_secret(argument) {
                return Err(
                    "启动参数疑似包含密钥。请改用“凭据”栏，让密钥保存到系统钥匙串。".to_string(),
                );
            }
        }
        if self.environment.len() > 64 {
            return Err("环境变量不能超过 64 项".to_string());
        }
        let mut seen_environment = std::collections::BTreeSet::new();
        for entry in &self.environment {
            if !valid_environment_key(&entry.key) {
                return Err(format!("环境变量名称无效：{}", entry.key));
            }
            if !seen_environment.insert(entry.key.to_ascii_uppercase()) {
                return Err(format!("环境变量重复：{}", entry.key));
            }
            if environment_key_is_sensitive(&entry.key) {
                return Err(format!(
                    "环境变量 {} 疑似用于保存密钥。请改用“凭据”栏。",
                    entry.key
                ));
            }
            if entry.value.len() > 16 * 1024 {
                return Err(format!("环境变量 {} 的值过长", entry.key));
            }
        }
        if let Some(model) = &self.model
            && model.chars().count() > 128
        {
            return Err("模型名称不能超过 128 个字符".to_string());
        }
        if self.working_directory_policy == WorkingDirectoryPolicy::Fixed {
            let directory = self
                .working_directory
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "固定工作目录策略需要选择目录".to_string())?;
            let path = Path::new(directory);
            if !path.is_dir() {
                return Err(format!("工作目录不存在或不是文件夹：{directory}"));
            }
            if path.to_string_lossy().chars().count() > 4096 {
                return Err("工作目录路径过长".to_string());
            }
        } else if self.working_directory.is_some() {
            return Err("当前工作目录策略不使用固定目录字段".to_string());
        }
        if !(1..=MAX_AGENT_TIMEOUT_SECONDS).contains(&self.timeout_seconds) {
            return Err(format!(
                "超时必须在 1 到 {MAX_AGENT_TIMEOUT_SECONDS} 秒之间"
            ));
        }
        if !(MIN_AGENT_OUTPUT_LIMIT_BYTES..=MAX_AGENT_OUTPUT_LIMIT_BYTES)
            .contains(&self.output_limit_bytes)
        {
            return Err(format!(
                "输出上限必须在 {} KiB 到 {} MiB 之间",
                MIN_AGENT_OUTPUT_LIMIT_BYTES / 1024,
                MAX_AGENT_OUTPUT_LIMIT_BYTES / (1024 * 1024)
            ));
        }
        if let Some(environment_key) = &self.credential_env_var {
            if !valid_environment_key(environment_key) {
                return Err("凭据环境变量名称无效".to_string());
            }
            if self
                .environment
                .iter()
                .any(|entry| entry.key.eq_ignore_ascii_case(environment_key))
            {
                return Err("凭据环境变量不能同时出现在普通环境变量中".to_string());
            }
        }
        if let Some(reference) = &self.credential_ref
            && !reference.starts_with("keychain://")
        {
            return Err("凭据引用只能指向系统钥匙串".to_string());
        }
        if contains_url_credentials(&self.executable) {
            return Err("可执行文件字段不能包含 URL 凭据".to_string());
        }
        Ok(())
    }

    pub fn default_working_directory(&self) -> PathBuf {
        if self.working_directory_policy == WorkingDirectoryPolicy::Fixed
            && let Some(directory) = self.working_directory.as_deref()
        {
            return PathBuf::from(directory);
        }
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn has_stored_credential(&self) -> bool {
        self.credential_ref.is_some()
    }

    pub fn duplicate_with_name(&self, name: &str) -> Self {
        let mut duplicate = self.clone();
        duplicate.id = new_profile_id();
        duplicate.name = name.to_string();
        duplicate.credential_ref = None;
        duplicate
    }
}

pub fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

pub fn new_profile_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let serial = NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed);
    format!("agent-{millis:x}-{:x}-{serial:x}", std::process::id())
}

pub fn environment_key_is_sensitive(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "token",
        "password",
        "passwd",
        "secret",
        "apikey",
        "accesskey",
        "privatekey",
        "credential",
        "bearer",
        "authorization",
        "authkey",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn argument_contains_secret(argument: &str) -> bool {
    let normalized = argument.trim().to_ascii_lowercase();
    if normalized.starts_with("--token=")
        || normalized.starts_with("--password=")
        || normalized.starts_with("--api-key=")
        || normalized.starts_with("--apikey=")
        || normalized.starts_with("--secret=")
    {
        return true;
    }
    if matches!(
        normalized.as_str(),
        "--token" | "--password" | "--api-key" | "--apikey" | "--secret" | "--auth"
    ) {
        return true;
    }
    contains_url_credentials(argument)
}

fn contains_url_credentials(value: &str) -> bool {
    let Some(scheme_end) = value.find("://") else {
        return false;
    };
    let rest = &value[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    authority.contains('@')
        && authority
            .split_once('@')
            .is_some_and(|(userinfo, _)| !userinfo.is_empty() && userinfo.contains(':'))
}

fn valid_environment_key(key: &str) -> bool {
    let mut characters = key.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> AgentProfile {
        AgentProfile {
            name: "Local Agent".to_string(),
            executable: "agent".to_string(),
            ..AgentProfile::default()
        }
    }

    #[test]
    fn new_profiles_get_ids_and_normalized_fields() {
        let profile = AgentProfile {
            name: "  Local Agent  ".to_string(),
            executable: "  agent  ".to_string(),
            ..AgentProfile::default()
        }
        .normalized()
        .expect("valid profile");
        assert!(profile.id.starts_with("agent-"));
        assert_eq!(profile.name, "Local Agent");
        assert_eq!(profile.executable, "agent");
    }

    #[test]
    fn rejects_secret_bearing_environment_and_arguments() {
        let mut environment = profile();
        environment.environment.push(AgentEnvironmentVariable {
            key: "OPENAI_API_KEY".to_string(),
            value: "secret-value".to_string(),
        });
        assert!(environment.normalized().unwrap_err().contains("凭据"));

        let mut arguments = profile();
        arguments.arguments.push("--token=secret-value".to_string());
        assert!(arguments.normalized().unwrap_err().contains("密钥"));
    }

    #[test]
    fn fixed_working_directory_must_exist() {
        let mut profile = profile();
        profile.working_directory_policy = WorkingDirectoryPolicy::Fixed;
        profile.working_directory = Some(std::env::temp_dir().to_string_lossy().into_owned());
        assert!(profile.clone().normalized().is_ok());

        profile.working_directory = Some("definitely-not-a-real-kite-directory".to_string());
        assert!(profile.normalized().is_err());
    }
}
