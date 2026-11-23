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
use crate::fs::VcsStatus;

/// Text larger than this is not diffed for the preview.
const DIFF_PREVIEW_MAX_BYTES: usize = 256 * 1024;
const DIFF_PREVIEW_MAX_LINES: usize = 400;

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
}
