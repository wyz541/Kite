//! Branch, tag, upstream, compare, and worktree operations shared by both UIs.
//!
//! Every command goes through the user's Git CLI. Write operations are
//! argument-vector based (never shell strings), and destructive operations make
//! the caller opt into force explicitly.

use std::path::{Path, PathBuf};

use crate::git::{output_error, run_git};

pub const DIRTY_WORKTREE_ERROR: &str =
    "工作区有未提交的更改。请先提交、暂存，或选择 stash 后切换。";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHeadSummary {
    pub repository_root: PathBuf,
    pub branch: Option<String>,
    pub detached: bool,
    pub short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitRefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
    Commit,
}

impl GitRefKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::LocalBranch => "本地分支",
            Self::RemoteBranch => "远程分支",
            Self::Tag => "标签",
            Self::Commit => "提交",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitDirtyPolicy {
    #[default]
    Cancel,
    Stash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBranch {
    /// Short display name. Remote branches include the remote prefix.
    pub name: String,
    /// Full ref name such as `refs/heads/main`.
    pub full_name: String,
    /// Remote name for remote-tracking branches.
    pub remote: Option<String>,
    pub commit_hash: String,
    pub short_hash: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub is_head: bool,
    pub is_merged: bool,
    pub last_commit_unix_seconds: Option<i64>,
    pub subject: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTag {
    pub name: String,
    pub target_hash: String,
    pub short_hash: String,
    pub is_annotated: bool,
    pub message: Option<String>,
    pub created_unix_seconds: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub head_hash: Option<String>,
    pub branch: Option<String>,
    pub is_current: bool,
    pub is_bare: bool,
    pub is_detached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBranchSnapshot {
    pub repository_root: PathBuf,
    pub head_branch: Option<String>,
    pub detached: bool,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
    pub local_branches: Vec<GitBranch>,
    pub remote_branches: Vec<GitBranch>,
    pub tags: Vec<GitTag>,
    pub worktrees: Vec<GitWorktree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCompareCommit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCompareFile {
    pub status: String,
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCompareResult {
    pub base: String,
    pub head: String,
    pub ahead: u32,
    pub behind: u32,
    pub commits: Vec<GitCompareCommit>,
    pub files: Vec<GitCompareFile>,
}

/// Reads the current branch and working-tree state for a toolbar label.
pub fn git_head_summary(path: &Path) -> Result<Option<GitHeadSummary>, String> {
    let Some(root) = repository_root(path)? else {
        return Ok(None);
    };
    let output = git(
        &root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ],
    )?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let mut summary = GitHeadSummary {
        repository_root: root,
        branch: None,
        detached: false,
        short_hash: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        dirty: false,
    };
    apply_status_headers(&mut summary, &output.stdout);
    Ok(Some(summary))
}

/// Returns local branches, remote-tracking branches, tags, and linked worktrees.
pub fn git_branch_snapshot(path: &Path) -> Result<GitBranchSnapshot, String> {
    let root = repository_root(path)?.ok_or_else(|| "所选项目不在 Git 工作区中".to_string())?;
    let head = git_head_summary(&root)?.ok_or_else(|| "所选项目不在 Git 工作区中".to_string())?;
    let merged = merged_local_branches(&root)?;
    let output = git(
        &root,
        &[
            "for-each-ref",
            "--format=%(refname)\t%(objectname)\t%(*objectname)\t%(objecttype)\t%(upstream:short)\t%(upstream:track)\t%(HEAD)\t%(contents:subject)\t%(creatordate:unix)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let mut local_branches = Vec::new();
    let mut remote_branches = Vec::new();
    let mut tags = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 9 {
            continue;
        }
        let (
            full_name,
            object_hash,
            peeled_hash,
            object_type,
            upstream,
            track,
            head_marker,
            subject,
            created,
        ) = (
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7],
            fields[8],
        );
        let (ahead, behind) = parse_track(track);
        if let Some(name) = full_name.strip_prefix("refs/heads/") {
            let commit_hash = nonempty(object_hash).unwrap_or_default().to_string();
            local_branches.push(GitBranch {
                name: name.to_string(),
                full_name: full_name.to_string(),
                remote: None,
                short_hash: short_hash(&commit_hash),
                commit_hash,
                upstream: nonempty(upstream).map(str::to_string),
                ahead,
                behind,
                is_head: head_marker == "*",
                is_merged: merged.iter().any(|merged_name| merged_name == name),
                last_commit_unix_seconds: created.parse().ok(),
                subject: nonempty(subject).map(str::to_string),
            });
        } else if let Some(name) = full_name.strip_prefix("refs/remotes/") {
            if name.ends_with("/HEAD") {
                continue;
            }
            let remote = name.split_once('/').map(|(remote, _)| remote.to_string());
            let commit_hash = nonempty(object_hash).unwrap_or_default().to_string();
            remote_branches.push(GitBranch {
                name: name.to_string(),
                full_name: full_name.to_string(),
                remote,
                short_hash: short_hash(&commit_hash),
                commit_hash,
                upstream: None,
                ahead,
                behind,
                is_head: false,
                is_merged: false,
                last_commit_unix_seconds: created.parse().ok(),
                subject: nonempty(subject).map(str::to_string),
            });
        } else if let Some(name) = full_name.strip_prefix("refs/tags/") {
            let target_hash = nonempty(peeled_hash)
                .or_else(|| nonempty(object_hash))
                .unwrap_or_default()
                .to_string();
            tags.push(GitTag {
                name: name.to_string(),
                short_hash: short_hash(&target_hash),
                target_hash,
                is_annotated: object_type == "tag",
                message: nonempty(subject).map(str::to_string),
                created_unix_seconds: created.parse().ok(),
            });
        }
    }
    local_branches.sort_by(|left, right| {
        right
            .is_head
            .cmp(&left.is_head)
            .then_with(|| left.name.cmp(&right.name))
    });
    remote_branches.sort_by(|left, right| left.name.cmp(&right.name));
    tags.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(GitBranchSnapshot {
        repository_root: root.clone(),
        head_branch: head.branch,
        detached: head.detached,
        head_short_hash: head.short_hash,
        upstream: head.upstream,
        ahead: head.ahead,
        behind: head.behind,
        dirty: head.dirty,
        local_branches,
        remote_branches,
        tags,
        worktrees: git_worktrees(&root)?,
    })
}
/// Checks out a branch, remote-tracking branch, tag, or commit.
///
/// A dirty working tree is rejected by default. With [`GitDirtyPolicy::Stash`]
/// the changes are stashed before checkout and restored afterwards. If restoring
/// the stash conflicts, the stash is retained and the returned error names it.
pub fn git_checkout(
    path: &Path,
    target: &str,
    kind: GitRefKind,
    dirty_policy: GitDirtyPolicy,
) -> Result<String, String> {
    let root = require_repository(path)?;
    let target = target.trim();
    if target.is_empty() {
        return Err("请选择要检出的分支、标签或提交".to_string());
    }
    let dirty = is_dirty(&root)?;
    if dirty && dirty_policy == GitDirtyPolicy::Cancel {
        return Err(DIRTY_WORKTREE_ERROR.to_string());
    }

    let stashed = if dirty {
        let message = format!("kite-checkout-{}", unique_suffix());
        let output = git(
            &root,
            &["stash", "push", "--include-untracked", "-m", &message],
        )?;
        if !output.status.success() {
            return Err(output_error(&output));
        }
        true
    } else {
        false
    };

    let checkout_result = match kind {
        GitRefKind::LocalBranch => git(&root, &["checkout", target]),
        GitRefKind::RemoteBranch => git(&root, &["checkout", "--track", target]),
        GitRefKind::Tag | GitRefKind::Commit => git(&root, &["checkout", "--detach", target]),
    };
    let output = match checkout_result {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            if stashed {
                restore_stash_after_failure(&root);
            }
            return Err(output_error(&output));
        }
        Err(error) => {
            if stashed {
                restore_stash_after_failure(&root);
            }
            return Err(error);
        }
    };

    if stashed {
        let pop = git(&root, &["stash", "pop", "stash@{0}"])?;
        if !pop.status.success() {
            return Err(format!(
                "已切换到 {target}，但恢复 stash 失败。更改仍保存在 stash 中。\n{}",
                output_error(&pop)
            ));
        }
    }

    let short = git(&root, &["rev-parse", "--short", "HEAD"])
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty());
    let suffix = short.map(|hash| format!("（{hash}）")).unwrap_or_default();
    let _ = output;
    Ok(format!("已切换到 {target}{suffix}"))
}

pub fn create_branch(path: &Path, name: &str, start_point: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_branch_name(&root, name)?;
    let name = name.trim();
    let start_point = start_point.trim();
    let output = if start_point.is_empty() || start_point == "HEAD" {
        git(&root, &["branch", name])?
    } else {
        git(&root, &["branch", name, start_point])?
    };
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!(
        "已从 {} 创建分支 {name}",
        if start_point.is_empty() {
            "HEAD"
        } else {
            start_point
        }
    ))
}

pub fn rename_branch(path: &Path, old_name: &str, new_name: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_branch_name(&root, old_name)?;
    validate_branch_name(&root, new_name)?;
    let output = git(&root, &["branch", "-m", old_name.trim(), new_name.trim()])?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!(
        "已将分支 {} 重命名为 {}",
        old_name.trim(),
        new_name.trim()
    ))
}

pub fn delete_local_branch(path: &Path, name: &str, force: bool) -> Result<String, String> {
    delete_local_branches(path, &[name.to_string()], force)
}

/// Deletes one or more local branches. Without `force`, all branches are
/// preflighted against HEAD so a batch is not partially deleted when one branch
/// still has unmerged commits.
pub fn delete_local_branches(path: &Path, names: &[String], force: bool) -> Result<String, String> {
    let root = require_repository(path)?;
    if names.is_empty() {
        return Err("请选择要删除的分支".to_string());
    }
    for name in names {
        validate_branch_name(&root, name)?;
    }
    if !force {
        let merged = merged_local_branches(&root)?;
        let mut unmerged = names
            .iter()
            .filter(|name| !merged.iter().any(|merged_name| merged_name == *name))
            .cloned()
            .collect::<Vec<_>>();
        unmerged.sort();
        if !unmerged.is_empty() {
            return Err(format!(
                "以下分支尚未合并到当前 HEAD，删除前请确认：{}",
                unmerged.join("、")
            ));
        }
    }
    for name in names {
        let flag = if force { "-D" } else { "-d" };
        let output = git(&root, &["branch", flag, name])?;
        if !output.status.success() {
            return Err(output_error(&output));
        }
    }
    Ok(format!("已删除 {} 个本地分支", names.len()))
}

pub fn delete_remote_branch(path: &Path, remote: &str, branch: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_remote(&root, remote)?;
    validate_branch_name(&root, branch)?;
    let output = git(
        &root,
        &[
            "push",
            "--force-with-lease",
            "--delete",
            remote,
            branch.trim(),
        ],
    )?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!("已删除远程分支 {remote}/{}", branch.trim()))
}

pub fn set_branch_upstream(
    path: &Path,
    branch: &str,
    upstream: Option<&str>,
) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_branch_name(&root, branch)?;
    let upstream = upstream.map(str::trim).filter(|value| !value.is_empty());
    let output = if let Some(upstream) = upstream {
        let flag = format!("--set-upstream-to={upstream}");
        git(&root, &["branch", &flag, branch.trim()])?
    } else {
        git(&root, &["branch", "--unset-upstream", branch.trim()])?
    };
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(match upstream {
        Some(upstream) => format!("已将 {} 的上游设置为 {upstream}", branch.trim()),
        None => format!("已清除 {} 的上游", branch.trim()),
    })
}

pub fn compare_branches(path: &Path, base: &str, head: &str) -> Result<GitCompareResult, String> {
    let root = require_repository(path)?;
    if base.trim().is_empty() || head.trim().is_empty() {
        return Err("请选择要比较的两个分支".to_string());
    }
    let range = format!("{}...{}", base.trim(), head.trim());
    let counts = git(&root, &["rev-list", "--left-right", "--count", &range])?;
    if !counts.status.success() {
        return Err(output_error(&counts));
    }
    let (behind, ahead) = parse_left_right_count(&String::from_utf8_lossy(&counts.stdout));

    let commit_range = format!("{}..{}", base.trim(), head.trim());
    let log = git(
        &root,
        &[
            "log",
            "--format=%H%x00%h%x00%an%x00%at%x00%s%x00",
            &commit_range,
        ],
    )?;
    if !log.status.success() {
        return Err(output_error(&log));
    }
    let commits = parse_compare_commits(&log.stdout);

    let diff = git(&root, &["diff", "--numstat", "--no-renames", &range])?;
    if !diff.status.success() {
        return Err(output_error(&diff));
    }
    let files = parse_compare_files(&String::from_utf8_lossy(&diff.stdout));
    Ok(GitCompareResult {
        base: base.trim().to_string(),
        head: head.trim().to_string(),
        ahead,
        behind,
        commits,
        files,
    })
}
pub fn create_tag(
    path: &Path,
    name: &str,
    target: &str,
    annotated: bool,
    message: Option<&str>,
) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_tag_name(&root, name)?;
    let name = name.trim();
    let target = target.trim();
    if target.is_empty() {
        return Err("请选择标签目标提交".to_string());
    }
    if annotated && message.is_none_or(|message| message.trim().is_empty()) {
        return Err("附注标签需要填写说明".to_string());
    }
    let output = if annotated {
        git(
            &root,
            &[
                "tag",
                "-a",
                name,
                target,
                "-m",
                message.unwrap_or_default().trim(),
            ],
        )?
    } else {
        git(&root, &["tag", name, target])?
    };
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(if annotated {
        format!("已创建附注标签 {name}")
    } else {
        format!("已创建轻量标签 {name}")
    })
}

pub fn delete_tag(path: &Path, name: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_tag_name(&root, name)?;
    let output = git(&root, &["tag", "-d", name.trim()])?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!("已删除本地标签 {}", name.trim()))
}

pub fn push_tag(path: &Path, remote: &str, name: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_remote(&root, remote)?;
    validate_tag_name(&root, name)?;
    let tag_ref = format!("refs/tags/{}", name.trim());
    let output = git(&root, &["push", remote, &tag_ref])?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!("已推送标签 {} 到 {remote}", name.trim()))
}

pub fn delete_remote_tag(path: &Path, remote: &str, name: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_remote(&root, remote)?;
    validate_tag_name(&root, name)?;
    let tag_ref = format!(":refs/tags/{}", name.trim());
    let output = git(&root, &["push", remote, &tag_ref])?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!("已从 {remote} 删除标签 {}", name.trim()))
}

pub fn create_worktree(path: &Path, destination: &Path, branch: &str) -> Result<String, String> {
    let root = require_repository(path)?;
    validate_branch_name(&root, branch)?;
    let destination = destination.to_string_lossy().into_owned();
    if destination.trim().is_empty() {
        return Err("请输入 worktree 目标路径".to_string());
    }
    let output = git(&root, &["worktree", "add", &destination, branch.trim()])?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(format!(
        "已在 {destination} 检出 {} 的新 worktree",
        branch.trim()
    ))
}

fn repository_root(path: &Path) -> Result<Option<PathBuf>, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let Some(output) = run_git(start, &["rev-parse", "--show-toplevel"]) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        dunce::canonicalize(&root).unwrap_or_else(|_| PathBuf::from(root)),
    ))
}

fn require_repository(path: &Path) -> Result<PathBuf, String> {
    repository_root(path)?.ok_or_else(|| "所选项目不在 Git 工作区中".to_string())
}

fn git(current_dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    run_git(current_dir, args)
        .ok_or_else(|| "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string())
}

fn apply_status_headers(summary: &mut GitHeadSummary, output: &[u8]) {
    for record in output.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let record = String::from_utf8_lossy(record);
        let Some(header) = record.strip_prefix("# ") else {
            summary.dirty = true;
            continue;
        };
        if let Some(oid) = header.strip_prefix("branch.oid ") {
            if oid != "(initial)" {
                summary.short_hash = Some(oid.chars().take(7).collect());
            }
        } else if let Some(head) = header.strip_prefix("branch.head ") {
            if head == "(detached)" {
                summary.detached = true;
            } else {
                summary.branch = Some(head.to_string());
            }
        } else if let Some(upstream) = header.strip_prefix("branch.upstream ") {
            summary.upstream = Some(upstream.to_string());
        } else if let Some(counts) = header.strip_prefix("branch.ab ") {
            let (ahead, behind) = parse_branch_ab(counts);
            summary.ahead = ahead;
            summary.behind = behind;
        }
    }
}

fn parse_branch_ab(value: &str) -> (u32, u32) {
    let mut parts = value.split_whitespace();
    let ahead = parts
        .next()
        .and_then(|value| value.strip_prefix('+'))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let behind = parts
        .next()
        .and_then(|value| value.strip_prefix('-'))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (ahead, behind)
}

fn parse_track(value: &str) -> (u32, u32) {
    let ahead = value
        .split_once("ahead ")
        .and_then(|(_, rest)| {
            rest.split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let behind = value
        .split_once("behind ")
        .and_then(|(_, rest)| {
            rest.split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (ahead, behind)
}

fn parse_left_right_count(value: &str) -> (u32, u32) {
    let mut parts = value.split_whitespace();
    (
        parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    )
}

fn parse_compare_commits(output: &[u8]) -> Vec<GitCompareCommit> {
    let mut commits = Vec::new();
    let mut fields = output.split(|byte| *byte == 0);
    loop {
        let Some(hash) = fields.next() else { break };
        let hash = String::from_utf8_lossy(hash).trim().to_string();
        if hash.is_empty() {
            continue;
        }
        let Some(short_hash) = fields.next() else {
            break;
        };
        let Some(author) = fields.next() else { break };
        let Some(seconds) = fields.next() else { break };
        let Some(subject) = fields.next() else { break };
        commits.push(GitCompareCommit {
            hash,
            short_hash: String::from_utf8_lossy(short_hash).into_owned(),
            author: String::from_utf8_lossy(author).into_owned(),
            unix_seconds: String::from_utf8_lossy(seconds).parse().unwrap_or(0),
            subject: String::from_utf8_lossy(subject).into_owned(),
        });
    }
    commits
}

fn parse_compare_files(output: &str) -> Vec<GitCompareFile> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let additions = fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let deletions = fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let path = fields.next()?.to_string();
            Some(GitCompareFile {
                status: if additions == 0 && deletions > 0 {
                    "D"
                } else if additions > 0 && deletions == 0 {
                    "A"
                } else {
                    "M"
                }
                .to_string(),
                path,
                additions,
                deletions,
            })
        })
        .collect()
}
fn merged_local_branches(root: &Path) -> Result<Vec<String>, String> {
    let has_head = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .is_ok_and(|output| output.status.success());
    if !has_head {
        return Ok(Vec::new());
    }
    let output = git(
        root,
        &[
            "for-each-ref",
            "--merged=HEAD",
            "--format=%(refname:short)",
            "refs/heads",
        ],
    )?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_worktrees(root: &Path) -> Result<Vec<GitWorktree>, String> {
    let output = git(root, &["worktree", "list", "--porcelain"])?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let mut worktrees = Vec::new();
    let mut current: Option<GitWorktree> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            current = Some(GitWorktree {
                path: PathBuf::from(path),
                head_hash: None,
                branch: None,
                is_current: false,
                is_bare: false,
                is_detached: false,
            });
        } else if let Some(worktree) = current.as_mut() {
            if let Some(hash) = line.strip_prefix("HEAD ") {
                worktree.head_hash = Some(hash.to_string());
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                worktree.branch = Some(branch.to_string());
            } else if line == "bare" {
                worktree.is_bare = true;
            } else if line == "detached" {
                worktree.is_detached = true;
            }
        }
    }
    if let Some(worktree) = current {
        worktrees.push(worktree);
    }
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    for worktree in &mut worktrees {
        let path = dunce::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
        worktree.is_current = path == root;
        worktree.path = path;
    }
    Ok(worktrees)
}

fn is_dirty(root: &Path) -> Result<bool, String> {
    let output = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    Ok(output.stdout.iter().any(|byte| *byte != 0))
}

fn restore_stash_after_failure(root: &Path) {
    let _ = git(root, &["stash", "pop", "stash@{0}"]);
}

fn validate_branch_name(root: &Path, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请输入分支名称".to_string());
    }
    let output = git(root, &["check-ref-format", "--branch", name])?;
    if !output.status.success() {
        return Err(format!("分支名称无效：{name}"));
    }
    Ok(())
}

fn validate_tag_name(root: &Path, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请输入标签名称".to_string());
    }
    let reference = format!("refs/tags/{name}");
    let output = git(root, &["check-ref-format", &reference])?;
    if !output.status.success() {
        return Err(format!("标签名称无效：{name}"));
    }
    Ok(())
}

fn validate_remote(root: &Path, remote: &str) -> Result<(), String> {
    if remote.trim().is_empty() {
        return Err("请选择远程仓库".to_string());
    }
    let output = git(root, &["remote", "get-url", remote])?;
    if !output.status.success() {
        return Err(format!("未找到远程仓库：{remote}"));
    }
    Ok(())
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(7).collect()
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kite-git-branches-{}-{}-{label}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    fn run(root: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never")
            .output()
            .expect("run git")
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = run(root, args);
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repo(label: &str) -> PathBuf {
        let path = test_dir(label);
        git_ok(&path, &["init", "-b", "main"]);
        git_ok(&path, &["config", "user.name", "Kite Test"]);
        git_ok(&path, &["config", "user.email", "kite@example.com"]);
        git_ok(&path, &["config", "commit.gpgsign", "false"]);
        fs::write(path.join("notes.txt"), "initial\n").expect("write file");
        git_ok(&path, &["add", "notes.txt"]);
        git_ok(&path, &["commit", "-m", "initial commit"]);
        path
    }

    fn current_branch(root: &Path) -> String {
        String::from_utf8_lossy(&run(root, &["branch", "--show-current"]).stdout)
            .trim()
            .to_string()
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn lists_local_remote_and_tag_refs() {
        if !git_available() {
            return;
        }
        let root = repo("refs");
        let bare = test_dir("refs-origin");
        git_ok(&bare, &["init", "--bare"]);
        git_ok(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_ok(&root, &["push", "-u", "origin", "main"]);
        git_ok(&root, &["branch", "feature"]);
        git_ok(&root, &["push", "-u", "origin", "feature"]);
        git_ok(&root, &["fetch", "origin"]);
        git_ok(&root, &["tag", "v1.0.0"]);
        git_ok(&root, &["tag", "-a", "v1.1.0", "-m", "stable release"]);
        git_ok(&root, &["checkout", "main"]);

        let snapshot = git_branch_snapshot(&root).expect("read branch snapshot");
        assert_eq!(snapshot.head_branch.as_deref(), Some("main"));
        assert!(!snapshot.detached);
        assert_eq!(
            snapshot
                .local_branches
                .iter()
                .find(|branch| branch.name == "main")
                .map(|branch| branch.is_head),
            Some(true)
        );
        assert_eq!(
            snapshot
                .local_branches
                .iter()
                .find(|branch| branch.name == "feature")
                .and_then(|branch| branch.upstream.as_deref()),
            Some("origin/feature")
        );
        assert!(
            snapshot
                .remote_branches
                .iter()
                .any(|branch| branch.name == "origin/main")
        );
        assert!(
            snapshot
                .remote_branches
                .iter()
                .any(|branch| branch.name == "origin/feature")
        );
        let annotated = snapshot
            .tags
            .iter()
            .find(|tag| tag.name == "v1.1.0")
            .expect("annotated tag");
        assert!(annotated.is_annotated);
        assert_eq!(annotated.message.as_deref(), Some("stable release"));
        assert!(
            snapshot
                .worktrees
                .iter()
                .any(|worktree| worktree.is_current)
        );

        cleanup(&root);
        cleanup(&bare);
    }

    #[test]
    fn checkout_rejects_dirty_tree_then_stashes_and_restores() {
        if !git_available() {
            return;
        }
        let root = repo("dirty-checkout");
        git_ok(&root, &["branch", "feature"]);
        fs::write(root.join("notes.txt"), "uncommitted change\n").expect("modify file");

        let error = git_checkout(
            &root,
            "feature",
            GitRefKind::LocalBranch,
            GitDirtyPolicy::Cancel,
        )
        .expect_err("dirty checkout must be rejected");
        assert!(error.contains("未提交"));
        assert_eq!(current_branch(&root), "main");

        git_checkout(
            &root,
            "feature",
            GitRefKind::LocalBranch,
            GitDirtyPolicy::Stash,
        )
        .expect("stash checkout");
        assert_eq!(current_branch(&root), "feature");
        assert_eq!(
            fs::read_to_string(root.join("notes.txt"))
                .expect("read restored file")
                .replace("\r\n", "\n"),
            "uncommitted change\n"
        );
        assert!(
            !String::from_utf8_lossy(&run(&root, &["stash", "list"]).stdout)
                .contains("kite-checkout-")
        );

        cleanup(&root);
    }

    #[test]
    fn unmerged_branch_requires_explicit_force() {
        if !git_available() {
            return;
        }
        let root = repo("delete-branch");
        git_ok(&root, &["checkout", "-b", "feature"]);
        fs::write(root.join("feature.txt"), "feature\n").expect("write feature file");
        git_ok(&root, &["add", "feature.txt"]);
        git_ok(&root, &["commit", "-m", "feature work"]);
        git_ok(&root, &["checkout", "main"]);

        let error = delete_local_branch(&root, "feature", false)
            .expect_err("unmerged branch must require force");
        assert!(error.contains("尚未合并"));
        assert_eq!(current_branch(&root), "main");

        delete_local_branch(&root, "feature", true).expect("force delete");
        let branches = String::from_utf8_lossy(&run(&root, &["branch"]).stdout).to_string();
        assert!(!branches.contains("feature"));
        cleanup(&root);
    }

    #[test]
    fn renames_sets_upstream_and_compares_branches() {
        if !git_available() {
            return;
        }
        let root = repo("compare");
        let bare = test_dir("compare-origin");
        git_ok(&bare, &["init", "--bare"]);
        git_ok(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_ok(&root, &["push", "-u", "origin", "main"]);
        create_branch(&root, "feature", "HEAD").expect("create feature");
        git_ok(&root, &["checkout", "feature"]);
        fs::write(root.join("notes.txt"), "initial\nfeature\n").expect("modify file");
        git_ok(&root, &["add", "notes.txt"]);
        git_ok(&root, &["commit", "-m", "feature change"]);
        git_ok(&root, &["push", "-u", "origin", "feature"]);
        rename_branch(&root, "feature", "feature-compare").expect("rename branch");
        git_ok(&root, &["push", "-u", "origin", "feature-compare"]);
        git_ok(&root, &["checkout", "main"]);

        set_branch_upstream(&root, "feature-compare", Some("origin/feature-compare"))
            .expect("set upstream");
        let snapshot = git_branch_snapshot(&root).expect("snapshot");
        assert_eq!(
            snapshot
                .local_branches
                .iter()
                .find(|branch| branch.name == "feature-compare")
                .and_then(|branch| branch.upstream.as_deref()),
            Some("origin/feature-compare")
        );

        let compare = compare_branches(&root, "main", "feature-compare").expect("compare");
        assert_eq!(compare.ahead, 1);
        assert_eq!(compare.behind, 0);
        assert_eq!(compare.commits[0].subject, "feature change");
        assert!(
            compare
                .files
                .iter()
                .any(|file| file.path == "notes.txt" && file.additions == 1)
        );

        cleanup(&root);
        cleanup(&bare);
    }

    #[test]
    fn pushes_and_deletes_lightweight_and_annotated_tags() {
        if !git_available() {
            return;
        }
        let root = repo("tags");
        let bare = test_dir("tags-origin");
        git_ok(&bare, &["init", "--bare"]);
        git_ok(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_ok(&root, &["push", "-u", "origin", "main"]);
        create_tag(&root, "light", "HEAD", false, None).expect("create lightweight tag");
        create_tag(&root, "annotated", "HEAD", true, Some("release notes"))
            .expect("create annotated tag");
        push_tag(&root, "origin", "light").expect("push tag");
        push_tag(&root, "origin", "annotated").expect("push annotated tag");
        let remote_tags =
            String::from_utf8_lossy(&run(&root, &["ls-remote", "--tags", "origin"]).stdout)
                .to_string();
        assert!(remote_tags.contains("refs/tags/light"));
        assert!(remote_tags.contains("refs/tags/annotated"));

        delete_remote_tag(&root, "origin", "light").expect("delete remote tag");
        delete_tag(&root, "annotated").expect("delete local tag");
        let remote_tags =
            String::from_utf8_lossy(&run(&root, &["ls-remote", "--tags", "origin"]).stdout)
                .to_string();
        assert!(!remote_tags.contains("refs/tags/light"));
        assert!(remote_tags.contains("refs/tags/annotated"));

        cleanup(&root);
        cleanup(&bare);
    }

    #[test]
    fn creates_an_adjacent_worktree() {
        if !git_available() {
            return;
        }
        let root = repo("worktree");
        git_ok(&root, &["branch", "worktree-feature"]);
        let destination = test_dir("worktree-target");
        let _ = fs::remove_dir_all(&destination);

        create_worktree(&root, &destination, "worktree-feature").expect("create worktree");
        assert!(destination.join("notes.txt").is_file());
        let snapshot = git_branch_snapshot(&root).expect("snapshot with worktree");
        assert!(
            snapshot
                .worktrees
                .iter()
                .any(|worktree| worktree.path == dunce::canonicalize(&destination).unwrap())
        );
        let _ = run(
            &root,
            &[
                "worktree",
                "remove",
                "--force",
                &destination.to_string_lossy(),
            ],
        );
        cleanup(&root);
    }

    #[test]
    fn deletes_a_remote_branch_with_lease() {
        if !git_available() {
            return;
        }
        let root = repo("delete-remote");
        let bare = test_dir("delete-remote-origin");
        git_ok(&bare, &["init", "--bare"]);
        git_ok(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_ok(&root, &["push", "-u", "origin", "main"]);
        git_ok(&root, &["branch", "feature"]);
        git_ok(&root, &["push", "-u", "origin", "feature"]);

        delete_remote_branch(&root, "origin", "feature").expect("delete remote branch");
        let heads =
            String::from_utf8_lossy(&run(&root, &["ls-remote", "--heads", "origin"]).stdout)
                .to_string();
        assert!(!heads.contains("refs/heads/feature"));
        cleanup(&root);
        cleanup(&bare);
    }
}
