//! Snapshot-based profile helpers used by the existing native settings UIs.
//!
//! `AgentProfileStore` remains the canonical persisted store. These adapters
//! expose one immutable snapshot to the UI and atomically replace that snapshot
//! after an edit, without defining a second profile model.

use std::path::PathBuf;

use super::{AgentConfigWarning, AgentProfileSnapshot, AgentProfileStore};

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentStoreLoad {
    pub store: AgentProfileSnapshot,
    pub config_path: String,
    pub warning: Option<String>,
    pub recovered_from_backup: bool,
}

pub fn agent_config_path() -> PathBuf {
    AgentProfileStore::new()
        .config_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("agent.json"))
}

pub fn load_agent_profiles() -> AgentStoreLoad {
    let store = AgentProfileStore::new();
    let snapshot = store.snapshot();
    AgentStoreLoad {
        warning: snapshot.warning.as_ref().map(format_warning),
        store: snapshot,
        config_path: store.config_path().unwrap_or_default(),
        recovered_from_backup: false,
    }
}

pub fn save_agent_profiles(snapshot: AgentProfileSnapshot) -> Result<AgentStoreLoad, String> {
    let store = AgentProfileStore::new();
    let saved = store
        .replace_snapshot(snapshot)
        .map_err(|error| error.to_string())?;
    Ok(AgentStoreLoad {
        store: saved,
        config_path: store.config_path().unwrap_or_default(),
        warning: None,
        recovered_from_backup: false,
    })
}

pub fn default_agent_profile_id() -> Option<String> {
    AgentProfileStore::new().snapshot().default_profile_id
}

fn format_warning(warning: &AgentConfigWarning) -> String {
    match warning {
        AgentConfigWarning::CorruptConfig { message } => {
            format!("Agent 配置损坏，已安全回退：{message}")
        }
        AgentConfigWarning::UnknownSchemaVersion { found, supported } => {
            format!("Agent 配置版本 {found} 不受支持；当前支持版本为 {supported}")
        }
        AgentConfigWarning::ConfigTooLarge { bytes, max_bytes } => {
            format!("Agent 配置过大（{bytes} 字节，上限 {max_bytes} 字节）")
        }
        AgentConfigWarning::InvalidProfile { field, message } => {
            format!("Agent 配置字段 {field} 无效：{message}")
        }
        AgentConfigWarning::StorageUnavailable { message } => {
            format!("Agent 配置存储不可用：{message}")
        }
        AgentConfigWarning::Io { message } => format!("Agent 配置读写失败：{message}"),
        AgentConfigWarning::MigrationApplied {
            from_version,
            to_version,
        } => format!("Agent 配置已从版本 {from_version} 迁移到 {to_version}"),
    }
}
