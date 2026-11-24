//! Adapter from the integrated Agent connection result to the neutral #157
//! diagnostics presentation records.

use crate::agent::{AgentConnectionInfo, AgentProfile};

use super::capstone_diagnostics::{AgentCapstoneProfileDiagnostic, redact_agent_text};

pub fn profile_diagnostic_from_connection(
    profile: &AgentProfile,
    resolved_command: Option<String>,
    connection: &AgentConnectionInfo,
) -> AgentCapstoneProfileDiagnostic {
    AgentCapstoneProfileDiagnostic {
        profile_id: profile.id.clone(),
        profile_name: profile.name.clone(),
        status: if connection.success {
            "ready".to_string()
        } else {
            "error".to_string()
        },
        message: redact_agent_text(connection.error.as_deref().unwrap_or("ACP 握手成功")),
        command_path: resolved_command,
        agent_name: (!connection.agent_name.trim().is_empty())
            .then(|| connection.agent_name.clone()),
        agent_version: (!connection.agent_version.trim().is_empty())
            .then(|| connection.agent_version.clone()),
        protocol_version: connection.success.then_some(connection.protocol_version),
        capabilities: connection.capabilities.clone(),
        auth_methods: Vec::new(),
        latency_ms: connection.latency_ms,
    }
}
