use std::fmt;

use crate::agent::{
    AgentCapabilities, AgentEnvironmentVariable, AgentProfile, AgentTransport, PermissionPolicy,
};

/// The ACP wire protocol version supported by this client.
pub const ACP_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentSessionConfig {
    pub transport: AgentTransport,
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: String,
    pub additional_directories: Vec<String>,
    /// Explicit additions to Kite's small environment whitelist.
    ///
    /// The child never inherits the full Kite environment.
    /// This is a runtime map: the profile store must persist credential
    /// references, not these resolved values.
    pub environment: Vec<AgentEnvironmentVariable>,
    pub startup_timeout_ms: u64,
    /// Upper bound for one prompt turn. A value of `0` is rejected.
    pub turn_timeout_ms: u64,
    /// Cancel a turn if neither protocol output nor user input arrives for this long.
    /// A value of `0` disables the idle check.
    pub idle_timeout_ms: u64,
    /// Grace period for an in-flight turn to acknowledge cancellation.
    pub shutdown_timeout_ms: u64,
    pub max_frame_bytes: u32,
    pub max_output_bytes: u64,
    /// Kite-side permission policy applied before an ACP option can be selected.
    pub permission_policy: PermissionPolicy,
    /// Profile declarations used by managed policies; not a process sandbox.
    pub capabilities: AgentCapabilities,
}

impl AgentSessionConfig {
    /// Build runtime launch options from the validated profile model (#150).
    ///
    /// `environment` carries only values resolved at runtime from the profile's
    /// opaque credential reference. Persist `AgentProfile.credential_ref`, never
    /// these resolved values.
    pub fn from_profile(
        profile: &AgentProfile,
        working_directory: impl Into<String>,
        resolved_environment: Vec<AgentEnvironmentVariable>,
    ) -> Self {
        let turn_timeout_ms = u64::from(profile.timeout_seconds).saturating_mul(1_000);
        let mut environment = profile.environment.clone();
        for variable in resolved_environment {
            if let Some(existing) = environment
                .iter_mut()
                .find(|existing| existing.name.eq_ignore_ascii_case(&variable.name))
            {
                *existing = variable;
            } else {
                environment.push(variable);
            }
        }
        Self {
            transport: profile.transport,
            program: profile.executable.clone().unwrap_or_default(),
            args: profile.arguments.clone(),
            working_directory: working_directory.into(),
            additional_directories: Vec::new(),
            environment,
            startup_timeout_ms: turn_timeout_ms.min(10_000),
            turn_timeout_ms,
            idle_timeout_ms: turn_timeout_ms.min(60_000),
            shutdown_timeout_ms: 2_000,
            max_frame_bytes: 256 * 1024,
            max_output_bytes: profile.output_limit_bytes,
            permission_policy: profile.permission,
            capabilities: profile.capabilities.clone(),
        }
    }
}

impl Default for AgentSessionConfig {
    fn default() -> Self {
        Self {
            transport: AgentTransport::AcpStdio,
            program: String::new(),
            args: Vec::new(),
            working_directory: String::new(),
            additional_directories: Vec::new(),
            environment: Vec::new(),
            startup_timeout_ms: 10_000,
            turn_timeout_ms: 5 * 60_000,
            idle_timeout_ms: 60_000,
            shutdown_timeout_ms: 2_000,
            max_frame_bytes: 256 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
            permission_policy: PermissionPolicy::default(),
            capabilities: AgentCapabilities::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum AgentSessionState {
    #[default]
    Idle,
    Starting,
    Ready,
    Running,
    Cancelling,
    Closed,
    Failed,
}

impl AgentSessionState {
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (Self::Idle, Self::Starting | Self::Closed)
                | (Self::Starting, Self::Ready | Self::Failed | Self::Closed)
                | (Self::Ready, Self::Running | Self::Failed | Self::Closed)
                | (
                    Self::Running,
                    Self::Ready | Self::Cancelling | Self::Failed | Self::Closed
                )
                | (
                    Self::Cancelling,
                    Self::Ready | Self::Cancelling | Self::Failed | Self::Closed
                )
                | (Self::Failed, Self::Closed)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Other { reason: String },
}

impl AgentStopReason {
    pub(crate) fn from_wire(reason: &str) -> Self {
        match reason {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "max_turn_requests" => Self::MaxTurnRequests,
            "refusal" => Self::Refusal,
            "cancelled" => Self::Cancelled,
            other => Self::Other {
                reason: other.to_string(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum AgentDiagnosticLevel {
    #[default]
    Debug,
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct AcpCapabilities {
    pub load_session: bool,
    pub prompt_image: bool,
    pub prompt_audio: bool,
    pub prompt_embedded_context: bool,
    pub mcp_http: bool,
    pub mcp_sse: bool,
    pub session_close: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentToolCall {
    pub tool_call_id: String,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub content: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentPlanEntry {
    pub content: String,
    pub status: Option<String>,
    pub priority: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentSessionPermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentSessionPermissionRequest {
    pub request_id: String,
    pub tool_call_id: String,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub options: Vec<AgentSessionPermissionOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentErrorKind {
    NotInstalled,
    AuthenticationRequired,
    UnsupportedVersion,
    Protocol,
    Timeout,
    Cancelled,
    OutputLimit,
    ProcessExited,
    InvalidConfiguration,
    Busy,
    Closed,
    Io,
    UnsupportedTransport,
    Other,
}

impl AgentErrorKind {
    pub fn default_retryable(self) -> bool {
        matches!(
            self,
            Self::AuthenticationRequired
                | Self::Timeout
                | Self::ProcessExited
                | Self::Busy
                | Self::Io
                | Self::Other
        )
    }
}

impl fmt::Display for AgentErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotInstalled => "not installed",
            Self::AuthenticationRequired => "authentication required",
            Self::UnsupportedVersion => "unsupported protocol version",
            Self::Protocol => "protocol error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::OutputLimit => "output limit exceeded",
            Self::ProcessExited => "agent process exited",
            Self::InvalidConfiguration => "invalid configuration",
            Self::Busy => "session busy",
            Self::Closed => "session closed",
            Self::Io => "I/O error",
            Self::UnsupportedTransport => "unsupported transport",
            Self::Other => "agent error",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentFailure {
    pub kind: AgentErrorKind,
    pub message: String,
    pub detail: Option<String>,
    pub retryable: bool,
}

impl AgentFailure {
    pub(crate) fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            retryable: kind.default_retryable(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentSessionEvent {
    StateChanged {
        state: AgentSessionState,
    },
    ProcessStarted {
        pid: u32,
    },
    ProtocolReady {
        protocol_version: u32,
        agent_name: String,
        agent_version: String,
        capabilities: AcpCapabilities,
    },
    SessionCreated {
        protocol_session_id: String,
    },
    MessageChunk {
        text: String,
        message_id: Option<String>,
    },
    ThoughtChunk {
        text: String,
        message_id: Option<String>,
    },
    ToolCallUpdated {
        tool_call: AgentToolCall,
    },
    PlanUpdated {
        entries: Vec<AgentPlanEntry>,
    },
    PermissionRequested {
        request: AgentSessionPermissionRequest,
    },
    Diagnostic {
        level: AgentDiagnosticLevel,
        message: String,
    },
    TurnCompleted {
        stop_reason: AgentStopReason,
        duration_ms: u64,
    },
    TurnCancelled {
        duration_ms: u64,
    },
    ProcessExited {
        exit_code: Option<i32>,
    },
    Error {
        failure: AgentFailure,
    },
}

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("{message}")]
    Failure {
        kind: AgentErrorKind,
        message: String,
        detail: Option<String>,
        retryable: bool,
    },
}

impl AgentError {
    pub(crate) fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        Self::Failure {
            kind,
            message,
            detail: None,
            retryable: kind.default_retryable(),
        }
    }

    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.trim().is_empty()
            && let Self::Failure { detail: stored, .. } = &mut self
        {
            *stored = Some(detail);
        }
        self
    }

    pub(crate) fn invalid_configuration(message: impl Into<String>) -> Self {
        Self::new(AgentErrorKind::InvalidConfiguration, message)
    }

    pub fn kind(&self) -> AgentErrorKind {
        match self {
            Self::Failure { kind, .. } => *kind,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Failure { message, .. } => message,
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Failure { detail, .. } => detail.as_deref(),
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Failure { retryable, .. } => *retryable,
        }
    }

    pub(crate) fn as_failure(&self) -> AgentFailure {
        match self {
            Self::Failure {
                kind,
                message,
                detail,
                retryable,
            } => AgentFailure {
                kind: *kind,
                message: message.clone(),
                detail: detail.clone(),
                retryable: *retryable,
            },
        }
    }
}

#[uniffi::export(callback_interface)]
pub trait AgentSessionEventCallback: Send + Sync {
    fn on_event(&self, event: AgentSessionEvent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_conversion_maps_runtime_limits_and_environment() {
        let mut profile = AgentProfile {
            executable: Some("agent".to_string()),
            arguments: vec!["--acp".to_string()],
            timeout_seconds: 120,
            output_limit_bytes: 4 * 1024 * 1024,
            permission: PermissionPolicy::ConfirmWrites,
            capabilities: AgentCapabilities {
                write_files: true,
                ..AgentCapabilities::default()
            },
            ..AgentProfile::default()
        };
        profile.environment = vec![AgentEnvironmentVariable {
            name: "LANG".to_string(),
            value: "en_US.UTF-8".to_string(),
        }];
        let config = AgentSessionConfig::from_profile(
            &profile,
            std::env::temp_dir().to_string_lossy(),
            vec![AgentEnvironmentVariable {
                name: "LANG".to_string(),
                value: "zh_CN.UTF-8".to_string(),
            }],
        );
        assert_eq!(config.program, "agent");
        assert_eq!(config.args, vec!["--acp"]);
        assert_eq!(config.turn_timeout_ms, 120_000);
        assert_eq!(config.max_output_bytes, 4 * 1024 * 1024);
        assert_eq!(config.environment.len(), 1);
        assert_eq!(config.environment[0].value, "zh_CN.UTF-8");
        assert_eq!(config.permission_policy, PermissionPolicy::ConfirmWrites);
        assert!(config.capabilities.write_files);
    }
}
