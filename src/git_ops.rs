//! Git history-changing operations shared by the Windows and macOS clients.
//!
//! All commands go through the user's git executable so hooks, signing,
//! credentials and configuration behave the same as they do in a terminal.
//! Destructive operations expose an explicit confirmation flag; the UI must
//! only set it after showing the user what will be lost.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::git::{GitDiffLine, GitDiffLineKind, GitOperationState};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const DIFF_MAX_BYTES: usize = 256 * 1024;
const DIFF_MAX_LINES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitResetMode {
    Soft,
    Mixed,
    Hard,
    Keep,
}

impl GitResetMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Mixed => "mixed",
            Self::Hard => "hard",
            Self::Keep => "keep",
        }
    }

    pub fn is_destructive(self) -> bool {
        self == Self::Hard
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStashEntry {
    pub selector: String,
    pub commit_hash: String,
    pub short_hash: String,
    pub date: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStashSaveOptions {
    pub message: Option<String>,
    pub include_untracked: bool,
    pub keep_index: bool,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryStatus {
    pub repository_root: PathBuf,
    pub branch: Option<String>,
    pub operation: Option<GitOperationState>,
    pub has_conflicts: bool,
    pub stash_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommandOutcome {
    pub message: String,
    pub operation: Option<GitOperationState>,
    pub has_conflicts: bool,
}

struct RepositoryContext {
    root: PathBuf,
    git_dir: PathBuf,
}

/// Reads the state of the repository containing `path`.
pub fn git_operation_status(path: &Path) -> Result<GitRepositoryStatus, String> {
    let context = repository_context(path)?;
    let branch_output = run_git(&context.root, ["branch", "--show-current"])?;
    let branch = branch_output
        .status
        .success()
        .then(|| {
            String::from_utf8_lossy(&branch_output.stdout)
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty());
    let stash_output = run_git(&context.root, ["stash", "list", "--format=%gd"])?;
    let stash_count = if stash_output.status.success() {
        String::from_utf8_lossy(&stash_output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u32
    } else {
        0
    };
    let has_conflicts = has_conflicts(&context.root)?;
    Ok(GitRepositoryStatus {
        repository_root: context.root,
        branch,
        operation: operation_state(&context.git_dir),
        has_conflicts,
        stash_count,
    })
}

/// Lists stashes newest first, with stable selectors such as `stash@{0}`.
pub fn git_stash_list(path: &Path) -> Result<Vec<GitStashEntry>, String> {
    let context = repository_context(path)?;
    let output = run_git(
        &context.root,
        ["stash", "list", "--format=%gd%x00%H%x00%h%x00%ci%x00%gs"],
    )?;
    if !output.status.success() {
        return Err(output_error("读取 Git stash 列表", &output));
    }
    Ok(parse_stash_list(&String::from_utf8_lossy(&output.stdout)))
}

/// Saves a stash. Empty `paths` means all changes in the work tree.
pub fn git_stash_save(path: &Path, options: GitStashSaveOptions) -> Result<String, String> {
    let context = repository_context(path)?;
    let before = git_stash_list(&context.root)?.len();
    let mut args = vec![OsString::from("stash"), OsString::from("push")];
    if let Some(message) = options
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        if message.contains('\0') {
            return Err("Stash 说明不能包含空字符".to_string());
        }
        args.push(OsString::from("--message"));
        args.push(OsString::from(message));
    }
    if options.include_untracked {
        args.push(OsString::from("--include-untracked"));
    }
    if options.keep_index {
        args.push(OsString::from("--keep-index"));
    }
    if !options.paths.is_empty() {
        args.push(OsString::from("--"));
        for path in options.paths {
            let relative = relative_path(&context.root, &path)?;
            args.push(relative.into_os_string());
        }
    }
    let output = run_git(&context.root, args)?;
    if !output.status.success() {
        return Err(output_error("保存 Git stash", &output));
    }
    let after = git_stash_list(&context.root)?.len();
    if after == before {
        return Ok("没有可保存的更改".to_string());
    }
    Ok(format!(
        "已创建 {}",
        git_stash_list(&context.root)?
            .first()
            .map(|stash| stash.selector.as_str())
            .unwrap_or("Git stash")
    ))
}

/// Returns the patch for one stash, bounded like the preview diff.
pub fn git_stash_diff(path: &Path, index: u32) -> Result<Vec<GitDiffLine>, String> {
    let context = repository_context(path)?;
    let selector = stash_selector(index);
    let mut output = run_git(
        &context.root,
        [
            "stash",
            "show",
            "--patch",
            "--include-untracked",
            "--no-ext-diff",
            "--no-color",
            selector.as_str(),
        ],
    )?;
    if !output.status.success() {
        // Older Git versions do not support --include-untracked here.
        output = run_git(
            &context.root,
            [
                "stash",
                "show",
                "--patch",
                "--no-ext-diff",
                "--no-color",
                selector.as_str(),
            ],
        )?;
    }
    if !output.status.success() {
        return Err(output_error("读取 Git stash 差异", &output));
    }
    if output.stdout.len() > DIFF_MAX_BYTES {
        return Err("Stash 差异过大，未显示".to_string());
    }
    Ok(parse_unified_diff(&String::from_utf8_lossy(&output.stdout)))
}

/// Applies a stash without removing it.
pub fn git_stash_apply(path: &Path, index: u32) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    let selector = stash_selector(index);
    let output = run_git(&context.root, ["stash", "apply", selector.as_str()])?;
    command_outcome(
        &context,
        output,
        "已应用 Git stash",
        "应用 Git stash 时发生冲突",
    )
}

/// Applies a stash and removes it when Git can do so safely.
pub fn git_stash_pop(path: &Path, index: u32) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    let selector = stash_selector(index);
    let output = run_git(&context.root, ["stash", "pop", selector.as_str()])?;
    command_outcome(
        &context,
        output,
        "已弹出 Git stash",
        "弹出 Git stash 时发生冲突",
    )
}

/// Drops one stash. Refuses unless the caller explicitly confirms.
pub fn git_stash_drop(path: &Path, index: u32, confirmed: bool) -> Result<String, String> {
    if !confirmed {
        return Err("删除 stash 前必须确认".to_string());
    }
    let context = repository_context(path)?;
    let selector = stash_selector(index);
    let output = run_git(&context.root, ["stash", "drop", selector.as_str()])?;
    if !output.status.success() {
        return Err(output_error("删除 Git stash", &output));
    }
    Ok(format!("已删除 {selector}"))
}

/// Clears every stash. Refuses unless the caller explicitly confirms.
pub fn git_stash_clear(path: &Path, confirmed: bool) -> Result<String, String> {
    if !confirmed {
        return Err("清空 stash 前必须确认".to_string());
    }
    let context = repository_context(path)?;
    let output = run_git(&context.root, ["stash", "clear"])?;
    if !output.status.success() {
        return Err(output_error("清空 Git stash", &output));
    }
    Ok("已清空全部 Git stash".to_string())
}

/// Creates a branch from a stash, checks it out and applies the stash.
pub fn git_stash_branch(
    path: &Path,
    index: u32,
    branch: &str,
) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    let branch = branch.trim();
    validate_branch_name(&context.root, branch)?;
    let selector = stash_selector(index);
    let output = run_git(
        &context.root,
        ["stash", "branch", branch, selector.as_str()],
    )?;
    command_outcome(
        &context,
        output,
        &format!("已从 {selector} 创建分支 {branch}"),
        "从 stash 创建分支时发生冲突",
    )
}

/// Resets the current branch to a commit. `hard` requires confirmation.
pub fn git_reset(
    path: &Path,
    mode: GitResetMode,
    target: &str,
    confirmed: bool,
) -> Result<String, String> {
    if mode.is_destructive() && !confirmed {
        return Err("reset --hard 前必须确认".to_string());
    }
    let context = repository_context(path)?;
    let commit = resolve_commit(&context.root, target)?;
    let output = run_git(
        &context.root,
        ["reset", &format!("--{}", mode.label()), commit.as_str()],
    )?;
    if !output.status.success() {
        return Err(output_error("Git reset", &output));
    }
    Ok(format!(
        "已执行 reset --{} 到 {}",
        mode.label(),
        short_hash(&commit)
    ))
}

/// Reverts one or more commits in the caller-supplied order.
pub fn git_revert(path: &Path, commits: &[String]) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    let commits = resolve_commits(&context.root, commits)?;
    let mut args = vec![OsString::from("revert"), OsString::from("--no-edit")];
    args.extend(commits.iter().map(OsString::from));
    let output = run_git(&context.root, args)?;
    command_outcome(&context, output, "Git 还原提交完成", "还原提交时发生冲突")
}

pub fn git_revert_continue(path: &Path) -> Result<GitCommandOutcome, String> {
    continue_operation(path, GitOperationState::Reverting, "revert")
}

pub fn git_revert_abort(path: &Path) -> Result<String, String> {
    abort_operation(path, GitOperationState::Reverting, "revert")
}

/// Cherry-picks one or more commits in the caller-supplied order.
pub fn git_cherry_pick(path: &Path, commits: &[String]) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    let commits = resolve_commits(&context.root, commits)?;
    let mut args = vec![OsString::from("cherry-pick")];
    args.extend(commits.iter().map(OsString::from));
    let output = run_git(&context.root, args)?;
    command_outcome(&context, output, "Git 拣选提交完成", "拣选提交时发生冲突")
}

pub fn git_cherry_pick_continue(path: &Path) -> Result<GitCommandOutcome, String> {
    continue_operation(path, GitOperationState::CherryPicking, "cherry-pick")
}

pub fn git_cherry_pick_skip(path: &Path) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    if operation_state(&context.git_dir) != Some(GitOperationState::CherryPicking) {
        return Err("没有进行中的 cherry-pick".to_string());
    }
    let output = run_git(&context.root, ["cherry-pick", "--skip"])?;
    command_outcome(
        &context,
        output,
        "已跳过当前 cherry-pick",
        "跳过当前提交后仍有冲突",
    )
}

pub fn git_cherry_pick_abort(path: &Path) -> Result<String, String> {
    abort_operation(path, GitOperationState::CherryPicking, "cherry-pick")
}

fn continue_operation(
    path: &Path,
    expected: GitOperationState,
    command_name: &str,
) -> Result<GitCommandOutcome, String> {
    let context = repository_context(path)?;
    if operation_state(&context.git_dir) != Some(expected) {
        return Err(format!("没有进行中的 {command_name}"));
    }
    let output = run_git(&context.root, [command_name, "--continue"])?;
    command_outcome(
        &context,
        output,
        &format!("{command_name} 已继续完成"),
        &format!("继续 {command_name} 后仍有冲突"),
    )
}

fn abort_operation(
    path: &Path,
    expected: GitOperationState,
    command_name: &str,
) -> Result<String, String> {
    let context = repository_context(path)?;
    if operation_state(&context.git_dir) != Some(expected) {
        return Err(format!("没有进行中的 {command_name}"));
    }
    let output = run_git(&context.root, [command_name, "--abort"])?;
    if !output.status.success() {
        return Err(output_error(&format!("中止 {command_name}"), &output));
    }
    Ok(format!("已中止 {command_name}"))
}

fn command_outcome(
    context: &RepositoryContext,
    output: Output,
    success: &str,
    conflict: &str,
) -> Result<GitCommandOutcome, String> {
    let operation = operation_state(&context.git_dir);
    let conflicts = has_conflicts(&context.root)?;
    if output.status.success() {
        return Ok(GitCommandOutcome {
            message: success.to_string(),
            operation,
            has_conflicts: conflicts,
        });
    }
    if conflicts
        || matches!(
            operation,
            Some(GitOperationState::CherryPicking | GitOperationState::Reverting)
        )
    {
        return Ok(GitCommandOutcome {
            message: format!("{conflict}。解决冲突后继续，或中止操作。"),
            operation,
            has_conflicts: true,
        });
    }
    Err(output_error(success, &output))
}

fn parse_stash_list(text: &str) -> Vec<GitStashEntry> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let selector = fields.next()?.trim();
            let commit_hash = fields.next()?.trim();
            let short_hash = fields.next()?.trim();
            let date = fields.next()?.trim();
            let message = fields.next()?.trim();
            if selector.is_empty() || commit_hash.is_empty() {
                return None;
            }
            Some(GitStashEntry {
                selector: selector.to_string(),
                commit_hash: commit_hash.to_string(),
                short_hash: short_hash.to_string(),
                date: date.to_string(),
                message: message.to_string(),
            })
        })
        .collect()
}

fn parse_unified_diff(text: &str) -> Vec<GitDiffLine> {
    let mut lines = Vec::new();
    let mut in_hunk = false;
    for line in text.lines() {
        if lines.len() >= DIFF_MAX_LINES {
            break;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            lines.push(GitDiffLine {
                kind: GitDiffLineKind::Hunk,
                text: line.to_string(),
            });
        } else if line.starts_with("diff --git") {
            in_hunk = false;
        } else if !in_hunk {
            if line.starts_with("Binary files") {
                lines.push(GitDiffLine {
                    kind: GitDiffLineKind::Hunk,
                    text: "二进制文件已变化".to_string(),
                });
            }
        } else {
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
    }
    lines
}

fn repository_context(path: &Path) -> Result<RepositoryContext, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let output = run_git(
        start,
        ["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    )?;
    if !output.status.success() {
        return Err("所选项目不在 Git 工作区中".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Err("无法读取 Git 工作区信息".to_string());
    };
    Ok(RepositoryContext {
        root: dunce::canonicalize(root.trim()).unwrap_or_else(|_| PathBuf::from(root.trim())),
        git_dir: dunce::canonicalize(git_dir.trim())
            .unwrap_or_else(|_| PathBuf::from(git_dir.trim())),
    })
}

fn relative_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法确定当前目录：{error}"))?
            .join(path)
    };
    let canonical = dunce::canonicalize(&absolute).or_else(|_| {
        let parent = absolute.parent().unwrap_or(&absolute);
        dunce::canonicalize(parent).map(|parent| {
            absolute
                .file_name()
                .map(|name| parent.join(name))
                .unwrap_or(parent)
        })
    });
    let canonical = canonical.unwrap_or(absolute);
    canonical
        .strip_prefix(root)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                PathBuf::from(relative.to_string_lossy().replace('\\', "/"))
            }
        })
        .map_err(|_| "所选项目不在 Git 工作区中".to_string())
}

fn resolve_commit(root: &Path, target: &str) -> Result<String, String> {
    let target = target.trim();
    if target.is_empty() || target.starts_with('-') {
        return Err("请输入有效的提交、分支或标签".to_string());
    }
    let spec = format!("{target}^{{commit}}");
    let output = run_git(root, ["rev-parse", "--verify", "--quiet", spec.as_str()])?;
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || hash.is_empty() {
        return Err(format!("找不到提交：{target}"));
    }
    Ok(hash)
}

fn resolve_commits(root: &Path, commits: &[String]) -> Result<Vec<String>, String> {
    if commits.is_empty() {
        return Err("请至少选择一个提交".to_string());
    }
    commits
        .iter()
        .map(|commit| resolve_commit(root, commit))
        .collect()
}

fn validate_branch_name(root: &Path, branch: &str) -> Result<(), String> {
    if branch.is_empty() || branch.starts_with('-') {
        return Err("请输入有效的分支名称".to_string());
    }
    let output = run_git(root, ["check-ref-format", "--branch", branch])?;
    if !output.status.success() {
        return Err(format!("分支名称无效：{branch}"));
    }
    Ok(())
}

fn has_conflicts(root: &Path) -> Result<bool, String> {
    let output = run_git(root, ["diff", "--name-only", "--diff-filter=U"])?;
    if !output.status.success() {
        return Err(output_error("检查 Git 冲突", &output));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
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

fn stash_selector(index: u32) -> String {
    format!("stash@{{{index}}}")
}

fn short_hash(hash: &str) -> &str {
    hash.get(..7).unwrap_or(hash)
}

fn run_git<I, S>(current_dir: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .current_dir(current_dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("LC_ALL", "C");
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string()
        } else {
            format!("无法运行 Git：{error}")
        }
    })
}

fn output_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    if detail.is_empty() {
        format!("{label}失败，退出码 {}", output.status.code().unwrap_or(-1))
    } else {
        format!(
            "{label}失败：{}",
            detail
                .replace(['\r', '\n'], " ")
                .chars()
                .take(500)
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

    fn git_ok(root: &Path, args: &[&str]) -> Output {
        let output = run_git(root, args).expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-git-ops-{label}-{}-{}",
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
        git_ok(&root, &["config", "commit.gpgsign", "false"]);
        root
    }

    fn write_and_commit(root: &Path, name: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(name), content).expect("write fixture");
        git_ok(root, &["add", "--", name]);
        git_ok(root, &["commit", "--quiet", "-m", message]);
        head(root)
    }

    fn head(root: &Path) -> String {
        String::from_utf8_lossy(&git_ok(root, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string()
    }

    fn status(root: &Path) -> String {
        String::from_utf8_lossy(&git_ok(root, &["status", "--porcelain"]).stdout).into_owned()
    }

    #[test]
    fn stash_round_trip_and_selected_paths() {
        if !git_available() {
            return;
        }
        let root = temp_repo("stash");
        write_and_commit(&root, "a.txt", "a0\n", "base a");
        write_and_commit(&root, "b.txt", "b0\n", "base b");
        std::fs::write(root.join("a.txt"), "a1\n").unwrap();
        std::fs::write(root.join("b.txt"), "b1\n").unwrap();

        let message = git_stash_save(
            &root,
            GitStashSaveOptions {
                message: Some("only a".into()),
                include_untracked: false,
                keep_index: false,
                paths: vec![root.join("a.txt")],
            },
        )
        .expect("save selected stash");
        assert!(message.contains("stash@{0}"));
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a0\n");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b1\n");
        assert!(status(&root).contains("b.txt"));

        let stashes = git_stash_list(&root).expect("list stashes");
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].selector, "stash@{0}");
        assert!(stashes[0].message.contains("only a"));
        let diff = git_stash_diff(&root, 0).expect("stash diff");
        assert!(diff.iter().any(|line| line.text == "a1"));

        let outcome = git_stash_pop(&root, 0).expect("pop stash");
        assert!(!outcome.has_conflicts);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a1\n");
        assert!(git_stash_list(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stash_can_include_untracked_and_keep_index() {
        if !git_available() {
            return;
        }
        let root = temp_repo("untracked");
        write_and_commit(&root, "tracked.txt", "base\n", "base");
        std::fs::write(root.join("tracked.txt"), "staged\n").unwrap();
        git_ok(&root, &["add", "tracked.txt"]);
        std::fs::write(root.join("tracked.txt"), "working\n").unwrap();
        std::fs::write(root.join("new.txt"), "untracked\n").unwrap();

        git_stash_save(
            &root,
            GitStashSaveOptions {
                message: Some("all".into()),
                include_untracked: true,
                keep_index: true,
                paths: Vec::new(),
            },
        )
        .expect("save stash");
        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "staged\n"
        );
        assert!(!root.join("new.txt").exists());
        let stashes = git_stash_list(&root).unwrap();
        assert_eq!(stashes.len(), 1);

        git_stash_apply(&root, 0).expect("apply stash");
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "untracked\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stash_branch_drop_and_clear_require_confirmation_where_needed() {
        if !git_available() {
            return;
        }
        let root = temp_repo("stash-branch");
        write_and_commit(&root, "a.txt", "base\n", "base");
        std::fs::write(root.join("a.txt"), "changed\n").unwrap();
        git_stash_save(
            &root,
            GitStashSaveOptions {
                message: Some("topic".into()),
                include_untracked: false,
                keep_index: false,
                paths: Vec::new(),
            },
        )
        .expect("save stash");

        assert!(git_stash_drop(&root, 0, false).is_err());
        let outcome = git_stash_branch(&root, 0, "from-stash").expect("stash branch");
        assert!(!outcome.has_conflicts);
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "changed\n"
        );
        assert!(git_stash_list(&root).unwrap().is_empty());

        std::fs::write(root.join("a.txt"), "second\n").unwrap();
        git_stash_save(
            &root,
            GitStashSaveOptions {
                message: None,
                include_untracked: false,
                keep_index: false,
                paths: Vec::new(),
            },
        )
        .unwrap();
        assert!(git_stash_clear(&root, false).is_err());
        assert_eq!(git_stash_list(&root).unwrap().len(), 1);
        git_stash_clear(&root, true).expect("clear confirmed");
        assert!(git_stash_list(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reset_modes_update_index_and_worktree_as_expected() {
        if !git_available() {
            return;
        }
        let root = temp_repo("reset");
        write_and_commit(&root, "a.txt", "one\n", "one");
        write_and_commit(&root, "a.txt", "two\n", "two");
        let second = head(&root);

        git_reset(&root, GitResetMode::Soft, "HEAD~1", false).expect("soft reset");
        assert_ne!(head(&root), second);
        assert!(status(&root).starts_with("M  a.txt") || status(&root).starts_with("M a.txt"));

        git_reset(&root, GitResetMode::Hard, &second, false).expect_err("hard needs confirmation");
        git_reset(&root, GitResetMode::Hard, &second, true).expect("confirmed hard reset");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "two\n"
        );

        git_reset(&root, GitResetMode::Mixed, "HEAD~1", false).expect("mixed reset");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(status(&root).contains("a.txt"));
        git_reset(&root, GitResetMode::Keep, "HEAD", false).expect("keep preserves changes");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "two\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn revert_conflict_can_continue() {
        if !git_available() {
            return;
        }
        let root = temp_repo("revert-continue");
        write_and_commit(&root, "a.txt", "base\n", "base");
        let revert_target = write_and_commit(&root, "a.txt", "one\n", "one");
        write_and_commit(&root, "a.txt", "two\n", "two");

        let outcome = git_revert(&root, &[revert_target]).expect("start revert");
        assert!(outcome.has_conflicts);
        assert_eq!(outcome.operation, Some(GitOperationState::Reverting));
        assert!(git_operation_status(&root).unwrap().has_conflicts);

        std::fs::write(root.join("a.txt"), "resolved\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        let outcome = git_revert_continue(&root).expect("continue revert");
        assert!(!outcome.has_conflicts);
        assert_eq!(git_operation_status(&root).unwrap().operation, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn revert_conflict_can_abort() {
        if !git_available() {
            return;
        }
        let root = temp_repo("revert-abort");
        write_and_commit(&root, "a.txt", "base\n", "base");
        let revert_target = write_and_commit(&root, "a.txt", "one\n", "one");
        write_and_commit(&root, "a.txt", "two\n", "two");
        let before = head(&root);

        let outcome = git_revert(&root, &[revert_target]).expect("start revert");
        assert!(outcome.has_conflicts);
        git_revert_abort(&root).expect("abort revert");
        assert_eq!(head(&root), before);
        assert_eq!(git_operation_status(&root).unwrap().operation, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cherry_pick_conflict_can_continue() {
        if !git_available() {
            return;
        }
        let root = temp_repo("pick-continue");
        write_and_commit(&root, "a.txt", "base\n", "base");
        git_ok(&root, &["checkout", "-b", "feature"]);
        let picked = write_and_commit(&root, "a.txt", "feature\n", "feature");
        git_ok(&root, &["checkout", "main"]);
        write_and_commit(&root, "a.txt", "main\n", "main");

        let outcome = git_cherry_pick(&root, &[picked]).expect("start pick");
        assert!(outcome.has_conflicts);
        std::fs::write(root.join("a.txt"), "resolved\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        let outcome = git_cherry_pick_continue(&root).expect("continue pick");
        assert!(!outcome.has_conflicts);
        assert_eq!(git_operation_status(&root).unwrap().operation, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cherry_pick_can_skip_and_abort() {
        if !git_available() {
            return;
        }
        let root = temp_repo("pick-skip");
        write_and_commit(&root, "a.txt", "base\n", "base");
        git_ok(&root, &["checkout", "-b", "feature"]);
        let conflicting = write_and_commit(&root, "a.txt", "feature\n", "feature");
        let following = write_and_commit(&root, "other.txt", "other\n", "other");
        git_ok(&root, &["checkout", "main"]);
        write_and_commit(&root, "a.txt", "main\n", "main");
        let before = head(&root);

        let outcome =
            git_cherry_pick(&root, &[conflicting.clone(), following]).expect("start pick");
        assert!(outcome.has_conflicts);
        let outcome = git_cherry_pick_skip(&root).expect("skip conflict");
        assert!(!outcome.has_conflicts);
        assert_eq!(git_operation_status(&root).unwrap().operation, None);
        assert_eq!(
            std::fs::read_to_string(root.join("other.txt")).unwrap(),
            "other\n"
        );

        git_ok(&root, &["reset", "--hard", before.as_str()]);
        let outcome = git_cherry_pick(&root, &[conflicting]).expect("start abort pick");
        assert!(outcome.has_conflicts);
        git_cherry_pick_abort(&root).expect("abort pick");
        assert_eq!(head(&root), before);
        assert_eq!(git_operation_status(&root).unwrap().operation, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_stash_list_records() {
        let entries = parse_stash_list(
            "stash@{0}\x00123456789abcdef\x00abc1234\x002026-11-23 10:00:00 +0800\x00WIP on main: message\n",
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].selector, "stash@{0}");
        assert_eq!(entries[0].short_hash, "abc1234");
        assert!(entries[0].message.contains("message"));
    }
}
