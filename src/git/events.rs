use std::path::PathBuf;
use std::sync::Arc;

use super::error::GitErrorKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitOperationState {
    Rebasing,
    Merging,
    CherryPicking,
    Reverting,
    Bisecting,
}

impl GitOperationState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rebasing => "正在变基",
            Self::Merging => "正在合并",
            Self::CherryPicking => "正在拣选",
            Self::Reverting => "正在还原提交",
            Self::Bisecting => "正在二分查找",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitCredentialKind {
    Https,
    SshPassphrase,
    GpgPinentry,
    Unknown,
}

impl GitCredentialKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Https => "HTTPS 凭据",
            Self::SshPassphrase => "SSH 密码短语",
            Self::GpgPinentry => "GPG 密码",
            Self::Unknown => "Git 凭据",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCredentialRequest {
    pub kind: GitCredentialKind,
    pub prompt: String,
}

impl GitCredentialRequest {
    pub fn classify(prompt: impl Into<String>) -> Self {
        let prompt = prompt.into();
        let lower = prompt.to_ascii_lowercase();
        let kind =
            if lower.contains("gpg") || lower.contains("pinentry") || lower.contains("signing") {
                GitCredentialKind::GpgPinentry
            } else if lower.contains("passphrase") || lower.contains("ssh") || lower.contains("key")
            {
                GitCredentialKind::SshPassphrase
            } else if lower.contains("username")
                || lower.contains("password")
                || lower.contains("credential")
                || lower.contains("token")
            {
                GitCredentialKind::Https
            } else {
                GitCredentialKind::Unknown
            };
        Self { kind, prompt }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitOutputStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitEvent {
    Started {
        label: String,
        repository: PathBuf,
        command: String,
    },
    Progress {
        text: String,
    },
    Output {
        stream: GitOutputStream,
        text: String,
    },
    CredentialRequested {
        kind: GitCredentialKind,
        prompt: String,
    },
    RepositoryChanged {
        repository: PathBuf,
        branch: Option<String>,
        head_oid: Option<String>,
        operation: Option<GitOperationState>,
    },
    Finished {
        duration_ms: u64,
        exit_code: Option<i32>,
    },
    Failed {
        kind: GitErrorKind,
        message: String,
        duration_ms: u64,
    },
}

pub trait GitEventSink: Send + Sync {
    fn emit(&self, event: GitEvent);
}

impl<F> GitEventSink for F
where
    F: Fn(GitEvent) + Send + Sync,
{
    fn emit(&self, event: GitEvent) {
        self(event);
    }
}

pub struct NoopGitEventSink;

impl GitEventSink for NoopGitEventSink {
    fn emit(&self, _event: GitEvent) {}
}

/// A synchronous credential request. The implementation may show UI and must
/// return `None` when the user cancels the prompt.
pub trait GitCredentialProvider: Send + Sync {
    fn request_credential(&self, request: &GitCredentialRequest) -> Option<String>;
}

impl<F> GitCredentialProvider for F
where
    F: Fn(&GitCredentialRequest) -> Option<String> + Send + Sync,
{
    fn request_credential(&self, request: &GitCredentialRequest) -> Option<String> {
        self(request)
    }
}

pub struct NoopGitCredentialProvider;

impl GitCredentialProvider for NoopGitCredentialProvider {
    fn request_credential(&self, _request: &GitCredentialRequest) -> Option<String> {
        None
    }
}

pub type SharedEventSink = Arc<dyn GitEventSink>;
pub type SharedCredentialProvider = Arc<dyn GitCredentialProvider>;
