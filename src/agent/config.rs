use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

pub const AGENT_CONFIG_SCHEMA_VERSION: u32 = 1;
const MAX_AGENT_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_AGENT_PROFILES: usize = 64;
const MAX_AGENT_ARGS: usize = 64;
const MAX_AGENT_ENV: usize = 64;
const MIN_AGENT_TIMEOUT_MS: u32 = 1_000;
const MAX_AGENT_TIMEOUT_MS: u32 = 30 * 60 * 1_000;
const MIN_AGENT_OUTPUT_BYTES: u64 = 16 * 1024;
const MAX_AGENT_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

static SAVE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    AcpStdio,
    Cli,
}

impl AgentTransport {
    pub fn label(self) -> &'static str {
        match self {
            Self::AcpStdio => "ACP / JSON-RPC (stdio)",
            Self::Cli => "CLI（权限由 Agent 管理）",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkingDirectoryPolicy {
    #[default]
    SelectedItem,
    SelectedParent,
    RepositoryRoot,
    Custom,
}

impl AgentWorkingDirectoryPolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::SelectedItem => "选中目录或仓库根目录",
            Self::SelectedParent => "文件所在目录",
            Self::RepositoryRoot => "Git 仓库根目录",
            Self::Custom => "自定义目录",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermissionMode {
    #[default]
    ReadOnly,
    WorkspaceWrite,
    AgentManaged,
}

impl AgentPermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "只读",
            Self::WorkspaceWrite => "工作区写入需确认",
            Self::AgentManaged => "由 Agent 自身管理",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentEnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub preset_id: Option<String>,
    pub transport: AgentTransport,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<AgentEnvVar>,
    pub model: Option<String>,
    pub working_directory_policy: AgentWorkingDirectoryPolicy,
    pub custom_working_directory: Option<String>,
    pub permission_mode: AgentPermissionMode,
    pub startup_timeout_ms: u32,
    pub idle_timeout_ms: u32,
    pub max_output_bytes: u64,
    pub credential_ref: Option<String>,
    pub enabled: bool,
}

impl Default for AgentProfile {
    fn default() -> Self {
        Self {
            id: "custom-agent".to_string(),
            name: "自定义 Agent".to_string(),
            preset_id: None,
            transport: AgentTransport::AcpStdio,
            command: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            model: None,
            working_directory_policy: AgentWorkingDirectoryPolicy::SelectedItem,
            custom_working_directory: None,
            permission_mode: AgentPermissionMode::ReadOnly,
            startup_timeout_ms: 20_000,
            idle_timeout_ms: 120_000,
            max_output_bytes: 2 * 1024 * 1024,
            credential_ref: None,
            enabled: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentProfileStore {
    pub schema_version: u32,
    pub default_profile_id: Option<String>,
    pub profiles: Vec<AgentProfile>,
}

impl Default for AgentProfileStore {
    fn default() -> Self {
        Self {
            schema_version: AGENT_CONFIG_SCHEMA_VERSION,
            default_profile_id: None,
            profiles: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentStoreLoad {
    pub store: AgentProfileStore,
    pub config_path: String,
    pub warning: Option<String>,
    pub recovered_from_backup: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
    pub install_hint: String,
    pub documentation_url: String,
    pub verified_version: String,
}

pub fn agent_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("KITE_AGENT_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Kite")
        .join("agent.json")
}

pub fn agent_presets() -> Vec<AgentPreset> {
    vec![
        AgentPreset {
            id: "claude-code-acp".to_string(),
            name: "Claude Code".to_string(),
            description: "通过 Zed 的官方 ACP adapter 使用已登录的 Claude Code。".to_string(),
            command: if cfg!(windows) {
                "claude-code-acp.cmd".to_string()
            } else {
                "claude-code-acp".to_string()
            },
            args: Vec::new(),
            install_hint: "npm install -g @zed-industries/claude-code-acp@0.16.2".to_string(),
            documentation_url: "https://www.npmjs.com/package/@zed-industries/claude-code-acp"
                .to_string(),
            verified_version: "0.16.2".to_string(),
        },
        AgentPreset {
            id: "codex-acp".to_string(),
            name: "Codex".to_string(),
            description: "通过 Zed 的官方 ACP adapter 使用已登录的 Codex。".to_string(),
            command: if cfg!(windows) {
                "codex-acp.cmd".to_string()
            } else {
                "codex-acp".to_string()
            },
            args: Vec::new(),
            install_hint: "npm install -g @zed-industries/codex-acp@0.16.0".to_string(),
            documentation_url: "https://www.npmjs.com/package/@zed-industries/codex-acp"
                .to_string(),
            verified_version: "0.16.0".to_string(),
        },
    ]
}

pub fn preset_profile(preset_id: &str) -> Option<AgentProfile> {
    let preset = agent_presets()
        .into_iter()
        .find(|preset| preset.id == preset_id)?;
    Some(AgentProfile {
        id: preset.id.clone(),
        name: preset.name,
        preset_id: Some(preset.id),
        transport: AgentTransport::AcpStdio,
        command: preset.command,
        args: preset.args,
        env: Vec::new(),
        model: None,
        working_directory_policy: AgentWorkingDirectoryPolicy::SelectedItem,
        custom_working_directory: None,
        permission_mode: AgentPermissionMode::ReadOnly,
        startup_timeout_ms: 20_000,
        idle_timeout_ms: 120_000,
        max_output_bytes: 2 * 1024 * 1024,
        credential_ref: None,
        enabled: true,
    })
}

pub fn load_agent_profiles() -> AgentStoreLoad {
    load_agent_profiles_at(&agent_config_path())
}

pub fn default_agent_profile_id() -> Option<String> {
    let load = load_agent_profiles();
    load.store.default_profile_id
}

pub fn save_agent_profiles(store: AgentProfileStore) -> Result<AgentStoreLoad, String> {
    save_agent_profiles_at(&agent_config_path(), store)
}

pub fn load_agent_profiles_at(path: &Path) -> AgentStoreLoad {
    let mut warning = None;
    let mut recovered_from_backup = false;
    let primary = load_agent_store_file(path, &mut warning);
    let candidate = match primary {
        Some(store) => Some(store),
        None => {
            recovered_from_backup = path.exists();
            load_agent_store_file(&backup_path(path), &mut warning)
        }
    };
    let store = candidate.unwrap_or_default();
    AgentStoreLoad {
        store,
        config_path: path.to_string_lossy().into_owned(),
        warning,
        recovered_from_backup,
    }
}

pub fn save_agent_profiles_at(
    path: &Path,
    store: AgentProfileStore,
) -> Result<AgentStoreLoad, String> {
    let _guard = SAVE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Agent 配置保存锁不可用".to_string())?;
    let store = normalize_store(store)?;
    let bytes = serde_json::to_vec_pretty(&store)
        .map_err(|error| format!("无法序列化 Agent 配置：{error}"))?;
    if bytes.len() as u64 > MAX_AGENT_CONFIG_BYTES {
        return Err("Agent 配置超过 512 KiB 上限".to_string());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 Agent 配置目录：{error}"))?;
    }
    let temporary = temporary_path(path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("无法创建 Agent 临时配置：{error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("无法写入 Agent 临时配置：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法同步 Agent 临时配置：{error}"))?;
    }
    let backup = backup_path(path);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| format!("无法替换 Agent 配置备份：{error}"))?;
        }
        fs::rename(path, &backup).map_err(|error| format!("无法轮换 Agent 配置：{error}"))?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() && !path.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法替换 Agent 配置：{error}"));
    }
    Ok(AgentStoreLoad {
        store,
        config_path: path.to_string_lossy().into_owned(),
        warning: None,
        recovered_from_backup: false,
    })
}

pub fn validate_agent_profile(profile: &AgentProfile) -> Result<(), String> {
    let name = profile.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("Agent 名称必须为 1–64 个字符".to_string());
    }
    if profile.id.trim().is_empty() || profile.id.chars().count() > 80 {
        return Err("Agent 标识无效".to_string());
    }
    if !profile
        .id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err("Agent 标识只能包含 ASCII 字母、数字、点、短横线和下划线".to_string());
    }
    if profile.command.trim().is_empty() {
        return Err("Agent 命令不能为空".to_string());
    }
    if profile.command.len() > 4_096 {
        return Err("Agent 命令过长".to_string());
    }
    if profile.args.len() > MAX_AGENT_ARGS || profile.args.iter().any(|arg| arg.len() > 4_096) {
        return Err("Agent 参数数量或长度超过限制".to_string());
    }
    if profile.env.len() > MAX_AGENT_ENV {
        return Err("Agent 环境变量数量超过限制".to_string());
    }
    let mut names = BTreeSet::new();
    for env in &profile.env {
        let name = env.name.trim();
        if !valid_env_name(name) {
            return Err(format!("环境变量名称无效：{}", env.name));
        }
        if !names.insert(name.to_ascii_uppercase()) {
            return Err(format!("环境变量重复：{name}"));
        }
        if sensitive_env_name(name) && !env.value.trim().is_empty() {
            return Err(format!(
                "环境变量 {name} 可能包含凭据；请使用凭据引用或 Agent 原生登录，不要写入 agent.json"
            ));
        }
    }
    if profile.working_directory_policy == AgentWorkingDirectoryPolicy::Custom
        && profile
            .custom_working_directory
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err("自定义工作目录不能为空".to_string());
    }
    if !(MIN_AGENT_TIMEOUT_MS..=MAX_AGENT_TIMEOUT_MS).contains(&profile.startup_timeout_ms) {
        return Err("启动超时必须介于 1 秒和 30 分钟之间".to_string());
    }
    if !(MIN_AGENT_TIMEOUT_MS..=MAX_AGENT_TIMEOUT_MS).contains(&profile.idle_timeout_ms) {
        return Err("空闲超时必须介于 1 秒和 30 分钟之间".to_string());
    }
    if !(MIN_AGENT_OUTPUT_BYTES..=MAX_AGENT_OUTPUT_BYTES).contains(&profile.max_output_bytes) {
        return Err("输出上限必须介于 16 KiB 和 64 MiB 之间".to_string());
    }
    if profile.permission_mode == AgentPermissionMode::ReadOnly
        && profile.transport == AgentTransport::Cli
    {
        return Err("普通 CLI 无法由 Kite 强制只读，请将权限模式设为由 Agent 自身管理".to_string());
    }
    if profile
        .credential_ref
        .as_deref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > 512)
    {
        return Err("凭据引用无效".to_string());
    }
    let serialized =
        serde_json::to_string(profile).map_err(|error| format!("无法检查 Agent 配置：{error}"))?;
    if contains_probable_secret(&serialized) {
        return Err("Agent 配置疑似包含明文凭据；请改用系统登录或凭据引用".to_string());
    }
    Ok(())
}

fn load_agent_store_file(path: &Path, warning: &mut Option<String>) -> Option<AgentProfileStore> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_AGENT_CONFIG_BYTES {
        *warning = Some("Agent 配置文件过大，已忽略".to_string());
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            *warning = Some(format!("Agent 配置损坏，已回退到安全默认值：{error}"));
            return None;
        }
    };
    let version = value.get("schema_version").and_then(|value| value.as_u64());
    if version != Some(AGENT_CONFIG_SCHEMA_VERSION as u64) {
        *warning = Some(format!(
            "Agent 配置 schema_version 不受支持：{}",
            version.map_or_else(|| "missing".to_string(), |value| value.to_string())
        ));
        return None;
    }
    match serde_json::from_value::<AgentProfileStore>(value)
        .map_err(|error| error.to_string())
        .and_then(normalize_store)
    {
        Ok(store) => Some(store),
        Err(error) => {
            *warning = Some(format!("Agent 配置校验失败，已回退到安全默认值：{error}"));
            None
        }
    }
}

fn normalize_store(mut store: AgentProfileStore) -> Result<AgentProfileStore, String> {
    if store.schema_version != AGENT_CONFIG_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 Agent schema_version：{}",
            store.schema_version
        ));
    }
    if store.profiles.len() > MAX_AGENT_PROFILES {
        return Err("Agent profile 数量超过 64 个".to_string());
    }
    let mut seen = BTreeSet::new();
    let mut profiles = Vec::with_capacity(store.profiles.len());
    for mut profile in store.profiles {
        profile.name = profile.name.trim().to_string();
        profile.command = profile.command.trim().to_string();
        profile.model = profile
            .model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        profile.credential_ref = profile
            .credential_ref
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        validate_agent_profile(&profile)?;
        if !seen.insert(profile.id.to_ascii_lowercase()) {
            return Err(format!("Agent 标识重复：{}", profile.id));
        }
        profiles.push(profile);
    }
    let default_profile_id = store
        .default_profile_id
        .filter(|id| profiles.iter().any(|profile| profile.id == *id))
        .or_else(|| profiles.first().map(|profile| profile.id.clone()));
    store.profiles = profiles;
    store.default_profile_id = default_profile_id;
    Ok(store)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "CREDENTIAL",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn contains_probable_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("sk-ant-")
        || lower.contains("sk-proj-")
        || lower.contains("anthropic_api_key=")
        || lower.contains("openai_api_key=")
        || lower.contains("authorization: bearer ")
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "kite-agent-{label}-{}-{unique}",
                std::process::id()
            ))
            .join("agent.json")
    }

    fn valid_profile() -> AgentProfile {
        AgentProfile {
            id: "test-agent".to_string(),
            name: "Test Agent".to_string(),
            command: "kite-agent-fixture".to_string(),
            ..AgentProfile::default()
        }
    }

    #[test]
    fn preset_profiles_are_valid_and_do_not_contain_secrets() {
        for preset in agent_presets() {
            let profile = preset_profile(&preset.id).expect("preset profile");
            validate_agent_profile(&profile).expect("valid preset");
            assert!(profile.env.is_empty());
            assert!(profile.credential_ref.is_none());
        }
    }

    #[test]
    fn store_round_trips_with_default_selection() {
        let path = temp_path("roundtrip");
        let store = AgentProfileStore {
            schema_version: AGENT_CONFIG_SCHEMA_VERSION,
            default_profile_id: Some("test-agent".to_string()),
            profiles: vec![valid_profile()],
        };
        save_agent_profiles_at(&path, store.clone()).expect("save");
        let loaded = load_agent_profiles_at(&path);
        assert_eq!(loaded.store, store);
        assert!(loaded.warning.is_none());
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn corrupt_primary_falls_back_to_backup() {
        let path = temp_path("backup");
        let first = AgentProfileStore {
            schema_version: AGENT_CONFIG_SCHEMA_VERSION,
            default_profile_id: Some("test-agent".to_string()),
            profiles: vec![valid_profile()],
        };
        save_agent_profiles_at(&path, first.clone()).expect("first save");
        let mut second = first.clone();
        second.profiles[0].name = "Updated".to_string();
        save_agent_profiles_at(&path, second).expect("second save");
        fs::write(&path, b"not-json").expect("corrupt primary");
        let loaded = load_agent_profiles_at(&path);
        assert!(loaded.recovered_from_backup);
        assert_eq!(loaded.store.profiles[0].name, "Test Agent");
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn rejects_plaintext_sensitive_environment_values() {
        let mut profile = valid_profile();
        profile.env.push(AgentEnvVar {
            name: "OPENAI_API_KEY".to_string(),
            value: "sk-proj-secret".to_string(),
        });
        assert!(validate_agent_profile(&profile).is_err());
    }

    #[test]
    fn rejects_plaintext_tokens_in_arguments() {
        let mut profile = valid_profile();
        profile
            .args
            .push("OPENAI_API_KEY=sk-proj-secret".to_string());
        assert!(validate_agent_profile(&profile).is_err());
    }
}
