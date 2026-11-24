use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::{Deserialize, Serialize};

use super::credentials;
use super::model::{
    AGENT_CONFIG_SCHEMA_VERSION, AgentProfile, MAX_AGENT_PROFILES, normalize_optional,
};

const MAX_AGENT_CONFIG_BYTES: u64 = 1024 * 1024;

static STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentConfig {
    pub schema_version: u32,
    pub default_profile_id: Option<String>,
    pub profiles: Vec<AgentProfile>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            schema_version: AGENT_CONFIG_SCHEMA_VERSION,
            default_profile_id: None,
            profiles: Vec::new(),
        }
    }
}

impl AgentConfig {
    pub fn profile(&self, id: &str) -> Option<&AgentProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn default_profile(&self) -> Option<&AgentProfile> {
        self.default_profile_id
            .as_deref()
            .and_then(|id| self.profile(id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentStoreSnapshot {
    pub config: AgentConfig,
    pub warning: Option<String>,
}

pub fn agent_config_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("KITE_AGENT_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }
    dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .map(|path| path.join("Kite").join("agent.json"))
        .ok_or_else(|| "无法确定 Agent 配置目录".to_string())
}

pub fn load_agent_config() -> AgentStoreSnapshot {
    agent_config_path()
        .map(|path| load_agent_config_at(&path))
        .unwrap_or_else(|error| AgentStoreSnapshot {
            config: AgentConfig::default(),
            warning: Some(error),
        })
}

pub fn save_agent_profile(
    profile: AgentProfile,
    credential: Option<String>,
    clear_credential: bool,
) -> Result<AgentConfig, String> {
    let path = agent_config_path()?;
    let _guard = store_lock()?;
    let mut snapshot = load_agent_config_at(&path);
    let mut profile = profile.normalized()?;
    let existing = snapshot.config.profile(&profile.id).cloned();
    let old_secret = existing
        .as_ref()
        .filter(|existing| existing.credential_ref.is_some())
        .map(|_| credentials::load_secret(&profile.id).ok().flatten())
        .unwrap_or(None);

    let new_credential = credential.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    let previous_credential_ref = existing
        .as_ref()
        .and_then(|existing| existing.credential_ref.clone());
    let mut installed_secret = false;
    if let Some(secret) = new_credential.as_deref() {
        profile.credential_ref = Some(credentials::save_secret(&profile.id, secret)?);
        installed_secret = true;
        if profile.credential_env_var.is_none() {
            profile.credential_env_var = Some("KITE_AGENT_TOKEN".to_string());
        }
    } else if clear_credential {
        profile.credential_ref = None;
    } else {
        profile.credential_ref = previous_credential_ref;
    }

    if existing.is_none() && snapshot.config.profiles.len() >= MAX_AGENT_PROFILES {
        if installed_secret {
            let _ = credentials::remove_secret(&profile.id);
        }
        return Err(format!("最多只能配置 {MAX_AGENT_PROFILES} 个 Agent"));
    }
    if let Some(duplicate) = snapshot.config.profiles.iter().find(|candidate| {
        candidate.id != profile.id && candidate.name.eq_ignore_ascii_case(&profile.name)
    }) {
        if installed_secret {
            restore_previous_secret(&profile.id, old_secret.as_deref());
        }
        return Err(format!("Agent 名称已存在：{}", duplicate.name));
    }

    match snapshot
        .config
        .profiles
        .iter_mut()
        .find(|candidate| candidate.id == profile.id)
    {
        Some(existing_profile) => *existing_profile = profile.clone(),
        None => snapshot.config.profiles.push(profile.clone()),
    }
    if snapshot.config.default_profile_id.is_none() {
        snapshot.config.default_profile_id = Some(profile.id.clone());
    }
    normalize_default(&mut snapshot.config);

    if let Err(error) = save_agent_config_at(&path, &snapshot.config) {
        if installed_secret {
            restore_previous_secret(&profile.id, old_secret.as_deref());
        }
        return Err(error);
    }
    if clear_credential && new_credential.is_none() {
        let _ = credentials::remove_secret(&profile.id);
    }
    Ok(snapshot.config)
}

pub fn delete_agent_profile(
    profile_id: &str,
    replacement_default_id: Option<String>,
) -> Result<AgentConfig, String> {
    let path = agent_config_path()?;
    let _guard = store_lock()?;
    let mut snapshot = load_agent_config_at(&path);
    let removed =
        remove_profile_from_config(&mut snapshot.config, profile_id, replacement_default_id)?;
    save_agent_config_at(&path, &snapshot.config)?;
    if removed.credential_ref.is_some() {
        let _ = credentials::remove_secret(profile_id);
    }
    Ok(snapshot.config)
}

pub fn set_default_agent_profile(profile_id: &str) -> Result<AgentConfig, String> {
    let path = agent_config_path()?;
    let _guard = store_lock()?;
    let mut snapshot = load_agent_config_at(&path);
    if !snapshot
        .config
        .profiles
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return Err("Agent 配置不存在".to_string());
    }
    snapshot.config.default_profile_id = Some(profile_id.to_string());
    save_agent_config_at(&path, &snapshot.config)?;
    Ok(snapshot.config)
}

pub fn duplicate_agent_profile(profile_id: &str) -> Result<AgentConfig, String> {
    let path = agent_config_path()?;
    let _guard = store_lock()?;
    let mut snapshot = load_agent_config_at(&path);
    let source = snapshot
        .config
        .profile(profile_id)
        .cloned()
        .ok_or_else(|| "Agent 配置不存在".to_string())?;
    if snapshot.config.profiles.len() >= MAX_AGENT_PROFILES {
        return Err(format!("最多只能配置 {MAX_AGENT_PROFILES} 个 Agent"));
    }
    let mut number = 2;
    let mut name = format!("{} 副本", source.name);
    while snapshot
        .config
        .profiles
        .iter()
        .any(|profile| profile.name.eq_ignore_ascii_case(&name))
    {
        name = format!("{} 副本 {}", source.name, number);
        number += 1;
    }
    let duplicate = source.duplicate_with_name(&name).normalized()?;
    snapshot.config.profiles.push(duplicate);
    save_agent_config_at(&path, &snapshot.config)?;
    Ok(snapshot.config)
}

pub fn resolve_credential(
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

fn remove_profile_from_config(
    config: &mut AgentConfig,
    profile_id: &str,
    replacement_default_id: Option<String>,
) -> Result<AgentProfile, String> {
    let Some(index) = config
        .profiles
        .iter()
        .position(|profile| profile.id == profile_id)
    else {
        return Err("Agent 配置不存在".to_string());
    };
    let was_default = config.default_profile_id.as_deref() == Some(profile_id);
    if was_default && config.profiles.len() > 1 {
        let replacement = normalize_optional(replacement_default_id)
            .ok_or_else(|| "删除默认 Agent 前必须选择新的默认 Agent".to_string())?;
        if replacement == profile_id
            || !config
                .profiles
                .iter()
                .any(|profile| profile.id == replacement)
        {
            return Err("新的默认 Agent 无效".to_string());
        }
        config.default_profile_id = Some(replacement);
    }
    let removed = config.profiles.remove(index);
    if was_default && config.profiles.is_empty() {
        config.default_profile_id = None;
    }
    normalize_default(config);
    Ok(removed)
}

fn store_lock() -> Result<MutexGuard<'static, ()>, String> {
    STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Agent 配置写入锁不可用".to_string())
}

fn restore_previous_secret(profile_id: &str, previous_secret: Option<&str>) {
    if let Some(secret) = previous_secret {
        let _ = credentials::save_secret(profile_id, secret);
    } else {
        let _ = credentials::remove_secret(profile_id);
    }
}

fn normalize_default(config: &mut AgentConfig) {
    config.schema_version = AGENT_CONFIG_SCHEMA_VERSION;
    if let Some(default_id) = config.default_profile_id.as_deref()
        && config
            .profiles
            .iter()
            .any(|profile| profile.id == default_id)
    {
        return;
    }
    config.default_profile_id = config.profiles.first().map(|profile| profile.id.clone());
}

pub fn load_agent_config_at(path: &Path) -> AgentStoreSnapshot {
    match load_config_file(path) {
        Ok(config) => AgentStoreSnapshot {
            config,
            warning: None,
        },
        Err(primary_error) => match load_config_file(&backup_path(path)) {
            Ok(config) => AgentStoreSnapshot {
                config,
                warning: Some(format!("Agent 主配置损坏，已读取备份：{primary_error}")),
            },
            Err(_) if !path.exists() => AgentStoreSnapshot {
                config: AgentConfig::default(),
                warning: None,
            },
            Err(_) => AgentStoreSnapshot {
                config: AgentConfig::default(),
                warning: Some(format!("Agent 配置损坏，已使用空配置：{primary_error}")),
            },
        },
    }
}

pub fn save_agent_config_at(path: &Path, config: &AgentConfig) -> Result<(), String> {
    let mut config = config.clone();
    config.schema_version = AGENT_CONFIG_SCHEMA_VERSION;
    normalize_config(&mut config)?;
    let bytes = serde_json::to_vec_pretty(&config)
        .map_err(|error| format!("无法序列化 Agent 配置：{error}"))?;
    if bytes.len() as u64 > MAX_AGENT_CONFIG_BYTES {
        return Err("Agent 配置超过 1 MiB，请精简环境变量或启动参数".to_string());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 Agent 配置目录：{error}"))?;
    }
    if load_config_file(path).is_ok() {
        let backup = backup_path(path);
        fs::copy(path, &backup).map_err(|error| format!("无法备份 Agent 配置：{error}"))?;
        secure_file_permissions(&backup)?;
    }
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(&bytes))
        .map_err(|error| format!("无法保存 Agent 配置：{error}"))?;
    secure_file_permissions(path)?;
    Ok(())
}

fn load_config_file(path: &Path) -> Result<AgentConfig, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("读取配置失败：{error}"))?;
    if !metadata.is_file() {
        return Err("配置路径不是文件".to_string());
    }
    if metadata.len() > MAX_AGENT_CONFIG_BYTES {
        return Err("Agent 配置超过 1 MiB".to_string());
    }
    let bytes = fs::read(path).map_err(|error| format!("读取配置失败：{error}"))?;
    let mut config = serde_json::from_slice::<AgentConfig>(&bytes)
        .map_err(|error| format!("JSON 配置无效：{error}"))?;
    if config.schema_version != AGENT_CONFIG_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 Agent 配置版本：{}（当前为 {}）",
            config.schema_version, AGENT_CONFIG_SCHEMA_VERSION
        ));
    }
    normalize_config(&mut config)?;
    Ok(config)
}

fn normalize_config(config: &mut AgentConfig) -> Result<(), String> {
    if config.profiles.len() > MAX_AGENT_PROFILES {
        return Err(format!("Agent 配置超过 {MAX_AGENT_PROFILES} 项"));
    }
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for profile in &config.profiles {
        let normalized = profile.clone().normalized()?;
        if normalized != *profile {
            return Err(format!("Agent 配置字段未规范化：{}", profile.name));
        }
        if !ids.insert(profile.id.clone()) {
            return Err(format!("Agent 标识重复：{}", profile.id));
        }
        let name = profile.name.to_ascii_lowercase();
        if !names.insert(name) {
            return Err(format!("Agent 名称重复：{}", profile.name));
        }
    }
    normalize_default(config);
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(unix)]
fn secure_file_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("无法设置 Agent 配置权限：{error}"))
}

#[cfg(not(unix))]
fn secure_file_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "kite-agent-store-{}-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn profile(name: &str) -> AgentProfile {
        AgentProfile {
            name: name.to_string(),
            executable: "agent".to_string(),
            ..AgentProfile::default()
        }
        .normalized()
        .expect("valid profile")
    }

    #[test]
    fn saves_and_loads_profiles_atomically() {
        let path = test_path();
        let mut config = AgentConfig::default();
        config.profiles.push(profile("One"));
        config.profiles.push(profile("Two"));
        normalize_default(&mut config);
        save_agent_config_at(&path, &config).expect("save config");
        let loaded = load_agent_config_at(&path);
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.config.profiles.len(), 2);
        assert_eq!(
            loaded.config.default_profile_id.as_deref(),
            Some(config.profiles[0].id.as_str())
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(backup_path(&path));
    }

    #[test]
    fn corrupt_primary_falls_back_to_backup() {
        let path = test_path();
        let mut config = AgentConfig::default();
        config.profiles.push(profile("Backup"));
        save_agent_config_at(&path, &config).expect("save initial");
        fs::copy(&path, backup_path(&path)).expect("copy backup");
        fs::write(&path, b"not json").expect("corrupt primary");
        let loaded = load_agent_config_at(&path);
        assert!(loaded.warning.is_some());
        assert_eq!(loaded.config.profiles[0].name, "Backup");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(backup_path(&path));
    }

    #[test]
    fn unknown_schema_falls_back_to_empty_config() {
        let path = test_path();
        fs::write(
            &path,
            br#"{"schema_version":999,"default_profile_id":null,"profiles":[]}"#,
        )
        .expect("write config");
        let loaded = load_agent_config_at(&path);
        assert!(loaded.warning.unwrap().contains("不支持"));
        assert!(loaded.config.profiles.is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn deleting_the_default_requires_and_applies_a_replacement() {
        let mut config = AgentConfig::default();
        let first = profile("One");
        let second = profile("Two");
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        config.profiles.push(first);
        config.profiles.push(second);
        normalize_default(&mut config);

        assert!(
            remove_profile_from_config(&mut config, &first_id, None)
                .unwrap_err()
                .contains("新的默认")
        );
        remove_profile_from_config(&mut config, &first_id, Some(second_id.clone()))
            .expect("delete default with replacement");
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(
            config.default_profile_id.as_deref(),
            Some(second_id.as_str())
        );
    }

    #[test]
    fn rejects_duplicate_names_and_ids() {
        let mut config = AgentConfig::default();
        let first = profile("Same");
        let mut second = profile("same");
        second.id = first.id.clone();
        config.profiles.push(first);
        config.profiles.push(second);
        assert!(normalize_config(&mut config).is_err());
    }

    #[test]
    fn plaintext_secret_never_reaches_serialized_config() {
        let mut profile = profile("Secret test");
        profile
            .environment
            .push(super::super::model::AgentEnvironmentVariable {
                key: "OPENAI_API_KEY".to_string(),
                value: "super-secret-value".to_string(),
            });
        assert!(profile.normalized().is_err());
        let config = AgentConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("super-secret-value"));
    }
}
