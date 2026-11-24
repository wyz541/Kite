//! Adapter from the verified preset catalog to the canonical `AgentProfile`.

use crate::agent::presets::{AgentPresetPermission, AgentPresetTransport, preset_spec};
use crate::agent::{AgentProfile, AgentTransport, PermissionPolicy, WorkingDirectoryPolicy};

pub fn preset_profile(preset_id: &str, profile_id: String) -> Option<AgentProfile> {
    let preset = preset_spec(preset_id)?;
    Some(AgentProfile {
        id: profile_id,
        name: preset.name.clone(),
        transport: match preset.transport {
            AgentPresetTransport::AcpStdio => AgentTransport::AcpStdio,
        },
        executable: Some(preset.executable),
        endpoint: None,
        arguments: preset.arguments,
        environment: Vec::new(),
        model: None,
        working_directory: WorkingDirectoryPolicy::SelectedItem,
        permission: match preset.permission {
            AgentPresetPermission::ReadOnly => PermissionPolicy::ReadOnly,
            AgentPresetPermission::AgentManaged => PermissionPolicy::AgentManaged,
        },
        timeout_seconds: 120,
        output_limit_bytes: 2 * 1024 * 1024,
        credential_ref: None,
        capabilities: Default::default(),
    })
}

pub fn apply_preset_to_existing(
    preset_id: &str,
    mut profile: AgentProfile,
) -> Option<AgentProfile> {
    let preset = preset_spec(preset_id)?;
    if profile
        .executable
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        profile.executable = Some(preset.executable);
    }
    if profile.arguments.is_empty() {
        profile.arguments = preset.arguments;
    }
    Some(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentEnvironmentVariable;

    #[test]
    fn preset_profile_keeps_auth_fields_empty() {
        let profile =
            preset_profile("claude-code-acp", "claude-profile".to_string()).expect("profile");
        assert_eq!(profile.executable.as_deref(), Some("claude-code-acp"));
        assert!(profile.environment.is_empty());
        assert!(profile.model.is_none());
        assert!(profile.credential_ref.is_none());
    }

    #[test]
    fn applying_preset_preserves_existing_connection_configuration() {
        let mut profile =
            preset_profile("claude-code-acp", "profile".to_string()).expect("profile");
        profile.executable = Some("my-wrapper".to_string());
        profile.arguments = vec!["--custom".to_string()];
        profile.environment.push(AgentEnvironmentVariable {
            name: "SAFE_SETTING".to_string(),
            value: "1".to_string(),
        });
        let merged = apply_preset_to_existing("codex-acp", profile.clone()).expect("merge");
        assert_eq!(merged.executable, profile.executable);
        assert_eq!(merged.arguments, profile.arguments);
        assert_eq!(merged.environment, profile.environment);
    }
}
