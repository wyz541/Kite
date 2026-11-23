//! Shared Git service for previews, merges, and conflict resolution.
//!
//! All operations use the git CLI so they follow the user's Git configuration.
//! Read-only preview queries are bounded to the selected path; merge and
//! conflict operations resolve the enclosing work tree first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::fs::VcsStatus;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitMergeStrategy {
    Merge,
    NoFastForward,
    FastForwardOnly,
    Squash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeSpec {
    pub target: String,
    pub strategy: GitMergeStrategy,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBranchSummary {
    pub name: String,
    pub short_hash: String,
    pub subject: String,
    pub is_remote: bool,
    pub is_current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitConflictKind {
    BothModified,
    ModifyDeleteOurs,
    ModifyDeleteTheirs,
    AddAdd,
    Rename,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitConflictEntry {
    pub path: String,
    pub base_path: Option<String>,
    pub ours_path: Option<String>,
    pub theirs_path: Option<String>,
    pub kind: GitConflictKind,
    pub base_present: bool,
    pub ours_present: bool,
    pub theirs_present: bool,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitConflictState {
    pub repository_root: PathBuf,
    pub operation: Option<GitOperationState>,
    pub entries: Vec<GitConflictEntry>,
    pub can_continue: bool,
    pub can_abort: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeOutcome {
    pub message: String,
    pub conflicts: Option<GitConflictState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitConflictSide {
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitConflictSides {
    pub base_text: Option<String>,
    pub ours_text: Option<String>,
    pub theirs_text: Option<String>,
    pub base_present: bool,
    pub ours_present: bool,
    pub theirs_present: bool,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitMergeLineKind {
    Context,
    Ours,
    Base,
    Theirs,
    Marker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeLine {
    pub kind: GitMergeLineKind,
    pub text: String,
    pub conflict_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeConflictHunk {
    pub index: u32,
    pub base_text: String,
    pub ours_text: String,
    pub theirs_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitThreeWayMerge {
    pub merged_text: String,
    pub lines: Vec<GitMergeLine>,
    pub conflicts: Vec<GitMergeConflictHunk>,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitConflictChoiceSide {
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitConflictChoice {
    pub index: u32,
    pub side: GitConflictChoiceSide,
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
    let Some(probe) = run_git(
        start,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    ) else {
        return Ok(None);
    };
    if !probe.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&probe.stdout).into_owned();
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

    if let Some(output) = run_git(
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
    ) && output.status.success()
    {
        apply_status_v2(&mut summary, &output.stdout);
    }

    if let Some(output) = run_git(
        &root,
        &[
            "log",
            "-1",
            "--format=%H%x00%h%x00%an%x00%at%x00%s",
            "--",
            &relative_arg,
        ],
    ) && output.status.success()
    {
        summary.last_commit = parse_commit_line(&String::from_utf8_lossy(&output.stdout));
    }

    if summary.head_short_hash.is_some()
        && let Some(output) = run_git(
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
        && output.status.success()
    {
        let (added, removed) = parse_numstat(&String::from_utf8_lossy(&output.stdout));
        summary.lines_added = added;
        summary.lines_removed = removed;
    }

    Ok(summary)
}

/// Returns the working-tree diff of a file against HEAD, bounded for preview.
pub fn git_file_diff(path: &Path) -> Result<Vec<GitDiffLine>, String> {
    let start = path.parent().unwrap_or(path);
    let probe = run_git(start, &["rev-parse", "--show-toplevel"])
        .ok_or_else(|| "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string())?;
    if !probe.status.success() {
        return Err("所选项目不在 Git 工作区中".to_string());
    }
    let root_text = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    let root = dunce::canonicalize(&root_text).unwrap_or_else(|_| PathBuf::from(root_text));
    let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| "所选项目不在 Git 工作区中".to_string())?
        .to_string_lossy()
        .replace('\\', "/");

    let has_head = run_git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .is_some_and(|output| output.status.success());
    let mut args = vec!["diff", "--no-ext-diff", "--no-color", "-U3"];
    if has_head {
        args.push("HEAD");
    } else {
        args.push("--cached");
    }
    args.push("--");
    args.push(&relative);
    let output = run_git(&root, &args).ok_or_else(|| "无法运行 git diff".to_string())?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    if output.stdout.len() > DIFF_PREVIEW_MAX_BYTES {
        return Err("差异过大，未在预览中显示".to_string());
    }
    Ok(parse_unified_diff(&String::from_utf8_lossy(&output.stdout)))
}

pub fn git_merge_branches(path: &Path) -> Result<Vec<GitBranchSummary>, String> {
    let repository = repository_for_path(path)?;
    let current = run_git(
        &repository.root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .filter(|output| output.status.success())
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    .filter(|value| !value.is_empty());
    let output = run_git(
        &repository.root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(refname:short)%00%(objectname:short)%00%(subject)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .ok_or_else(|| "无法列出 Git 分支".to_string())?;
    if !output.status.success() {
        return Err(output_error(&output));
    }

    let mut branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let reference = fields.next()?;
            let name = fields.next()?.to_string();
            let short_hash = fields.next().unwrap_or_default().to_string();
            let subject = fields.next().unwrap_or_default().to_string();
            if reference.ends_with("/HEAD") || name.is_empty() || current.as_deref() == Some(&name)
            {
                return None;
            }
            Some(GitBranchSummary {
                name,
                short_hash,
                subject,
                is_remote: reference.starts_with("refs/remotes/"),
                is_current: false,
            })
        })
        .collect::<Vec<_>>();
    branches.sort_by(|left, right| {
        left.is_remote
            .cmp(&right.is_remote)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(branches)
}

pub fn git_merge(path: &Path, spec: &GitMergeSpec) -> Result<GitMergeOutcome, String> {
    let repository = repository_for_path(path)?;
    let target = spec.target.trim();
    if target.is_empty() {
        return Err("请选择要合并的分支或提交".to_string());
    }
    if target.starts_with('-') {
        return Err("无效的合并目标".to_string());
    }
    if run_git(
        &repository.root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{target}^{{commit}}"),
        ],
    )
    .is_none_or(|output| !output.status.success())
    {
        return Err(format!("找不到合并目标：{target}"));
    }

    let message = spec
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut args = vec!["merge", "--no-edit"];
    match spec.strategy {
        GitMergeStrategy::Merge => {}
        GitMergeStrategy::NoFastForward => args.push("--no-ff"),
        GitMergeStrategy::FastForwardOnly => args.push("--ff-only"),
        GitMergeStrategy::Squash => args.push("--squash"),
    }
    if let Some(message) = message
        && spec.strategy != GitMergeStrategy::Squash
    {
        args.push("-m");
        args.push(message);
    }
    args.push(target);
    let output = run_git(&repository.root, &args)
        .ok_or_else(|| "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string())?;
    let state = git_conflicts_for_repository(&repository)?;

    if !output.status.success() && state.entries.is_empty() {
        return Err(output_error(&output));
    }

    if !state.entries.is_empty() {
        return Ok(GitMergeOutcome {
            message: format!(
                "合并产生 {} 个冲突，请在冲突解决视图中处理",
                state.entries.len()
            ),
            conflicts: Some(state),
        });
    }

    if spec.strategy == GitMergeStrategy::Squash {
        if let Some(message) = message {
            let commit = run_git(&repository.root, &["commit", "-m", message])
                .ok_or_else(|| "无法提交压缩合并结果".to_string())?;
            if !commit.status.success() {
                return Err(output_error(&commit));
            }
            return Ok(GitMergeOutcome {
                message: format!("已压缩合并并提交：{}", first_nonempty_line(&commit.stdout)),
                conflicts: None,
            });
        }
        return Ok(GitMergeOutcome {
            message: "已压缩合并到暂存区，提交消息留空所以尚未提交".to_string(),
            conflicts: None,
        });
    }

    Ok(GitMergeOutcome {
        message: if output.status.success() {
            let summary = first_nonempty_line(&output.stdout);
            if summary.is_empty() {
                "合并完成".to_string()
            } else {
                summary
            }
        } else {
            "合并完成".to_string()
        },
        conflicts: None,
    })
}

pub fn git_conflicts(path: &Path) -> Result<Option<GitConflictState>, String> {
    let Some(repository) = repository_for_path_optional(path)? else {
        return Ok(None);
    };
    git_conflicts_for_repository(&repository).map(Some)
}

pub fn git_conflict_sides(
    path: &Path,
    conflict: &GitConflictEntry,
) -> Result<GitConflictSides, String> {
    let repository = repository_for_path(path)?;
    let base = conflict
        .base_path
        .as_deref()
        .and_then(|value| read_stage_blob(&repository, 1, value));
    let ours = conflict
        .ours_path
        .as_deref()
        .and_then(|value| read_stage_blob(&repository, 2, value));
    let theirs = conflict
        .theirs_path
        .as_deref()
        .and_then(|value| read_stage_blob(&repository, 3, value));
    let binary = [base.as_deref(), ours.as_deref(), theirs.as_deref()]
        .into_iter()
        .flatten()
        .any(is_binary);
    Ok(GitConflictSides {
        base_text: if binary {
            None
        } else {
            base.as_deref().map(text_from_bytes)
        },
        ours_text: if binary {
            None
        } else {
            ours.as_deref().map(text_from_bytes)
        },
        theirs_text: if binary {
            None
        } else {
            theirs.as_deref().map(text_from_bytes)
        },
        base_present: base.is_some(),
        ours_present: ours.is_some(),
        theirs_present: theirs.is_some(),
        binary,
    })
}

pub fn git_three_way_merge(
    path: &Path,
    conflict: &GitConflictEntry,
) -> Result<GitThreeWayMerge, String> {
    let sides = git_conflict_sides(path, conflict)?;
    if sides.binary {
        return Ok(GitThreeWayMerge {
            merged_text: String::new(),
            lines: Vec::new(),
            conflicts: Vec::new(),
            binary: true,
        });
    }
    if conflict.kind == GitConflictKind::Rename {
        return Err("重命名冲突请选择保留自己、对方或两者".to_string());
    }

    let directory = temporary_git_directory("merge-file");
    let ours_path = directory.join("ours.txt");
    let base_path = directory.join("base.txt");
    let theirs_path = directory.join("theirs.txt");
    std::fs::write(&ours_path, sides.ours_text.as_deref().unwrap_or_default())
        .map_err(|error| format!("无法准备三方合并：{error}"))?;
    std::fs::write(&base_path, sides.base_text.as_deref().unwrap_or_default())
        .map_err(|error| format!("无法准备三方合并：{error}"))?;
    std::fs::write(
        &theirs_path,
        sides.theirs_text.as_deref().unwrap_or_default(),
    )
    .map_err(|error| format!("无法准备三方合并：{error}"))?;
    let output = Command::new("git")
        .args([
            "merge-file",
            "-p",
            "--diff3",
            "-L",
            "ours",
            "-L",
            "base",
            "-L",
            "theirs",
        ])
        .arg(&ours_path)
        .arg(&base_path)
        .arg(&theirs_path)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("无法运行 git merge-file：{error}"))?;
    let _ = std::fs::remove_dir_all(&directory);
    match output.status.code() {
        Some(0 | 1) => {}
        _ => return Err(output_error(&output)),
    }
    let merged_text = String::from_utf8_lossy(&output.stdout).into_owned();
    let (lines, conflicts) = parse_three_way_merge(&merged_text);
    Ok(GitThreeWayMerge {
        merged_text,
        lines,
        conflicts,
        binary: false,
    })
}

pub fn apply_conflict_choices(merged_text: &str, choices: &[GitConflictChoice]) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Context,
        Ours(u32),
        Base(u32),
        Theirs(u32),
    }

    let ends_with_newline = merged_text.ends_with('\n');
    let uses_crlf = merged_text.contains("\r\n");
    let normalized = if uses_crlf {
        merged_text.replace("\r\n", "\n")
    } else {
        merged_text.to_string()
    };
    let mut state = State::Context;
    let mut output = Vec::new();
    let mut raw_block = Vec::new();
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    let mut conflict_index = 0u32;

    for line in normalized.lines() {
        match state {
            State::Context if line.starts_with("<<<<<<<") => {
                raw_block.push(line.to_string());
                state = State::Ours(conflict_index);
            }
            State::Ours(index) if line.starts_with("|||||||") => {
                raw_block.push(line.to_string());
                state = State::Base(index);
            }
            State::Ours(index) | State::Base(index) if line == "=======" => {
                raw_block.push(line.to_string());
                state = State::Theirs(index);
            }
            State::Theirs(index) if line.starts_with(">>>>>>>") => {
                raw_block.push(line.to_string());
                let choice = choices
                    .iter()
                    .rev()
                    .find(|choice| choice.index == index)
                    .map(|choice| choice.side);
                match choice {
                    Some(GitConflictChoiceSide::Ours) => output.extend(ours.iter().cloned()),
                    Some(GitConflictChoiceSide::Theirs) => output.extend(theirs.iter().cloned()),
                    Some(GitConflictChoiceSide::Both) => {
                        output.extend(ours.iter().cloned());
                        output.extend(theirs.iter().cloned());
                    }
                    None => output.extend(raw_block.iter().cloned()),
                }
                raw_block.clear();
                ours.clear();
                theirs.clear();
                conflict_index = conflict_index.saturating_add(1);
                state = State::Context;
            }
            State::Context => output.push(line.to_string()),
            State::Ours(index) => {
                ours.push(line.to_string());
                raw_block.push(line.to_string());
                state = State::Ours(index);
            }
            State::Base(index) => {
                raw_block.push(line.to_string());
                state = State::Base(index);
            }
            State::Theirs(index) => {
                theirs.push(line.to_string());
                raw_block.push(line.to_string());
                state = State::Theirs(index);
            }
        }
    }
    if !raw_block.is_empty() {
        output.extend(raw_block);
    }
    let mut result = output.join("\n");
    if ends_with_newline {
        result.push('\n');
    }
    if uses_crlf {
        result = result.replace('\n', "\r\n");
    }
    result
}

pub fn git_resolve_conflict(
    path: &Path,
    relative_path: &str,
    content: Option<&str>,
) -> Result<(), String> {
    let repository = repository_for_path(path)?;
    let relative = validate_relative_path(relative_path)?;
    let target = repository.root.join(&relative);
    if let Some(content) = content {
        if contains_conflict_markers(content) {
            return Err("合并结果中仍有冲突标记，请先解决所有冲突块".to_string());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建合并结果目录：{error}"))?;
        }
        std::fs::write(&target, content).map_err(|error| format!("无法写入合并结果：{error}"))?;
    } else if target.exists() {
        std::fs::remove_file(&target).map_err(|error| format!("无法删除合并结果：{error}"))?;
    }
    let relative_arg = git_path_string(&relative);
    let output = run_git(&repository.root, &["add", "-A", "--", &relative_arg])
        .ok_or_else(|| "无法运行 git add".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(&output))
    }
}

pub fn git_choose_conflict_side(
    path: &Path,
    conflict: &GitConflictEntry,
    side: GitConflictSide,
) -> Result<(), String> {
    let repository = repository_for_path(path)?;
    if conflict.kind == GitConflictKind::Rename {
        let ours = conflict
            .ours_path
            .as_deref()
            .ok_or_else(|| "重命名冲突缺少自己的版本".to_string())?;
        let theirs = conflict
            .theirs_path
            .as_deref()
            .ok_or_else(|| "重命名冲突缺少对方的版本".to_string())?;
        let base = conflict.base_path.as_deref().unwrap_or(ours);
        match side {
            GitConflictSide::Ours => {
                stage_delete(&repository, &[base, theirs])?;
                write_stage_to_path(&repository, 2, ours)?;
            }
            GitConflictSide::Theirs => {
                stage_delete(&repository, &[base, ours])?;
                write_stage_to_path(&repository, 3, theirs)?;
            }
            GitConflictSide::Both => {
                stage_delete(&repository, &[base])?;
                write_stage_to_path(&repository, 2, ours)?;
                write_stage_to_path(&repository, 3, theirs)?;
            }
        }
        return Ok(());
    }

    if side == GitConflictSide::Both {
        return Err("同一路径不能同时保留双方，请在三方合并视图中逐块选择".to_string());
    }
    let (stage, target) = match side {
        GitConflictSide::Ours => (2, conflict.ours_path.as_deref()),
        GitConflictSide::Theirs => (3, conflict.theirs_path.as_deref()),
        GitConflictSide::Both => unreachable!(),
    };
    match target {
        Some(target) => write_stage_to_path(&repository, stage, target),
        None => {
            let target = conflict
                .ours_path
                .as_deref()
                .or(conflict.theirs_path.as_deref())
                .or(conflict.base_path.as_deref())
                .unwrap_or(&conflict.path);
            stage_delete(&repository, &[target])
        }
    }
}

pub fn git_continue_operation(path: &Path) -> Result<String, String> {
    let repository = repository_for_path(path)?;
    let state = git_conflicts_for_repository(&repository)?;
    if !state.entries.is_empty() {
        return Err(format!("仍有 {} 个冲突文件未解决", state.entries.len()));
    }
    let args: &[&str] = match state.operation {
        Some(GitOperationState::Merging) => &["commit", "--no-edit"],
        Some(GitOperationState::Rebasing) => &["rebase", "--continue"],
        Some(GitOperationState::CherryPicking) => &["cherry-pick", "--continue"],
        Some(GitOperationState::Reverting) => &["revert", "--continue"],
        Some(GitOperationState::Bisecting) => {
            return Err("当前正在进行 bisect，请先执行 git bisect reset".to_string());
        }
        None => return Err("当前没有可继续的 Git 操作".to_string()),
    };
    let output = run_git_with_env(
        &repository.root,
        args,
        &[("GIT_EDITOR", "true"), ("GIT_SEQUENCE_EDITOR", "true")],
    )
    .ok_or_else(|| "无法继续 Git 操作".to_string())?;
    if output.status.success() {
        let summary = first_nonempty_line(&output.stdout);
        Ok(if summary.is_empty() {
            "已继续 Git 操作".to_string()
        } else {
            summary
        })
    } else {
        Err(output_error(&output))
    }
}

pub fn git_abort_operation(path: &Path) -> Result<String, String> {
    let repository = repository_for_path(path)?;
    let state = git_conflicts_for_repository(&repository)?;
    let args: &[&str] = match state.operation {
        Some(GitOperationState::Merging) => &["merge", "--abort"],
        Some(GitOperationState::Rebasing) => &["rebase", "--abort"],
        Some(GitOperationState::CherryPicking) => &["cherry-pick", "--abort"],
        Some(GitOperationState::Reverting) => &["revert", "--abort"],
        Some(GitOperationState::Bisecting) => &["bisect", "reset"],
        None => return Err("当前没有可中止的 Git 操作".to_string()),
    };
    let output = run_git(&repository.root, args).ok_or_else(|| "无法中止 Git 操作".to_string())?;
    if output.status.success() {
        Ok("已中止当前 Git 操作".to_string())
    } else {
        Err(output_error(&output))
    }
}

pub fn git_open_merge_tool(path: &Path, conflict: &GitConflictEntry) -> Result<String, String> {
    let repository = repository_for_path(path)?;
    let configured = run_git(&repository.root, &["config", "--get", "merge.tool"])
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "未配置 merge.tool，请先在 Git 配置中选择外部合并工具".to_string())?;
    let target = conflict
        .ours_path
        .as_deref()
        .or(conflict.theirs_path.as_deref())
        .or(conflict.base_path.as_deref())
        .unwrap_or(&conflict.path);
    let relative = validate_relative_path(target)?;
    let relative = git_path_string(&relative);
    let args = [
        "mergetool",
        "--no-prompt",
        "--tool",
        configured.as_str(),
        "--",
        relative.as_str(),
    ];
    let output = run_git_with_env(&repository.root, &args, &[("GIT_EDITOR", "true")])
        .ok_or_else(|| "无法启动外部合并工具".to_string())?;
    if output.status.success() {
        Ok(format!("已使用 {configured} 处理 {target}"))
    } else {
        Err(output_error(&output))
    }
}

#[derive(Clone, Debug)]
struct GitRepository {
    root: PathBuf,
    git_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct GitStageEntry {
    stage: u8,
    path: String,
}

fn repository_for_path(path: &Path) -> Result<GitRepository, String> {
    repository_for_path_optional(path)?.ok_or_else(|| "所选项目不在 Git 工作区中".to_string())
}

fn repository_for_path_optional(path: &Path) -> Result<Option<GitRepository>, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let Some(output) = run_git(
        start,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    ) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    Ok(Some(GitRepository {
        root: dunce::canonicalize(root.trim()).unwrap_or_else(|_| PathBuf::from(root.trim())),
        git_dir: dunce::canonicalize(git_dir.trim())
            .unwrap_or_else(|_| PathBuf::from(git_dir.trim())),
    }))
}

fn git_conflicts_for_repository(repository: &GitRepository) -> Result<GitConflictState, String> {
    let output = run_git(&repository.root, &["ls-files", "-u", "-z"])
        .ok_or_else(|| "无法读取 Git 冲突".to_string())?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let records = parse_stage_entries(&output.stdout);
    let mut grouped: BTreeMap<String, BTreeMap<u8, GitStageEntry>> = BTreeMap::new();
    for record in records {
        grouped
            .entry(record.path.clone())
            .or_default()
            .insert(record.stage, record);
    }

    let mut rename_paths = Vec::new();
    let mut entries = Vec::new();
    for [base, ours, theirs] in rename_conflict_groups(repository, &grouped) {
        rename_paths.extend([base.clone(), ours.clone(), theirs.clone()]);
        let binary = [
            read_stage_blob(repository, 1, &base),
            read_stage_blob(repository, 2, &ours),
            read_stage_blob(repository, 3, &theirs),
        ]
        .into_iter()
        .flatten()
        .any(|bytes| is_binary(&bytes));
        entries.push(GitConflictEntry {
            path: format!("{ours} ↔ {theirs}"),
            base_path: Some(base),
            ours_path: Some(ours),
            theirs_path: Some(theirs),
            kind: GitConflictKind::Rename,
            base_present: true,
            ours_present: true,
            theirs_present: true,
            binary,
        });
    }

    for (path, stages) in grouped {
        if rename_paths.contains(&path) {
            continue;
        }
        let base = stages.get(&1);
        let ours = stages.get(&2);
        let theirs = stages.get(&3);
        let kind = match (base.is_some(), ours.is_some(), theirs.is_some()) {
            (true, true, true) => GitConflictKind::BothModified,
            (true, true, false) => GitConflictKind::ModifyDeleteTheirs,
            (true, false, true) => GitConflictKind::ModifyDeleteOurs,
            (false, true, true) => GitConflictKind::AddAdd,
            _ => GitConflictKind::Other,
        };
        let binary = [base, ours, theirs]
            .into_iter()
            .flatten()
            .filter_map(|entry| read_stage_blob(repository, entry.stage, &entry.path))
            .any(|bytes| is_binary(&bytes));
        entries.push(GitConflictEntry {
            base_path: base.map(|entry| entry.path.clone()),
            ours_path: ours.map(|entry| entry.path.clone()),
            theirs_path: theirs.map(|entry| entry.path.clone()),
            kind,
            base_present: base.is_some(),
            ours_present: ours.is_some(),
            theirs_present: theirs.is_some(),
            binary,
            path,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let operation = operation_state(&repository.git_dir);
    Ok(GitConflictState {
        repository_root: repository.root.clone(),
        operation,
        entries,
        can_continue: matches!(
            operation,
            Some(
                GitOperationState::Merging
                    | GitOperationState::Rebasing
                    | GitOperationState::CherryPicking
                    | GitOperationState::Reverting
            )
        ),
        can_abort: operation.is_some(),
    })
}

fn parse_stage_entries(bytes: &[u8]) -> Vec<GitStageEntry> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            let (metadata, path) = record.split_once('\t')?;
            let stage = metadata.split_whitespace().nth(2)?.parse().ok()?;
            Some(GitStageEntry {
                stage,
                path: path.to_string(),
            })
        })
        .collect()
}

fn rename_conflict_groups(
    repository: &GitRepository,
    grouped: &BTreeMap<String, BTreeMap<u8, GitStageEntry>>,
) -> Vec<[String; 3]> {
    let Some(output) = run_git(
        &repository.root,
        &["status", "--porcelain=v2", "-z", "--untracked-files=no"],
    ) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            record.strip_prefix("u ")?;
            record.splitn(11, ' ').nth(10).map(str::to_string)
        })
        .collect::<Vec<_>>();
    let mut groups = Vec::new();
    let mut index = 0;
    while index + 2 < paths.len() {
        let candidates = [&paths[index], &paths[index + 1], &paths[index + 2]];
        if candidates[0] != candidates[1]
            && candidates[0] != candidates[2]
            && candidates[1] != candidates[2]
            && candidates.iter().all(|path| grouped.contains_key(*path))
        {
            let stages = candidates.map(|path| {
                grouped
                    .get(path)
                    .and_then(|stages| (stages.len() == 1).then(|| stages.keys().next().copied()))
                    .flatten()
            });
            if let [Some(first), Some(second), Some(third)] = stages
                && [first, second, third].contains(&1)
                && [first, second, third].contains(&2)
                && [first, second, third].contains(&3)
            {
                let base =
                    candidates[stages.iter().position(|stage| *stage == Some(1)).unwrap()].clone();
                let ours =
                    candidates[stages.iter().position(|stage| *stage == Some(2)).unwrap()].clone();
                let theirs =
                    candidates[stages.iter().position(|stage| *stage == Some(3)).unwrap()].clone();
                groups.push([base, ours, theirs]);
                index += 3;
                continue;
            }
        }
        index += 1;
    }
    groups
}

fn read_stage_blob(repository: &GitRepository, stage: u8, path: &str) -> Option<Vec<u8>> {
    let relative = validate_relative_path(path).ok()?;
    let relative = git_path_string(&relative);
    let spec = format!(":{stage}:{relative}");
    let output = run_git(&repository.root, &["cat-file", "blob", &spec])?;
    output.status.success().then_some(output.stdout)
}

fn write_stage_to_path(repository: &GitRepository, stage: u8, path: &str) -> Result<(), String> {
    let bytes = read_stage_blob(repository, stage, path)
        .ok_or_else(|| format!("冲突文件 {path} 在索引中缺少对应版本"))?;
    let relative = validate_relative_path(path)?;
    let target = repository.root.join(&relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建冲突文件目录：{error}"))?;
    }
    std::fs::write(&target, bytes).map_err(|error| format!("无法写入冲突文件：{error}"))?;
    let relative = git_path_string(&relative);
    let output = run_git(&repository.root, &["add", "--", &relative])
        .ok_or_else(|| "无法暂存冲突结果".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(&output))
    }
}

fn stage_delete(repository: &GitRepository, paths: &[&str]) -> Result<(), String> {
    let relative = paths
        .iter()
        .map(|path| validate_relative_path(path).map(|path| git_path_string(&path)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut args = vec!["rm", "-f", "--ignore-unmatch", "--"];
    args.extend(relative.iter().map(String::as_str));
    let output = run_git(&repository.root, &args).ok_or_else(|| "无法删除冲突文件".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(&output))
    }
}

fn validate_relative_path(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("无效的 Git 路径".to_string());
    }
    Ok(path)
}

fn git_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn text_from_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn contains_conflict_markers(content: &str) -> bool {
    content.lines().any(|line| {
        line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line.starts_with("|||||||")
    })
}

fn temporary_git_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "kite-git-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&directory);
    directory
}

fn parse_three_way_merge(text: &str) -> (Vec<GitMergeLine>, Vec<GitMergeConflictHunk>) {
    #[derive(Clone, Copy)]
    enum State {
        Context,
        Ours(u32),
        Base(u32),
        Theirs(u32),
    }

    let mut state = State::Context;
    let mut lines = Vec::new();
    let mut conflicts = Vec::new();
    let mut ours = Vec::new();
    let mut base = Vec::new();
    let mut theirs = Vec::new();
    let mut conflict_index = 0u32;
    for line in text.lines() {
        match state {
            State::Context if line.starts_with("<<<<<<<") => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(conflict_index),
                });
                state = State::Ours(conflict_index);
            }
            State::Ours(index) if line.starts_with("|||||||") => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Base(index);
            }
            State::Ours(index) | State::Base(index) if line == "=======" => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Theirs(index);
            }
            State::Theirs(index) if line.starts_with(">>>>>>>") => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                conflicts.push(GitMergeConflictHunk {
                    index,
                    base_text: join_lines(&base),
                    ours_text: join_lines(&ours),
                    theirs_text: join_lines(&theirs),
                });
                ours.clear();
                base.clear();
                theirs.clear();
                conflict_index = conflict_index.saturating_add(1);
                state = State::Context;
            }
            State::Context => lines.push(GitMergeLine {
                kind: GitMergeLineKind::Context,
                text: line.to_string(),
                conflict_index: None,
            }),
            State::Ours(index) => {
                ours.push(line.to_string());
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Ours,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Ours(index);
            }
            State::Base(index) => {
                base.push(line.to_string());
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Base,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Base(index);
            }
            State::Theirs(index) => {
                theirs.push(line.to_string());
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Theirs,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Theirs(index);
            }
        }
    }
    (lines, conflicts)
}

fn join_lines(lines: &[String]) -> String {
    let mut value = lines.join("\n");
    if !value.is_empty() {
        value.push('\n');
    }
    value
}

fn first_nonempty_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
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

fn run_git(current_dir: &Path, args: &[&str]) -> Option<Output> {
    run_git_with_env(current_dir, args, &[])
}

fn run_git_with_env(
    current_dir: &Path,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Option<Output> {
    let mut command = Command::new("git");
    command
        .current_dir(current_dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("LC_ALL", "C");
    for (key, value) in environment {
        command.env(key, value);
    }
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command.output().ok()
}

fn output_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!(
            "Git 操作失败，退出码 {}",
            output.status.code().unwrap_or(-1)
        )
    } else {
        format!(
            "Git 操作失败：{}",
            stderr
                .replace(['\r', '\n'], " ")
                .chars()
                .take(300)
                .collect::<String>()
        )
    }
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

    fn git_output(root: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git")
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = git_output(root, args);
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
    fn resolves_a_modify_delete_conflict_and_updates_status() {
        if !git_available() {
            return;
        }
        let root = temp_repo("modify-delete");
        std::fs::write(root.join("f.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("f.txt"), "side\n").expect("modify side");
        git_ok(&root, &["commit", "--quiet", "-am", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        std::fs::remove_file(root.join("f.txt")).expect("delete main file");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "delete"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.operation, Some(GitOperationState::Merging));
        assert_eq!(state.entries.len(), 1);
        let conflict = &state.entries[0];
        assert_eq!(conflict.kind, GitConflictKind::ModifyDeleteOurs);
        assert!(!conflict.ours_present);
        assert!(conflict.theirs_present);
        git_choose_conflict_side(&root, conflict, GitConflictSide::Ours)
            .expect("choose deleted side");

        assert!(!root.join("f.txt").exists());
        assert!(!git_status_text(&root).contains("UU"));
        assert!(!git_status_text(&root).contains(" DU "));
        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        assert!(state.entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_a_binary_conflict_by_choosing_one_side() {
        if !git_available() {
            return;
        }
        let root = temp_repo("binary");
        std::fs::write(root.join("b.bin"), [0, 1, b'b', b'a', b's', b'e'])
            .expect("write binary base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("b.bin"), [0, 2, b's', b'i', b'd', b'e'])
            .expect("write binary side");
        git_ok(&root, &["commit", "--quiet", "-am", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        std::fs::write(root.join("b.bin"), [0, 3, b'm', b'a', b'i', b'n'])
            .expect("write binary main");
        git_ok(&root, &["commit", "--quiet", "-am", "main"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        let conflict = state.entries.first().expect("binary conflict");
        assert!(conflict.binary);
        git_choose_conflict_side(&root, conflict, GitConflictSide::Theirs).expect("choose theirs");
        assert_eq!(
            std::fs::read(root.join("b.bin")).expect("read resolution"),
            [0, 2, b's', b'i', b'd', b'e']
        );
        assert!(
            git_conflicts(&root)
                .expect("conflicts")
                .expect("repo")
                .entries
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn groups_a_rename_conflict_and_keeps_the_chosen_rename() {
        if !git_available() {
            return;
        }
        let root = temp_repo("rename");
        std::fs::write(root.join("original.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        git_ok(&root, &["mv", "original.txt", "side-name.txt"]);
        std::fs::write(root.join("side-name.txt"), "base\nside\n").expect("modify side");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "side rename"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        git_ok(&root, &["mv", "original.txt", "main-name.txt"]);
        std::fs::write(root.join("main-name.txt"), "base\nmain\n").expect("modify main");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "main rename"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.entries.len(), 1);
        let conflict = &state.entries[0];
        assert_eq!(conflict.kind, GitConflictKind::Rename);
        assert_eq!(conflict.base_path.as_deref(), Some("original.txt"));
        assert_eq!(conflict.ours_path.as_deref(), Some("main-name.txt"));
        assert_eq!(conflict.theirs_path.as_deref(), Some("side-name.txt"));
        git_choose_conflict_side(&root, conflict, GitConflictSide::Ours).expect("keep ours");

        assert!(root.join("main-name.txt").exists());
        assert!(!root.join("side-name.txt").exists());
        assert!(!root.join("original.txt").exists());
        assert!(
            git_conflicts(&root)
                .expect("conflicts")
                .expect("repo")
                .entries
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn groups_multiple_rename_conflicts_independently() {
        if !git_available() {
            return;
        }
        let root = temp_repo("rename-multiple");
        for name in ["original-a.txt", "original-b.txt"] {
            std::fs::write(root.join(name), "base\n").expect("write base");
        }
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        for name in ["original-a", "original-b"] {
            git_ok(
                &root,
                &["mv", &format!("{name}.txt"), &format!("{name}-side.txt")],
            );
            std::fs::write(root.join(format!("{name}-side.txt")), "base\nside\n")
                .expect("modify side");
        }
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "side renames"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        for name in ["original-a", "original-b"] {
            git_ok(
                &root,
                &["mv", &format!("{name}.txt"), &format!("{name}-main.txt")],
            );
            std::fs::write(root.join(format!("{name}-main.txt")), "base\nmain\n")
                .expect("modify main");
        }
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "main renames"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.entries.len(), 2);
        assert!(
            state
                .entries
                .iter()
                .all(|entry| entry.kind == GitConflictKind::Rename)
        );
        let first = &state.entries[0];
        assert_eq!(first.base_path.as_deref(), Some("original-a.txt"));
        assert_eq!(first.ours_path.as_deref(), Some("original-a-main.txt"));
        assert_eq!(first.theirs_path.as_deref(), Some("original-a-side.txt"));
        let second = &state.entries[1];
        assert_eq!(second.base_path.as_deref(), Some("original-b.txt"));
        assert_eq!(second.ours_path.as_deref(), Some("original-b-main.txt"));
        assert_eq!(second.theirs_path.as_deref(), Some("original-b-side.txt"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn three_way_merge_exposes_conflict_blocks_and_applies_choices() {
        let merged =
            "one\n<<<<<<< ours\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> theirs\ntwo\n";
        let (lines, conflicts) = parse_three_way_merge(merged);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].ours_text, "ours\n");
        assert_eq!(conflicts[0].theirs_text, "theirs\n");
        assert!(lines.iter().any(|line| {
            line.conflict_index == Some(0) && line.kind == GitMergeLineKind::Marker
        }));
        let chosen = apply_conflict_choices(
            merged,
            &[GitConflictChoice {
                index: 0,
                side: GitConflictChoiceSide::Theirs,
            }],
        );
        assert_eq!(chosen, "one\ntheirs\ntwo\n");
        let both = apply_conflict_choices(
            merged,
            &[GitConflictChoice {
                index: 0,
                side: GitConflictChoiceSide::Both,
            }],
        );
        assert_eq!(both, "one\nours\ntheirs\ntwo\n");
        let crlf = "one\r\n<<<<<<< ours\r\nours\r\n||||||| base\r\nbase\r\n=======\r\ntheirs\r\n>>>>>>> theirs\r\ntwo\r\n";
        let selected = apply_conflict_choices(
            crlf,
            &[GitConflictChoice {
                index: 0,
                side: GitConflictChoiceSide::Ours,
            }],
        );
        assert_eq!(selected, "one\r\nours\r\ntwo\r\n");
        assert!(contains_conflict_markers(
            "<<<<<<< ours\nours\n=======\ntheirs\n>>>>>>> theirs\n"
        ));
        assert!(!contains_conflict_markers(
            "Markdown divider\n=======\ntext\n"
        ));
    }

    #[test]
    fn real_three_way_merge_and_resolution_update_the_index() {
        if !git_available() {
            return;
        }
        let root = temp_repo("three-way");
        std::fs::write(root.join("f.txt"), "one\nbase\ntwo\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("f.txt"), "one\ntheirs\ntwo\n").expect("write side");
        git_ok(&root, &["commit", "--quiet", "-am", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        std::fs::write(root.join("f.txt"), "one\nours\ntwo\n").expect("write main");
        git_ok(&root, &["commit", "--quiet", "-am", "main"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        let conflict = state.entries.first().expect("text conflict");
        let merge = git_three_way_merge(&root, conflict).expect("three way merge");
        assert!(!merge.binary);
        assert_eq!(merge.conflicts.len(), 1);
        let resolved = apply_conflict_choices(
            &merge.merged_text,
            &[GitConflictChoice {
                index: 0,
                side: GitConflictChoiceSide::Ours,
            }],
        );
        git_resolve_conflict(&root, &conflict.path, Some(&resolved)).expect("resolve");
        assert!(
            git_conflicts(&root)
                .expect("conflicts")
                .expect("repo")
                .entries
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).expect("read result"),
            "one\nours\ntwo\n"
        );
        assert!(!git_status_text(&root).contains("UU"));
        git_continue_operation(&root).expect("continue merge");
        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.operation, None);
        assert!(state.entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn detects_all_resumable_operation_markers() {
        use std::fs::File;
        let git_dir = std::env::temp_dir().join(format!(
            "kite-git-operation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&git_dir).expect("create git dir");
        for (file, expected) in [
            ("MERGE_HEAD", GitOperationState::Merging),
            ("CHERRY_PICK_HEAD", GitOperationState::CherryPicking),
            ("REVERT_HEAD", GitOperationState::Reverting),
            ("BISECT_LOG", GitOperationState::Bisecting),
        ] {
            let _ = std::fs::remove_file(git_dir.join("MERGE_HEAD"));
            let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
            let _ = std::fs::remove_file(git_dir.join("REVERT_HEAD"));
            let _ = std::fs::remove_file(git_dir.join("BISECT_LOG"));
            File::create(git_dir.join(file)).expect("create marker");
            assert_eq!(operation_state(&git_dir), Some(expected));
        }
        let _ = std::fs::remove_file(git_dir.join("MERGE_HEAD"));
        let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
        let _ = std::fs::remove_file(git_dir.join("REVERT_HEAD"));
        let _ = std::fs::remove_file(git_dir.join("BISECT_LOG"));
        std::fs::create_dir_all(git_dir.join("rebase-merge")).expect("create rebase marker");
        assert_eq!(operation_state(&git_dir), Some(GitOperationState::Rebasing));
        let _ = std::fs::remove_dir_all(git_dir);
    }

    #[test]
    fn lists_branches_and_creates_a_merge_commit_with_a_custom_message() {
        if !git_available() {
            return;
        }
        let root = temp_repo("merge-clean");
        std::fs::write(root.join("base.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("side.txt"), "side\n").expect("write side");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);

        let branches = git_merge_branches(&root).expect("branches");
        assert!(branches.iter().any(|branch| branch.name == "side"));
        let outcome = git_merge(
            &root,
            &GitMergeSpec {
                target: "side".to_string(),
                strategy: GitMergeStrategy::NoFastForward,
                message: Some("custom merge message".to_string()),
            },
        )
        .expect("merge");
        assert!(outcome.conflicts.is_none());
        assert!(root.join("side.txt").exists());
        assert_eq!(
            String::from_utf8_lossy(&git_output(&root, &["log", "-1", "--format=%s"]).stdout)
                .trim(),
            "custom merge message"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lists_stash_pop_conflicts_without_a_resumable_operation_marker() {
        if !git_available() {
            return;
        }
        let root = temp_repo("stash-pop");
        std::fs::write(root.join("f.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        std::fs::write(root.join("f.txt"), "stashed\n").expect("write stash change");
        git_ok(&root, &["stash", "push", "--quiet", "-m", "stash fixture"]);
        std::fs::write(root.join("f.txt"), "committed\n").expect("write committed change");
        git_ok(&root, &["commit", "--quiet", "-am", "committed"]);
        assert!(!git_output(&root, &["stash", "pop"]).status.success());

        let state = git_conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.operation, None);
        assert!(!state.can_continue);
        assert!(!state.entries.is_empty());
        assert!(
            state
                .entries
                .iter()
                .all(|entry| entry.kind == GitConflictKind::BothModified)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn git_status_text(root: &Path) -> String {
        String::from_utf8_lossy(&git_output(root, &["status", "--porcelain=v1"]).stdout)
            .into_owned()
    }
}
