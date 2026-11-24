//! Adapter from the verified preset catalog to the AgentProfile shape used by
//! the delivered #157 branch.

use crate::agent::presets::{AgentPresetPermission, AgentPresetTransport, preset_spec};
use crate::agent::{
    AgentEnvVar, AgentPermissionMode, AgentProfile, AgentTransport, AgentWorkingDirectoryPolicy,
};

pub fn preset_profile(preset_id: &str, profile_id: String) -> Option<AgentProfile> {
    let preset = preset_spec(preset_id)?;
    Some(AgentProfile {
        id: profile_id,
        name: preset.name.clone(),
        preset_id: Some(preset.id),
        transport: match preset.transport {
            AgentPresetTransport::AcpStdio => AgentTransport::AcpStdio,
        },
        command: preset.executable,
        args: preset.arguments,
        env: Vec::<AgentEnvVar>::new(),
        model: None,
        working_directory_policy: AgentWorkingDirectoryPolicy::SelectedItem,
        custom_working_directory: None,
        permission_mode: match preset.permission {
            AgentPresetPermission::ReadOnly => AgentPermissionMode::ReadOnly,
            AgentPresetPermission::AgentManaged => AgentPermissionMode::AgentManaged,
        },
        startup_timeout_ms: 20_000,
        idle_timeout_ms: 120_000,
        max_output_bytes: 2 * 1024 * 1024,
        credential_ref: None,
        enabled: true,
    })
}

pub fn apply_preset_to_existing(
    preset_id: &str,
    mut profile: AgentProfile,
) -> Option<AgentProfile> {
    let preset = preset_spec(preset_id)?;
    if profile.command.trim().is_empty() {
        profile.command = preset.executable;
        profile.preset_id = Some(preset.id.clone());
    }
    if profile.args.is_empty() {
        profile.args = preset.arguments;
        profile.preset_id.get_or_insert(preset.id);
    }
    Some(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_profile_keeps_auth_fields_empty() {
        let profile =
            preset_profile("claude-code-acp", "claude-profile".to_string()).expect("profile");
        assert_eq!(profile.command, "claude-code-acp");
        assert_eq!(profile.preset_id.as_deref(), Some("claude-code-acp"));
        assert!(profile.env.is_empty());
        assert!(profile.model.is_none());
        assert!(profile.credential_ref.is_none());
    }

    #[test]
    fn applying_preset_preserves_existing_connection_configuration() {
        let mut profile =
            preset_profile("claude-code-acp", "profile".to_string()).expect("profile");
        profile.command = "my-wrapper".to_string();
        profile.args = vec!["--custom".to_string()];
        profile.env.push(AgentEnvVar {
            name: "SAFE_SETTING".to_string(),
            value: "1".to_string(),
        });
        let merged = apply_preset_to_existing("codex-acp", profile.clone()).expect("merge");
        assert_eq!(merged.command, profile.command);
        assert_eq!(merged.args, profile.args);
        assert_eq!(merged.env, profile.env);
    }
}
