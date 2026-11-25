use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use crate::agent::AgentTransport as AgentTransportMode;
use crate::agent::acp::AcpStdioTransport;
use crate::agent::config::ValidatedSessionConfig;
use crate::agent::events::EventDispatcher;
use crate::agent::model::{
    AcpCapabilities, AgentError, AgentErrorKind, AgentSessionPermissionRequest, AgentStopReason,
};
use crate::agent::process::ProcessHandle;

#[derive(Clone, Debug)]
pub(crate) struct AgentHandshake {
    pub(crate) protocol_version: u32,
    pub(crate) agent_name: String,
    pub(crate) agent_version: String,
    pub(crate) capabilities: AcpCapabilities,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentPrompt {
    pub(crate) text: String,
    pub(crate) context_paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum TransportEvent {
    Activity,
    PermissionRequested(AgentSessionPermissionRequest),
    PromptCompleted(AgentStopReason),
    ProcessExited(Option<i32>),
}

pub(crate) trait AgentTransport: Send {
    fn start(
        &mut self,
        config: &ValidatedSessionConfig,
        events: Arc<EventDispatcher>,
        process_slot: &OnceLock<Arc<ProcessHandle>>,
        timeout: Duration,
    ) -> Result<AgentHandshake, AgentError>;

    fn create_session(
        &mut self,
        cwd: &std::path::Path,
        additional_directories: &[PathBuf],
        timeout: Duration,
    ) -> Result<String, AgentError>;

    fn send_prompt(&mut self, session_id: &str, prompt: &AgentPrompt) -> Result<(), AgentError>;

    fn wait_for_event(&mut self, timeout: Duration) -> Result<Option<TransportEvent>, AgentError>;

    /// Returns whether the request was still pending.
    fn respond_permission(
        &mut self,
        request_id: &str,
        option_id: Option<&str>,
    ) -> Result<bool, AgentError>;

    fn cancel(&mut self, session_id: &str) -> Result<(), AgentError>;

    fn shutdown(&mut self);

    fn try_wait(&mut self) -> Option<std::process::ExitStatus>;
}

pub(crate) fn transport_for(
    kind: AgentTransportMode,
) -> Result<Box<dyn AgentTransport>, AgentError> {
    match kind {
        AgentTransportMode::AcpStdio => Ok(Box::new(AcpStdioTransport::new())),
        AgentTransportMode::AcpHttp | AgentTransportMode::Cli => Err(AgentError::new(
            AgentErrorKind::UnsupportedTransport,
            format!("{} 当前仅支持连接测试，不能启动 Agent 会话。", kind.label()),
        )),
    }
}
