pub mod capstone_diagnostics;
pub mod capstone_diagnostics_adapter;
mod config;
pub mod context;
mod diagnostics;
pub mod preset_adapter;
pub mod presets;
mod process;
mod restore;
mod runtime;

pub use config::{
    AgentEnvVar, AgentPermissionMode, AgentPreset, AgentProfile, AgentProfileStore, AgentStoreLoad,
    AgentTransport, AgentWorkingDirectoryPolicy, agent_config_path, agent_presets,
    default_agent_profile_id, load_agent_profiles, load_agent_profiles_at, preset_profile,
    save_agent_profiles, save_agent_profiles_at, validate_agent_profile,
};
pub use context::{
    AgentContext, AgentContextMode, AgentLaunchRequest, AgentMenuCommand, AgentPath,
    AgentSessionPhase, default_working_directory, is_text_like_path,
};
pub use diagnostics::{
    AgentConnectionInfo, AgentDiagnosticEntry, AgentDiagnostics, AgentProfileDiagnostic,
    agent_log_path, clear_agent_diagnostics, diagnose_agents, probe_agent_connection,
    record_agent_event, redact_agent_text,
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
