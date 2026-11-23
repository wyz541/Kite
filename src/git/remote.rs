//! Git remote, synchronization, clone, and initialization services.
//!
//! All network-facing Git work runs through this module. Operations stream
//! progress to a shared monitor so both GPUI and SwiftUI can present a
//! cancellable background task without duplicating Git command handling.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const MIN_GIT_VERSION: (u32, u32) = (2, 23);

static NEXT_CLONE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitNetworkOperation {
    Fetch,
    Pull,
    Push,
    Clone,
}

impl GitNetworkOperation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fetch => "获取远程更新",
            Self::Pull => "拉取远程更新",
            Self::Push => "推送提交",
            Self::Clone => "克隆仓库",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitProgressPhase {
    #[default]
    Idle,
    Preparing,
    Transferring,
    Resolving,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
}

impl GitProgressPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "等待开始",
            Self::Preparing => "准备",
            Self::Transferring => "传输",
            Self::Resolving => "处理更改",
            Self::Finalizing => "完成",
            Self::Completed => "已完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GitOperationProgress {
    pub operation: Option<GitNetworkOperation>,
    pub phase: GitProgressPhase,
    pub message: String,
    pub detail: String,
    pub fraction: Option<f64>,
    pub active: bool,
    pub cancelled: bool,
}

#[derive(Debug)]
pub struct GitOperationMonitor {
    progress: Mutex<GitOperationProgress>,
    cancelled: AtomicBool,
}

impl Default for GitOperationMonitor {
    fn default() -> Self {
        Self {
            progress: Mutex::new(GitOperationProgress::default()),
            cancelled: AtomicBool::new(false),
        }
    }
}

impl GitOperationMonitor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn snapshot(&self) -> GitOperationProgress {
        self.progress.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut progress) = self.progress.lock() {
            progress.cancelled = true;
            progress.message = "正在取消…".to_string();
        }
    }

    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
        if let Ok(mut progress) = self.progress.lock() {
            *progress = GitOperationProgress::default();
        }
    }

    fn begin(&self, operation: GitNetworkOperation, message: impl Into<String>) {
        self.cancelled.store(false, Ordering::SeqCst);
        if let Ok(mut progress) = self.progress.lock() {
            *progress = GitOperationProgress {
                operation: Some(operation),
                phase: GitProgressPhase::Preparing,
                message: message.into(),
                active: true,
                ..GitOperationProgress::default()
            };
        }
    }

    fn update(&self, phase: GitProgressPhase, message: impl Into<String>) {
        if let Ok(mut progress) = self.progress.lock() {
            progress.phase = phase;
            progress.message = message.into();
            progress.active = true;
        }
    }

    fn record_line(&self, line: &str) {
        let trimmed = line.trim().trim_end_matches('\r');
        if trimmed.is_empty() {
            return;
        }
        let lower = trimmed.to_ascii_lowercase();
        let phase = if lower.contains("resolving deltas")
            || lower.contains("checking out")
            || lower.contains("updating files")
            || lower.contains("merge")
            || lower.contains("rebase")
        {
            GitProgressPhase::Resolving
        } else if lower.contains("finalizing")
            || lower.contains("done.")
            || lower.contains("writing objects")
        {
            GitProgressPhase::Finalizing
        } else if lower.contains('%')
            || lower.contains("receiving objects")
            || lower.contains("remote:")
            || lower.contains("enumerating objects")
            || lower.contains("compressing objects")
            || lower.contains("counting objects")
        {
            GitProgressPhase::Transferring
        } else {
            GitProgressPhase::Preparing
        };
        let fraction = progress_fraction(trimmed);
        if let Ok(mut progress) = self.progress.lock() {
            progress.phase = phase;
            progress.detail = trimmed.chars().take(500).collect();
            progress.message = progress_message(phase, &progress.detail);
            if fraction.is_some() {
                progress.fraction = fraction;
            }
            progress.active = true;
        }
    }

    fn finish(&self, phase: GitProgressPhase, message: impl Into<String>) {
        if let Ok(mut progress) = self.progress.lock() {
            progress.phase = phase;
            progress.message = message.into();
            progress.active = false;
            progress.cancelled = phase == GitProgressPhase::Cancelled;
            if phase == GitProgressPhase::Completed {
                progress.fraction = Some(1.0);
            }
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

fn progress_fraction(line: &str) -> Option<f64> {
    let percent = line.find('%')?;
    let number = line[..percent]
        .rsplit(|character: char| !character.is_ascii_digit() && character != '.')
        .next()?
        .parse::<f64>()
        .ok()?;
    Some((number / 100.0).clamp(0.0, 1.0))
}

fn progress_message(phase: GitProgressPhase, detail: &str) -> String {
    if phase == GitProgressPhase::Transferring {
        return detail
            .split("  ")
            .next()
            .unwrap_or(detail)
            .trim()
            .to_string();
    }
    format!("{}：{detail}", phase.label())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepository {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub branch: Option<String>,
    pub head_oid: Option<String>,
    pub upstream: Option<String>,
    pub remotes: Vec<GitRemote>,
    pub dirty: bool,
    pub operation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommit {
    pub oid: String,
    pub short_oid: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitFileChange {
    pub status: String,
    pub path: String,
    pub original_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitPullStrategy {
    Merge,
    Rebase,
    FastForwardOnly,
}

impl GitPullStrategy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "合并",
            Self::Rebase => "变基",
            Self::FastForwardOnly => "仅快进",
        }
    }

    fn argument(self) -> &'static str {
        match self {
            Self::Merge => "--no-rebase",
            Self::Rebase => "--rebase",
            Self::FastForwardOnly => "--ff-only",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitFetchOptions {
    pub remote: Option<String>,
    pub all_remotes: bool,
    pub prune: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPullOptions {
    pub strategy: GitPullStrategy,
    pub auto_stash: bool,
    pub remote: Option<String>,
    pub branch: Option<String>,
}

impl Default for GitPullOptions {
    fn default() -> Self {
        Self {
            strategy: GitPullStrategy::Merge,
            auto_stash: true,
            remote: None,
            branch: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitPushOptions {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub include_tags: bool,
    pub force_with_lease: bool,
    pub up_to_commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPushPreview {
    pub remote: String,
    pub branch: String,
    pub source_oid: Option<String>,
    pub commits: Vec<GitCommit>,
    pub tags: Vec<String>,
    pub protected_branch: bool,
    pub force_with_lease: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitCloneOptions {
    pub shallow_depth: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCloneResult {
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitFailureKind {
    NotRepository,
    CommandFailed,
    AuthenticationRequired,
    PushRejected,
    ProtectedBranch,
    WorkingTreeDirty,
    Conflict,
    IdentityMissing,
    RepositoryLocked,
    GitTooOld,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitFailure {
    pub kind: GitFailureKind,
    pub message: String,
    pub partial_clone_path: Option<PathBuf>,
}

impl GitFailure {
    fn new(kind: GitFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            partial_clone_path: None,
        }
    }

    fn cancelled(partial_clone_path: Option<PathBuf>) -> Self {
        Self {
            kind: GitFailureKind::Cancelled,
            message: "Git 操作已取消".to_string(),
            partial_clone_path,
        }
    }
}

impl std::fmt::Display for GitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitFailure {}

#[derive(Clone, Debug)]
struct RepositoryContext {
    root: PathBuf,
    git_dir: PathBuf,
    branch: Option<String>,
    head_oid: Option<String>,
    upstream: Option<String>,
}

#[derive(Debug)]
struct CommandCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: ExitStatus,
}

pub fn repository(path: &Path) -> Result<GitRepository, GitFailure> {
    let context = repository_context(path)?;
    let remotes = list_remotes_from_root(&context.root)?;
    let dirty = command_success(
        &context.root,
        &["status", "--porcelain", "--untracked-files=normal"],
        None,
    )
    .map(|capture| !capture.stdout.is_empty())
    .unwrap_or(false);
    Ok(GitRepository {
        root: context.root.clone(),
        git_dir: context.git_dir.clone(),
        branch: context.branch,
        head_oid: context.head_oid,
        upstream: context.upstream,
        remotes,
        dirty,
        operation: operation_state(&context.git_dir),
    })
}

pub fn list_remotes(path: &Path) -> Result<Vec<GitRemote>, GitFailure> {
    let context = repository_context(path)?;
    list_remotes_from_root(&context.root)
}

fn list_remotes_from_root(root: &Path) -> Result<Vec<GitRemote>, GitFailure> {
    let capture = command_success(root, &["remote", "-v"], None)?;
    let mut remotes = BTreeMap::<String, GitRemote>::new();
    for line in String::from_utf8_lossy(&capture.stdout).lines() {
        let Some((name, rest)) = line.split_once('\t') else {
            continue;
        };
        let Some((url, kind)) = rest.rsplit_once(" (") else {
            continue;
        };
        let entry = remotes
            .entry(name.to_string())
            .or_insert_with(|| GitRemote {
                name: name.to_string(),
                fetch_url: String::new(),
                push_url: None,
            });
        match kind.trim_end_matches(')') {
            "fetch" => entry.fetch_url = url.to_string(),
            "push" if url != entry.fetch_url => entry.push_url = Some(url.to_string()),
            _ => {}
        }
    }
    Ok(remotes.into_values().collect())
}

pub fn add_remote(path: &Path, name: &str, url: &str) -> Result<(), GitFailure> {
    validate_remote_name(name)?;
    validate_remote_url(url)?;
    let context = repository_context(path)?;
    command_success(&context.root, &["remote", "add", name, url], None).map(|_| ())
}

pub fn rename_remote(path: &Path, old: &str, new: &str) -> Result<(), GitFailure> {
    validate_remote_name(old)?;
    validate_remote_name(new)?;
    let context = repository_context(path)?;
    command_success(&context.root, &["remote", "rename", old, new], None).map(|_| ())
}

pub fn remove_remote(path: &Path, name: &str) -> Result<(), GitFailure> {
    validate_remote_name(name)?;
    let context = repository_context(path)?;
    command_success(&context.root, &["remote", "remove", name], None).map(|_| ())
}

pub fn set_remote_url(
    path: &Path,
    name: &str,
    fetch_url: &str,
    push_url: Option<&str>,
) -> Result<(), GitFailure> {
    validate_remote_name(name)?;
    validate_remote_url(fetch_url)?;
    if let Some(url) = push_url {
        validate_remote_url(url)?;
    }
    let context = repository_context(path)?;
    command_success(&context.root, &["remote", "set-url", name, fetch_url], None)?;
    if let Some(url) = push_url.filter(|url| !url.is_empty()) {
        command_success(
            &context.root,
            &["remote", "set-url", "--push", name, url],
            None,
        )?;
    } else {
        let key = format!("remote.{name}.pushurl");
        let mut command = git_command(&context.root);
        command
            .args(["config", "--unset-all", &key])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = command.status();
    }
    Ok(())
}

pub fn fetch(
    path: &Path,
    options: &GitFetchOptions,
    credentials: Option<&GitCredentials>,
    monitor: &GitOperationMonitor,
) -> Result<String, GitFailure> {
    ensure_supported_git()?;
    let context = repository_context(path)?;
    let operation = GitNetworkOperation::Fetch;
    monitor.begin(operation, "正在获取远程更新…");
    let mut args = vec!["fetch", "--progress"];
    if options.prune {
        args.push("--prune");
    }
    if options.all_remotes {
        args.push("--all");
    } else if let Some(remote) = options.remote.as_deref() {
        validate_remote_name(remote)?;
        args.push(remote);
    }
    let command = network_command(&context.root, &args, credentials)?;
    let capture = run_streaming(command, operation, monitor, credentials)?;
    finish_command(operation, monitor, capture, "远程更新已获取")
}

pub fn pull(
    path: &Path,
    options: &GitPullOptions,
    credentials: Option<&GitCredentials>,
    monitor: &GitOperationMonitor,
) -> Result<String, GitFailure> {
    ensure_supported_git()?;
    let context = repository_context(path)?;
    let dirty = !command_success(
        &context.root,
        &["status", "--porcelain", "--untracked-files=normal"],
        None,
    )?
    .stdout
    .is_empty();
    if dirty && !options.auto_stash {
        return Err(GitFailure::new(
            GitFailureKind::WorkingTreeDirty,
            "工作区有未提交更改。启用自动暂存后再拉取。",
        ));
    }

    let operation = GitNetworkOperation::Pull;
    monitor.begin(operation, format!("正在{}拉取…", options.strategy.label()));
    let stashed = if dirty {
        monitor.update(GitProgressPhase::Preparing, "正在暂存未提交更改…");
        command_success(
            &context.root,
            &[
                "stash",
                "push",
                "--include-untracked",
                "-m",
                "Kite: pull auto-stash",
            ],
            credentials,
        )?;
        true
    } else {
        false
    };

    let mut args = vec![
        "pull",
        "--progress",
        "--no-edit",
        options.strategy.argument(),
    ];
    if let Some(remote) = options.remote.as_deref() {
        validate_remote_name(remote)?;
        args.push(remote);
        if let Some(branch) = options.branch.as_deref() {
            validate_branch_name(branch)?;
            args.push(branch);
        }
    }
    let command = network_command(&context.root, &args, credentials)?;
    let pull_result = run_streaming(command, operation, monitor, credentials)
        .and_then(|capture| finish_command(operation, monitor, capture, "拉取完成"));

    if stashed {
        monitor.update(GitProgressPhase::Finalizing, "正在恢复自动暂存的更改…");
        let restore = command_success(&context.root, &["stash", "pop"], credentials);
        if let Err(restore_error) = restore {
            monitor.finish(GitProgressPhase::Failed, "拉取完成，但恢复更改失败");
            return Err(GitFailure::new(
                GitFailureKind::Conflict,
                format!(
                    "{}；自动暂存的更改未能恢复：{}",
                    pull_result
                        .err()
                        .map(|failure| failure.message)
                        .unwrap_or_else(|| "拉取完成".to_string()),
                    restore_error.message
                ),
            ));
        }
    }

    pull_result
}

pub fn push_preview(path: &Path, options: &GitPushOptions) -> Result<GitPushPreview, GitFailure> {
    let context = repository_context(path)?;
    let remote = resolve_remote(&context, options.remote.as_deref())?;
    let branch = resolve_push_branch(&context, options.branch.as_deref())?;
    validate_branch_name(&branch)?;
    let source = options.up_to_commit.as_deref().unwrap_or("HEAD");
    let source_oid = rev_parse_optional(&context.root, source)?;
    let remote_ref = format!("refs/remotes/{remote}/{branch}");
    let base = rev_parse_optional(&context.root, &remote_ref)?;
    let commits = commits_between(&context.root, source, base.as_deref())?;
    let tags = tags_between(&context.root, source, base.as_deref())?;
    Ok(GitPushPreview {
        remote: remote.clone(),
        branch: branch.clone(),
        source_oid,
        commits,
        tags,
        protected_branch: is_protected_branch(&context, &branch),
        force_with_lease: options.force_with_lease,
    })
}

pub fn commit_files(path: &Path, oid: &str) -> Result<Vec<GitFileChange>, GitFailure> {
    validate_object_id(oid)?;
    let context = repository_context(path)?;
    let capture = command_success(
        &context.root,
        &[
            "show",
            "--format=",
            "--name-status",
            "--find-renames",
            "--no-ext-diff",
            oid,
            "--",
        ],
        None,
    )?;
    let mut changes = Vec::new();
    for line in String::from_utf8_lossy(&capture.stdout).lines() {
        let mut fields = line.split('\t');
        let Some(status) = fields.next().filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(first_path) = fields.next() else {
            continue;
        };
        let original_path = fields.next().map(ToOwned::to_owned);
        let display_path = original_path.as_deref().unwrap_or(first_path).to_string();
        changes.push(GitFileChange {
            status: status.chars().next().unwrap_or('?').to_string(),
            path: display_path,
            original_path: original_path.map(|_| first_path.to_string()),
        });
    }
    Ok(changes)
}

pub fn push(
    path: &Path,
    options: &GitPushOptions,
    credentials: Option<&GitCredentials>,
    monitor: &GitOperationMonitor,
) -> Result<String, GitFailure> {
    ensure_supported_git()?;
    let context = repository_context(path)?;
    let branch = resolve_push_branch(&context, options.branch.as_deref())?;
    validate_branch_name(&branch)?;
    if options.force_with_lease && is_protected_branch(&context, &branch) {
        return Err(GitFailure::new(
            GitFailureKind::ProtectedBranch,
            format!("分支 {branch} 受保护，不能强制推送。"),
        ));
    }
    let remote = resolve_remote(&context, options.remote.as_deref())?;

    let source = options.up_to_commit.as_deref().unwrap_or("HEAD");
    let source_oid = rev_parse_optional(&context.root, source)?.ok_or_else(|| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("找不到要推送的提交：{source}"),
        )
    })?;
    let args = push_arguments(&remote, &branch, &source_oid, options);
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();

    let operation = GitNetworkOperation::Push;
    monitor.begin(operation, format!("正在推送到 {remote}/{branch}…"));
    let command = network_command(&context.root, &args, credentials)?;
    let capture = run_streaming(command, operation, monitor, credentials)?;
    finish_command(
        operation,
        monitor,
        capture,
        &format!("已推送到 {remote}/{branch}"),
    )
}

fn push_arguments(
    remote: &str,
    branch: &str,
    source_oid: &str,
    options: &GitPushOptions,
) -> Vec<String> {
    let mut args = vec!["push".to_string(), "--progress".to_string()];
    if options.include_tags {
        args.push("--tags".to_string());
    }
    if options.force_with_lease {
        args.push(format!("--force-with-lease=refs/heads/{branch}"));
    }
    args.push(remote.to_string());
    args.push(format!("{source_oid}:refs/heads/{branch}"));
    args
}

pub fn clone_repository(
    url: &str,
    target: &Path,
    options: &GitCloneOptions,
    credentials: Option<&GitCredentials>,
    monitor: &GitOperationMonitor,
) -> Result<GitCloneResult, GitFailure> {
    ensure_supported_git()?;
    validate_remote_url(url)?;
    if options.shallow_depth == Some(0) {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "浅克隆深度必须大于 0。",
        ));
    }
    if target.exists() {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "目标目录已存在。请选择新的目录。",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| GitFailure::new(GitFailureKind::CommandFailed, "目标目录缺少父目录。"))?;
    fs::create_dir_all(parent).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法创建目标目录的父目录：{error}"),
        )
    })?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| GitFailure::new(GitFailureKind::CommandFailed, "目标目录名称无效。"))?;
    let staging = staging_clone_path(parent, name);

    let operation = GitNetworkOperation::Clone;
    monitor.begin(operation, format!("正在克隆到 {}…", target.display()));
    let mut args = vec!["clone", "--progress"];
    let depth = options.shallow_depth.map(|value| value.to_string());
    if let Some(depth) = depth.as_deref() {
        args.push("--depth");
        args.push(depth);
    }
    args.push("--");
    args.push(url);
    let staging_text = staging.to_string_lossy().into_owned();
    args.push(&staging_text);
    let command = network_command(parent, &args, credentials)?;
    let result = run_streaming(command, operation, monitor, credentials);
    let capture = match result {
        Ok(capture) => capture,
        Err(failure) if failure.kind == GitFailureKind::Cancelled => {
            let partial = staging.exists().then(|| staging.clone());
            let mut cancelled = GitFailure::cancelled(partial);
            let partial_detail = if staging.exists() {
                format!("未完成目录保留在 {}", staging.display())
            } else {
                String::new()
            };
            cancelled.message = format!("克隆已取消。{partial_detail}");
            return Err(cancelled);
        }
        Err(mut failure) => {
            if staging.exists() {
                failure.partial_clone_path = Some(staging.clone());
            }
            return Err(failure);
        }
    };
    if !capture.status.success() {
        let failure = command_failure("克隆仓库", &capture, Some(staging.clone()));
        return Err(failure);
    }
    fs::rename(&staging, target).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!(
                "仓库已下载，但无法移动到目标目录：{error}。未完成目录保留在 {}",
                staging.display()
            ),
        )
    })?;
    monitor.finish(GitProgressPhase::Completed, "克隆完成");
    Ok(GitCloneResult {
        path: target.to_path_buf(),
    })
}

pub fn init_repository(path: &Path) -> Result<PathBuf, GitFailure> {
    ensure_supported_git()?;
    fs::create_dir_all(path).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法创建目录：{error}"),
        )
    })?;
    command_success(path, &["init"], None)?;
    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(canonical)
}

pub fn delete_partial_clone(path: &Path) -> Result<(), GitFailure> {
    if !is_partial_clone_path(path) {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "拒绝删除不是 Kite 克隆临时目录的路径。",
        ));
    }
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| {
            GitFailure::new(
                GitFailureKind::CommandFailed,
                format!("无法删除未完成的克隆目录：{error}"),
            )
        })?;
    }
    Ok(())
}

pub fn adopt_partial_clone(path: &Path, target: &Path) -> Result<(), GitFailure> {
    if !is_partial_clone_path(path) {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "拒绝移动不是 Kite 克隆临时目录的路径。",
        ));
    }
    if target.exists() {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "目标目录已存在。",
        ));
    }
    fs::rename(path, target).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法保留未完成的克隆目录：{error}"),
        )
    })
}

fn repository_context(path: &Path) -> Result<RepositoryContext, GitFailure> {
    ensure_supported_git()?;
    let start = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let mut command = git_command(start);
    command.args(["rev-parse", "--show-toplevel", "--absolute-git-dir"]);
    let capture = output_command(command).map_err(|error| {
        GitFailure::new(
            GitFailureKind::NotRepository,
            format!("未找到 Git：{error}"),
        )
    })?;
    if !capture.status.success() {
        return Err(GitFailure::new(
            GitFailureKind::NotRepository,
            "所选目录不在 Git 工作区中。",
        ));
    }
    let text = String::from_utf8_lossy(&capture.stdout);
    let mut lines = text.lines();
    let Some(root) = lines.next().filter(|value| !value.is_empty()) else {
        return Err(GitFailure::new(
            GitFailureKind::NotRepository,
            "Git 未返回工作区路径。",
        ));
    };
    let Some(git_dir) = lines.next().filter(|value| !value.is_empty()) else {
        return Err(GitFailure::new(
            GitFailureKind::NotRepository,
            "Git 未返回仓库目录。",
        ));
    };
    let root = dunce::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let git_dir = dunce::canonicalize(git_dir).unwrap_or_else(|_| PathBuf::from(git_dir));
    let branch = git_stdout_optional(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let head_oid = git_stdout_optional(&root, &["rev-parse", "--verify", "HEAD"])?;
    let upstream = git_stdout_optional(
        &root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    Ok(RepositoryContext {
        root,
        git_dir,
        branch,
        head_oid,
        upstream,
    })
}

fn resolve_remote(
    context: &RepositoryContext,
    requested: Option<&str>,
) -> Result<String, GitFailure> {
    if let Some(remote) = requested.filter(|value| !value.trim().is_empty()) {
        validate_remote_name(remote)?;
        return Ok(remote.to_string());
    }
    if let Some(upstream) = context.upstream.as_deref()
        && let Some((remote, _)) = upstream.split_once('/')
    {
        return Ok(remote.to_string());
    }
    if let Some(branch) = context.branch.as_deref()
        && let Some(remote) = git_stdout_optional(
            &context.root,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )?
        && remote != "."
    {
        return Ok(remote);
    }
    let remotes = list_remotes_from_root(&context.root)?;
    if remotes.iter().any(|remote| remote.name == "origin") {
        Ok("origin".to_string())
    } else {
        remotes
            .first()
            .map(|remote| remote.name.clone())
            .ok_or_else(|| GitFailure::new(GitFailureKind::CommandFailed, "仓库没有配置远程。"))
    }
}

fn resolve_push_branch(
    context: &RepositoryContext,
    requested: Option<&str>,
) -> Result<String, GitFailure> {
    if let Some(branch) = requested.filter(|value| !value.trim().is_empty()) {
        return Ok(branch.to_string());
    }
    context.branch.clone().ok_or_else(|| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            "当前处于分离 HEAD 状态，请选择目标分支。",
        )
    })
}

fn commits_between(
    root: &Path,
    source: &str,
    base: Option<&str>,
) -> Result<Vec<GitCommit>, GitFailure> {
    let mut args = vec![
        "log",
        "--reverse",
        "--date=unix",
        "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e",
        source,
    ];
    if let Some(base) = base {
        args.push("--not");
        args.push(base);
    }
    let capture = command_success(root, &args, None)?;
    let text = String::from_utf8_lossy(&capture.stdout);
    let mut commits = Vec::new();
    for record in text.split('\x1e') {
        let record = record.trim_matches(['\r', '\n']);
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split('\x1f');
        let Some(oid) = fields.next().filter(|value| !value.is_empty()) else {
            continue;
        };
        commits.push(GitCommit {
            oid: oid.to_string(),
            short_oid: fields.next().unwrap_or_default().to_string(),
            author: fields.next().unwrap_or_default().to_string(),
            unix_seconds: fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            subject: fields.next().unwrap_or_default().to_string(),
        });
    }
    Ok(commits)
}

fn tags_between(root: &Path, source: &str, base: Option<&str>) -> Result<Vec<String>, GitFailure> {
    let mut args = vec!["tag", "--merged", source];
    if let Some(base) = base {
        args.push("--no-merged");
        args.push(base);
    }
    let capture = command_success(root, &args, None)?;
    Ok(String::from_utf8_lossy(&capture.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn rev_parse_optional(root: &Path, revision: &str) -> Result<Option<String>, GitFailure> {
    let capture = command_success(root, &["rev-parse", "--verify", "--quiet", revision], None).ok();
    Ok(capture.and_then(|capture| {
        capture
            .status
            .success()
            .then(|| String::from_utf8_lossy(&capture.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    }))
}

fn git_stdout_optional(root: &Path, args: &[&str]) -> Result<Option<String>, GitFailure> {
    let capture = command_success(root, args, None);
    match capture {
        Ok(capture) if capture.status.success() => {
            let value = String::from_utf8_lossy(&capture.stdout).trim().to_string();
            Ok((!value.is_empty()).then_some(value))
        }
        Ok(_) => Ok(None),
        Err(failure) if failure.kind == GitFailureKind::CommandFailed => Ok(None),
        Err(failure) => Err(failure),
    }
}

fn is_protected_branch(context: &RepositoryContext, branch: &str) -> bool {
    if matches!(branch, "main" | "master" | "trunk")
        || branch.starts_with("release/")
        || branch.starts_with("hotfix/")
    {
        return true;
    }
    let bool_key = format!("branch.{branch}.kiteProtected");
    if git_stdout_optional(&context.root, &["config", "--bool", "--get", &bool_key])
        .ok()
        .flatten()
        .is_some_and(|value| value == "true")
    {
        return true;
    }
    let configured = git_stdout_optional(
        &context.root,
        &["config", "--get-all", "kite.protectedBranch"],
    )
    .ok()
    .flatten()
    .unwrap_or_default();
    configured
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .any(|pattern| protected_pattern_matches(pattern, branch))
}

fn protected_pattern_matches(pattern: &str, branch: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if pattern == branch {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return branch.starts_with(prefix);
    }
    false
}

fn operation_state(git_dir: &Path) -> Option<String> {
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        Some("rebase".to_string())
    } else if git_dir.join("MERGE_HEAD").exists() {
        Some("merge".to_string())
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        Some("cherry-pick".to_string())
    } else if git_dir.join("REVERT_HEAD").exists() {
        Some("revert".to_string())
    } else if git_dir.join("BISECT_LOG").exists() {
        Some("bisect".to_string())
    } else {
        None
    }
}

fn ensure_supported_git() -> Result<(), GitFailure> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .map_err(|error| {
            GitFailure::new(
                GitFailureKind::CommandFailed,
                format!("未找到 Git。请确认 Git 已安装并位于 PATH 中：{error}"),
            )
        })?;
    if !output.status.success() {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "无法读取 Git 版本。",
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .find_map(|field| {
            let mut parts = field.split('.');
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .unwrap_or((0, 0));
    if version < MIN_GIT_VERSION {
        return Err(GitFailure::new(
            GitFailureKind::GitTooOld,
            format!(
                "Git {}.{} 或更高版本是必需的，当前版本为 {}。",
                MIN_GIT_VERSION.0,
                MIN_GIT_VERSION.1,
                version.0 as f64 + version.1 as f64 / 10.0
            ),
        ));
    }
    Ok(())
}

fn validate_remote_name(name: &str) -> Result<(), GitFailure> {
    if name.trim().is_empty()
        || name.starts_with('-')
        || name.contains([' ', '\t', '\r', '\n', '\0'])
    {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "远程名称无效。",
        ));
    }
    Ok(())
}

fn validate_branch_name(name: &str) -> Result<(), GitFailure> {
    if name.trim().is_empty()
        || name.starts_with('-')
        || name.contains("..")
        || name.contains([
            ' ', '\t', '\r', '\n', '\0', '~', '^', ':', '?', '*', '[', '\\',
        ])
    {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "分支名称无效。",
        ));
    }
    Ok(())
}

fn validate_remote_url(url: &str) -> Result<(), GitFailure> {
    if url.trim().is_empty() || url.starts_with('-') || url.contains(['\r', '\n', '\0']) {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "远程 URL 无效。",
        ));
    }
    Ok(())
}

fn validate_object_id(value: &str) -> Result<(), GitFailure> {
    if value.is_empty()
        || value.len() > 64
        || !value.chars().all(|character| character.is_ascii_hexdigit())
    {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "提交标识无效。",
        ));
    }
    Ok(())
}

fn git_command(current_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(current_dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("LC_ALL", "C")
        .env("LANG", "C");
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn network_command(
    current_dir: &Path,
    args: &[&str],
    credentials: Option<&GitCredentials>,
) -> Result<Command, GitFailure> {
    let mut command = git_command(current_dir);
    command.args(args);
    if credentials.is_some() {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "credential.helper")
            .env("GIT_CONFIG_VALUE_0", "");
    }
    Ok(command)
}

fn command_success(
    current_dir: &Path,
    args: &[&str],
    _credentials: Option<&GitCredentials>,
) -> Result<CommandCapture, GitFailure> {
    let mut command = git_command(current_dir);
    command.args(args);
    let capture = output_command(command).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法运行 Git：{error}"),
        )
    })?;
    if capture.status.success() {
        Ok(capture)
    } else {
        Err(command_failure("Git 操作", &capture, None))
    }
}

fn output_command(mut command: Command) -> std::io::Result<CommandCapture> {
    let output = command.output()?;
    Ok(CommandCapture {
        stdout: output.stdout,
        stderr: output.stderr,
        status: output.status,
    })
}

fn run_streaming(
    mut command: Command,
    operation: GitNetworkOperation,
    monitor: &GitOperationMonitor,
    credentials: Option<&GitCredentials>,
) -> Result<CommandCapture, GitFailure> {
    let _askpass = configure_askpass(&mut command, credentials)?;
    #[cfg(unix)]
    command.process_group(0);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法启动 {}：{error}", operation.label()),
        )
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (sender, receiver) = mpsc::channel::<(bool, Vec<u8>)>();
    let stdout_thread = stdout.map(|stream| read_stream(stream, false, sender.clone()));
    let stderr_thread = stderr.map(|stream| read_stream(stream, true, sender));

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    loop {
        while let Ok((is_stderr, chunk)) = receiver.try_recv() {
            if is_stderr {
                capture_output_chunk(&mut stderr_bytes, &chunk);
            } else {
                capture_output_chunk(&mut stdout_bytes, &chunk);
            }
            let text = String::from_utf8_lossy(&chunk);
            for line in text.split(['\r', '\n']) {
                monitor.record_line(line);
            }
        }
        if monitor.is_cancelled() {
            terminate_child(&mut child);
            let status = child.wait().map_err(|error| {
                GitFailure::new(
                    GitFailureKind::CommandFailed,
                    format!("等待 Git 进程结束时失败：{error}"),
                )
            })?;
            if let Some(thread) = stdout_thread {
                let _ = thread.join();
            }
            if let Some(thread) = stderr_thread {
                let _ = thread.join();
            }
            monitor.finish(GitProgressPhase::Cancelled, "操作已取消");
            let _ = status;
            return Err(GitFailure::cancelled(None));
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            GitFailure::new(
                GitFailureKind::CommandFailed,
                format!("无法读取 Git 进程状态：{error}"),
            )
        })? {
            if let Some(thread) = stdout_thread {
                let _ = thread.join();
            }
            if let Some(thread) = stderr_thread {
                let _ = thread.join();
            }
            while let Ok((is_stderr, chunk)) = receiver.try_recv() {
                if is_stderr {
                    capture_output_chunk(&mut stderr_bytes, &chunk);
                } else {
                    capture_output_chunk(&mut stdout_bytes, &chunk);
                }
            }
            return Ok(CommandCapture {
                stdout: stdout_bytes,
                stderr: stderr_bytes,
                status,
            });
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", child.id())])
            .status();
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = command.status();
    }
    let _ = child.kill();
}

fn read_stream<R: Read + Send + 'static>(
    mut stream: R,
    is_stderr: bool,
    sender: mpsc::Sender<(bool, Vec<u8>)>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 2048];
        while let Ok(read) = stream.read(&mut buffer) {
            if read == 0 {
                break;
            }
            if sender.send((is_stderr, buffer[..read].to_vec())).is_err() {
                break;
            }
        }
    })
}

fn capture_output_chunk(buffer: &mut Vec<u8>, chunk: &[u8]) {
    if buffer.len() >= MAX_CAPTURE_BYTES {
        return;
    }
    let remaining = MAX_CAPTURE_BYTES - buffer.len();
    buffer.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}

fn finish_command(
    operation: GitNetworkOperation,
    monitor: &GitOperationMonitor,
    capture: CommandCapture,
    success_message: &str,
) -> Result<String, GitFailure> {
    if !capture.status.success() {
        monitor.finish(
            GitProgressPhase::Failed,
            format!("{}失败", operation.label()),
        );
        return Err(command_failure(operation.label(), &capture, None));
    }
    monitor.finish(GitProgressPhase::Completed, success_message);
    Ok(success_message.to_string())
}

fn command_failure(
    label: &str,
    capture: &CommandCapture,
    partial_clone_path: Option<PathBuf>,
) -> GitFailure {
    let stderr = String::from_utf8_lossy(&capture.stderr);
    let stdout = String::from_utf8_lossy(&capture.stdout);
    let lower = format!("{stderr}\n{stdout}").to_ascii_lowercase();
    let kind = if lower.contains("could not read username")
        || lower.contains("could not read password")
        || lower.contains("authentication failed")
        || lower.contains("permission denied (publickey")
        || lower.contains("terminal prompts disabled")
        || lower.contains("http basic: access denied")
        || lower.contains("invalid username or password")
    {
        GitFailureKind::AuthenticationRequired
    } else if lower.contains("[rejected]")
        || lower.contains("non-fast-forward")
        || lower.contains("fetch first")
        || lower.contains("stale info")
    {
        GitFailureKind::PushRejected
    } else if lower.contains("protected branch") || lower.contains("pre-receive hook declined") {
        GitFailureKind::ProtectedBranch
    } else if lower.contains("conflict") || lower.contains("needs merge") {
        GitFailureKind::Conflict
    } else if lower.contains("please tell me who you are")
        || lower.contains("author identity unknown")
    {
        GitFailureKind::IdentityMissing
    } else if lower.contains("index.lock") || lower.contains("another git process") {
        GitFailureKind::RepositoryLocked
    } else {
        GitFailureKind::CommandFailed
    };
    let detail = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let detail = sanitize_git_output(detail.as_ref());
    let message = if detail.is_empty() {
        format!(
            "{label}失败，退出码 {}",
            capture.status.code().unwrap_or(-1)
        )
    } else {
        format!("{label}失败：{detail}")
    };
    GitFailure {
        kind,
        message,
        partial_clone_path,
    }
}

fn sanitize_git_output(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len().min(800));
    for token in value.split_whitespace() {
        if !redacted.is_empty() {
            redacted.push(' ');
        }
        if let Some(scheme) = token.find("://")
            && token[scheme + 3..].contains('@')
        {
            redacted.push_str(&token[..scheme + 3]);
            redacted.push_str("***@");
            if let Some(host) = token[scheme + 3..].split_once('@').map(|(_, host)| host) {
                redacted.push_str(host);
            }
        } else {
            redacted.push_str(token);
        }
        if redacted.len() >= 800 {
            break;
        }
    }
    redacted.chars().take(800).collect()
}

struct AskpassScript {
    directory: PathBuf,
}

impl Drop for AskpassScript {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn configure_askpass(
    command: &mut Command,
    credentials: Option<&GitCredentials>,
) -> Result<Option<AskpassScript>, GitFailure> {
    let Some(credentials) = credentials else {
        return Ok(None);
    };
    if credentials.username.contains(['\r', '\n', '\0'])
        || credentials.password.contains(['\r', '\n', '\0'])
    {
        return Err(GitFailure::new(
            GitFailureKind::CommandFailed,
            "凭据包含不支持的字符。",
        ));
    }
    let id = NEXT_CLONE_ID.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("kite-git-askpass-{}-{id}", std::process::id()));
    fs::create_dir_all(&directory).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法创建凭据辅助目录：{error}"),
        )
    })?;

    #[cfg(target_os = "windows")]
    let script = directory.join("askpass.cmd");
    #[cfg(not(target_os = "windows"))]
    let script = directory.join("askpass.sh");

    #[cfg(target_os = "windows")]
    let contents = "@echo off\r\npowershell -NoProfile -NonInteractive -Command \"if ($args[0] -match 'sername') { [Console]::Out.Write($env:KITE_GIT_USERNAME) } else { [Console]::Out.Write($env:KITE_GIT_PASSWORD) }\" -- %*\r\n";
    #[cfg(not(target_os = "windows"))]
    let contents = "#!/bin/sh\ncase \"$1\" in\n  *sername*) printf '%s\\n' \"$KITE_GIT_USERNAME\" ;;\n  *) printf '%s\\n' \"$KITE_GIT_PASSWORD\" ;;\nesac\n";

    let mut file = fs::File::create(&script).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法创建凭据辅助程序：{error}"),
        )
    })?;
    file.write_all(contents.as_bytes()).map_err(|error| {
        GitFailure::new(
            GitFailureKind::CommandFailed,
            format!("无法写入凭据辅助程序：{error}"),
        )
    })?;
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).map_err(|error| {
            GitFailure::new(
                GitFailureKind::CommandFailed,
                format!("无法保护凭据辅助程序：{error}"),
            )
        })?;
    }

    command
        .env("GIT_ASKPASS", &script)
        .env("SSH_ASKPASS", &script)
        .env("GIT_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("KITE_GIT_USERNAME", &credentials.username)
        .env("KITE_GIT_PASSWORD", &credentials.password);
    Ok(Some(AskpassScript { directory }))
}

fn staging_clone_path(parent: &Path, name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_CLONE_ID.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{name}.kite-clone-{}-{stamp}-{id}",
        std::process::id()
    ))
}

fn is_partial_clone_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name.contains(".kite-clone-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn run(root: &Path, args: &[&str]) -> CommandCapture {
        let mut command = Command::new("git");
        command.current_dir(root).args(args);
        output_command(command).expect("run git")
    }

    fn git_ok(root: &Path, args: &[&str]) -> CommandCapture {
        let capture = run(root, args);
        assert!(
            capture.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&capture.stderr)
        );
        capture
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kite-git-remote-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn configured_repo(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("create repo");
        git_ok(root, &["init", "--quiet", "-b", "main"]);
        git_ok(root, &["config", "user.email", "kite@example.com"]);
        git_ok(root, &["config", "user.name", "Kite Test"]);
        git_ok(root, &["config", "core.autocrlf", "false"]);
        root.to_path_buf()
    }

    fn commit(path: &Path, name: &str, contents: &str, message: &str) -> String {
        fs::write(path.join(name), contents).expect("write file");
        git_ok(path, &["add", name]);
        git_ok(path, &["commit", "--quiet", "-m", message]);
        String::from_utf8_lossy(&git_ok(path, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string()
    }

    #[test]
    fn rejects_force_push_to_protected_branch_before_network() {
        if !git_available() {
            return;
        }
        let root = configured_repo(&temp_root("protected"));
        commit(&root, "a.txt", "one\n", "initial");
        let monitor = GitOperationMonitor::new();
        let error = push(
            &root,
            &GitPushOptions {
                force_with_lease: true,
                ..GitPushOptions::default()
            },
            None,
            &monitor,
        )
        .expect_err("protected branch must reject force push");
        assert_eq!(error.kind, GitFailureKind::ProtectedBranch);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn force_push_uses_only_force_with_lease() {
        let args = push_arguments(
            "origin",
            "feature/test",
            "0123456789abcdef",
            &GitPushOptions {
                force_with_lease: true,
                include_tags: true,
                ..GitPushOptions::default()
            },
        );
        assert!(args.contains(&"--force-with-lease=refs/heads/feature/test".to_string()));
        assert!(args.contains(&"--tags".to_string()));
        assert!(!args.iter().any(|argument| argument == "--force"));
        assert_eq!(
            args.last().map(String::as_str),
            Some("0123456789abcdef:refs/heads/feature/test")
        );
    }

    #[test]
    fn detects_rejected_push_with_a_local_bare_remote() {
        if !git_available() {
            return;
        }
        let root = temp_root("rejected");
        let remote = root.join("remote.git");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&root).expect("create test root");
        git_ok(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_string_lossy().as_ref(),
            ],
        );
        configured_repo(&first);
        git_ok(&first, &["config", "user.email", "kite@example.com"]);
        git_ok(&first, &["config", "user.name", "Kite Test"]);
        commit(&first, "a.txt", "one\n", "initial");
        git_ok(
            &first,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );
        git_ok(&first, &["push", "--quiet", "-u", "origin", "main"]);
        git_ok(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_ok(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_string_lossy().as_ref(),
                second.to_string_lossy().as_ref(),
            ],
        );
        git_ok(&second, &["config", "user.email", "kite@example.com"]);
        git_ok(&second, &["config", "user.name", "Kite Test"]);
        commit(&second, "b.txt", "remote\n", "remote change");
        git_ok(&second, &["push", "--quiet", "origin", "main"]);
        commit(&first, "c.txt", "local\n", "local change");

        let monitor = GitOperationMonitor::new();
        let error = push(&first, &GitPushOptions::default(), None, &monitor)
            .expect_err("push must be rejected");
        assert_eq!(error.kind, GitFailureKind::PushRejected);
        assert!(error.message.contains("拒绝") || error.message.contains("push"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rebase_then_push_recovers_a_rejected_push() {
        if !git_available() {
            return;
        }
        let root = temp_root("recover-rejected");
        let remote = root.join("remote.git");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&root).expect("create test root");
        git_ok(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_string_lossy().as_ref(),
            ],
        );
        configured_repo(&first);
        commit(&first, "a.txt", "one\n", "initial");
        git_ok(
            &first,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );
        git_ok(&first, &["push", "--quiet", "-u", "origin", "main"]);
        git_ok(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_ok(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_string_lossy().as_ref(),
                second.to_string_lossy().as_ref(),
            ],
        );
        git_ok(&second, &["config", "user.email", "kite@example.com"]);
        git_ok(&second, &["config", "user.name", "Kite Test"]);
        commit(&second, "remote.txt", "remote\n", "remote change");
        git_ok(&second, &["push", "--quiet", "origin", "main"]);
        commit(&first, "local.txt", "local\n", "local change");

        let monitor = GitOperationMonitor::new();
        assert_eq!(
            push(&first, &GitPushOptions::default(), None, &monitor)
                .expect_err("initial push rejected")
                .kind,
            GitFailureKind::PushRejected
        );
        pull(
            &first,
            &GitPullOptions {
                strategy: GitPullStrategy::Rebase,
                auto_stash: true,
                remote: None,
                branch: None,
            },
            None,
            &monitor,
        )
        .expect("rebase pull");
        push(&first, &GitPushOptions::default(), None, &monitor).expect("push after rebase");

        let remote_log = git_ok(&remote, &["log", "--format=%s", "main"]);
        let remote_log = String::from_utf8_lossy(&remote_log.stdout);
        assert!(remote_log.contains("local change"));
        assert!(remote_log.contains("remote change"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn previews_pending_commits_and_commit_files() {
        if !git_available() {
            return;
        }
        let root = temp_root("preview");
        let remote = root.join("remote.git");
        let work = root.join("work");
        fs::create_dir_all(&root).expect("create test root");
        git_ok(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_string_lossy().as_ref(),
            ],
        );
        configured_repo(&work);
        commit(&work, "a.txt", "one\n", "initial");
        git_ok(
            &work,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );
        git_ok(&work, &["push", "--quiet", "-u", "origin", "main"]);
        git_ok(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let oid = commit(&work, "b.txt", "two\n", "second commit");
        git_ok(&work, &["fetch", "--quiet", "origin"]);

        let preview = push_preview(&work, &GitPushOptions::default()).expect("preview");
        assert_eq!(preview.remote, "origin");
        assert_eq!(preview.branch, "main");
        assert_eq!(preview.commits.len(), 1);
        assert_eq!(preview.commits[0].subject, "second commit");
        let files = commit_files(&work, &oid).expect("commit files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "b.txt");
        assert_eq!(files[0].status, "A");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pull_auto_stashes_and_restores_local_changes() {
        if !git_available() {
            return;
        }
        let root = temp_root("pull-stash");
        let remote = root.join("remote.git");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&root).expect("create test root");
        git_ok(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_string_lossy().as_ref(),
            ],
        );
        configured_repo(&first);
        commit(&first, "a.txt", "one\n", "initial");
        git_ok(
            &first,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );
        git_ok(&first, &["push", "--quiet", "-u", "origin", "main"]);
        git_ok(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_ok(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_string_lossy().as_ref(),
                second.to_string_lossy().as_ref(),
            ],
        );
        git_ok(&second, &["config", "user.email", "kite@example.com"]);
        git_ok(&second, &["config", "user.name", "Kite Test"]);
        commit(&second, "remote.txt", "remote\n", "remote change");
        git_ok(&second, &["push", "--quiet", "origin", "main"]);
        fs::write(first.join("local.txt"), "local\n").expect("write local change");

        let monitor = GitOperationMonitor::new();
        pull(&first, &GitPullOptions::default(), None, &monitor).expect("pull with stash");
        assert!(first.join("remote.txt").is_file());
        assert_eq!(
            fs::read(first.join("local.txt")).expect("local file"),
            b"local\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn progressive_command_can_be_cancelled() {
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "printf 'Receiving objects: 10%'; sleep 10"]);
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "ping -n 20 127.0.0.1 >NUL"]);
            command
        };
        let monitor = GitOperationMonitor::new();
        monitor.begin(GitNetworkOperation::Fetch, "test");
        let worker_monitor = monitor.clone();
        let worker = thread::spawn(move || {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            run_streaming(command, GitNetworkOperation::Fetch, &worker_monitor, None)
        });
        thread::sleep(Duration::from_millis(150));
        monitor.cancel();
        let error = worker.join().expect("worker").expect_err("cancelled");
        assert_eq!(error.kind, GitFailureKind::Cancelled);
        assert_eq!(monitor.snapshot().phase, GitProgressPhase::Cancelled);
    }

    #[test]
    fn remote_management_round_trips() {
        if !git_available() {
            return;
        }
        let root = configured_repo(&temp_root("remotes"));
        add_remote(&root, "origin", "https://example.com/repo.git").expect("add");
        let remotes = list_remotes(&root).expect("list");
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "origin");
        set_remote_url(
            &root,
            "origin",
            "https://example.com/new.git",
            Some("ssh://example.com/push.git"),
        )
        .expect("set url");
        set_remote_url(&root, "origin", "https://example.com/new.git", None)
            .expect("clear push url");
        let remotes = list_remotes(&root).expect("list after clearing push url");
        assert_eq!(remotes[0].push_url, None);
        rename_remote(&root, "origin", "upstream").expect("rename");
        remove_remote(&root, "upstream").expect("remove");
        assert!(list_remotes(&root).expect("list empty").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clones_through_a_staging_directory_and_opens_the_target() {
        if !git_available() {
            return;
        }
        let root = temp_root("clone-success");
        let source = root.join("source");
        let target = root.join("target");
        configured_repo(&source);
        commit(&source, "README.md", "hello\n", "initial");

        let monitor = GitOperationMonitor::new();
        let result = clone_repository(
            source.to_string_lossy().as_ref(),
            &target,
            &GitCloneOptions::default(),
            None,
            &monitor,
        )
        .expect("clone succeeds");
        assert_eq!(result.path, target);
        assert_eq!(
            fs::read(target.join("README.md")).expect("cloned file"),
            b"hello\n"
        );
        assert!(
            fs::read_dir(&root)
                .expect("read clone root")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".kite-clone-"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn partial_clone_delete_is_limited_to_kite_staging_directories() {
        let root = temp_root("partial");
        fs::create_dir_all(&root).expect("create root");
        let unrelated = root.join("important");
        fs::create_dir_all(&unrelated).expect("create unrelated");
        assert!(delete_partial_clone(&unrelated).is_err());
        let staging = staging_clone_path(&root, "repo");
        fs::create_dir_all(&staging).expect("create staging");
        delete_partial_clone(&staging).expect("delete staging");
        assert!(!staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_authentication_and_rejection_errors() {
        let status = Command::new("sh")
            .args(["-c", "exit 1"])
            .status()
            .or_else(|_| Command::new("cmd").args(["/C", "exit 1"]).status())
            .expect("create failed process status");
        let auth = command_failure(
            "fetch",
            &CommandCapture {
                stdout: Vec::new(),
                stderr: b"fatal: could not read Username for 'https://example.com': terminal prompts disabled".to_vec(),
                status,
            },
            None,
        );
        assert_eq!(auth.kind, GitFailureKind::AuthenticationRequired);
        let rejected = command_failure(
            "push",
            &CommandCapture {
                stdout: Vec::new(),
                stderr: b"! [rejected] main -> main (fetch first)".to_vec(),
                status: Command::new("git")
                    .arg("--version")
                    .output()
                    .unwrap()
                    .status,
            },
            None,
        );
        assert_eq!(rejected.kind, GitFailureKind::PushRejected);
    }
}
