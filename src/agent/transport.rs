use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use crate::agent::AgentTransport as AgentTransportMode;
use crate::agent::acp::AcpStdioTransport;
use crate::agent::config::ValidatedSessionConfig;
use crate::agent::events::EventDispatcher;
use crate::agent::model::{
    AcpCapabilities, AgentError, AgentErrorKind, AgentPermissionRequest, AgentStopReason,
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
    PermissionRequested(AgentPermissionRequest),
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

pub(crate) fn transport_for(kind: AgentTransportMode) -> Box<dyn AgentTransport> {
    match kind {
        AgentTransportMode::AcpStdio => Box::new(AcpStdioTransport::new()),
        AgentTransportMode::AcpHttp | AgentTransportMode::Cli => Box::new(CliTransport),
    }
}

struct CliTransport;

impl AgentTransport for CliTransport {
    fn start(
        &mut self,
        _config: &ValidatedSessionConfig,
        _events: Arc<EventDispatcher>,
        _process_slot: &OnceLock<Arc<ProcessHandle>>,
        _timeout: Duration,
    ) -> Result<AgentHandshake, AgentError> {
        Err(AgentError::new(
            AgentErrorKind::UnsupportedTransport,
            "当前 Agent 传输方式尚未启用。",
        ))
    }

    fn create_session(
        &mut self,
        _cwd: &std::path::Path,
        _additional_directories: &[PathBuf],
        _timeout: Duration,
    ) -> Result<String, AgentError> {
        Err(unsupported_cli())
    }

    fn send_prompt(&mut self, _session_id: &str, _prompt: &AgentPrompt) -> Result<(), AgentError> {
        Err(unsupported_cli())
    }

    fn wait_for_event(&mut self, _timeout: Duration) -> Result<Option<TransportEvent>, AgentError> {
        Err(unsupported_cli())
    }

    fn respond_permission(
        &mut self,
        _request_id: &str,
        _option_id: Option<&str>,
    ) -> Result<bool, AgentError> {
        Err(unsupported_cli())
    }

    fn cancel(&mut self, _session_id: &str) -> Result<(), AgentError> {
        Err(unsupported_cli())
    }

    fn shutdown(&mut self) {}

    fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        None
    }
}

fn unsupported_cli() -> AgentError {
    AgentError::new(
        AgentErrorKind::UnsupportedTransport,
        "当前 Agent 传输方式尚未启用。",
    )
}
