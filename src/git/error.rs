use std::fmt;
use std::path::PathBuf;

/// A stable error category that UI layers can switch on without parsing stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitErrorKind {
    NotInstalled,
    NotRepository,
    OutsideRepository,
    PathNotFound,
    InvalidPath,
    NoHistory,
    InvalidCursor,
    Authentication,
    Conflict,
    Rejected,
    MissingIdentity,
    RepositoryLocked,
    UnsupportedVersion,
    Cancelled,
    InvalidOperation,
    Io,
    Other,
}

impl GitErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotInstalled => "未找到 Git",
            Self::NotRepository => "不是 Git 仓库",
            Self::OutsideRepository => "范围超出仓库",
            Self::PathNotFound => "路径不存在",
            Self::InvalidPath => "路径无效",
            Self::NoHistory => "没有提交历史",
            Self::InvalidCursor => "提交历史游标失效",
            Self::Authentication => "认证失败",
            Self::Conflict => "存在冲突",
            Self::Rejected => "远程拒绝",
            Self::MissingIdentity => "未配置提交身份",
            Self::RepositoryLocked => "仓库被占用",
            Self::UnsupportedVersion => "Git 版本过低",
            Self::Cancelled => "操作已取消",
            Self::InvalidOperation => "不支持的操作",
            Self::Io => "文件系统错误",
            Self::Other => "Git 操作失败",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub kind: GitErrorKind,
    pub message: String,
    /// Raw command output is retained for the Git console, never for normal UI copy.
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
    pub command: Option<String>,
    pub repository: Option<PathBuf>,
}

impl GitError {
    pub fn new(kind: GitErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            exit_code: None,
            command: None,
            repository: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.trim().is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    pub fn with_exit_code(mut self, exit_code: Option<i32>) -> Self {
        self.exit_code = exit_code;
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_repository(mut self, repository: impl Into<PathBuf>) -> Self {
        self.repository = Some(repository.into());
        self
    }

    pub fn cancelled() -> Self {
        Self::new(GitErrorKind::Cancelled, "Git 操作已取消")
    }

    pub fn classified(
        stderr: &str,
        stdout: &str,
        exit_code: Option<i32>,
        command: impl Into<String>,
        repository: impl Into<PathBuf>,
    ) -> Self {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        let kind = classify_git_failure(detail, exit_code);
        let message = match kind {
            GitErrorKind::Authentication => {
                "Git 认证失败。请检查账户、访问令牌、SSH 密钥或凭据助手。".to_string()
            }
            GitErrorKind::Conflict => "Git 检测到未解决的冲突。请解决冲突后继续。".to_string(),
            GitErrorKind::Rejected => {
                "远程仓库拒绝了这次更新。请先获取远程变更再重试。".to_string()
            }
            GitErrorKind::MissingIdentity => {
                "尚未配置 Git 提交身份。请设置 user.name 和 user.email。".to_string()
            }
            GitErrorKind::RepositoryLocked => {
                "仓库正被另一个 Git 进程占用。请等待该进程结束后重试。".to_string()
            }
            GitErrorKind::UnsupportedVersion => {
                "当前 Git 版本低于 Kite 支持的最低版本。请升级 Git。".to_string()
            }
            GitErrorKind::NotRepository => "所选路径不在 Git 仓库中。".to_string(),
            GitErrorKind::OutsideRepository => "所选范围超出 Git 仓库。".to_string(),
            GitErrorKind::PathNotFound => "所选路径不存在。".to_string(),
            GitErrorKind::InvalidPath => "所选路径无效。".to_string(),
            GitErrorKind::NoHistory => "所选范围没有可显示的提交历史。".to_string(),
            GitErrorKind::InvalidCursor => "提交历史游标已失效，请重新加载。".to_string(),
            _ if detail.is_empty() => {
                format!("Git 操作失败，退出码 {}", exit_code.unwrap_or(-1))
            }
            _ => "Git 操作失败。请查看 Git 控制台了解详情。".to_string(),
        };
        Self {
            kind,
            message,
            detail: (!detail.is_empty()).then(|| detail.to_string()),
            exit_code,
            command: Some(command.into()),
            repository: Some(repository.into()),
        }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitError {}

impl From<std::io::Error> for GitError {
    fn from(error: std::io::Error) -> Self {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            GitErrorKind::NotInstalled
        } else {
            GitErrorKind::Io
        };
        let message = if kind == GitErrorKind::NotInstalled {
            "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string()
        } else {
            format!("无法执行 Git：{error}")
        };
        Self::new(kind, message).with_detail(error.to_string())
    }
}

pub fn classify_git_failure(detail: &str, exit_code: Option<i32>) -> GitErrorKind {
    let lower = detail.to_ascii_lowercase();
    let contains = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));

    if exit_code == Some(130) {
        return GitErrorKind::Cancelled;
    }
    if contains(&[
        "authentication failed",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "permission denied (publickey",
        "permission denied (publickey)",
        "host key verification failed",
        "invalid username or password",
        "access denied",
        "credential helper",
        "fatal: could not read from remote repository",
    ]) {
        return GitErrorKind::Authentication;
    }
    if contains(&[
        "author identity unknown",
        "unable to auto-detect email address",
        "tell me who you are",
        "please tell me who you are",
        "missing user.name",
        "missing user.email",
    ]) {
        return GitErrorKind::MissingIdentity;
    }
    if contains(&[
        "index.lock",
        "cannot lock ref",
        "unable to create",
        "another git process",
        "repository is locked",
        "could not lock config file",
    ]) && contains(&["lock", "another git process"])
    {
        return GitErrorKind::RepositoryLocked;
    }
    if contains(&[
        "automatic merge failed",
        "unmerged files",
        "fix conflicts",
        "needs merge",
        "conflict (",
        "fatal: conflicting",
    ]) {
        return GitErrorKind::Conflict;
    }
    if contains(&[
        "[rejected]",
        "non-fast-forward",
        "failed to push some refs",
        "updates were rejected",
        "fetch first",
        "remote contains work that you do not have",
    ]) {
        return GitErrorKind::Rejected;
    }
    if contains(&[
        "not a git repository",
        "outside repository",
        "does not have a commit checked out",
    ]) {
        return GitErrorKind::NotRepository;
    }
    if contains(&[
        "requires git version",
        "minimum git version",
        "unknown option",
        "unrecognized argument",
    ]) {
        return GitErrorKind::UnsupportedVersion;
    }
    GitErrorKind::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_failures_without_exposing_raw_output() {
        let cases = [
            (
                "fatal: Authentication failed for 'https://example.test/repo.git/'",
                GitErrorKind::Authentication,
            ),
            (
                "error: Your local changes would be overwritten\nCONFLICT (content): Merge conflict in a.txt",
                GitErrorKind::Conflict,
            ),
            (
                "! [rejected] main -> main (non-fast-forward)",
                GitErrorKind::Rejected,
            ),
            (
                "Author identity unknown\n*** Please tell me who you are.",
                GitErrorKind::MissingIdentity,
            ),
            (
                "fatal: Unable to create '/repo/.git/index.lock': File exists.",
                GitErrorKind::RepositoryLocked,
            ),
            (
                "fatal: not a git repository (or any of the parent directories): .git",
                GitErrorKind::NotRepository,
            ),
        ];

        for (detail, expected) in cases {
            let error = GitError::classified(detail, "", Some(1), "git test", "/repo");
            assert_eq!(error.kind, expected);
            assert!(error.detail.is_some());
            assert_ne!(error.message, detail);
        }
    }

    #[test]
    fn cancellation_is_distinct_from_failure() {
        let mut error = GitError::cancelled();
        assert_eq!(error.kind, GitErrorKind::Cancelled);
        assert!(error.detail.is_none());
        error.detail = Some("killed by user".into());
        assert_eq!(error.kind, GitErrorKind::Cancelled);
    }
}
