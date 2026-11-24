//! Verified Agent integration presets.
//!
//! The preset catalog is self-contained so it can be tested without a UI or a
//! running Agent. The canonical profile adapter lives in preset_adapter.rs.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentPresetTransport {
    #[default]
    AcpStdio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentPresetPermission {
    /// Kite can constrain the connection to read-only behavior.
    ReadOnly,
    /// The external Agent owns authentication and permission enforcement.
    #[default]
    AgentManaged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentPresetSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub transport: AgentPresetTransport,
    pub permission: AgentPresetPermission,
    pub install_hint: String,
    pub documentation_url: String,
    pub verified_version: String,
    pub verification_note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub transport: AgentPresetTransport,
    pub permission: AgentPresetPermission,
    pub install_hint: String,
    pub documentation_url: String,
    pub verified_version: String,
    pub verification_note: String,
}

impl From<AgentPresetSpec> for AgentPreset {
    fn from(preset: AgentPresetSpec) -> Self {
        Self {
            id: preset.id,
            name: preset.name,
            description: preset.description,
            executable: preset.executable,
            arguments: preset.arguments,
            transport: preset.transport,
            permission: preset.permission,
            install_hint: preset.install_hint,
            documentation_url: preset.documentation_url,
            verified_version: preset.verified_version,
            verification_note: preset.verification_note,
        }
    }
}

/// Presets with a documented ACP stdio entry point and a pinned adapter
/// version. Authentication and Agent-native configuration are never included.
pub fn preset_specs() -> Vec<AgentPresetSpec> {
    vec![
        AgentPresetSpec {
            id: "claude-code-acp".to_string(),
            name: "Claude Code".to_string(),
            description: "通过 Zed 维护的 ACP 适配器接入 Claude Code。".to_string(),
            executable: "claude-code-acp".to_string(),
            arguments: Vec::new(),
            transport: AgentPresetTransport::AcpStdio,
            permission: AgentPresetPermission::AgentManaged,
            install_hint: "npm install -g @zed-industries/claude-code-acp@0.16.2".to_string(),
            documentation_url: "https://www.npmjs.com/package/@zed-industries/claude-code-acp"
                .to_string(),
            verified_version: "0.16.2".to_string(),
            verification_note:
                "测试连接会执行 ACP initialize 握手；登录、额度和写入权限由 Claude Code 管理。"
                    .to_string(),
        },
        AgentPresetSpec {
            id: "codex-acp".to_string(),
            name: "Codex".to_string(),
            description: "通过 Zed 维护的官方 ACP 适配器接入 Codex。".to_string(),
            executable: "codex-acp".to_string(),
            arguments: Vec::new(),
            transport: AgentPresetTransport::AcpStdio,
            permission: AgentPresetPermission::AgentManaged,
            install_hint: "npm install -g @zed-industries/codex-acp@0.16.0".to_string(),
            documentation_url: "https://www.npmjs.com/package/@zed-industries/codex-acp"
                .to_string(),
            verified_version: "0.16.0".to_string(),
            verification_note:
                "测试连接会执行 ACP initialize 握手；登录、额度和写入权限由 Codex 管理。"
                    .to_string(),
        },
    ]
}

pub fn preset_spec(id: &str) -> Option<AgentPresetSpec> {
    preset_specs().into_iter().find(|preset| preset.id == id)
}

pub fn agent_presets() -> Vec<AgentPreset> {
    preset_specs().into_iter().map(AgentPreset::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_presets_are_stable_and_do_not_embed_credentials() {
        let presets = preset_specs();
        assert!((2..=3).contains(&presets.len()));
        let mut ids = presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
        for preset in presets {
            assert!(!preset.executable.trim().is_empty());
            assert!(!preset.install_hint.trim().is_empty());
            assert!(!preset.verified_version.trim().is_empty());
            assert!(!preset.verification_note.trim().is_empty());
            assert!(!preset.documentation_url.contains("token="));
            assert!(!preset.arguments.iter().any(|arg| arg.contains("sk-")));
        }
    }

    #[test]
    fn lookup_by_id_returns_the_same_preset() {
        for preset in preset_specs() {
            assert_eq!(preset_spec(&preset.id), Some(preset));
        }
        assert!(preset_spec("unverified-agent").is_none());
    }
}
