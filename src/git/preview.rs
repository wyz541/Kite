//! Git summary for a single path, shared by the Windows and macOS previews.
//!
//! The summary is read with the git CLI so it follows the user's git
//! configuration. Every query is bounded to the selected path; a path outside
//! a work tree returns `None` after one `rev-parse` probe.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

use super::GitService;
use super::command::GitCommandResult;
use super::error::GitError;
use super::events::GitOperationState;
use crate::fs::VcsStatus;

/// Text larger than this is not diffed for the preview.
const DIFF_PREVIEW_MAX_BYTES: usize = 256 * 1024;
const DIFF_PREVIEW_MAX_LINES: usize = 400;
const MAX_REPOSITORY_CHANGED_FILES: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitSummary {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitChangeCounts {
    pub modified: u32,
    pub added: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl GitChangeCounts {
    pub fn total(&self) -> u32 {
        self.modified + self.added + self.deleted + self.renamed + self.untracked + self.conflicted
    }

    fn record(&mut self, status: VcsStatus) {
        match status {
            VcsStatus::Modified | VcsStatus::PropertyModified => self.modified += 1,
            VcsStatus::Added => self.added += 1,
            VcsStatus::Deleted | VcsStatus::Missing => self.deleted += 1,
            VcsStatus::Renamed => self.renamed += 1,
            VcsStatus::Untracked => self.untracked += 1,
            VcsStatus::Conflicted => self.conflicted += 1,
            VcsStatus::Ignored | VcsStatus::External => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitDiffLineKind {
    Context,
    Added,
    Removed,
    Hunk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffLine {
    pub kind: GitDiffLineKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPathSummary {
    pub repository_root: PathBuf,
    pub relative_path: String,
    /// `None` on a detached HEAD.
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<GitOperationState>,
    /// Index column of `git status` for a file.
    pub staged_status: Option<VcsStatus>,
    /// Work-tree column of `git status` for a file.
    pub unstaged_status: Option<VcsStatus>,
    pub ignored: bool,
    pub last_commit: Option<GitCommitSummary>,
    pub lines_added: u32,
    pub lines_removed: u32,
    /// Change counts below a directory. Zero for files.
    pub changes: GitChangeCounts,
    pub is_directory: bool,
}

/// One changed path in a repository working tree. Statuses are kept separate
/// so the Git client can show staged and unstaged work without losing one side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangedFile {
    pub path: String,
    pub staged_status: Option<VcsStatus>,
    pub unstaged_status: Option<VcsStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositorySnapshot {
    pub repository_root: PathBuf,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<GitOperationState>,
    pub changes: GitChangeCounts,
    pub changed_files: Vec<GitChangedFile>,
    pub recent_commits: Vec<GitCommitSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitOperationAction {
    Continue,
    Skip,
    Abort,
}

/// Reads the Git summary for `path`. Returns `Ok(None)` when the path is not
/// inside a Git work tree or Git is not installed.
pub fn git_path_summary(path: &Path) -> Result<Option<GitPathSummary>, String> {
    let is_directory = path.is_dir();
    let start = if is_directory {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let Ok(probe) = run_git(
        start,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    ) else {
        return Ok(None);
    };
    let text = probe.stdout;
    let mut lines = text.lines();
    let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    summary_in(path, is_directory, root, git_dir).map(Some)
}

/// Resolve the work-tree root that contains `path`. This is a single bounded
/// `rev-parse` probe and is safe to use before opening the Git client window.
pub fn git_repository_root(path: &Path) -> Result<Option<PathBuf>, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let Ok(output) = run_git(start, &["rev-parse", "--show-toplevel"]) else {
        return Ok(None);
    };
    let root = output.stdout.trim().to_string();
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        dunce::canonicalize(&root).unwrap_or_else(|_| PathBuf::from(root)),
    ))
}

/// Reads the data needed by the Git client shell. The command count is bounded:
/// one repository probe, one status query, and one recent-log query.
pub fn git_repository_snapshot(path: &Path) -> Result<Option<GitRepositorySnapshot>, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let Ok(probe) = run_git(
        start,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    ) else {
        return Ok(None);
    };
    let text = probe.stdout;
    let mut lines = text.lines();
    let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    let root = dunce::canonicalize(root.trim()).unwrap_or_else(|_| PathBuf::from(root.trim()));

    let mut summary = GitPathSummary {
        repository_root: root.clone(),
        relative_path: ".".to_string(),
        branch: None,
        head_short_hash: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        operation: operation_state(Path::new(git_dir.trim())),
        staged_status: None,
        unstaged_status: None,
        ignored: false,
        last_commit: None,
        lines_added: 0,
        lines_removed: 0,
        changes: GitChangeCounts::default(),
        is_directory: true,
    };
    let mut changed_files = Vec::new();
    if let Ok(output) = run_git(
        &root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
    ) {
        apply_status_v2(&mut summary, output.stdout.as_bytes());
        changed_files = parse_changed_files(output.stdout.as_bytes());
    }

    let recent_commits = recent_commits(&root, 100);
    Ok(Some(GitRepositorySnapshot {
        repository_root: root,
        branch: summary.branch,
        head_short_hash: summary.head_short_hash,
        upstream: summary.upstream,
        ahead: summary.ahead,
        behind: summary.behind,
        operation: summary.operation,
        changes: summary.changes,
        changed_files,
        recent_commits,
    }))
}

fn summary_in(
    path: &Path,
    is_directory: bool,
    root: &str,
    git_dir: &str,
) -> Result<GitPathSummary, String> {
    let root = dunce::canonicalize(root.trim()).unwrap_or_else(|_| PathBuf::from(root.trim()));
    let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| "所选项目不在 Git 工作区中".to_string())?;
    let relative_arg = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    };

    let mut summary = GitPathSummary {
        repository_root: root.clone(),
        relative_path: relative_arg.clone(),
        branch: None,
        head_short_hash: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        operation: operation_state(Path::new(git_dir.trim())),
        staged_status: None,
        unstaged_status: None,
        ignored: false,
        last_commit: None,
        lines_added: 0,
        lines_removed: 0,
        changes: GitChangeCounts::default(),
        is_directory,
    };

    if let Ok(output) = run_git(
        &root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            &relative_arg,
        ],
    ) {
        apply_status_v2(&mut summary, output.stdout.as_bytes());
    }

    if let Ok(output) = run_git(
        &root,
        &[
            "log",
            "-1",
            "--format=%H%x00%h%x00%an%x00%at%x00%s",
            "--",
            &relative_arg,
        ],
    ) {
        summary.last_commit = parse_commit_line(&output.stdout);
    }

    if summary.head_short_hash.is_some()
        && let Ok(output) = run_git(
            &root,
            &[
                "diff",
                "HEAD",
                "--numstat",
                "--no-ext-diff",
                "--",
                &relative_arg,
            ],
        )
    {
        let (added, removed) = parse_numstat(&output.stdout);
        summary.lines_added = added;
        summary.lines_removed = removed;
    }

    Ok(summary)
}

/// Returns the working-tree diff of a file against HEAD, bounded for preview.
pub fn git_file_diff(path: &Path) -> Result<Vec<GitDiffLine>, String> {
    let start = path.parent().unwrap_or(path);
    let probe = run_git(start, &["rev-parse", "--show-toplevel"]).map_err(|error| error.message)?;
    let root_text = probe.stdout.trim().to_string();
    let root = dunce::canonicalize(&root_text).unwrap_or_else(|_| PathBuf::from(root_text));
    let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| "所选项目不在 Git 工作区中".to_string())?
        .to_string_lossy()
        .replace('\\', "/");

    let has_head = run_git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();
    let mut args = vec!["diff", "--no-ext-diff", "--no-color", "-U3"];
    if has_head {
        args.push("HEAD");
    } else {
        args.push("--cached");
    }
    args.push("--");
    args.push(&relative);
    let output = run_git(&root, &args).map_err(|error| error.message)?;
    if output.stdout.len() > DIFF_PREVIEW_MAX_BYTES {
        return Err("差异过大，未在预览中显示".to_string());
    }
    Ok(parse_unified_diff(&output.stdout))
}

/// Continue, skip, or abort the operation currently recorded in the Git
/// directory. The command is chosen from the marker files rather than trusting
/// a stale UI value.
pub fn run_git_operation_control(
    path: &Path,
    action: GitOperationAction,
) -> Result<String, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let probe = run_git(
        start,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    )
    .map_err(|error| error.message)?;
    let text = probe.stdout;
    let mut lines = text.lines();
    let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Err("无法确定 Git 仓库".to_string());
    };
    let root = dunce::canonicalize(root.trim()).unwrap_or_else(|_| PathBuf::from(root.trim()));
    let operation = operation_state(Path::new(git_dir.trim()))
        .ok_or_else(|| "当前没有进行中的 Git 操作".to_string())?;
    let args = operation_control_args(operation, action)?;
    GitService::shared()
        .run_preview_command(&root, args)
        .map_err(|error| error.message)?;
    let verb = match action {
        GitOperationAction::Continue => "继续",
        GitOperationAction::Skip => "跳过",
        GitOperationAction::Abort => "中止",
    };
    Ok(format!("已{verb}{}", operation.label()))
}

fn operation_control_args(
    operation: GitOperationState,
    action: GitOperationAction,
) -> Result<&'static [&'static str], String> {
    match (operation, action) {
        (GitOperationState::Rebasing, GitOperationAction::Continue) => {
            Ok(&["rebase", "--continue"])
        }
        (GitOperationState::Rebasing, GitOperationAction::Skip) => Ok(&["rebase", "--skip"]),
        (GitOperationState::Rebasing, GitOperationAction::Abort) => Ok(&["rebase", "--abort"]),
        (GitOperationState::Merging, GitOperationAction::Continue) => Ok(&["merge", "--continue"]),
        (GitOperationState::Merging, GitOperationAction::Abort) => Ok(&["merge", "--abort"]),
        (GitOperationState::CherryPicking, GitOperationAction::Continue) => {
            Ok(&["cherry-pick", "--continue"])
        }
        (GitOperationState::CherryPicking, GitOperationAction::Skip) => {
            Ok(&["cherry-pick", "--skip"])
        }
        (GitOperationState::CherryPicking, GitOperationAction::Abort) => {
            Ok(&["cherry-pick", "--abort"])
        }
        (GitOperationState::Reverting, GitOperationAction::Continue) => {
            Ok(&["revert", "--continue"])
        }
        (GitOperationState::Reverting, GitOperationAction::Skip) => Ok(&["revert", "--skip"]),
        (GitOperationState::Reverting, GitOperationAction::Abort) => Ok(&["revert", "--abort"]),
        (GitOperationState::Bisecting, GitOperationAction::Abort) => Ok(&["bisect", "reset"]),
        (GitOperationState::Merging, GitOperationAction::Skip) => {
            Err("合并操作不支持跳过".to_string())
        }
        (GitOperationState::Bisecting, _) => {
            Err("二分查找需要明确标记 good 或 bad 提交".to_string())
        }
    }
}

fn parse_changed_files(output: &[u8]) -> Vec<GitChangedFile> {
    let mut files = Vec::new();
    let mut records = output.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if files.len() >= MAX_REPOSITORY_CHANGED_FILES {
            break;
        }
        let record = String::from_utf8_lossy(record);
        if record.starts_with("# ") || record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(2, ' ');
        let kind = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        let (path, staged, unstaged) = match kind {
            "1" => {
                let fields = rest.splitn(8, ' ').collect::<Vec<_>>();
                let (staged, unstaged) = status_pair(fields.first().copied());
                (fields.get(7).copied().unwrap_or_default(), staged, unstaged)
            }
            "2" => {
                let fields = rest.splitn(9, ' ').collect::<Vec<_>>();
                let (staged, unstaged) = status_pair(fields.first().copied());
                let _ = records.next();
                (fields.get(8).copied().unwrap_or_default(), staged, unstaged)
            }
            "u" => {
                let fields = rest.splitn(10, ' ').collect::<Vec<_>>();
                (
                    fields.get(9).copied().unwrap_or_default(),
                    Some(VcsStatus::Conflicted),
                    None,
                )
            }
            "?" => (rest, None, Some(VcsStatus::Untracked)),
            "!" => (rest, None, None),
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        files.push(GitChangedFile {
            path: path.replace('\\', "/"),
            staged_status: staged,
            unstaged_status: unstaged,
        });
    }
    files
}

fn status_pair(xy: Option<&str>) -> (Option<VcsStatus>, Option<VcsStatus>) {
    let Some(xy) = xy else {
        return (None, None);
    };
    let bytes = xy.as_bytes();
    (
        bytes.first().and_then(|code| status_from_xy_column(*code)),
        bytes.get(1).and_then(|code| status_from_xy_column(*code)),
    )
}

fn recent_commits(root: &Path, limit: usize) -> Vec<GitCommitSummary> {
    let limit = limit.clamp(1, 500).to_string();
    let Ok(output) = run_git(
        root,
        &[
            "log",
            "--max-count",
            &limit,
            "--format=%H%x00%h%x00%an%x00%at%x00%s",
        ],
    ) else {
        return Vec::new();
    };
    output
        .stdout
        .lines()
        .filter_map(parse_commit_line)
        .collect()
}

fn apply_status_v2(summary: &mut GitPathSummary, output: &[u8]) {
    let mut records = output.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        let record = String::from_utf8_lossy(record);
        if let Some(header) = record.strip_prefix("# ") {
            apply_branch_header(summary, header);
            continue;
        }
        let mut fields = record.splitn(2, ' ');
        let kind = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        match kind {
            "1" | "2" => {
                let xy = rest.get(..2).unwrap_or("..").as_bytes();
                let staged = status_from_xy_column(xy[0]);
                let unstaged = status_from_xy_column(xy[1]);
                if kind == "2" {
                    // Rename and copy records carry the original path next.
                    let _ = records.next();
                }
                record_entry(summary, staged, unstaged);
            }
            "u" => record_entry(summary, Some(VcsStatus::Conflicted), None),
            "?" => record_entry(summary, None, Some(VcsStatus::Untracked)),
            "!" if !summary.is_directory => summary.ignored = true,
            _ => {}
        }
    }
}

fn record_entry(
    summary: &mut GitPathSummary,
    staged: Option<VcsStatus>,
    unstaged: Option<VcsStatus>,
) {
    if summary.is_directory {
        if let Some(status) = crate::fs::worse_status(staged, unstaged) {
            summary.changes.record(status);
        }
    } else {
        summary.staged_status = staged;
        summary.unstaged_status = unstaged;
    }
}

fn apply_branch_header(summary: &mut GitPathSummary, header: &str) {
    if let Some(oid) = header.strip_prefix("branch.oid ") {
        if oid != "(initial)" {
            summary.head_short_hash = Some(oid.chars().take(7).collect());
        }
    } else if let Some(head) = header.strip_prefix("branch.head ") {
        if head != "(detached)" {
            summary.branch = Some(head.to_string());
        }
    } else if let Some(upstream) = header.strip_prefix("branch.upstream ") {
        summary.upstream = Some(upstream.to_string());
    } else if let Some(counts) = header.strip_prefix("branch.ab ") {
        for value in counts.split_whitespace() {
            if let Some(ahead) = value.strip_prefix('+') {
                summary.ahead = ahead.parse().unwrap_or(0);
            } else if let Some(behind) = value.strip_prefix('-') {
                summary.behind = behind.parse().unwrap_or(0);
            }
        }
    }
}

fn status_from_xy_column(code: u8) -> Option<VcsStatus> {
    match code {
        b'M' | b'T' => Some(VcsStatus::Modified),
        b'A' => Some(VcsStatus::Added),
        b'D' => Some(VcsStatus::Deleted),
        b'R' | b'C' => Some(VcsStatus::Renamed),
        b'U' => Some(VcsStatus::Conflicted),
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

fn parse_commit_line(text: &str) -> Option<GitCommitSummary> {
    let line = text.trim_end_matches(['\r', '\n']);
    let mut fields = line.split('\0');
    let hash = fields.next().filter(|value| !value.is_empty())?;
    Some(GitCommitSummary {
        hash: hash.to_string(),
        short_hash: fields.next()?.to_string(),
        author: fields.next()?.to_string(),
        unix_seconds: fields.next()?.parse().ok()?,
        subject: fields.next().unwrap_or_default().to_string(),
    })
}

fn parse_numstat(text: &str) -> (u32, u32) {
    text.lines().fold((0, 0), |(added, removed), line| {
        let mut fields = line.split('\t');
        // Binary files report "-" for both counts.
        let line_added = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let line_removed = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (added + line_added, removed + line_removed)
    })
}

fn parse_unified_diff(text: &str) -> Vec<GitDiffLine> {
    let mut lines = Vec::new();
    let mut in_hunk = false;
    for line in text.lines() {
        if lines.len() >= DIFF_PREVIEW_MAX_LINES {
            break;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            lines.push(GitDiffLine {
                kind: GitDiffLineKind::Hunk,
                text: line.to_string(),
            });
            continue;
        }
        if line.starts_with("diff --git") {
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if line.starts_with("Binary files") {
                lines.push(GitDiffLine {
                    kind: GitDiffLineKind::Hunk,
                    text: "二进制文件已变化".to_string(),
                });
            }
            continue;
        }
        let (kind, body) = match line.as_bytes().first() {
            Some(b'+') => (GitDiffLineKind::Added, &line[1..]),
            Some(b'-') => (GitDiffLineKind::Removed, &line[1..]),
            Some(b' ') => (GitDiffLineKind::Context, &line[1..]),
            _ => continue,
        };
        lines.push(GitDiffLine {
            kind,
            text: body.to_string(),
        });
    }
    lines
}

fn run_git(current_dir: &Path, args: &[&str]) -> Result<GitCommandResult, GitError> {
    GitService::shared().run_preview_command(current_dir, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

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
            "kite-git-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo root");
        git_ok(&root, &["init", "--quiet", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "kite@example.com"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "core.autocrlf", "false"]);
        root
    }

    #[test]
    fn parses_status_v2_for_a_file() {
        let mut summary = GitPathSummary {
            repository_root: PathBuf::from("/repo"),
            relative_path: "a.rs".into(),
            branch: Some("main".into()),
            head_short_hash: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            operation: None,
            staged_status: None,
            unstaged_status: None,
            ignored: false,
            last_commit: None,
            lines_added: 0,
            lines_removed: 0,
            changes: GitChangeCounts::default(),
            is_directory: false,
        };
        let output = b"# branch.oid 0123456789abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\x001 MM N... 100644 100644 100644 aaa bbb a.rs\0";
        apply_status_v2(&mut summary, output);
        assert_eq!(summary.head_short_hash.as_deref(), Some("0123456"));
        assert_eq!(summary.upstream.as_deref(), Some("origin/main"));
        assert_eq!((summary.ahead, summary.behind), (2, 1));
        assert_eq!(summary.staged_status, Some(VcsStatus::Modified));
        assert_eq!(summary.unstaged_status, Some(VcsStatus::Modified));
    }

    #[test]
    fn counts_directory_changes_and_skips_rename_source() {
        let mut summary = GitPathSummary {
            repository_root: PathBuf::from("/repo"),
            relative_path: ".".into(),
            branch: None,
            head_short_hash: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            operation: None,
            staged_status: None,
            unstaged_status: None,
            ignored: false,
            last_commit: None,
            lines_added: 0,
            lines_removed: 0,
            changes: GitChangeCounts::default(),
            is_directory: true,
        };
        let output = b"1 .M N... 100644 100644 100644 a b src/a.rs\x002 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0u UU N... 100644 100644 100644 100644 a b c conflict.rs\0? notes.txt\0! build/\0";
        apply_status_v2(&mut summary, output);
        assert_eq!(summary.changes.modified, 1);
        assert_eq!(summary.changes.renamed, 1);
        assert_eq!(summary.changes.conflicted, 1);
        assert_eq!(summary.changes.untracked, 1);
        assert_eq!(summary.changes.total(), 4);
        assert!(!summary.ignored);
    }

    #[test]
    fn parses_unified_diff_lines() {
        let diff = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n";
        let lines = parse_unified_diff(diff);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].kind, GitDiffLineKind::Hunk);
        assert_eq!(
            lines[1],
            GitDiffLine {
                kind: GitDiffLineKind::Context,
                text: "keep".into()
            }
        );
        assert_eq!(lines[2].kind, GitDiffLineKind::Removed);
        assert_eq!(lines[3].kind, GitDiffLineKind::Added);
    }

    #[test]
    fn parses_numstat_and_ignores_binary_counts() {
        assert_eq!(
            parse_numstat("3\t1\ta.rs\n-\t-\timg.png\n2\t0\tb.rs\n"),
            (5, 1)
        );
    }

    #[test]
    fn returns_none_outside_a_work_tree() {
        if !git_available() {
            return;
        }
        let root = std::env::temp_dir().join(format!("kite-git-none-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create plain dir");
        // The temp directory itself may sit inside a repository on CI hosts.
        if let Ok(Some(summary)) = git_path_summary(&root) {
            assert!(!root.starts_with(&summary.repository_root) || summary.relative_path != ".");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn summarizes_a_file_and_directory_in_a_real_repository() {
        if !git_available() {
            return;
        }
        let root = temp_repo("summary");
        std::fs::create_dir_all(root.join("src")).expect("create src");
        std::fs::write(root.join("src").join("a.txt"), "one\ntwo\n").expect("write a");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial commit"]);
        std::fs::write(root.join("src").join("a.txt"), "one\nTWO\nthree\n").expect("edit a");
        std::fs::write(root.join("src").join("new.txt"), "fresh\n").expect("write new");

        let file = git_path_summary(&root.join("src").join("a.txt"))
            .expect("file summary")
            .expect("inside repo");
        assert_eq!(file.branch.as_deref(), Some("main"));
        assert_eq!(file.unstaged_status, Some(VcsStatus::Modified));
        assert_eq!(file.staged_status, None);
        assert_eq!((file.lines_added, file.lines_removed), (2, 1));
        let commit = file.last_commit.expect("last commit");
        assert_eq!(commit.subject, "initial commit");
        assert_eq!(commit.author, "Kite Test");

        let directory = git_path_summary(&root.join("src"))
            .expect("dir summary")
            .expect("inside repo");
        assert!(directory.is_directory);
        assert_eq!(directory.changes.modified, 1);
        assert_eq!(directory.changes.untracked, 1);

        let diff = git_file_diff(&root.join("src").join("a.txt")).expect("diff");
        assert!(
            diff.iter()
                .any(|line| line.kind == GitDiffLineKind::Removed && line.text == "two")
        );
        assert!(
            diff.iter()
                .any(|line| line.kind == GitDiffLineKind::Added && line.text == "three")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handles_an_unborn_branch() {
        if !git_available() {
            return;
        }
        let root = temp_repo("unborn");
        std::fs::write(root.join("a.txt"), "x\n").expect("write");
        git_ok(&root, &["add", "a.txt"]);
        let file = git_path_summary(&root.join("a.txt"))
            .expect("summary")
            .expect("inside repo");
        assert_eq!(file.staged_status, Some(VcsStatus::Added));
        assert_eq!(file.branch.as_deref(), Some("main"));
        assert!(file.last_commit.is_none());
        let diff = git_file_diff(&root.join("a.txt")).expect("diff");
        assert!(diff.iter().any(|line| line.kind == GitDiffLineKind::Added));
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn parses_changed_file_paths_and_statuses() {
        let output = b"# branch.oid 0123456789abcdef\0# branch.head main\0\
1 .M N... 100644 100644 100644 a b src/a file.rs\0\
2 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c conflict.rs\0\
? notes.txt\0";
        let files = parse_changed_files(output);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].path, "src/a file.rs");
        assert_eq!(files[0].unstaged_status, Some(VcsStatus::Modified));
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].staged_status, Some(VcsStatus::Renamed));
        assert_eq!(files[2].path, "conflict.rs");
        assert_eq!(files[2].staged_status, Some(VcsStatus::Conflicted));
        assert_eq!(files[3].path, "notes.txt");
        assert_eq!(files[3].unstaged_status, Some(VcsStatus::Untracked));
    }

    #[test]
    fn maps_operation_controls_for_supported_states() {
        for (operation, action, expected) in [
            (
                GitOperationState::Rebasing,
                GitOperationAction::Continue,
                &["rebase", "--continue"][..],
            ),
            (
                GitOperationState::Rebasing,
                GitOperationAction::Skip,
                &["rebase", "--skip"][..],
            ),
            (
                GitOperationState::Rebasing,
                GitOperationAction::Abort,
                &["rebase", "--abort"][..],
            ),
            (
                GitOperationState::Merging,
                GitOperationAction::Continue,
                &["merge", "--continue"][..],
            ),
            (
                GitOperationState::Merging,
                GitOperationAction::Abort,
                &["merge", "--abort"][..],
            ),
            (
                GitOperationState::CherryPicking,
                GitOperationAction::Continue,
                &["cherry-pick", "--continue"][..],
            ),
            (
                GitOperationState::CherryPicking,
                GitOperationAction::Skip,
                &["cherry-pick", "--skip"][..],
            ),
            (
                GitOperationState::CherryPicking,
                GitOperationAction::Abort,
                &["cherry-pick", "--abort"][..],
            ),
            (
                GitOperationState::Reverting,
                GitOperationAction::Continue,
                &["revert", "--continue"][..],
            ),
            (
                GitOperationState::Reverting,
                GitOperationAction::Skip,
                &["revert", "--skip"][..],
            ),
            (
                GitOperationState::Reverting,
                GitOperationAction::Abort,
                &["revert", "--abort"][..],
            ),
        ] {
            assert_eq!(operation_control_args(operation, action).unwrap(), expected);
        }
        assert!(
            operation_control_args(GitOperationState::Merging, GitOperationAction::Skip).is_err()
        );
    }

    #[test]
    fn reads_repository_snapshot_for_the_git_client() {
        if !git_available() {
            return;
        }
        let root = dunce::canonicalize(temp_repo("snapshot")).expect("canonical temp repo");
        std::fs::write(root.join("tracked.txt"), "one\n").expect("write tracked");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial commit"]);
        std::fs::write(root.join("tracked.txt"), "one\ntwo\n").expect("edit tracked");
        std::fs::write(root.join("new file.txt"), "fresh\n").expect("write untracked");

        assert_eq!(
            git_repository_root(&root.join("tracked.txt")).expect("root"),
            Some(root.clone())
        );
        let snapshot = git_repository_snapshot(&root.join("tracked.txt"))
            .expect("snapshot")
            .expect("repo");
        assert_eq!(snapshot.repository_root, root);
        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.changes.modified, 1);
        assert_eq!(snapshot.changes.untracked, 1);
        assert_eq!(snapshot.changed_files.len(), 2);
        assert_eq!(snapshot.recent_commits.len(), 1);
        assert_eq!(snapshot.recent_commits[0].subject, "initial commit");

        let _ = std::fs::remove_dir_all(snapshot.repository_root);
    }

    #[test]
    fn detects_rebase_merge_cherry_pick_and_revert_markers() {
        let root = std::env::temp_dir().join(format!(
            "kite-git-operation-markers-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("rebase-merge")).expect("create rebase marker");
        assert_eq!(operation_state(&root), Some(GitOperationState::Rebasing));
        std::fs::remove_dir_all(root.join("rebase-merge")).expect("remove rebase marker");

        for (marker, expected) in [
            ("MERGE_HEAD", GitOperationState::Merging),
            ("CHERRY_PICK_HEAD", GitOperationState::CherryPicking),
            ("REVERT_HEAD", GitOperationState::Reverting),
            ("BISECT_LOG", GitOperationState::Bisecting),
        ] {
            std::fs::write(root.join(marker), "marker\n").expect("write marker");
            assert_eq!(operation_state(&root), Some(expected));
            std::fs::remove_file(root.join(marker)).expect("remove marker");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
