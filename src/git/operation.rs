use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::command::{CancellationToken, GitCommandOptions, GitCommandResult, GitExecutor};
use super::error::GitError;
use super::events::{
    GitCredentialProvider, GitEvent, GitEventSink, NoopGitCredentialProvider, NoopGitEventSink,
};
use super::repository::{discover_repository, repository_state};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitOperation {
    Add,
    Unstage,
    Commit(String),
    Pull,
    Push,
    Fetch,
    Stash,
    Revert,
    Clean,
    Clone { url: String, destination: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitQuery {
    Diff,
    Log,
}

impl GitOperation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Add => "Git 添加",
            Self::Unstage => "Git 取消暂存",
            Self::Commit(_) => "Git 提交",
            Self::Pull => "Git 拉取",
            Self::Push => "Git 推送",
            Self::Fetch => "Git 获取",
            Self::Stash => "Git 贮藏",
            Self::Revert => "Git 还原",
            Self::Clean => "Git 清理",
            Self::Clone { .. } => "Git 克隆",
        }
    }

    pub fn has_progress(&self) -> bool {
        matches!(
            self,
            Self::Pull | Self::Push | Self::Fetch | Self::Clone { .. }
        )
    }
}

impl GitQuery {
    pub fn label(self) -> &'static str {
        match self {
            Self::Diff => "Git 差异",
            Self::Log => "Git 日志",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GitOperationResult {
    pub message: String,
    pub command: GitCommandResult,
}

pub fn execute_operation(
    executor: &GitExecutor,
    target: &Path,
    operation: GitOperation,
    cancellation: &CancellationToken,
    events: &dyn GitEventSink,
    credentials: Option<&dyn GitCredentialProvider>,
) -> Result<GitOperationResult, GitError> {
    if let GitOperation::Clone { url, destination } = &operation {
        return clone_repository(
            executor,
            url,
            destination,
            cancellation,
            events,
            credentials,
        );
    }

    let repository = discover_repository(executor, target)?;
    let relative = repository.relative_arg(target)?;
    let args = operation_args(&operation, relative)?;
    let options = GitCommandOptions::new(operation.label()).with_progress(operation.has_progress());
    let command = executor.run(
        &repository.work_tree,
        args,
        options,
        cancellation,
        events,
        credentials,
    )?;

    if let Ok(state) = repository_state(executor, &repository) {
        events.emit(GitEvent::RepositoryChanged {
            repository: repository.work_tree.clone(),
            branch: state.branch().map(str::to_string),
            head_oid: state.head_oid().map(str::to_string),
            operation: state.operation,
        });
    }

    Ok(GitOperationResult {
        message: format!("{}完成", operation.label()),
        command,
    })
}

fn operation_args(operation: &GitOperation, relative: OsString) -> Result<Vec<OsString>, GitError> {
    let args: Vec<&str> = match operation {
        GitOperation::Add => vec!["add", "--"],
        GitOperation::Unstage => vec!["restore", "--staged", "--"],
        GitOperation::Commit(_) => vec!["commit", "-m"],
        GitOperation::Pull => {
            return Ok(vec!["pull".into(), "--no-edit".into(), "--progress".into()]);
        }
        GitOperation::Push => return Ok(vec!["push".into(), "--progress".into()]),
        GitOperation::Fetch => {
            return Ok(vec!["fetch".into(), "--prune".into(), "--progress".into()]);
        }
        GitOperation::Stash => {
            return Ok(vec![
                "stash".into(),
                "push".into(),
                "--include-untracked".into(),
            ]);
        }
        GitOperation::Revert => vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"],
        GitOperation::Clean => vec!["clean", "-fd", "--"],
        GitOperation::Clone { .. } => unreachable!("clone is handled separately"),
    };

    let mut result = args.into_iter().map(OsString::from).collect::<Vec<_>>();
    if let GitOperation::Commit(message) = operation {
        result.push(message.clone().into());
        result.push("--".into());
    }
    result.push(relative);
    Ok(result)
}

fn clone_repository(
    executor: &GitExecutor,
    url: &str,
    destination: &Path,
    cancellation: &CancellationToken,
    events: &dyn GitEventSink,
    credentials: Option<&dyn GitCredentialProvider>,
) -> Result<GitOperationResult, GitError> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination_arg = destination
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| destination.as_os_str().to_os_string());
    let args = vec![
        OsString::from("clone"),
        OsString::from(url),
        destination_arg,
    ];
    let command = executor.run(
        parent,
        args,
        GitCommandOptions::new("Git 克隆").with_progress(true),
        cancellation,
        events,
        credentials,
    )?;

    if let Ok(repository) = discover_repository(executor, destination)
        && let Ok(state) = repository_state(executor, &repository)
    {
        events.emit(GitEvent::RepositoryChanged {
            repository: repository.work_tree.clone(),
            branch: state.branch().map(str::to_string),
            head_oid: state.head_oid().map(str::to_string),
            operation: state.operation,
        });
    }

    Ok(GitOperationResult {
        message: "Git 克隆完成".to_string(),
        command,
    })
}

pub fn execute_query(
    executor: &GitExecutor,
    target: &Path,
    query: GitQuery,
) -> Result<String, GitError> {
    let repository = discover_repository(executor, target)?;
    let relative = repository.relative_arg(target)?;
    let cancellation = CancellationToken::default();
    let noop_events = NoopGitEventSink;
    let credentials = NoopGitCredentialProvider;

    match query {
        GitQuery::Diff => {
            let working = executor.run(
                &repository.work_tree,
                vec![
                    "diff".into(),
                    "--no-ext-diff".into(),
                    "--no-color".into(),
                    "--".into(),
                    relative.clone(),
                ],
                GitCommandOptions::new("Git 差异"),
                &cancellation,
                &noop_events,
                Some(&credentials),
            )?;
            if !working.stdout.trim().is_empty() {
                return Ok(working.stdout.trim().to_string());
            }
            let staged = executor.run(
                &repository.work_tree,
                vec![
                    "diff".into(),
                    "--cached".into(),
                    "--no-ext-diff".into(),
                    "--no-color".into(),
                    "--".into(),
                    relative,
                ],
                GitCommandOptions::new("Git 暂存差异"),
                &cancellation,
                &noop_events,
                Some(&credentials),
            )?;
            let value = staged.stdout.trim();
            Ok(if value.is_empty() {
                "没有可显示内容。".to_string()
            } else {
                value.to_string()
            })
        }
        GitQuery::Log => {
            let result = executor.run(
                &repository.work_tree,
                vec![
                    "log".into(),
                    "-n".into(),
                    "100".into(),
                    "--date=short".into(),
                    "--pretty=format:%h  %ad  %an  %s".into(),
                    "--".into(),
                    relative,
                ],
                GitCommandOptions::new("Git 日志"),
                &cancellation,
                &noop_events,
                Some(&credentials),
            )?;
            let value = result.stdout.trim();
            Ok(if value.is_empty() {
                "没有可显示内容。".to_string()
            } else {
                value.to_string()
            })
        }
    }
}

/// Used by the existing VCS operations while the UI migrates to the richer
/// service API. The command and progress behavior is identical.
pub fn execute_vcs_operation(
    executor: &GitExecutor,
    target: &Path,
    operation: &crate::vcs::VcsOperation,
) -> Result<String, GitError> {
    execute_vcs_operation_cancellable(executor, target, operation, &CancellationToken::default())
}

pub(crate) fn execute_vcs_operation_cancellable(
    executor: &GitExecutor,
    target: &Path,
    operation: &crate::vcs::VcsOperation,
    cancellation: &CancellationToken,
) -> Result<String, GitError> {
    use crate::vcs::VcsOperation;

    let operation = match operation {
        VcsOperation::Add => GitOperation::Add,
        VcsOperation::Unstage => GitOperation::Unstage,
        VcsOperation::Commit(message) => GitOperation::Commit(message.clone()),
        VcsOperation::Pull => GitOperation::Pull,
        VcsOperation::Push => GitOperation::Push,
        VcsOperation::Fetch => GitOperation::Fetch,
        VcsOperation::Stash => GitOperation::Stash,
        VcsOperation::Revert => GitOperation::Revert,
        VcsOperation::Clean => GitOperation::Clean,
        VcsOperation::Update
        | VcsOperation::Lock
        | VcsOperation::Unlock
        | VcsOperation::Cleanup => {
            return Err(GitError::new(
                super::error::GitErrorKind::InvalidOperation,
                "Git 不支持此操作。",
            ));
        }
    };
    let events = NoopGitEventSink;
    execute_operation(executor, target, operation, cancellation, &events, None)
        .map(|result| result.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-git-operation-test-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "user.email", "kite@example.test"]);
        root
    }

    #[test]
    fn builds_path_safe_operation_args() {
        let relative = OsString::from("src/a b.txt");
        assert_eq!(
            operation_args(&GitOperation::Add, relative.clone()).unwrap(),
            vec![
                OsString::from("add"),
                OsString::from("--"),
                relative.clone()
            ]
        );
        assert_eq!(
            operation_args(&GitOperation::Fetch, relative).unwrap(),
            vec![
                OsString::from("fetch"),
                OsString::from("--prune"),
                OsString::from("--progress")
            ]
        );
    }

    #[test]
    fn runs_existing_git_write_operations_end_to_end() {
        if !git_available() {
            return;
        }
        let root = temp_repo("write");
        let file = root.join("a.txt");
        std::fs::write(&file, "one\n").expect("write file");

        let executor = GitExecutor::new();
        let events = NoopGitEventSink;
        execute_operation(
            &executor,
            &file,
            GitOperation::Add,
            &CancellationToken::default(),
            &events,
            None,
        )
        .expect("git add");
        execute_operation(
            &executor,
            &file,
            GitOperation::Commit("initial".into()),
            &CancellationToken::default(),
            &events,
            None,
        )
        .expect("git commit");

        std::fs::write(&file, "two\n").expect("edit file");
        let diff = execute_query(&executor, &file, GitQuery::Diff).expect("git diff");
        assert!(diff.contains("-one"));
        assert!(diff.contains("+two"));
        execute_operation(
            &executor,
            &file,
            GitOperation::Revert,
            &CancellationToken::default(),
            &events,
            None,
        )
        .expect("git revert");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn maps_unsupported_git_vcs_operations_to_structured_errors() {
        let executor = GitExecutor::new();
        let root = std::env::temp_dir();
        let error = execute_vcs_operation(&executor, &root, &crate::vcs::VcsOperation::Lock)
            .expect_err("git lock should be rejected");
        assert_eq!(
            error.kind,
            super::super::error::GitErrorKind::InvalidOperation
        );
    }
}
