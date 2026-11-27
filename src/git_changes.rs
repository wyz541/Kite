//! Repository-wide Git change inspection and hunk/line-level staging.
//!
//! The UI receives a flat, stable change list from one `git status` process.
//! Whole-path operations delegate to Git, while partial operations build a
//! small full-file patch and apply it to the index or work tree. Keeping the
//! patch construction in Core gives Windows and macOS identical semantics.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::fs::VcsStatus;
use crate::git::{GitHead, GitOperationState, GitRepository};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const MAX_DIFF_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIFF_LINES: usize = 20_000;
const MAX_WORKTREE_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitChangeGroup {
    Staged,
    Unstaged,
    Untracked,
    Conflicted,
    Ignored,
}

impl GitChangeGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Staged => "已暂存",
            Self::Unstaged => "未暂存",
            Self::Untracked => "未跟踪",
            Self::Conflicted => "冲突",
            Self::Ignored => "已忽略",
        }
    }

    fn order(self) -> u8 {
        match self {
            Self::Staged => 0,
            Self::Unstaged => 1,
            Self::Conflicted => 2,
            Self::Untracked => 3,
            Self::Ignored => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeFile {
    pub path: String,
    pub old_path: Option<String>,
    pub group: GitChangeGroup,
    pub status: VcsStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryChanges {
    pub repository_root: PathBuf,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    /// Structured HEAD state from the same status snapshot. Detached and unborn
    /// HEAD must stay distinct from an ordinary branch with an empty name.
    pub head: Option<GitHead>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<GitOperationState>,
    pub files: Vec<GitChangeFile>,
    pub crlf_warning: Option<String>,
    pub includes_ignored: bool,
}

impl GitRepositoryChanges {
    pub fn count(&self, group: GitChangeGroup) -> usize {
        self.files.iter().filter(|file| file.group == group).count()
    }

    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitDiffScope {
    Unstaged,
    Staged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHunkLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHunkLine {
    pub kind: GitHunkLineKind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffHunk {
    pub index: u32,
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<GitHunkLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitFileChangesDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub scope: GitDiffScope,
    pub hunks: Vec<GitDiffHunk>,
    pub binary: bool,
    pub truncated: bool,
    pub line_ending_warning: Option<String>,
}

impl GitFileChangesDiff {
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty() && !self.binary
    }

    /// Returns one canonical selection per changed line or paired modification.
    pub fn selection_units_for_hunk(&self, hunk_index: u32) -> Vec<GitLineSelection> {
        let mut units = selection_map(self)
            .into_values()
            .filter(|selection| selection.hunk_index == hunk_index)
            .collect::<Vec<_>>();
        units.sort_by_key(|selection| (selection.old_line, selection.new_line));
        units.dedup();
        units
    }

    pub fn all_selection_units(&self) -> Vec<GitLineSelection> {
        self.hunks
            .iter()
            .flat_map(|hunk| self.selection_units_for_hunk(hunk.index))
            .collect()
    }
}

/// A single changed line selected from a [`GitFileChangesDiff`].
///
/// Removed lines have only `old_line`; added lines have only `new_line`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GitLineSelection {
    pub hunk_index: u32,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeOperationResult {
    pub message: String,
    pub changes: GitRepositoryChanges,
}

/// Reads all changes in a repository with one bounded `git status` process.
///
/// The returned list intentionally stays flat so a 10,000-file repository does
/// not pay for a directory model in Core. Either UI can build its preferred tree
/// or filtered list without additional git invocations.
pub fn repository_changes(
    path: &Path,
    include_ignored: bool,
) -> Result<GitRepositoryChanges, String> {
    let repository = resolve_repository(path)?;
    repository_changes_inner(&repository.root, &repository.git_dir, include_ignored, true)
        .map(|(changes, _raw_status)| changes)
}

/// Loads menu state for a repository that has already been discovered. This
/// intentionally skips auxiliary config/diff/log probes: opening a menu should
/// cost one `git status --porcelain=v2` process and retain its raw bytes for
/// stale-write revalidation.
pub(crate) fn repository_changes_for_menu(
    repository: &GitRepository,
    include_ignored: bool,
) -> Result<(GitRepositoryChanges, Vec<u8>), String> {
    repository_changes_inner(
        &repository.work_tree,
        &repository.git_dir,
        include_ignored,
        false,
    )
}

fn repository_changes_inner(
    repository_root: &Path,
    git_dir: &Path,
    include_ignored: bool,
    include_warnings: bool,
) -> Result<(GitRepositoryChanges, Vec<u8>), String> {
    let mut command = git_command(repository_root);
    command.args([
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        "--untracked-files=all",
        "--find-renames",
    ]);
    if include_ignored {
        command.args(["--ignored=traditional"]);
    }
    let output = command.output().map_err(|error| git_spawn_error(&error))?;
    if !output.status.success() {
        return Err(output_error(&output));
    }

    let (mut branch, mut files) = parse_status_v2(&output.stdout, include_ignored);
    branch.finish_head();
    files.sort_by(|left, right| {
        left.group
            .order()
            .cmp(&right.group.order())
            .then_with(|| left.path.cmp(&right.path))
    });

    let crlf_warning = include_warnings
        .then(|| core_autocrlf_warning(repository_root))
        .flatten();
    let raw_status = output.stdout;
    let changes = GitRepositoryChanges {
        repository_root: repository_root.to_path_buf(),
        head: branch.head.clone(),
        branch: branch.branch,
        head_short_hash: branch.head_short_hash,
        upstream: branch.upstream,
        ahead: branch.ahead,
        behind: branch.behind,
        operation: operation_state(git_dir),
        files,
        crlf_warning,
        includes_ignored: include_ignored,
    };
    Ok((changes, raw_status))
}

/// Loads a structured diff for one path. Untracked paths receive a synthetic
/// all-added diff so line staging remains available before the first `git add`.
pub fn file_changes_diff(
    repository_root: &Path,
    relative_path: &str,
    scope: GitDiffScope,
) -> Result<GitFileChangesDiff, String> {
    let repository = resolve_repository(repository_root)?;
    let relative_path = safe_relative_path(relative_path)?;
    let absolute = repository.root.join(&relative_path);
    let status = change_status_for_path(&repository.root, &relative_path)?;
    let old_path = status.as_ref().and_then(|status| status.old_path.clone());

    if scope == GitDiffScope::Unstaged
        && status
            .as_ref()
            .is_some_and(|status| status.unstaged == Some(VcsStatus::Untracked))
    {
        let bytes = read_worktree_file(&absolute)?;
        let (binary, hunks, truncated) = synthetic_added_diff(&bytes);
        return Ok(GitFileChangesDiff {
            path: relative_path,
            old_path,
            scope,
            hunks,
            binary,
            truncated,
            line_ending_warning: crlf_warning_for_path(&repository.root, &absolute),
        });
    }

    if status.as_ref().is_some_and(|status| status.conflicted) {
        return Ok(GitFileChangesDiff {
            path: relative_path,
            old_path,
            scope,
            hunks: Vec::new(),
            binary: false,
            truncated: false,
            line_ending_warning: crlf_warning_for_path(&repository.root, &absolute),
        });
    }

    let mut command = git_command(&repository.root);
    command.args([
        "diff",
        "--no-ext-diff",
        "--no-color",
        "--diff-algorithm=myers",
        "--unified=3",
    ]);
    if scope == GitDiffScope::Staged {
        command.arg("--cached");
    }
    command.arg("--").arg(&relative_path);
    let output = command.output().map_err(|error| git_spawn_error(&error))?;
    if !output.status.success() {
        return Err(output_error(&output));
    }

    let (bytes, truncated) = bounded_prefix(&output.stdout, MAX_DIFF_RESPONSE_BYTES);
    let text = String::from_utf8_lossy(bytes);
    let binary = text.contains("Binary files ") || text.contains("GIT binary patch");
    let hunks = if binary {
        Vec::new()
    } else {
        parse_diff_hunks(&text)
    };
    Ok(GitFileChangesDiff {
        path: relative_path,
        old_path,
        scope,
        hunks,
        binary,
        truncated,
        line_ending_warning: crlf_warning_for_path(&repository.root, &absolute),
    })
}

/// Stages the selected changed lines of an unstaged diff.
pub fn stage_lines(
    repository_root: &Path,
    relative_path: &str,
    selections: &[GitLineSelection],
) -> Result<GitChangeOperationResult, String> {
    let repository = resolve_repository(repository_root)?;
    let relative_path = safe_relative_path(relative_path)?;
    let diff = file_changes_diff(&repository.root, &relative_path, GitDiffScope::Unstaged)?;
    validate_partial_diff(&diff, "暂存")?;
    let selected = selection_set(&diff, selections)?;
    let (base_exists, base) = read_index_blob(&repository.root, &relative_path)?;
    let (target_exists, target) = read_worktree_blob(&repository.root.join(&relative_path))?;
    let line_ending = preferred_line_ending(&base, &target);
    let desired = apply_diff_selection(&base, &target, &diff, &selected, true, Some(line_ending))?;
    if base_exists == target_exists && desired == base {
        return Err("没有可暂存的选择".to_string());
    }
    let base_mode = index_mode(&repository.root, &relative_path)
        .or_else(|| head_mode(&repository.root, &relative_path));
    let target_mode = if target_exists {
        Some(worktree_mode(
            &repository.root.join(&relative_path),
            base_mode.as_deref(),
        ))
    } else {
        None
    };
    let patch = full_file_patch(
        &relative_path,
        base_exists.then_some(base.as_slice()),
        target_exists.then_some(desired.as_slice()),
        base_mode.as_deref(),
        target_mode.as_deref(),
    );
    apply_patch(&repository.root, &patch, true)?;
    Ok(GitChangeOperationResult {
        message: format!("已暂存 {relative_path} 的所选行"),
        changes: repository_changes(&repository.root, false)?,
    })
}

/// Removes the selected staged lines from the index while preserving the work
/// tree and every unselected staged edit.
pub fn unstage_lines(
    repository_root: &Path,
    relative_path: &str,
    selections: &[GitLineSelection],
) -> Result<GitChangeOperationResult, String> {
    let repository = resolve_repository(repository_root)?;
    let relative_path = safe_relative_path(relative_path)?;
    let status = change_status_for_path(&repository.root, &relative_path)?;
    if status.as_ref().is_some_and(StatusEntry::is_rename) {
        return Err("重命名文件请使用整文件取消暂存".to_string());
    }
    let diff = file_changes_diff(&repository.root, &relative_path, GitDiffScope::Staged)?;
    validate_partial_diff(&diff, "取消暂存")?;
    let selected = selection_set(&diff, selections)?;
    let (head_exists, head) = read_head_blob(&repository.root, &relative_path)?;
    let (index_exists, index) = read_index_blob(&repository.root, &relative_path)?;
    let line_ending = preferred_line_ending(&head, &index);
    let desired = apply_diff_selection(&head, &index, &diff, &selected, false, Some(line_ending))?;
    if index_exists == head_exists && desired == index {
        return Err("没有可取消暂存的选择".to_string());
    }
    let index_mode = index_mode(&repository.root, &relative_path);
    let head_mode = head_mode(&repository.root, &relative_path);
    let patch = full_file_patch(
        &relative_path,
        index_exists.then_some(index.as_slice()),
        head_exists.then_some(desired.as_slice()),
        index_mode.as_deref(),
        head_mode.as_deref(),
    );
    apply_patch(&repository.root, &patch, true)?;
    Ok(GitChangeOperationResult {
        message: format!("已取消暂存 {relative_path} 的所选行"),
        changes: repository_changes(&repository.root, false)?,
    })
}

/// Discards selected unstaged lines. The caller must confirm destructive
/// actions before invoking this function.
pub fn discard_lines(
    repository_root: &Path,
    relative_path: &str,
    selections: &[GitLineSelection],
) -> Result<GitChangeOperationResult, String> {
    let repository = resolve_repository(repository_root)?;
    let relative_path = safe_relative_path(relative_path)?;
    let status = change_status_for_path(&repository.root, &relative_path)?;
    if status.as_ref().is_some_and(|status| status.conflicted) {
        return Err("请先解决冲突再丢弃行".to_string());
    }
    let diff = file_changes_diff(&repository.root, &relative_path, GitDiffScope::Unstaged)?;
    validate_partial_diff(&diff, "丢弃")?;
    let selected = selection_set(&diff, selections)?;
    let (base_exists, base) = read_index_blob(&repository.root, &relative_path)?;
    let (target_exists, target) = read_worktree_blob(&repository.root.join(&relative_path))?;
    let line_ending = preferred_line_ending(&target, &base);
    let desired = apply_diff_selection(&base, &target, &diff, &selected, false, Some(line_ending))?;
    if target_exists == base_exists && desired == target {
        return Err("没有可丢弃的选择".to_string());
    }
    let index_mode = index_mode(&repository.root, &relative_path);
    let target_mode = if target_exists {
        Some(worktree_mode(
            &repository.root.join(&relative_path),
            index_mode.as_deref(),
        ))
    } else {
        None
    };
    let desired_exists = target_exists || base_exists;
    let patch = full_file_patch(
        &relative_path,
        target_exists.then_some(target.as_slice()),
        desired_exists.then_some(desired.as_slice()),
        target_mode.as_deref(),
        index_mode.as_deref(),
    );
    apply_patch(&repository.root, &patch, false)?;
    Ok(GitChangeOperationResult {
        message: format!("已丢弃 {relative_path} 的所选行"),
        changes: repository_changes(&repository.root, false)?,
    })
}

/// Stages one or more paths, including directories and deleted files.
pub fn stage_paths(
    repository_root: &Path,
    relative_paths: &[String],
) -> Result<GitChangeOperationResult, String> {
    let repository = resolve_repository(repository_root)?;
    let paths = safe_path_list(relative_paths)?;
    let mut command = git_command(&repository.root);
    command.arg("add").arg("-A").arg("--");
    command.args(&paths);
    run_git_mutation(command, "暂存")?;
    Ok(GitChangeOperationResult {
        message: if paths.len() == 1 {
            format!("已暂存 {}", paths[0])
        } else {
            format!("已暂存 {} 个项目", paths.len())
        },
        changes: repository_changes(&repository.root, false)?,
    })
}

/// Unstages one or more paths and includes rename source paths automatically.
pub fn unstage_paths(
    repository_root: &Path,
    relative_paths: &[String],
) -> Result<GitChangeOperationResult, String> {
    let repository = resolve_repository(repository_root)?;
    let mut paths = safe_path_list(relative_paths)?;
    expand_rename_sources(&repository.root, &mut paths)?;
    let has_head = run_git(
        &repository.root,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
    )
    .is_some_and(|output| output.status.success());
    let mut command = git_command(&repository.root);
    if has_head {
        command.args(["restore", "--staged", "--"]);
    } else {
        command.args(["rm", "--cached", "-r", "--ignore-unmatch", "--"]);
    }
    command.args(&paths);
    run_git_mutation(command, "取消暂存")?;
    Ok(GitChangeOperationResult {
        message: if paths.len() == 1 {
            format!("已取消暂存 {}", paths[0])
        } else {
            format!("已取消暂存 {} 个项目", paths.len())
        },
        changes: repository_changes(&repository.root, false)?,
    })
}

/// Discards work-tree changes for tracked paths and sends untracked paths to
/// the operating system trash. Staged changes are intentionally preserved.
pub fn discard_paths(
    repository_root: &Path,
    relative_paths: &[String],
) -> Result<GitChangeOperationResult, String> {
    let repository = resolve_repository(repository_root)?;
    let paths = safe_path_list(relative_paths)?;
    let changes = repository_changes(&repository.root, false)?;
    let mut untracked = Vec::new();
    let mut tracked = Vec::new();
    for path in &paths {
        let absolute = repository.root.join(path);
        let has_tracked = path_has_tracked_entries(&repository.root, path)?;
        let untracked_below = changes.files.iter().any(|file| {
            file.group == GitChangeGroup::Untracked && path_is_same_or_below(&file.path, path)
        });
        if !has_tracked && untracked_below {
            untracked.push(absolute);
        } else {
            if has_tracked {
                tracked.push(path.clone());
            }
            for file in changes.files.iter().filter(|file| {
                file.group == GitChangeGroup::Untracked && path_is_same_or_below(&file.path, path)
            }) {
                let candidate = repository.root.join(&file.path);
                if !untracked
                    .iter()
                    .any(|existing: &PathBuf| existing == &candidate)
                {
                    untracked.push(candidate);
                }
            }
        }
    }
    if !tracked.is_empty() {
        let mut command = git_command(&repository.root);
        command.args(["restore", "--worktree", "--"]);
        command.args(&tracked);
        run_git_mutation(command, "丢弃工作区改动")?;
    }
    for path in &untracked {
        discard_untracked_path(path)?;
    }
    Ok(GitChangeOperationResult {
        message: if paths.len() == 1 {
            format!("已丢弃 {}", paths[0])
        } else {
            format!("已丢弃 {} 个项目", paths.len())
        },
        changes: repository_changes(&repository.root, false)?,
    })
}

/// Adds `relative_path` (or an explicit pattern) to `.gitignore` or the
/// repository-local exclude file.
pub fn add_to_gitignore(
    repository_root: &Path,
    relative_path: &str,
    pattern: Option<&str>,
    local_exclude: bool,
) -> Result<String, String> {
    let repository = resolve_repository(repository_root)?;
    let relative_path = safe_relative_path(relative_path)?;
    let target = if local_exclude {
        repository.git_dir.join("info").join("exclude")
    } else {
        repository.root.join(".gitignore")
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建忽略文件目录：{error}"))?;
    }
    let pattern = match pattern.map(str::trim).filter(|value| !value.is_empty()) {
        Some(pattern) => pattern.to_string(),
        None => {
            let is_directory = repository.root.join(&relative_path).is_dir();
            format!("/{relative_path}{}", if is_directory { "/" } else { "" })
        }
    };
    let existing = fs::read_to_string(&target).unwrap_or_default();
    if !existing.lines().any(|line| line.trim() == pattern) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|error| format!("无法写入忽略文件：{error}"))?;
        if !existing.is_empty() && !existing.ends_with('\n') {
            writeln!(file).map_err(|error| format!("无法写入忽略文件：{error}"))?;
        }
        writeln!(file, "{pattern}").map_err(|error| format!("无法写入忽略文件：{error}"))?;
    }
    Ok(format!("已添加忽略规则：{pattern}"))
}

#[derive(Clone, Debug)]
struct RepositoryPaths {
    root: PathBuf,
    git_dir: PathBuf,
}

fn resolve_repository(path: &Path) -> Result<RepositoryPaths, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let output = run_git(
        start,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    )
    .ok_or_else(|| "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string())?;
    if !output.status.success() {
        return Err("所选路径不在 Git 工作区中".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let root = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "无法读取 Git 仓库根目录".to_string())?;
    let git_dir = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "无法读取 Git 元数据目录".to_string())?;
    let root = dunce::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let git_dir = dunce::canonicalize(git_dir).unwrap_or_else(|_| PathBuf::from(git_dir));
    Ok(RepositoryPaths { root, git_dir })
}

#[derive(Clone, Debug)]
struct StatusEntry {
    path: String,
    old_path: Option<String>,
    staged: Option<VcsStatus>,
    unstaged: Option<VcsStatus>,
    conflicted: bool,
}

#[derive(Clone, Debug, Default)]
struct StatusBranch {
    branch: Option<String>,
    head_short_hash: Option<String>,
    head_oid: Option<String>,
    detached: bool,
    upstream: Option<String>,
    ahead: u32,
    behind: u32,
    head: Option<GitHead>,
}

impl StatusEntry {
    fn is_rename(&self) -> bool {
        self.staged == Some(VcsStatus::Renamed) || self.unstaged == Some(VcsStatus::Renamed)
    }
}

fn parse_status_v2(bytes: &[u8], include_ignored: bool) -> (StatusBranch, Vec<GitChangeFile>) {
    let mut branch = StatusBranch::default();
    let mut files = Vec::new();
    let mut records = bytes.split(|byte| *byte == 0).peekable();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let record = String::from_utf8_lossy(record);
        if let Some(header) = record.strip_prefix("# ") {
            apply_branch_header(header, &mut branch);
            continue;
        }
        if let Some(entry) = parse_status_record(&record, &mut records, include_ignored) {
            append_status_entry(&mut files, entry);
        }
    }
    (branch, files)
}

fn parse_status_record<'a>(
    record: &str,
    records: &mut impl Iterator<Item = &'a [u8]>,
    include_ignored: bool,
) -> Option<StatusEntry> {
    let (kind, rest) = record.split_once(' ')?;
    match kind {
        "1" => {
            let fields = rest.splitn(8, ' ').collect::<Vec<_>>();
            if fields.len() < 8 {
                return None;
            }
            let xy = fields[0].as_bytes();
            Some(StatusEntry {
                path: fields[7].to_string(),
                old_path: None,
                staged: status_from_column(xy.first().copied()),
                unstaged: status_from_column(xy.get(1).copied()),
                conflicted: false,
            })
        }
        "2" => {
            let fields = rest.splitn(9, ' ').collect::<Vec<_>>();
            if fields.len() < 9 {
                return None;
            }
            let xy = fields[0].as_bytes();
            Some(StatusEntry {
                path: fields[8].to_string(),
                old_path: records
                    .next()
                    .map(|value| String::from_utf8_lossy(value).into_owned()),
                staged: status_from_column(xy.first().copied()),
                unstaged: status_from_column(xy.get(1).copied()),
                conflicted: false,
            })
        }
        "u" => {
            let fields = rest.splitn(10, ' ').collect::<Vec<_>>();
            if fields.len() < 10 {
                return None;
            }
            Some(StatusEntry {
                path: fields[9].to_string(),
                old_path: None,
                staged: Some(VcsStatus::Conflicted),
                unstaged: Some(VcsStatus::Conflicted),
                conflicted: true,
            })
        }
        "?" => Some(StatusEntry {
            path: rest.to_string(),
            old_path: None,
            staged: None,
            unstaged: Some(VcsStatus::Untracked),
            conflicted: false,
        }),
        "!" if include_ignored => Some(StatusEntry {
            path: rest.to_string(),
            old_path: None,
            staged: None,
            unstaged: Some(VcsStatus::Ignored),
            conflicted: false,
        }),
        _ => None,
    }
}

fn append_status_entry(files: &mut Vec<GitChangeFile>, entry: StatusEntry) {
    if entry.conflicted {
        files.push(GitChangeFile {
            path: entry.path,
            old_path: entry.old_path,
            group: GitChangeGroup::Conflicted,
            status: VcsStatus::Conflicted,
        });
        return;
    }
    if entry.unstaged == Some(VcsStatus::Untracked) {
        files.push(GitChangeFile {
            path: entry.path,
            old_path: entry.old_path,
            group: GitChangeGroup::Untracked,
            status: VcsStatus::Untracked,
        });
        return;
    }
    if entry.unstaged == Some(VcsStatus::Ignored) {
        files.push(GitChangeFile {
            path: entry.path,
            old_path: entry.old_path,
            group: GitChangeGroup::Ignored,
            status: VcsStatus::Ignored,
        });
        return;
    }
    if let Some(status) = entry.staged {
        files.push(GitChangeFile {
            path: entry.path.clone(),
            old_path: entry.old_path.clone(),
            group: GitChangeGroup::Staged,
            status,
        });
    }
    if let Some(status) = entry.unstaged {
        files.push(GitChangeFile {
            path: entry.path,
            old_path: entry.old_path,
            group: GitChangeGroup::Unstaged,
            status,
        });
    }
}

fn apply_branch_header(header: &str, branch: &mut StatusBranch) {
    if let Some(oid) = header.strip_prefix("branch.oid ") {
        if oid != "(initial)" {
            branch.head_short_hash = Some(oid.chars().take(7).collect());
            branch.head_oid = Some(oid.to_string());
        }
    } else if let Some(head) = header.strip_prefix("branch.head ") {
        if head != "(detached)" {
            branch.branch = Some(head.to_string());
        } else {
            branch.detached = true;
        }
    } else if let Some(value) = header.strip_prefix("branch.upstream ") {
        branch.upstream = Some(value.to_string());
    } else if let Some(counts) = header.strip_prefix("branch.ab ") {
        for value in counts.split_whitespace() {
            if let Some(value) = value.strip_prefix('+') {
                branch.ahead = value.parse().unwrap_or(0);
            } else if let Some(value) = value.strip_prefix('-') {
                branch.behind = value.parse().unwrap_or(0);
            }
        }
    }
}

impl StatusBranch {
    fn finish_head(&mut self) {
        self.head = if self.detached {
            self.head_oid.clone().map(|oid| GitHead::Detached { oid })
        } else if let Some(name) = self.branch.clone() {
            if let Some(oid) = self.head_oid.clone() {
                Some(GitHead::Branch {
                    name,
                    oid: Some(oid),
                })
            } else {
                Some(GitHead::Unborn { name })
            }
        } else {
            None
        };
    }
}

fn status_from_column(code: Option<u8>) -> Option<VcsStatus> {
    match code {
        Some(b'M' | b'T') => Some(VcsStatus::Modified),
        Some(b'A') => Some(VcsStatus::Added),
        Some(b'D') => Some(VcsStatus::Deleted),
        Some(b'R' | b'C') => Some(VcsStatus::Renamed),
        Some(b'U') => Some(VcsStatus::Conflicted),
        _ => None,
    }
}

fn operation_state(git_dir: &Path) -> Option<GitOperationState> {
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        Some(GitOperationState::Rebasing)
    } else if git_dir.join("MERGE_HEAD").exists() {
        Some(GitOperationState::Merging)
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        Some(GitOperationState::CherryPicking)
    } else if git_dir.join("REVERT_HEAD").exists() {
        Some(GitOperationState::Reverting)
    } else if git_dir.join("BISECT_LOG").exists() {
        Some(GitOperationState::Bisecting)
    } else {
        None
    }
}

fn core_autocrlf_warning(root: &Path) -> Option<String> {
    let output = run_git(root, &["config", "--get", "core.autocrlf"])?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    matches!(value.as_str(), "true" | "input")
        .then(|| format!("Git 的 core.autocrlf={value} 可能转换换行符；按行暂存前请检查 CRLF 差异"))
}

fn crlf_warning_for_path(root: &Path, absolute: &Path) -> Option<String> {
    let bytes = fs::read(absolute).ok()?;
    if !contains_crlf(&bytes) {
        return None;
    }
    let output = run_git(root, &["config", "--get", "core.autocrlf"])?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    matches!(value.as_str(), "true" | "input")
        .then(|| "工作区文件使用 CRLF，而 Git 会自动转换换行符；请检查暂存结果".to_string())
}

fn contains_crlf(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|pair| pair == b"\r\n")
}

fn synthetic_added_diff(bytes: &[u8]) -> (bool, Vec<GitDiffHunk>, bool) {
    if is_binary(bytes) {
        return (true, Vec::new(), false);
    }
    let (lines, truncated) = text_lines_for_diff(bytes);
    let hunk = GitDiffHunk {
        index: 0,
        header: format!("@@ -0,0 +1,{} @@", lines.len()),
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: lines.len() as u32,
        lines: lines
            .into_iter()
            .enumerate()
            .map(|(index, text)| GitHunkLine {
                kind: GitHunkLineKind::Added,
                text,
                old_line: None,
                new_line: Some(index as u32 + 1),
            })
            .collect(),
    };
    (false, vec![hunk], truncated)
}

fn text_lines_for_diff(bytes: &[u8]) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    for line in split_lines(bytes) {
        if lines.len() >= MAX_DIFF_LINES {
            return (lines, true);
        }
        lines.push(
            String::from_utf8_lossy(line)
                .trim_end_matches(['\r', '\n'])
                .to_string(),
        );
    }
    (lines, false)
}

fn parse_diff_hunks(text: &str) -> Vec<GitDiffHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<GitDiffHunk> = None;
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let mut line_count = 0usize;

    for line in text.lines() {
        if line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            if let Some(header) = parse_hunk_header(line) {
                old_line = header.1;
                new_line = header.3;
                current = Some(GitDiffHunk {
                    index: hunks.len() as u32,
                    header: header.0,
                    old_start: header.1,
                    old_count: header.2,
                    new_start: header.3,
                    new_count: header.4,
                    lines: Vec::new(),
                });
            }
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            continue;
        };
        if line_count >= MAX_DIFF_LINES {
            break;
        }
        if line.starts_with('\\') {
            continue;
        }
        let Some(prefix) = line.as_bytes().first().copied() else {
            continue;
        };
        let text = line.get(1..).unwrap_or_default().to_string();
        match prefix {
            b' ' => {
                hunk.lines.push(GitHunkLine {
                    kind: GitHunkLineKind::Context,
                    text,
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                });
                old_line += 1;
                new_line += 1;
            }
            b'-' => {
                hunk.lines.push(GitHunkLine {
                    kind: GitHunkLineKind::Removed,
                    text,
                    old_line: Some(old_line),
                    new_line: None,
                });
                old_line += 1;
            }
            b'+' => {
                hunk.lines.push(GitHunkLine {
                    kind: GitHunkLineKind::Added,
                    text,
                    old_line: None,
                    new_line: Some(new_line),
                });
                new_line += 1;
            }
            _ => {}
        }
        line_count += 1;
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

fn parse_hunk_header(line: &str) -> Option<(String, u32, u32, u32, u32)> {
    let close = line[2..].find(" @@")? + 2;
    let header = line[..close + 3].to_string();
    let ranges = &line[3..close];
    let mut values = ranges.split_whitespace();
    let old = values.next()?.strip_prefix('-')?;
    let new = values.next()?.strip_prefix('+')?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((header, old_start, old_count, new_start, new_count))
}

fn parse_range(value: &str) -> Option<(u32, u32)> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn validate_partial_diff(diff: &GitFileChangesDiff, action: &str) -> Result<(), String> {
    if diff.binary {
        return Err(format!("二进制文件不支持按行{action}"));
    }
    if diff.truncated {
        return Err(format!("差异过大，仅支持整文件{action}"));
    }
    if diff.hunks.is_empty() {
        return Err("没有可操作的差异".to_string());
    }
    Ok(())
}

fn selection_set(
    diff: &GitFileChangesDiff,
    selections: &[GitLineSelection],
) -> Result<HashSet<GitLineSelection>, String> {
    if selections.is_empty() {
        return Err("请选择要操作的修改行".to_string());
    }
    let available = selection_map(diff);
    let mut selected = HashSet::new();
    for selection in selections {
        let Some(canonical) = available.get(selection) else {
            return Err("选择的修改行已过期，请刷新差异".to_string());
        };
        selected.insert(*canonical);
    }
    Ok(selected)
}

fn selection_key(hunk_index: u32, line: &GitHunkLine) -> GitLineSelection {
    GitLineSelection {
        hunk_index,
        old_line: line.old_line,
        new_line: line.new_line,
    }
}

fn selection_map(diff: &GitFileChangesDiff) -> HashMap<GitLineSelection, GitLineSelection> {
    let mut map = HashMap::new();
    for hunk in &diff.hunks {
        let mut index = 0usize;
        while index < hunk.lines.len() {
            let line = &hunk.lines[index];
            if line.kind == GitHunkLineKind::Context {
                index += 1;
                continue;
            }
            if line.kind == GitHunkLineKind::Removed {
                let removed_start = index;
                while index < hunk.lines.len() && hunk.lines[index].kind == GitHunkLineKind::Removed
                {
                    index += 1;
                }
                let added_start = index;
                while index < hunk.lines.len() && hunk.lines[index].kind == GitHunkLineKind::Added {
                    index += 1;
                }
                let removed = &hunk.lines[removed_start..added_start];
                let added = &hunk.lines[added_start..index];
                let paired = removed.len().min(added.len());
                for pair in 0..paired {
                    let canonical = GitLineSelection {
                        hunk_index: hunk.index,
                        old_line: removed[pair].old_line,
                        new_line: added[pair].new_line,
                    };
                    map.insert(selection_key(hunk.index, &removed[pair]), canonical);
                    map.insert(selection_key(hunk.index, &added[pair]), canonical);
                    map.insert(canonical, canonical);
                }
                for line in &removed[paired..] {
                    let key = selection_key(hunk.index, line);
                    map.insert(key, key);
                }
                for line in &added[paired..] {
                    let key = selection_key(hunk.index, line);
                    map.insert(key, key);
                }
            } else {
                let key = selection_key(hunk.index, line);
                map.insert(key, key);
                index += 1;
            }
        }
    }
    map
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEndingStyle {
    Lf,
    CrLf,
}

fn preferred_line_ending(primary: &[u8], fallback: &[u8]) -> LineEndingStyle {
    if contains_crlf(primary) {
        LineEndingStyle::CrLf
    } else if primary.contains(&b'\n') {
        LineEndingStyle::Lf
    } else if contains_crlf(fallback) {
        LineEndingStyle::CrLf
    } else {
        LineEndingStyle::Lf
    }
}

fn apply_diff_selection(
    base: &[u8],
    target: &[u8],
    diff: &GitFileChangesDiff,
    selected: &HashSet<GitLineSelection>,
    apply_selected: bool,
    line_ending: Option<LineEndingStyle>,
) -> Result<Vec<u8>, String> {
    let base_lines = split_lines(base);
    let target_lines = split_lines(target);
    let selection_map = selection_map(diff);
    let mut replacements = Vec::with_capacity(diff.hunks.len());
    for hunk in &diff.hunks {
        let start = if hunk.old_count == 0 {
            usize::try_from(hunk.old_start).unwrap_or(0)
        } else {
            usize::try_from(hunk.old_start.saturating_sub(1)).unwrap_or(0)
        };
        let end = start
            .checked_add(hunk.old_count as usize)
            .ok_or_else(|| "差异行范围溢出".to_string())?;
        if end > base_lines.len() {
            return Err("差异与当前索引不一致，请刷新后重试".to_string());
        }
        let mut replacement = Vec::new();
        for line in &hunk.lines {
            match line.kind {
                GitHunkLineKind::Context => {
                    let index = line
                        .old_line
                        .and_then(|number| number.checked_sub(1))
                        .ok_or_else(|| "差异缺少上下文行号".to_string())?
                        as usize;
                    replacement.extend_from_slice(
                        base_lines
                            .get(index)
                            .ok_or_else(|| "差异上下文超出文件范围".to_string())?,
                    );
                }
                GitHunkLineKind::Removed => {
                    let line_key = selection_key(hunk.index, line);
                    let Some(selection) = selection_map.get(&line_key) else {
                        return Err("差异中的修改行无法配对".to_string());
                    };
                    let selected_line = selected.contains(selection);
                    let apply = if apply_selected {
                        selected_line
                    } else {
                        !selected_line
                    };
                    if !apply {
                        let index = line
                            .old_line
                            .and_then(|number| number.checked_sub(1))
                            .ok_or_else(|| "删除行缺少旧行号".to_string())?
                            as usize;
                        replacement.extend_from_slice(
                            base_lines
                                .get(index)
                                .ok_or_else(|| "删除行超出文件范围".to_string())?,
                        );
                    }
                }
                GitHunkLineKind::Added => {
                    let line_key = selection_key(hunk.index, line);
                    let Some(selection) = selection_map.get(&line_key) else {
                        return Err("差异中的修改行无法配对".to_string());
                    };
                    let selected_line = selected.contains(selection);
                    let apply = if apply_selected {
                        selected_line
                    } else {
                        !selected_line
                    };
                    if apply {
                        let index = line
                            .new_line
                            .and_then(|number| number.checked_sub(1))
                            .ok_or_else(|| "新增行缺少新行号".to_string())?
                            as usize;
                        let source = target_lines
                            .get(index)
                            .ok_or_else(|| "新增行超出文件范围".to_string())?;
                        replacement.extend_from_slice(&normalize_ending(source, line_ending));
                    }
                }
            }
        }
        replacements.push((start, end, replacement));
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    let mut desired_lines = base_lines
        .iter()
        .map(|line| line.to_vec())
        .collect::<Vec<_>>();
    for (start, end, replacement) in replacements.into_iter().rev() {
        let replacement = split_lines(&replacement)
            .into_iter()
            .map(Vec::from)
            .collect::<Vec<_>>();
        desired_lines.splice(start..end, replacement);
    }
    Ok(desired_lines.into_iter().flatten().collect())
}

fn normalize_ending(line: &[u8], style: Option<LineEndingStyle>) -> Vec<u8> {
    let Some(style) = style else {
        return line.to_vec();
    };
    let (body, has_lf) = if let Some(body) = line.strip_suffix(b"\r\n") {
        (body, true)
    } else if let Some(body) = line.strip_suffix(b"\n") {
        (body, true)
    } else {
        (line, false)
    };
    let mut normalized = body.to_vec();
    if has_lf {
        if style == LineEndingStyle::CrLf {
            normalized.extend_from_slice(b"\r\n");
        } else {
            normalized.push(b'\n');
        }
    }
    normalized
}

fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&bytes[start..=index]);
            start = index + 1;
        }
    }
    if start < bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}

fn full_file_patch(
    relative_path: &str,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_mode: Option<&str>,
    new_mode: Option<&str>,
) -> Vec<u8> {
    let a = quoted_patch_path("a/", relative_path);
    let b = quoted_patch_path("b/", relative_path);
    let mut patch = Vec::new();
    patch.extend_from_slice(format!("diff --git {a} {b}\n").as_bytes());
    match (old.is_some(), new.is_some()) {
        (false, true) => patch.extend_from_slice(
            format!("new file mode {}\n", new_mode.unwrap_or("100644")).as_bytes(),
        ),
        (true, false) => patch.extend_from_slice(
            format!("deleted file mode {}\n", old_mode.unwrap_or("100644")).as_bytes(),
        ),
        _ => {}
    }
    let old_label = if old.is_some() {
        a
    } else {
        "/dev/null".to_string()
    };
    let new_label = if new.is_some() {
        b
    } else {
        "/dev/null".to_string()
    };
    patch.extend_from_slice(format!("--- {old_label}\n+++ {new_label}\n").as_bytes());
    let old_lines = old.map(split_lines).unwrap_or_default();
    let new_lines = new.map(split_lines).unwrap_or_default();
    let old_start = if old_lines.is_empty() { 0 } else { 1 };
    let new_start = if new_lines.is_empty() { 0 } else { 1 };
    patch.extend_from_slice(
        format!(
            "@@ -{},{} +{},{} @@\n",
            old_start,
            old_lines.len(),
            new_start,
            new_lines.len()
        )
        .as_bytes(),
    );
    for line in old_lines {
        patch.push(b'-');
        patch.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            patch.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
    }
    for line in new_lines {
        patch.push(b'+');
        patch.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            patch.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
    }
    patch
}

fn quoted_patch_path(prefix: &str, relative_path: &str) -> String {
    let path = format!("{prefix}{relative_path}");
    if path
        .bytes()
        .all(|byte| (0x20..=0x7e).contains(&byte) && !matches!(byte, b' ' | b'"' | b'\\'))
    {
        return path;
    }
    let mut quoted = String::from("\"");
    for byte in path.bytes() {
        match byte {
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            0x20..=0x7e => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

fn apply_patch(root: &Path, patch: &[u8], cached: bool) -> Result<(), String> {
    let mut command = git_command(root);
    command.args(["apply", "--whitespace=nowarn", "--unidiff-zero"]);
    if cached {
        command.arg("--cached");
    }
    run_git_with_stdin(command, patch, "应用补丁")
}

fn read_index_blob(root: &Path, relative_path: &str) -> Result<(bool, Vec<u8>), String> {
    let spec = format!(":{relative_path}");
    let output = run_git(root, &["show", &spec]).ok_or_else(|| "无法运行 git show".to_string())?;
    if output.status.success() {
        return Ok((true, output.stdout));
    }
    if index_mode(root, relative_path).is_none() {
        Ok((false, Vec::new()))
    } else {
        Err(output_error(&output))
    }
}

fn read_head_blob(root: &Path, relative_path: &str) -> Result<(bool, Vec<u8>), String> {
    let spec = format!("HEAD:{relative_path}");
    let output = run_git(root, &["show", &spec]).ok_or_else(|| "无法运行 git show".to_string())?;
    if output.status.success() {
        return Ok((true, output.stdout));
    }
    if head_mode(root, relative_path).is_none() {
        Ok((false, Vec::new()))
    } else {
        Err(output_error(&output))
    }
}

fn read_worktree_blob(path: &Path) -> Result<(bool, Vec<u8>), String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > MAX_WORKTREE_FILE_BYTES {
                return Err("文件超过 10 MiB，仅支持整文件操作".to_string());
            }
            fs::read(path)
                .map(|bytes| (true, bytes))
                .map_err(|error| format!("无法读取工作区文件：{error}"))
        }
        Ok(_) => Err("所选路径不是普通文件".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((false, Vec::new())),
        Err(error) => Err(format!("无法读取工作区文件：{error}")),
    }
}

fn read_worktree_file(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取文件信息：{error}"))?;
    if !metadata.is_file() {
        return Err("所选路径不是普通文件".to_string());
    }
    if metadata.len() > MAX_WORKTREE_FILE_BYTES {
        return Err("文件超过 10 MiB，未加载完整差异".to_string());
    }
    fs::read(path).map_err(|error| format!("无法读取文件：{error}"))
}

fn index_mode(root: &Path, relative_path: &str) -> Option<String> {
    let output = run_git(root, &["ls-files", "--stage", "-z", "--", relative_path])?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split('\0')
        .next()?
        .split_whitespace()
        .next()
        .filter(|mode| *mode == "100644" || *mode == "100755" || *mode == "120000")
        .map(str::to_string)
}

fn head_mode(root: &Path, relative_path: &str) -> Option<String> {
    let output = run_git(root, &["ls-tree", "HEAD", "--", relative_path])?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .next()
        .filter(|mode| *mode == "100644" || *mode == "100755" || *mode == "120000")
        .map(str::to_string)
}

fn worktree_mode(path: &Path, fallback: Option<&str>) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
        {
            return "100755".to_string();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    fallback.unwrap_or("100644").to_string()
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8 * 1024).any(|byte| *byte == 0)
}

fn bounded_prefix(bytes: &[u8], limit: usize) -> (&[u8], bool) {
    if bytes.len() <= limit {
        return (bytes, false);
    }
    let end = bytes[..limit]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .unwrap_or(limit);
    (&bytes[..end], true)
}

fn change_status_for_path(root: &Path, relative_path: &str) -> Result<Option<StatusEntry>, String> {
    let mut command = git_command(root);
    command.args([
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
        "--find-renames",
        "--ignored=traditional",
        "--",
        relative_path,
    ]);
    let output = command.output().map_err(|error| git_spawn_error(&error))?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let mut entries = Vec::new();
    parse_status_entries(&output.stdout, &mut entries);
    Ok(entries
        .into_iter()
        .find(|entry| entry.path == relative_path))
}

fn parse_status_entries(bytes: &[u8], entries: &mut Vec<StatusEntry>) {
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() || record.starts_with(b"# ") {
            continue;
        }
        let record = String::from_utf8_lossy(record);
        if let Some(entry) = parse_status_record(&record, &mut records, true) {
            entries.push(entry);
        }
    }
}

fn expand_rename_sources(root: &Path, paths: &mut Vec<String>) -> Result<(), String> {
    let changes = repository_changes(root, false)?;
    for file in changes.files {
        if file.group == GitChangeGroup::Staged
            && file.status == VcsStatus::Renamed
            && paths.iter().any(|path| path == &file.path)
            && let Some(old_path) = file.old_path
            && !paths.contains(&old_path)
        {
            paths.push(old_path);
        }
    }
    Ok(())
}

fn path_has_tracked_entries(root: &Path, relative_path: &str) -> Result<bool, String> {
    let output = run_git(root, &["ls-files", "--", relative_path])
        .ok_or_else(|| "无法运行 git ls-files".to_string())?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(!output.stdout.is_empty())
}

fn path_is_same_or_below(path: &str, parent: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn discard_untracked_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    trash::delete(path).map_err(|error| format!("无法将未跟踪项目移到回收站：{error}"))
}

fn safe_path_list(paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Err("请选择一个项目".to_string());
    }
    let mut safe = Vec::with_capacity(paths.len());
    for path in paths {
        let path = safe_relative_path(path)?;
        if !safe.contains(&path) {
            safe.push(path);
        }
    }
    Ok(safe)
}

fn safe_relative_path(path: &str) -> Result<String, String> {
    let normalized = path.trim().replace('\\', "/");
    if normalized.is_empty() || normalized == "." {
        return Ok(".".to_string());
    }
    let candidate = Path::new(&normalized);
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("Git 路径必须位于仓库内".to_string());
    }
    Ok(normalized.trim_start_matches("./").to_string())
}

fn git_command(current_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(current_dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("LC_ALL", "C");
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn run_git(current_dir: &Path, args: &[&str]) -> Option<Output> {
    git_command(current_dir).args(args).output().ok()
}

fn run_git_mutation(mut command: Command, label: &str) -> Result<(), String> {
    let output = command.output().map_err(|error| git_spawn_error(&error))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error_with_label(label, &output))
    }
}

fn run_git_with_stdin(mut command: Command, input: &[u8], label: &str) -> Result<(), String> {
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| git_spawn_error(&error))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("{label}失败：无法写入 Git 输入：{error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("{label}失败：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error_with_label(label, &output))
    }
}

fn git_spawn_error(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string()
    } else {
        format!("无法运行 Git：{error}")
    }
}

fn output_error(output: &Output) -> String {
    output_error_with_label("Git 操作", output)
}

fn output_error_with_label(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let detail = detail
        .replace(['\r', '\n'], " ")
        .chars()
        .take(500)
        .collect::<String>();
    if detail.is_empty() {
        format!("{label}失败，退出码 {}", output.status.code().unwrap_or(-1))
    } else {
        format!("{label}失败：{detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn temp_repo(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kite-git-changes-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create repository");
        git_ok(&root, &["init", "--quiet", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "kite@example.com"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "core.autocrlf", "false"]);
        root
    }

    fn git_ok(root: &Path, args: &[&str]) -> Vec<u8> {
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
        output.stdout
    }

    fn commit_all(root: &Path, message: &str) {
        git_ok(root, &["add", "-A"]);
        git_ok(root, &["commit", "--quiet", "-m", message]);
    }

    fn hunk_selection(diff: &GitFileChangesDiff, hunk_index: u32) -> Vec<GitLineSelection> {
        diff.hunks
            .iter()
            .find(|hunk| hunk.index == hunk_index)
            .expect("hunk")
            .lines
            .iter()
            .filter(|line| line.kind != GitHunkLineKind::Context)
            .map(|line| GitLineSelection {
                hunk_index,
                old_line: line.old_line,
                new_line: line.new_line,
            })
            .collect()
    }

    fn index_text(root: &Path, path: &str) -> String {
        String::from_utf8(git_ok(root, &["show", &format!(":{path}")])).expect("index text is utf8")
    }

    #[test]
    fn parses_status_v2_groups_renames_and_ignored_paths() {
        let bytes = b"# branch.oid 0123456789abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\x001 MM N... 100644 100644 100644 aaa bbb both.txt\x002 R. N... 100644 100644 100644 aaa bbb R100 new.txt\0old.txt\0u UU N... 100644 100644 100644 100644 a b c conflict.txt\0? new-file.txt\0! ignored.log\0";
        let (branch, files) = parse_status_v2(bytes, true);

        assert_eq!(branch.branch.as_deref(), Some("main"));
        assert_eq!(branch.head_short_hash.as_deref(), Some("0123456"));
        assert_eq!(branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((branch.ahead, branch.behind), (2, 1));
        assert_eq!(files.len(), 6);
        assert!(files.iter().any(|file| {
            file.path == "new.txt"
                && file.old_path.as_deref() == Some("old.txt")
                && file.group == GitChangeGroup::Staged
                && file.status == VcsStatus::Renamed
        }));
        assert!(
            files
                .iter()
                .any(|file| { file.path == "both.txt" && file.group == GitChangeGroup::Staged })
        );
        assert!(
            files
                .iter()
                .any(|file| { file.path == "both.txt" && file.group == GitChangeGroup::Unstaged })
        );
        assert!(files.iter().any(|file| {
            file.path == "conflict.txt" && file.group == GitChangeGroup::Conflicted
        }));
        assert!(
            files.iter().any(|file| {
                file.path == "ignored.log" && file.group == GitChangeGroup::Ignored
            })
        );
    }

    #[test]
    fn parses_hunk_ranges_and_line_numbers() {
        let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -2,3 +2,3 @@\n keep\n-old\n+new\n tail\n";
        let hunks = parse_diff_hunks(diff);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (2, 3));
        assert_eq!((hunk.new_start, hunk.new_count), (2, 3));
        assert_eq!(hunk.lines[0].old_line, Some(2));
        assert_eq!(hunk.lines[1].old_line, Some(3));
        assert_eq!(hunk.lines[2].new_line, Some(3));
        assert_eq!(hunk.lines[3].new_line, Some(4));
    }

    #[test]
    fn stage_selected_lines_handles_modified_deleted_and_added_lines() {
        if !git_available() {
            return;
        }
        let root = temp_repo("stage-lines");
        fs::write(
            root.join("sample.txt"),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
        )
        .expect("write");
        commit_all(&root, "initial");
        fs::write(
            root.join("sample.txt"),
            "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\n",
        )
        .expect("edit");

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        assert_eq!(diff.hunks.len(), 2);
        let first = hunk_selection(&diff, 0);
        stage_lines(&root, "sample.txt", &first).expect("stage first hunk");
        assert_eq!(
            index_text(&root, "sample.txt"),
            "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n"
        );

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        let second = hunk_selection(&diff, 0);
        stage_lines(&root, "sample.txt", &second).expect("stage second hunk");
        assert_eq!(
            index_text(&root, "sample.txt"),
            "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("sample.txt")).expect("worktree"),
            "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stages_one_line_from_a_replacement() {
        if !git_available() {
            return;
        }
        let root = temp_repo("single-line");
        fs::write(root.join("sample.txt"), "a\nb\nc\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), "a\nB\nc\n").expect("edit");

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        let added = diff.hunks[0]
            .lines
            .iter()
            .find(|line| line.kind == GitHunkLineKind::Added)
            .expect("added line");
        stage_lines(
            &root,
            "sample.txt",
            &[GitLineSelection {
                hunk_index: 0,
                old_line: added.old_line,
                new_line: added.new_line,
            }],
        )
        .expect("stage added line");
        assert_eq!(index_text(&root, "sample.txt"), "a\nB\nc\n");
        assert_eq!(
            fs::read_to_string(root.join("sample.txt")).expect("worktree"),
            "a\nB\nc\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_selected_lines_preserves_missing_final_newline() {
        if !git_available() {
            return;
        }
        let root = temp_repo("no-final-newline");
        fs::write(root.join("sample.txt"), b"a\nb").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), b"a\nB").expect("edit");

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        stage_lines(&root, "sample.txt", &hunk_selection(&diff, 0)).expect("stage");
        assert_eq!(git_ok(&root, &["show", ":sample.txt"]), b"a\nB");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stages_a_pure_deleted_line() {
        if !git_available() {
            return;
        }
        let root = temp_repo("deleted-line");
        fs::write(root.join("sample.txt"), "a\nb\nc\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), "a\nc\n").expect("delete line");

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        let removed = diff.hunks[0]
            .lines
            .iter()
            .find(|line| line.kind == GitHunkLineKind::Removed)
            .expect("removed line");
        stage_lines(
            &root,
            "sample.txt",
            &[GitLineSelection {
                hunk_index: 0,
                old_line: removed.old_line,
                new_line: None,
            }],
        )
        .expect("stage deleted line");
        assert_eq!(index_text(&root, "sample.txt"), "a\nc\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stages_a_line_in_a_path_with_spaces() {
        if !git_available() {
            return;
        }
        let root = temp_repo("path-spaces");
        fs::write(root.join("my file.txt"), "a\nb\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("my file.txt"), "a\nB\n").expect("edit");
        let diff = file_changes_diff(&root, "my file.txt", GitDiffScope::Unstaged).expect("diff");
        stage_lines(&root, "my file.txt", &hunk_selection(&diff, 0)).expect("stage");
        assert_eq!(index_text(&root, "my file.txt"), "a\nB\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stage_selected_lines_preserves_crlf_content() {
        if !git_available() {
            return;
        }
        let root = temp_repo("crlf");
        fs::write(root.join("sample.txt"), b"a\r\nb\r\nc\r\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), b"a\r\nB\r\nc\r\n").expect("edit");

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        let added = diff.hunks[0]
            .lines
            .iter()
            .find(|line| line.kind == GitHunkLineKind::Added && line.text == "B");
        assert!(added.is_some());
        stage_lines(
            &root,
            "sample.txt",
            &[GitLineSelection {
                hunk_index: 0,
                old_line: added.and_then(|line| line.old_line),
                new_line: added.and_then(|line| line.new_line),
            }],
        )
        .expect("stage CRLF line");
        assert_eq!(git_ok(&root, &["show", ":sample.txt"]), b"a\r\nB\r\nc\r\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unstages_only_the_selected_staged_line() {
        if !git_available() {
            return;
        }
        let root = temp_repo("unstage-lines");
        fs::write(root.join("sample.txt"), "a\nb\nc\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), "a\nB\nC\n").expect("edit");
        git_ok(&root, &["add", "sample.txt"]);

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Staged).expect("diff");
        stage_unstage_one_added_line(&root, &diff);
        assert_eq!(index_text(&root, "sample.txt"), "a\nb\nC\n");
        assert_eq!(
            fs::read_to_string(root.join("sample.txt")).expect("worktree"),
            "a\nB\nC\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn stage_unstage_one_added_line(root: &Path, diff: &GitFileChangesDiff) {
        let added = diff.hunks[0]
            .lines
            .iter()
            .find(|line| line.kind == GitHunkLineKind::Added && line.text == "B")
            .expect("added line");
        unstage_lines(
            root,
            "sample.txt",
            &[GitLineSelection {
                hunk_index: 0,
                old_line: added.old_line,
                new_line: added.new_line,
            }],
        )
        .expect("unstage line");
    }

    #[test]
    fn discards_only_the_selected_unstaged_line() {
        if !git_available() {
            return;
        }
        let root = temp_repo("discard-lines");
        fs::write(root.join("sample.txt"), "a\nb\nc\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), "a\nB\nC\n").expect("edit");

        let diff = file_changes_diff(&root, "sample.txt", GitDiffScope::Unstaged).expect("diff");
        let removed_b = diff.hunks[0]
            .lines
            .iter()
            .find(|line| line.kind == GitHunkLineKind::Removed && line.text == "b")
            .expect("removed line");
        discard_lines(
            &root,
            "sample.txt",
            &[GitLineSelection {
                hunk_index: 0,
                old_line: removed_b.old_line,
                new_line: None,
            }],
        )
        .expect("discard");
        assert_eq!(
            fs::read_to_string(root.join("sample.txt")).expect("worktree"),
            "a\nb\nC\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stages_selected_lines_of_an_untracked_file() {
        if !git_available() {
            return;
        }
        let root = temp_repo("untracked-lines");
        fs::write(root.join("new.txt"), "first\nsecond\n").expect("write");
        file_changes_diff(&root, "new.txt", GitDiffScope::Unstaged).expect("diff");
        let first = GitLineSelection {
            hunk_index: 0,
            old_line: None,
            new_line: Some(1),
        };
        stage_lines(&root, "new.txt", &[first]).expect("stage first line");
        assert_eq!(index_text(&root, "new.txt"), "first\n");
        assert!(
            repository_changes(&root, false)
                .expect("changes")
                .files
                .iter()
                .any(|file| file.path == "new.txt" && file.group == GitChangeGroup::Unstaged)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discards_selected_lines_of_an_untracked_file() {
        if !git_available() {
            return;
        }
        let root = temp_repo("untracked-discard-lines");
        fs::write(root.join("new.txt"), "first\nsecond\n").expect("write");
        file_changes_diff(&root, "new.txt", GitDiffScope::Unstaged).expect("diff");
        discard_lines(
            &root,
            "new.txt",
            &[GitLineSelection {
                hunk_index: 0,
                old_line: None,
                new_line: Some(1),
            }],
        )
        .expect("discard first line");
        assert_eq!(
            fs::read_to_string(root.join("new.txt")).expect("worktree"),
            "second\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn detects_renames_and_writes_ignore_rules() {
        if !git_available() {
            return;
        }
        let root = temp_repo("rename-ignore");
        fs::write(root.join("old.txt"), "content\n").expect("write");
        commit_all(&root, "initial");
        fs::rename(root.join("old.txt"), root.join("new.txt")).expect("rename");
        git_ok(&root, &["add", "-A"]);
        fs::write(root.join("scratch.log"), "log\n").expect("write ignored candidate");

        let changes = repository_changes(&root, true).expect("changes");
        assert!(changes.files.iter().any(|file| {
            file.path == "new.txt"
                && file.old_path.as_deref() == Some("old.txt")
                && file.status == VcsStatus::Renamed
        }));

        add_to_gitignore(&root, "scratch.log", None, false).expect("ignore");
        let changes = repository_changes(&root, true).expect("changes after ignore");
        assert!(
            changes.files.iter().any(|file| {
                file.path == "scratch.log" && file.group == GitChangeGroup::Ignored
            })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stages_and_unstages_a_directory() {
        if !git_available() {
            return;
        }
        let root = temp_repo("paths");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src").join("a.txt"), "a\n").expect("write a");
        commit_all(&root, "initial");
        fs::write(root.join("src").join("a.txt"), "A\n").expect("edit a");
        fs::write(root.join("src").join("b.txt"), "b\n").expect("write b");

        stage_paths(&root, &["src".to_string()]).expect("stage directory");
        let changes = repository_changes(&root, false).expect("changes");
        assert_eq!(changes.count(GitChangeGroup::Staged), 2);
        assert_eq!(changes.count(GitChangeGroup::Unstaged), 0);

        unstage_paths(&root, &["src".to_string()]).expect("unstage directory");
        let changes = repository_changes(&root, false).expect("changes");
        assert_eq!(changes.count(GitChangeGroup::Staged), 0);
        assert_eq!(changes.count(GitChangeGroup::Unstaged), 1);
        assert_eq!(changes.count(GitChangeGroup::Untracked), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discard_path_restores_index_content_without_unstaging() {
        if !git_available() {
            return;
        }
        let root = temp_repo("discard-path");
        fs::write(root.join("sample.txt"), "base\n").expect("write");
        commit_all(&root, "initial");
        fs::write(root.join("sample.txt"), "staged\n").expect("edit staged");
        git_ok(&root, &["add", "sample.txt"]);
        fs::write(root.join("sample.txt"), "worktree\n").expect("edit worktree");

        discard_paths(&root, &["sample.txt".to_string()]).expect("discard path");
        assert_eq!(
            fs::read_to_string(root.join("sample.txt")).expect("worktree"),
            "staged\n"
        );
        assert_eq!(index_text(&root, "sample.txt"), "staged\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_exclude_ignores_the_selected_path() {
        if !git_available() {
            return;
        }
        let root = temp_repo("local-ignore");
        fs::write(root.join("scratch.log"), "log\n").expect("write");
        add_to_gitignore(&root, "scratch.log", None, true).expect("local exclude");
        let exclude = root.join(".git").join("info").join("exclude");
        let text = fs::read_to_string(exclude).expect("exclude text");
        assert!(text.lines().any(|line| line == "/scratch.log"));
        let changes = repository_changes(&root, true).expect("changes");
        assert!(
            changes.files.iter().any(|file| {
                file.path == "scratch.log" && file.group == GitChangeGroup::Ignored
            })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "moves an untracked test file through the OS trash"]
    fn discards_untracked_file_to_trash() {
        if !git_available() {
            return;
        }
        let root = temp_repo("trash");
        fs::write(root.join("untracked.txt"), "temporary\n").expect("write");
        discard_paths(&root, &["untracked.txt".to_string()]).expect("discard untracked");
        assert!(!root.join("untracked.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "10,000-file performance baseline"]
    fn benchmark_ten_thousand_changed_files() {
        if !git_available() {
            return;
        }
        let root = temp_repo("perf-10k");
        for directory in 0..100 {
            let path = root.join(format!("dir-{directory:03}"));
            fs::create_dir_all(&path).expect("create directory");
            for file in 0..100 {
                fs::write(path.join(format!("file-{file:03}.txt")), "base\n").expect("write");
            }
        }
        commit_all(&root, "initial");
        for directory in 0..100 {
            let path = root.join(format!("dir-{directory:03}"));
            for file in 0..100 {
                fs::write(path.join(format!("file-{file:03}.txt")), "changed\n").expect("edit");
            }
        }
        let started = std::time::Instant::now();
        let changes = repository_changes(&root, false).expect("changes");
        let elapsed = started.elapsed();
        eprintln!(
            "git_changes_10k: files={} elapsed_ms={}",
            changes.files.len(),
            elapsed.as_millis()
        );
        assert_eq!(changes.files.len(), 10_000);
        let _ = fs::remove_dir_all(root);
    }
}
