//! Shared Git service used by the Windows and macOS frontends.
//!
//! All Git commands go through one executor so environment hardening, path
//! handling, cancellation, progress, askpass, structured errors and console
//! logging are implemented once.

mod command;
mod error;
mod events;
mod external;
mod operation;
mod repository;
mod status;
mod watch;

pub mod preview;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use crate::fs::{FileEntry, VcsStatus};

pub use command::{
    CancellationToken, GitCommandOptions, GitCommandResult, GitConsole, GitConsoleEntry,
    GitExecutor, GitVersion, MIN_GIT_VERSION, detect_git_version,
};
pub use error::{GitError, GitErrorKind, classify_git_failure};
pub use events::{
    GitCredentialKind, GitCredentialProvider, GitCredentialRequest, GitEvent, GitEventSink,
    GitOperationState, GitOutputStream, NoopGitCredentialProvider, NoopGitEventSink,
    SharedCredentialProvider, SharedEventSink,
};
pub use operation::{GitOperation, GitOperationResult, GitQuery, execute_operation, execute_query};
pub use preview::{
    GitChangeCounts, GitChangedFile, GitCommitSummary, GitDiffLine, GitDiffLineKind,
    GitOperationAction, GitPathSummary, GitRepositorySnapshot, git_file_diff, git_path_summary,
    git_repository_root, git_repository_snapshot, run_git_operation_control,
};
pub use repository::{
    GitHead, GitRepository, GitRepositoryState, discover_repository, operation_state,
    repository_state,
};
pub use watch::{GitRepositoryEvent, GitRepositoryWatcher};

pub(crate) use status::{GitPathStatus, git_statuses};

#[derive(Clone)]
pub struct GitService {
    executor: Arc<GitExecutor>,
    version: Arc<Mutex<Option<GitVersion>>>,
}

impl Default for GitService {
    fn default() -> Self {
        Self::new()
    }
}

impl GitService {
    pub fn new() -> Self {
        Self {
            executor: Arc::new(GitExecutor::new()),
            version: Arc::new(Mutex::new(None)),
        }
    }

    pub fn shared() -> &'static Self {
        static SERVICE: OnceLock<GitService> = OnceLock::new();
        SERVICE.get_or_init(Self::new)
    }

    pub fn console(&self) -> Arc<GitConsole> {
        self.executor.console()
    }

    pub fn console_entries(&self) -> Vec<GitConsoleEntry> {
        self.executor.console().entries()
    }

    pub fn clear_console(&self) {
        self.executor.console().clear();
    }

    pub fn git_version(&self) -> Result<GitVersion, GitError> {
        if let Ok(version) = self.version.lock()
            && let Some(version) = version.as_ref()
        {
            return Ok(version.clone());
        }
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let result = self.executor.run(
            &current_dir,
            vec![OsString::from("--version")],
            GitCommandOptions::new("Git 版本"),
            &CancellationToken::default(),
            &NoopGitEventSink,
            None,
        )?;
        let version = GitVersion::parse(&result.stdout).ok_or_else(|| {
            GitError::new(
                GitErrorKind::UnsupportedVersion,
                "无法识别当前安装的 Git 版本。",
            )
            .with_detail(result.stdout)
            .with_exit_code(result.exit_code)
        })?;
        if let Ok(mut cached) = self.version.lock() {
            *cached = Some(version.clone());
        }
        Ok(version)
    }

    pub fn require_supported_version(&self) -> Result<GitVersion, GitError> {
        let version = self.git_version()?;
        if version.is_supported() {
            Ok(version)
        } else {
            Err(GitError::new(
                GitErrorKind::UnsupportedVersion,
                format!(
                    "Kite 需要 Git {} 或更高版本，当前为 {}。",
                    MIN_GIT_VERSION.display(),
                    version.display()
                ),
            )
            .with_detail(version.raw))
        }
    }

    pub fn repository_state(&self, path: &Path) -> Result<GitRepositoryState, GitError> {
        let repository = discover_repository(&self.executor, path)?;
        repository_state(&self.executor, &repository)
    }

    pub fn discover_repository(&self, path: &Path) -> Result<GitRepository, GitError> {
        discover_repository(&self.executor, path)
    }

    pub fn run_operation(
        &self,
        target: &Path,
        operation: GitOperation,
        cancellation: &CancellationToken,
        events: &dyn GitEventSink,
        credentials: Option<&dyn GitCredentialProvider>,
    ) -> Result<GitOperationResult, GitError> {
        self.require_supported_version()?;
        execute_operation(
            &self.executor,
            target,
            operation,
            cancellation,
            events,
            credentials,
        )
    }

    pub fn run_query(
        &self,
        target: &Path,
        query: GitQuery,
        events: &dyn GitEventSink,
        credentials: Option<&dyn GitCredentialProvider>,
    ) -> Result<String, GitError> {
        // Queries do not expose cancellation yet, but still use the shared
        // executor, console and askpass bridge.
        self.require_supported_version()?;
        let repository = self.discover_repository(target)?;
        let relative = repository.relative_arg(target)?;
        let cancellation = CancellationToken::default();
        let executor = &*self.executor;
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
                    events,
                    credentials,
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
                    events,
                    credentials,
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
                    events,
                    credentials,
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

    pub fn start_operation(
        &self,
        target: PathBuf,
        operation: GitOperation,
        events: SharedEventSink,
        credentials: Option<SharedCredentialProvider>,
    ) -> Arc<GitOperationTask> {
        let service = self.clone();
        let cancellation = CancellationToken::default();
        let state = Arc::new(Mutex::new(GitOperationTaskState::Running));
        let worker_state = state.clone();
        let worker_cancel = cancellation.clone();
        let thread = thread::spawn(move || {
            let result = service.run_operation(
                &target,
                operation,
                &worker_cancel,
                events.as_ref(),
                credentials.as_deref(),
            );
            if let Ok(mut state) = worker_state.lock() {
                *state = GitOperationTaskState::Finished(result);
            }
        });
        Arc::new(GitOperationTask {
            cancellation,
            state,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(crate) fn run_preview_command(
        &self,
        current_dir: &Path,
        args: &[&str],
    ) -> Result<GitCommandResult, GitError> {
        self.executor.run(
            current_dir,
            args.iter().map(OsString::from).collect::<Vec<_>>(),
            GitCommandOptions::new("Git 预览"),
            &CancellationToken::default(),
            &NoopGitEventSink,
            None,
        )
    }

    pub fn run_legacy_operation(
        &self,
        target: &Path,
        operation: &crate::vcs::VcsOperation,
    ) -> Result<String, GitError> {
        self.require_supported_version()?;
        operation::execute_vcs_operation(&self.executor, target, operation)
    }

    pub fn run_legacy_operation_cancellable(
        &self,
        target: &Path,
        operation: &crate::vcs::VcsOperation,
        cancellation: &CancellationToken,
    ) -> Result<String, GitError> {
        self.require_supported_version()?;
        operation::execute_vcs_operation_cancellable(
            &self.executor,
            target,
            operation,
            cancellation,
        )
    }

    pub fn run_legacy_query(
        &self,
        target: &Path,
        query: crate::vcs::VcsQuery,
    ) -> Result<String, GitError> {
        let query = match query {
            crate::vcs::VcsQuery::Diff => GitQuery::Diff,
            crate::vcs::VcsQuery::Log => GitQuery::Log,
        };
        self.run_query(target, query, &NoopGitEventSink, None)
    }

    pub fn watch_repository(
        &self,
        path: &Path,
        on_event: impl FnMut(GitRepositoryEvent) + Send + 'static,
    ) -> Result<GitRepositoryWatcher, GitError> {
        let repository = self.discover_repository(path)?;
        GitRepositoryWatcher::new(repository, self.executor.clone(), on_event)
            .map_err(|message| GitError::new(GitErrorKind::Io, message).with_repository(path))
    }
}

enum GitOperationTaskState {
    Running,
    Finished(Result<GitOperationResult, GitError>),
}

pub struct GitOperationTask {
    cancellation: CancellationToken,
    state: Arc<Mutex<GitOperationTaskState>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl GitOperationTask {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub fn is_finished(&self) -> bool {
        self.state
            .lock()
            .map(|state| matches!(*state, GitOperationTaskState::Finished(_)))
            .unwrap_or(false)
    }

    pub fn wait(&self) -> Result<GitOperationResult, GitError> {
        if let Ok(mut thread) = self.thread.lock()
            && let Some(thread) = thread.take()
        {
            let _ = thread.join();
        }
        self.state
            .lock()
            .map_err(|_| GitError::new(GitErrorKind::Io, "Git 操作状态不可用。"))
            .and_then(|state| match &*state {
                GitOperationTaskState::Running => {
                    Err(GitError::new(GitErrorKind::Other, "Git 操作尚未完成。"))
                }
                GitOperationTaskState::Finished(result) => result.clone(),
            })
    }
}

impl Drop for GitOperationTask {
    fn drop(&mut self) {
        self.cancel();
        if let Ok(mut thread) = self.thread.lock()
            && let Some(thread) = thread.take()
        {
            let _ = thread.join();
        }
    }
}

/// Compat shim for the existing VCS status snapshot. Git data now comes from
/// the Git service while SVN remains in `vcs.rs`.
pub(crate) fn apply_git_statuses(
    entries: &mut [FileEntry],
    view_root: &Path,
    statuses: &[GitPathStatus],
) {
    use std::collections::HashMap;
    use std::path::Component;

    fn path_key(path: &Path) -> String {
        let value = path.to_string_lossy();
        #[cfg(target_os = "windows")]
        {
            value.replace('/', "\\").to_lowercase()
        }
        #[cfg(not(target_os = "windows"))]
        {
            value.into_owned()
        }
    }

    fn merge(map: &mut HashMap<String, VcsStatus>, key: String, status: VcsStatus) {
        let merged = crate::fs::worse_status(map.get(&key).copied(), Some(status));
        if let Some(merged) = merged {
            map.insert(key, merged);
        }
    }

    let mut exact = HashMap::new();
    let mut directories = HashMap::new();
    for item in statuses {
        let Ok(relative) = item.path.strip_prefix(view_root) else {
            continue;
        };
        let mut components = relative.components();
        let Some(first) = components.next() else {
            continue;
        };
        if !matches!(first, Component::Normal(_)) {
            continue;
        }
        merge(&mut exact, path_key(&item.path), item.status);
        if components.next().is_some() {
            let directory = view_root.join(first.as_os_str());
            merge(&mut directories, path_key(&directory), item.status);
        }
    }

    for entry in entries {
        let key = path_key(&entry.path);
        let aggregate = entry
            .is_dir()
            .then(|| directories.get(&key).copied())
            .flatten();
        entry.git_status = crate::fs::worse_status(exact.get(&key).copied(), aggregate);
    }
}

pub(crate) use external::open_log_window;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_git_statuses_on_parent_directories() {
        use crate::fs::EntryKind;

        let root = std::env::temp_dir().join("kite-git-status-aggregate");
        let mut entries = vec![
            FileEntry {
                path: root.join("src"),
                name: "src".into(),
                kind: EntryKind::Directory,
                size: 0,
                modified: None,
                extension: String::new(),
                git_status: None,
                svn_status: None,
            },
            FileEntry {
                path: root.join("other"),
                name: "other".into(),
                kind: EntryKind::Directory,
                size: 0,
                modified: None,
                extension: String::new(),
                git_status: None,
                svn_status: None,
            },
        ];
        let statuses = vec![GitPathStatus {
            path: root.join("src").join("a.rs"),
            status: VcsStatus::Modified,
        }];
        apply_git_statuses(&mut entries, &root, &statuses);
        assert_eq!(entries[0].git_status, Some(VcsStatus::Modified));
        assert_eq!(entries[1].git_status, None);
    }

    #[test]
    fn service_exposes_version_and_console() {
        let service = GitService::new();
        let _ = service.console_entries();
        assert_eq!(MIN_GIT_VERSION.display(), "2.30.0");
    }

    #[test]
    fn operation_task_can_be_cancelled_before_start() {
        let service = GitService::new();
        let events: SharedEventSink = Arc::new(NoopGitEventSink);
        let task = service.start_operation(PathBuf::from("."), GitOperation::Fetch, events, None);
        task.cancel();
        assert!(task.is_cancelled());
    }
}
