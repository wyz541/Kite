//! Interactive rebase planning and in-memory history rewriting.
//!
//! The UI edits a todo list, but the actual rewrite is performed with Git
//! plumbing commands. `git merge-tree` applies each commit to its new parent,
//! and `git commit-tree` creates the replacement commit. The working tree and
//! index are never checked out or reset, so reword/squash operations do not
//! disturb local edits.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const STATE_FILE: &str = "kite-rebase-state.json";
const MAX_ERROR_DETAIL: usize = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitRebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl GitRebaseAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Edit => "edit",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Pick => "Pick",
            Self::Reword => "Reword",
            Self::Edit => "Edit",
            Self::Squash => "Squash",
            Self::Fixup => "Fixup",
            Self::Drop => "Drop",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRebaseTodo {
    pub commit: String,
    pub action: GitRebaseAction,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRebaseOptions {
    pub update_refs: bool,
    pub autosquash: bool,
    pub preserve_merges: bool,
    pub allow_rewrite_pushed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRebaseSpec {
    pub upstream: String,
    pub onto: String,
    pub options: GitRebaseOptions,
    pub todo: Vec<GitRebaseTodo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRebaseCommit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
    pub message: String,
    pub parents: Vec<String>,
    pub pushed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRebasePlan {
    pub repository_root: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub upstream: String,
    pub onto: String,
    pub operation: Option<String>,
    pub protected: bool,
    pub has_merges: bool,
    pub pushed_commits: Vec<String>,
    pub commits: Vec<GitRebaseCommit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitRebaseStatus {
    InProgress,
    Stopped,
    Completed,
    Aborted,
}

impl GitRebaseStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::InProgress => "进行中",
            Self::Stopped => "已暂停",
            Self::Completed => "已完成",
            Self::Aborted => "已中止",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRebaseState {
    pub status: GitRebaseStatus,
    pub head: String,
    pub message: String,
    pub remaining: u32,
}

#[derive(Clone, Debug)]
struct RepositoryContext {
    root: PathBuf,
    git_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct CommitMetadata {
    hash: String,
    short_hash: String,
    author_name: String,
    author_email: String,
    author_date: String,
    unix_seconds: i64,
    subject: String,
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredRebaseState {
    branch: Option<String>,
    original_head: String,
    upstream: String,
    todo: Vec<GitRebaseTodo>,
    next_index: usize,
    new_head: String,
    edit_parent: String,
    previous_message: String,
    previous_message_before_edit: String,
    previous_old_commit: String,
    resume_parent: Option<String>,
    mapping: Vec<(String, String)>,
    options: GitRebaseOptions,
    status: GitRebaseStatus,
    message: String,
}

/// Builds the interactive rebase plan for the repository containing `path`.
pub fn git_rebase_plan(path: &Path) -> Result<GitRebasePlan, String> {
    let context = repository_context(path)?;
    plan_for_context(&context)
}

/// Returns the current in-memory rebase state, or a CLI rebase marker state.
pub fn git_rebase_status(path: &Path) -> Result<Option<GitRebaseState>, String> {
    let context = repository_context(path)?;
    if let Some(stored) = read_state(&context.git_dir)? {
        return Ok(Some(state_from_stored(&stored)));
    }
    if let Some(operation) = operation_label(&context.git_dir) {
        if operation != "Rebase" {
            return Ok(None);
        }
        let head = rev_parse(&context.root, "HEAD").unwrap_or_default();
        return Ok(Some(GitRebaseState {
            status: GitRebaseStatus::InProgress,
            head,
            message: "Rebase 正在进行".to_string(),
            remaining: 0,
        }));
    }
    Ok(None)
}

/// Rewrites the selected history according to `spec`.
pub fn git_rebase(path: &Path, spec: &GitRebaseSpec) -> Result<GitRebaseState, String> {
    let context = repository_context(path)?;
    rebase_with_context(&context, spec)
}

/// Continues a stopped in-memory rebase.
pub fn git_rebase_continue(path: &Path) -> Result<GitRebaseState, String> {
    let context = repository_context(path)?;
    if operation_label(&context.git_dir).is_some() && read_state(&context.git_dir)?.is_none() {
        return run_cli_rebase_control(&context.root, "continue");
    }
    let mut stored =
        read_state(&context.git_dir)?.ok_or_else(|| "没有可继续的 Rebase".to_string())?;
    if stored.status != GitRebaseStatus::Stopped {
        return Err("当前 Rebase 不处于暂停状态".to_string());
    }
    stored.status = GitRebaseStatus::InProgress;
    let result = continue_stored(&context, stored);
    match &result {
        Ok(state) if state.status == GitRebaseStatus::Completed => {
            remove_state(&context.git_dir)?;
        }
        Ok(_) => {}
        Err(_) => {}
    }
    result
}

/// Skips the commit at the current `edit` stop.
pub fn git_rebase_skip(path: &Path) -> Result<GitRebaseState, String> {
    let context = repository_context(path)?;
    if operation_label(&context.git_dir).is_some() && read_state(&context.git_dir)?.is_none() {
        return run_cli_rebase_control(&context.root, "skip");
    }
    let mut stored =
        read_state(&context.git_dir)?.ok_or_else(|| "没有可跳过的 Rebase".to_string())?;
    if stored.status != GitRebaseStatus::Stopped {
        return Err("当前 Rebase 不处于暂停状态".to_string());
    }
    let current_head = rev_parse(&context.root, "HEAD")?;
    update_current_ref(
        &context.root,
        stored.branch.as_deref(),
        &current_head,
        &stored.edit_parent,
    )?;
    if stored.options.update_refs {
        update_other_refs(
            &context.root,
            stored.branch.as_deref(),
            &[(current_head.clone(), stored.edit_parent.clone())],
        )?;
    }
    stored
        .mapping
        .retain(|(_, replacement)| replacement != &current_head);
    stored.new_head = stored.edit_parent.clone();
    stored.previous_message = stored.previous_message_before_edit.clone();
    let resume_parent = rev_parse(&context.root, &format!("{}^", stored.edit_parent)).ok();
    stored.resume_parent = resume_parent;
    stored.previous_old_commit = stored.edit_parent.clone();
    stored.status = GitRebaseStatus::InProgress;
    let result = continue_stored(&context, stored);
    if result
        .as_ref()
        .is_ok_and(|state| state.status == GitRebaseStatus::Completed)
    {
        remove_state(&context.git_dir)?;
    }
    result
}

/// Aborts a stopped in-memory rebase or a CLI rebase.
pub fn git_rebase_abort(path: &Path) -> Result<GitRebaseState, String> {
    let context = repository_context(path)?;
    if operation_label(&context.git_dir).is_some() && read_state(&context.git_dir)?.is_none() {
        return run_cli_rebase_control(&context.root, "abort");
    }
    let stored = read_state(&context.git_dir)?.ok_or_else(|| "没有可中止的 Rebase".to_string())?;
    if !matches!(
        stored.status,
        GitRebaseStatus::Stopped | GitRebaseStatus::InProgress
    ) {
        return Err("当前 Rebase 不处于可中止状态".to_string());
    }
    if stored.options.update_refs {
        for (old, replacement) in &stored.mapping {
            update_other_refs(
                &context.root,
                stored.branch.as_deref(),
                &[(replacement.clone(), old.clone())],
            )?;
        }
    }
    update_current_ref(
        &context.root,
        stored.branch.as_deref(),
        &stored.new_head,
        &stored.original_head,
    )?;
    remove_state(&context.git_dir)?;
    Ok(GitRebaseState {
        status: GitRebaseStatus::Aborted,
        head: stored.original_head,
        message: "已中止 Rebase 并恢复原分支".to_string(),
        remaining: 0,
    })
}

/// Rewords one commit without switching the working tree.
pub fn git_rebase_reword(
    path: &Path,
    commit: &str,
    message: &str,
    allow_rewrite_pushed: bool,
) -> Result<GitRebaseState, String> {
    if message.trim().is_empty() {
        return Err("提交信息不能为空".to_string());
    }
    let plan = git_rebase_plan(path)?;
    let todo = plan
        .commits
        .iter()
        .map(|item| GitRebaseTodo {
            commit: item.hash.clone(),
            action: if item.hash == commit {
                GitRebaseAction::Reword
            } else {
                GitRebaseAction::Pick
            },
            message: (item.hash == commit).then(|| message.to_string()),
        })
        .collect();
    git_rebase(
        path,
        &GitRebaseSpec {
            upstream: plan.upstream,
            onto: plan.onto,
            options: GitRebaseOptions {
                allow_rewrite_pushed,
                ..Default::default()
            },
            todo,
        },
    )
}

/// Squashes or fixups a contiguous selection of commits.
pub fn git_rebase_squash(
    path: &Path,
    commits: &[String],
    fixup: bool,
    message: Option<String>,
    allow_rewrite_pushed: bool,
) -> Result<GitRebaseState, String> {
    if commits.is_empty() {
        return Err("请选择要合并的提交".to_string());
    }
    let plan = git_rebase_plan(path)?;
    let selected = commits
        .iter()
        .map(|hash| {
            plan.commits
                .iter()
                .position(|item| item.hash == *hash)
                .ok_or_else(|| format!("提交 {hash} 不在 Rebase 范围内"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut sorted = selected.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return Err("请选择连续提交再执行 squash/fixup".to_string());
    }
    if fixup && message.is_some() {
        return Err("fixup 不接收新的提交信息".to_string());
    }
    let first = *sorted
        .first()
        .ok_or_else(|| "请选择要合并的提交".to_string())?;
    let last = *sorted
        .last()
        .ok_or_else(|| "请选择要合并的提交".to_string())?;
    let single = sorted.len() == 1;
    let first_action = if single && message.is_some() {
        GitRebaseAction::Reword
    } else {
        GitRebaseAction::Pick
    };
    let todo = plan
        .commits
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let action = if index == first {
                first_action
            } else if sorted.contains(&index) {
                if fixup {
                    GitRebaseAction::Fixup
                } else {
                    GitRebaseAction::Squash
                }
            } else {
                GitRebaseAction::Pick
            };
            let message = if !fixup && (index == last || (single && index == first)) {
                message.clone()
            } else {
                None
            };
            GitRebaseTodo {
                commit: item.hash.clone(),
                action,
                message,
            }
        })
        .collect();
    git_rebase(
        path,
        &GitRebaseSpec {
            upstream: plan.upstream,
            onto: plan.onto,
            options: GitRebaseOptions {
                allow_rewrite_pushed,
                ..Default::default()
            },
            todo,
        },
    )
}

/// Creates a `fixup!` commit from tracked working-tree changes and folds it
/// into the selected historical commit with autosquash.
pub fn git_rebase_fixup_worktree(
    path: &Path,
    commit: &str,
    allow_rewrite_pushed: bool,
) -> Result<GitRebaseState, String> {
    let context = repository_context(path)?;
    if operation_label(&context.git_dir).is_some() {
        return Err("已有 Git 操作正在进行，请先继续或中止".to_string());
    }
    let plan = plan_for_context(&context)?;
    if plan.protected {
        return Err(format!(
            "分支 {} 受保护，不允许改写历史",
            plan.branch.as_deref().unwrap_or("HEAD")
        ));
    }
    if !plan.pushed_commits.is_empty() && !allow_rewrite_pushed {
        return Err("选中的历史已推送到远程，继续会改写已发布历史".to_string());
    }
    let target = plan
        .commits
        .iter()
        .find(|item| item.hash == commit)
        .ok_or_else(|| format!("提交 {commit} 不在 Rebase 范围内"))?;
    let parent = target
        .parents
        .first()
        .ok_or_else(|| "暂不支持把改动并入根提交".to_string())?;
    let stash = run_git(&context.root, ["stash", "create"])?;
    if !stash.status.success() {
        return Err(output_error("读取当前改动", &stash));
    }
    let stash_hash = String::from_utf8_lossy(&stash.stdout).trim().to_string();
    if stash_hash.is_empty() {
        return Err("没有可加入提交的已跟踪改动".to_string());
    }
    let old_head = rev_parse(&context.root, "HEAD")?;
    let merge_base = format!("--merge-base={old_head}");
    let merged = run_git(
        &context.root,
        [
            "merge-tree",
            "--write-tree",
            "--no-messages",
            merge_base.as_str(),
            target.hash.as_str(),
            stash_hash.as_str(),
        ],
    )?;
    if !merged.status.success() {
        return Err(output_error("把当前改动并入目标提交", &merged));
    }
    let tree = String::from_utf8_lossy(&merged.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if tree.is_empty() {
        return Err("Git 没有返回 fixup 合并后的文件树".to_string());
    }
    let message = format!("fixup! {}", target.subject);
    let fixup = create_current_identity_commit(&context.root, &tree, &old_head, &message)?;
    update_current_ref(&context.root, plan.branch.as_deref(), &old_head, &fixup)?;

    let range = commits_in_range(&context.root, parent, "HEAD")?;
    let todo = range
        .iter()
        .map(|item| GitRebaseTodo {
            commit: item.hash.clone(),
            action: GitRebaseAction::Pick,
            message: None,
        })
        .collect();
    let spec = GitRebaseSpec {
        upstream: parent.clone(),
        onto: parent.clone(),
        options: GitRebaseOptions {
            autosquash: true,
            allow_rewrite_pushed,
            ..Default::default()
        },
        todo,
    };
    match rebase_with_context(&context, &spec) {
        Ok(state) => {
            let reset = run_git(&context.root, ["reset", "--mixed", "HEAD"])?;
            if !reset.status.success() {
                return Err(output_error("同步 fixup 后的暂存区", &reset));
            }
            Ok(state)
        }
        Err(error) => {
            let _ = update_current_ref(&context.root, plan.branch.as_deref(), &fixup, &old_head);
            Err(error)
        }
    }
}

fn plan_for_context(context: &RepositoryContext) -> Result<GitRebasePlan, String> {
    let operation = operation_label(&context.git_dir);
    let branch = current_branch(&context.root);
    let head = rev_parse(&context.root, "HEAD")?;
    let upstream = default_upstream(&context.root).unwrap_or_else(|| head.clone());
    let onto = upstream.clone();
    let protected = branch.as_deref().is_some_and(is_protected_branch);
    let commits = if operation.is_some() {
        Vec::new()
    } else {
        commits_in_range(&context.root, &upstream, "HEAD")?
    };
    let has_merges = commits.iter().any(|commit| commit.parents.len() > 1);
    let pushed_commits = commits
        .iter()
        .filter(|commit| commit.pushed)
        .map(|commit| commit.hash.clone())
        .collect();
    Ok(GitRebasePlan {
        repository_root: context.root.clone(),
        branch,
        head,
        upstream,
        onto,
        operation,
        protected,
        has_merges,
        pushed_commits,
        commits,
    })
}

fn rebase_with_context(
    context: &RepositoryContext,
    spec: &GitRebaseSpec,
) -> Result<GitRebaseState, String> {
    if operation_label(&context.git_dir).is_some() {
        return Err("已有 Git 操作正在进行，请先继续或中止".to_string());
    }
    let plan = plan_for_context(context)?;
    if plan.protected {
        return Err(format!(
            "分支 {} 受保护，不允许改写历史",
            plan.branch.as_deref().unwrap_or("HEAD")
        ));
    }
    if !plan.pushed_commits.is_empty() && !spec.options.allow_rewrite_pushed {
        return Err(format!(
            "选中的提交已推送到远程（{}），继续会改写已发布历史",
            plan.pushed_commits
                .iter()
                .map(|hash| short_hash(context, hash))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if spec.upstream.trim().is_empty() || spec.onto.trim().is_empty() {
        return Err("上游和 onto 不能为空".to_string());
    }
    let old_head = rev_parse(&context.root, "HEAD")?;
    let onto = rev_parse(&context.root, &spec.onto)?;
    let range = commits_in_range(&context.root, &spec.upstream, "HEAD")?;
    if range.is_empty() {
        return Ok(GitRebaseState {
            status: GitRebaseStatus::Completed,
            head: old_head,
            message: "没有需要改写的提交".to_string(),
            remaining: 0,
        });
    }
    let todo = normalize_todo(&range, &spec.todo, spec.options.autosquash)?;

    if plan.has_merges || spec.options.preserve_merges {
        if todo.iter().any(|item| item.action != GitRebaseAction::Pick) {
            return Err(
                "保留合并提交时暂不支持 reword/squash/fixup/drop，请只使用 pick".to_string(),
            );
        }
        let mut args = vec![OsString::from("rebase"), OsString::from("--rebase-merges")];
        if spec.upstream == "--root" {
            args.push(OsString::from("--root"));
        }
        args.push(OsString::from("--onto"));
        args.push(OsString::from(&spec.onto));
        if spec.upstream != "--root" {
            args.push(OsString::from(&spec.upstream));
        }
        if spec.options.update_refs {
            args.push(OsString::from("--update-refs"));
        }
        let output = run_git(&context.root, args)?;
        if !output.status.success() {
            return Err(output_error("Rebase", &output));
        }
        return Ok(GitRebaseState {
            status: GitRebaseStatus::Completed,
            head: rev_parse(&context.root, "HEAD")?,
            message: "Rebase 已完成".to_string(),
            remaining: 0,
        });
    }

    let mut new_head = onto;
    let mut previous_message = String::new();
    let mut previous_parent: Option<String> = None;
    let mut previous_source_commit: Option<String> = None;
    let mut squash_group: Vec<String> = Vec::new();
    let mut mapping = Vec::new();
    for (index, item) in todo.iter().enumerate() {
        let commit = range
            .iter()
            .find(|commit| commit.hash == item.commit)
            .ok_or_else(|| format!("提交 {} 不在 Rebase 范围内", item.commit))?;
        if item.action == GitRebaseAction::Drop {
            continue;
        }
        if commit.parents.len() > 1 {
            return Err("当前 Rebase 不支持合并提交，请开启保留合并提交".to_string());
        }
        let squashing = matches!(
            item.action,
            GitRebaseAction::Squash | GitRebaseAction::Fixup
        );
        let parent = if squashing {
            previous_parent
                .clone()
                .ok_or_else(|| "第一个提交不能是 squash 或 fixup".to_string())?
        } else {
            new_head.clone()
        };
        let merge_onto = new_head.clone();
        let tree = apply_commit_tree(&context.root, commit, &merge_onto)?;
        let message = commit_message(item, commit, &previous_message)?;
        let source = if squashing {
            previous_source_commit
                .as_deref()
                .unwrap_or(commit.hash.as_str())
        } else {
            commit.hash.as_str()
        };
        let replacement = create_commit(&context.root, &tree, &parent, &message, source)?;
        if squashing {
            for old in &squash_group {
                update_mapping(&mut mapping, old, &replacement);
            }
            squash_group.push(commit.hash.clone());
            mapping.push((commit.hash.clone(), replacement.clone()));
        } else {
            mapping.push((commit.hash.clone(), replacement.clone()));
            squash_group.clear();
            squash_group.push(commit.hash.clone());
            previous_parent = Some(parent.clone());
            previous_source_commit = Some(commit.hash.clone());
        }
        let edit_parent = parent.clone();
        let previous_message_before_edit = previous_message.clone();
        new_head = replacement;
        previous_message = message;

        if item.action == GitRebaseAction::Edit {
            update_current_ref(&context.root, plan.branch.as_deref(), &old_head, &new_head)?;
            if spec.options.update_refs {
                update_other_refs(&context.root, plan.branch.as_deref(), &mapping)?;
            }
            let stored = StoredRebaseState {
                branch: plan.branch,
                original_head: old_head,
                upstream: spec.upstream.clone(),
                todo: todo.clone(),
                next_index: index + 1,
                new_head: new_head.clone(),
                edit_parent,
                previous_message: previous_message.clone(),
                previous_message_before_edit,
                previous_old_commit: commit.hash.clone(),
                resume_parent: previous_parent.clone(),
                mapping: mapping.clone(),
                options: spec.options.clone(),
                status: GitRebaseStatus::Stopped,
                message: format!("已停在 {}。检查改动后可以继续或跳过。", commit.short_hash),
            };
            write_state(&context.git_dir, &stored)?;
            return Ok(state_from_stored(&stored));
        }
    }

    update_current_ref(&context.root, plan.branch.as_deref(), &old_head, &new_head)?;
    if spec.options.update_refs {
        update_other_refs(&context.root, plan.branch.as_deref(), &mapping)?;
    }
    Ok(GitRebaseState {
        status: GitRebaseStatus::Completed,
        head: new_head,
        message: "Rebase 已完成".to_string(),
        remaining: 0,
    })
}

fn continue_stored(
    context: &RepositoryContext,
    mut stored: StoredRebaseState,
) -> Result<GitRebaseState, String> {
    let range = commits_in_range(&context.root, &stored.upstream, &stored.original_head)?;
    let remaining = stored.todo[stored.next_index.min(stored.todo.len())..].to_vec();
    let ref_old = rev_parse(&context.root, "HEAD")?;
    let mut new_head = stored.new_head.clone();
    let mut previous_message = stored.previous_message.clone();
    let mut previous_parent = stored.resume_parent.clone();
    let mut previous_source_commit = Some(stored.previous_old_commit.clone());
    let mut squash_group = vec![stored.previous_old_commit.clone()];
    let mut mapping = stored.mapping.clone();
    for (offset, item) in remaining.iter().enumerate() {
        let index = stored.next_index + offset;
        let commit = range
            .iter()
            .find(|commit| commit.hash == item.commit)
            .ok_or_else(|| format!("提交 {} 不在 Rebase 范围内", item.commit))?;
        if item.action == GitRebaseAction::Drop {
            continue;
        }
        if commit.parents.len() > 1 {
            return Err("当前 Rebase 不支持合并提交，请开启保留合并提交".to_string());
        }
        let squashing = matches!(
            item.action,
            GitRebaseAction::Squash | GitRebaseAction::Fixup
        );
        let parent = if squashing {
            previous_parent
                .clone()
                .ok_or_else(|| "第一个提交不能是 squash 或 fixup".to_string())?
        } else {
            new_head.clone()
        };
        let merge_onto = new_head.clone();
        let tree = apply_commit_tree(&context.root, commit, &merge_onto)?;
        let message = commit_message(item, commit, &previous_message)?;
        let source = if squashing {
            previous_source_commit
                .as_deref()
                .unwrap_or(commit.hash.as_str())
        } else {
            commit.hash.as_str()
        };
        let replacement = create_commit(&context.root, &tree, &parent, &message, source)?;
        if squashing {
            for old in &squash_group {
                update_mapping(&mut mapping, old, &replacement);
            }
            squash_group.push(commit.hash.clone());
            mapping.push((commit.hash.clone(), replacement.clone()));
        } else {
            mapping.push((commit.hash.clone(), replacement.clone()));
            squash_group.clear();
            squash_group.push(commit.hash.clone());
            previous_parent = Some(parent.clone());
            previous_source_commit = Some(commit.hash.clone());
        }
        let edit_parent = parent.clone();
        let previous_message_before_edit = previous_message.clone();
        new_head = replacement;
        previous_message = message;
        if item.action == GitRebaseAction::Edit {
            update_current_ref(&context.root, stored.branch.as_deref(), &ref_old, &new_head)?;
            if stored.options.update_refs {
                update_other_refs(&context.root, stored.branch.as_deref(), &mapping)?;
            }
            stored.next_index = index + 1;
            stored.new_head = new_head.clone();
            stored.edit_parent = edit_parent;
            stored.previous_message = previous_message;
            stored.previous_message_before_edit = previous_message_before_edit;
            stored.previous_old_commit = commit.hash.clone();
            stored.resume_parent = previous_parent.clone();
            stored.mapping = mapping.clone();
            stored.status = GitRebaseStatus::Stopped;
            stored.message = format!("已停在 {}。检查改动后可以继续或跳过。", commit.short_hash);
            write_state(&context.git_dir, &stored)?;
            return Ok(state_from_stored(&stored));
        }
    }
    update_current_ref(&context.root, stored.branch.as_deref(), &ref_old, &new_head)?;
    if stored.options.update_refs {
        update_other_refs(&context.root, stored.branch.as_deref(), &mapping)?;
    }
    Ok(GitRebaseState {
        status: GitRebaseStatus::Completed,
        head: new_head,
        message: "Rebase 已完成".to_string(),
        remaining: 0,
    })
}

fn update_mapping(mapping: &mut Vec<(String, String)>, old: &str, replacement: &str) {
    if let Some(entry) = mapping.iter_mut().find(|(hash, _)| hash == old) {
        entry.1 = replacement.to_string();
    } else {
        mapping.push((old.to_string(), replacement.to_string()));
    }
}

fn normalize_todo(
    range: &[GitRebaseCommit],
    todo: &[GitRebaseTodo],
    autosquash: bool,
) -> Result<Vec<GitRebaseTodo>, String> {
    let mut normalized = todo.to_vec();
    let mut seen = std::collections::HashSet::new();
    for item in &normalized {
        if !range.iter().any(|commit| commit.hash == item.commit) {
            return Err(format!("提交 {} 不在 Rebase 范围内", item.commit));
        }
        if !seen.insert(item.commit.clone()) {
            return Err(format!("提交 {} 在 todo 中重复", item.commit));
        }
    }
    if autosquash {
        normalized = apply_autosquash(range, normalized);
    }
    let first = normalized
        .iter()
        .find(|item| item.action != GitRebaseAction::Drop);
    if first.is_some_and(|item| {
        matches!(
            item.action,
            GitRebaseAction::Squash | GitRebaseAction::Fixup
        )
    }) {
        return Err("第一个提交不能是 squash 或 fixup".to_string());
    }
    Ok(normalized)
}

fn apply_autosquash(range: &[GitRebaseCommit], mut todo: Vec<GitRebaseTodo>) -> Vec<GitRebaseTodo> {
    for _ in 0..todo.len().saturating_mul(2) {
        let mut changed = false;
        for index in 0..todo.len() {
            if todo[index].action != GitRebaseAction::Pick {
                continue;
            }
            let Some(commit) = range
                .iter()
                .find(|commit| commit.hash == todo[index].commit)
            else {
                continue;
            };
            let (action, target_subject) =
                if let Some(subject) = commit.subject.strip_prefix("fixup! ") {
                    (GitRebaseAction::Fixup, subject)
                } else if let Some(subject) = commit.subject.strip_prefix("squash! ") {
                    (GitRebaseAction::Squash, subject)
                } else {
                    continue;
                };
            let Some(target_index) = todo.iter().position(|item| {
                item.action != GitRebaseAction::Drop
                    && range
                        .iter()
                        .find(|candidate| candidate.hash == item.commit)
                        .is_some_and(|candidate| candidate.subject == target_subject)
            }) else {
                continue;
            };
            if target_index == index {
                continue;
            }
            let mut item = todo.remove(index);
            item.action = action;
            let insert_at = if target_index < index {
                target_index + 1
            } else {
                target_index
            };
            todo.insert(insert_at, item);
            changed = true;
            break;
        }
        if !changed {
            break;
        }
    }
    todo
}

fn commit_message(
    item: &GitRebaseTodo,
    commit: &GitRebaseCommit,
    previous_message: &str,
) -> Result<String, String> {
    let message = match item.action {
        GitRebaseAction::Pick | GitRebaseAction::Edit => commit.message.clone(),
        GitRebaseAction::Reword => item
            .message
            .clone()
            .filter(|message| !message.trim().is_empty())
            .ok_or_else(|| format!("{} 的 reword 提交信息为空", commit.short_hash))?,
        GitRebaseAction::Squash => item
            .message
            .clone()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{}\n\n{}",
                    previous_message.trim_end(),
                    commit.message.trim()
                )
            }),
        GitRebaseAction::Fixup => previous_message.to_string(),
        GitRebaseAction::Drop => String::new(),
    };
    if message.trim().is_empty() {
        return Err(format!("{} 的提交信息为空", commit.short_hash));
    }
    Ok(message)
}

fn apply_commit_tree(
    root: &Path,
    commit: &GitRebaseCommit,
    new_parent: &str,
) -> Result<String, String> {
    let Some(old_parent) = commit.parents.first() else {
        let revision = format!("{}^{{tree}}", commit.hash);
        return git_text(root, ["rev-parse", revision.as_str()])
            .map(|text| text.trim().to_string());
    };
    let merge_base = format!("--merge-base={old_parent}");
    let output = run_git(
        root,
        [
            "merge-tree",
            "--write-tree",
            "--no-messages",
            merge_base.as_str(),
            new_parent,
            commit.hash.as_str(),
        ],
    )?;
    if !output.status.success() {
        return Err(output_error("应用提交", &output));
    }
    let tree = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if tree.is_empty() {
        return Err("Git 没有返回合并后的文件树".to_string());
    }
    Ok(tree)
}

fn create_commit(
    root: &Path,
    tree: &str,
    parent: &str,
    message: &str,
    source_commit: &str,
) -> Result<String, String> {
    let metadata = commit_metadata(root, source_commit)?;
    let mut command = configured_command("git", root);
    command
        .args(["commit-tree", tree, "-p", parent, "-m", message])
        .env("GIT_AUTHOR_NAME", metadata.author_name)
        .env("GIT_AUTHOR_EMAIL", metadata.author_email)
        .env("GIT_AUTHOR_DATE", metadata.author_date);
    let output = command
        .output()
        .map_err(|error| format!("创建提交失败：{error}"))?;
    if !output.status.success() {
        return Err(output_error("创建提交", &output));
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.is_empty() {
        return Err("Git 没有返回新提交哈希".to_string());
    }
    Ok(hash)
}

fn create_current_identity_commit(
    root: &Path,
    tree: &str,
    parent: &str,
    message: &str,
) -> Result<String, String> {
    let output = run_git(root, ["commit-tree", tree, "-p", parent, "-m", message])?;
    if !output.status.success() {
        return Err(output_error("创建 fixup 提交", &output));
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.is_empty() {
        return Err("Git 没有返回 fixup 提交哈希".to_string());
    }
    Ok(hash)
}

fn commits_in_range(
    root: &Path,
    upstream: &str,
    head: &str,
) -> Result<Vec<GitRebaseCommit>, String> {
    let output = if upstream == "--root" {
        run_git(
            root,
            [
                "rev-list",
                "--reverse",
                "--topo-order",
                "--parents",
                "--root",
                head,
            ],
        )?
    } else {
        let revision = format!("{upstream}..{head}");
        run_git(
            root,
            [
                "rev-list",
                "--reverse",
                "--topo-order",
                "--parents",
                revision.as_str(),
            ],
        )?
    };
    if !output.status.success() {
        return Err(output_error("读取 Rebase 提交", &output));
    }
    let mut commits = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let metadata = commit_metadata(root, hash)?;
        commits.push(GitRebaseCommit {
            hash: metadata.hash,
            short_hash: metadata.short_hash,
            author: metadata.author_name,
            unix_seconds: metadata.unix_seconds,
            subject: metadata.subject,
            message: metadata.message,
            parents: parts.map(str::to_string).collect(),
            pushed: is_pushed(root, hash)?,
        });
    }
    Ok(commits)
}

fn commit_metadata(root: &Path, commit: &str) -> Result<CommitMetadata, String> {
    let format = "%H%x00%h%x00%an%x00%ae%x00%aI%x00%at%x00%s%x00%B";
    let format_argument = format!("--format={format}");
    let text = git_text(root, ["show", "-s", format_argument.as_str(), commit])?;
    let mut parts = text.splitn(8, '\0');
    let hash = parts.next().unwrap_or_default().to_string();
    let short_hash = parts.next().unwrap_or_default().to_string();
    let author_name = parts.next().unwrap_or_default().to_string();
    let author_email = parts.next().unwrap_or_default().to_string();
    let author_date = parts.next().unwrap_or_default().to_string();
    let unix_seconds = parts
        .next()
        .unwrap_or_default()
        .parse::<i64>()
        .unwrap_or_default();
    let subject = parts.next().unwrap_or_default().to_string();
    let message = parts
        .next()
        .unwrap_or_default()
        .trim_end_matches('\n')
        .to_string();
    if hash.is_empty() {
        return Err(format!("无法读取提交 {commit}"));
    }
    Ok(CommitMetadata {
        hash,
        short_hash,
        author_name,
        author_email,
        author_date,
        unix_seconds,
        subject,
        message,
    })
}

fn current_branch(root: &Path) -> Option<String> {
    run_git(root, ["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|branch| !branch.is_empty())
}

fn default_upstream(root: &Path) -> Option<String> {
    let upstream = run_git(
        root,
        [
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    .filter(|value| !value.is_empty());
    if let Some(upstream) = upstream {
        let head = rev_parse(root, "HEAD").ok();
        let upstream_hash = rev_parse(root, &upstream).ok();
        if upstream_hash.is_some() && upstream_hash != head {
            return Some(upstream);
        }
    }
    run_git(root, ["rev-parse", "--verify", "HEAD~10^{commit}"])
        .ok()
        .filter(|output| output.status.success())
        .map(|_| "HEAD~10".to_string())
        .or_else(|| {
            run_git(root, ["rev-list", "--max-parents=0", "HEAD"])
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .map(|hash| hash.lines().next().unwrap_or_default().to_string())
                .filter(|hash| !hash.is_empty())
        })
}

fn is_pushed(root: &Path, commit: &str) -> Result<bool, String> {
    let output = run_git(
        root,
        [
            "for-each-ref",
            "--contains",
            commit,
            "--format=%(refname)",
            "refs/remotes",
        ],
    )?;
    Ok(output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn short_hash(context: &RepositoryContext, commit: &str) -> String {
    git_text(&context.root, ["rev-parse", "--short", commit])
        .map(|hash| hash.trim().to_string())
        .unwrap_or_else(|_| commit.chars().take(7).collect())
}

fn rev_parse(root: &Path, revision: &str) -> Result<String, String> {
    let output = run_git(root, ["rev-parse", "--verify", revision])?;
    if !output.status.success() {
        return Err(output_error("解析 Git 版本", &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn update_current_ref(
    root: &Path,
    branch: Option<&str>,
    old_head: &str,
    new_head: &str,
) -> Result<(), String> {
    let reference = branch
        .map(|branch| format!("refs/heads/{branch}"))
        .unwrap_or_else(|| "HEAD".to_string());
    let output = run_git(root, ["update-ref", reference.as_str(), new_head, old_head])?;
    if !output.status.success() {
        return Err(output_error("更新分支", &output));
    }
    Ok(())
}

fn update_other_refs(
    root: &Path,
    current_branch: Option<&str>,
    mapping: &[(String, String)],
) -> Result<(), String> {
    if mapping.is_empty() {
        return Ok(());
    }
    let output = run_git(
        root,
        [
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/tags",
        ],
    )?;
    if !output.status.success() {
        return Err(output_error("读取需要更新的引用", &output));
    }
    let current_ref = current_branch.map(|branch| format!("refs/heads/{branch}"));
    for reference in String::from_utf8_lossy(&output.stdout).lines() {
        if current_ref.as_deref() == Some(reference) {
            continue;
        }
        let Ok(value) = rev_parse(root, reference) else {
            continue;
        };
        let Some((_, replacement)) = mapping.iter().find(|(old, _)| old == &value) else {
            continue;
        };
        let object_type = git_text(root, ["cat-file", "-t", reference]).unwrap_or_default();
        if reference.starts_with("refs/tags/") && object_type != "commit" {
            continue;
        }
        let output = run_git(
            root,
            [
                "update-ref",
                reference,
                replacement.as_str(),
                value.as_str(),
            ],
        )?;
        if !output.status.success() {
            return Err(output_error("更新引用", &output));
        }
    }
    Ok(())
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
        return Err(output_error("定位 Git 仓库", &output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let Some(root) = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err("无法确定 Git 工作区".to_string());
    };
    let Some(git_dir) = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err("无法确定 Git 目录".to_string());
    };
    Ok(RepositoryContext {
        root: dunce::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root)),
        git_dir: dunce::canonicalize(git_dir).unwrap_or_else(|_| PathBuf::from(git_dir)),
    })
}

fn operation_label(git_dir: &Path) -> Option<String> {
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        Some("Rebase".to_string())
    } else if git_dir.join("MERGE_HEAD").exists() {
        Some("Merge".to_string())
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        Some("Cherry-pick".to_string())
    } else if git_dir.join("REVERT_HEAD").exists() {
        Some("Revert".to_string())
    } else if git_dir.join("BISECT_LOG").exists() {
        Some("Bisect".to_string())
    } else {
        None
    }
}

fn is_protected_branch(branch: &str) -> bool {
    let configured = std::env::var("KITE_PROTECTED_BRANCHES").ok();
    let patterns = configured
        .as_deref()
        .unwrap_or("main,master,trunk,release/*,hotfix/*")
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty());
    patterns.into_iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            branch.starts_with(prefix)
        } else {
            branch == pattern
        }
    })
}

fn state_path(git_dir: &Path) -> PathBuf {
    git_dir.join(STATE_FILE)
}

fn read_state(git_dir: &Path) -> Result<Option<StoredRebaseState>, String> {
    let path = state_path(git_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("读取 Rebase 状态失败：{error}"))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| format!("解析 Rebase 状态失败：{error}"))
}

fn write_state(git_dir: &Path, state: &StoredRebaseState) -> Result<(), String> {
    let path = state_path(git_dir);
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| format!("序列化 Rebase 状态失败：{error}"))?;
    std::fs::write(&path, text).map_err(|error| format!("保存 Rebase 状态失败：{error}"))
}

fn remove_state(git_dir: &Path) -> Result<(), String> {
    let path = state_path(git_dir);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|error| format!("删除 Rebase 状态失败：{error}"))?;
    }
    Ok(())
}

fn state_from_stored(stored: &StoredRebaseState) -> GitRebaseState {
    GitRebaseState {
        status: stored.status,
        head: stored.new_head.clone(),
        message: stored.message.clone(),
        remaining: stored.todo.len().saturating_sub(stored.next_index) as u32,
    }
}

fn run_cli_rebase_control(root: &Path, action: &str) -> Result<GitRebaseState, String> {
    let control = format!("--{action}");
    let output = run_git(root, ["rebase", control.as_str()])?;
    if !output.status.success() {
        return Err(output_error("控制 Rebase", &output));
    }
    let status = match action {
        "abort" => GitRebaseStatus::Aborted,
        _ => {
            if operation_label(&repository_context(root)?.git_dir).is_some() {
                GitRebaseStatus::Stopped
            } else {
                GitRebaseStatus::Completed
            }
        }
    };
    Ok(GitRebaseState {
        status,
        head: rev_parse(root, "HEAD").unwrap_or_default(),
        message: match status {
            GitRebaseStatus::Aborted => "已中止 Rebase".to_string(),
            GitRebaseStatus::Stopped => "Rebase 仍在进行".to_string(),
            _ => "Rebase 已完成".to_string(),
        },
        remaining: 0,
    })
}

fn configured_command(program: &str, current_dir: &Path) -> Command {
    let mut command = Command::new(program);
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

fn run_git<I, S>(current_dir: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = configured_command("git", current_dir);
    command.args(args);
    command
        .output()
        .map_err(|error| format!("无法运行 Git：{error}"))
}

fn git_text<I, S>(current_dir: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = run_git(current_dir, args)?;
    if !output.status.success() {
        return Err(output_error("Git 命令", &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn output_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let detail = detail
        .replace(['\r', '\n'], " ")
        .chars()
        .take(MAX_ERROR_DETAIL)
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

    fn temp_repo(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("kite-rebase-{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create repo");
        git_ok(&path, &["init", "-q"]);
        git_ok(&path, &["config", "user.name", "Kite Test"]);
        git_ok(&path, &["config", "user.email", "kite@example.com"]);
        git_ok(&path, &["symbolic-ref", "HEAD", "refs/heads/feature"]);
        path
    }

    fn git_ok(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit_file(root: &Path, name: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(name), content).expect("write file");
        git_ok(root, &["add", name]);
        git_ok(root, &["commit", "-qm", message]);
        git_ok(root, &["rev-parse", "HEAD"])
    }

    fn plan_for(root: &Path) -> GitRebasePlan {
        git_rebase_plan(root).expect("plan")
    }

    #[test]
    fn plan_lists_rewritable_commits_oldest_first() {
        let root = temp_repo("plan");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "a.txt", "two\n", "second");
        let plan = plan_for(&root);
        assert_eq!(plan.commits.len(), 1);
        assert_eq!(plan.commits[0].hash, second);
        assert_eq!(plan.commits[0].subject, "second");
        assert!(!plan.protected);
    }

    #[test]
    fn reword_preserves_tree_and_changes_message() {
        let root = temp_repo("reword");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "a.txt", "two\n", "second");
        commit_file(&root, "a.txt", "three\n", "third");
        let tree_before = git_ok(&root, &["rev-parse", "HEAD^{tree}"]);
        let state = git_rebase_reword(&root, &second, "second reworded", true).expect("reword");
        assert_eq!(state.status, GitRebaseStatus::Completed);
        assert_eq!(git_ok(&root, &["rev-parse", "HEAD^{tree}"]), tree_before);
        assert_eq!(
            git_ok(&root, &["log", "-1", "--format=%s", "HEAD~1"]),
            "second reworded"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).expect("read"),
            "three\n"
        );
    }

    #[test]
    fn squash_combines_commits_and_preserves_tree() {
        let root = temp_repo("squash");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "a.txt", "two\n", "second");
        let third = commit_file(&root, "a.txt", "three\n", "third");
        let tree_before = git_ok(&root, &["rev-parse", "HEAD^{tree}"]);
        let state = git_rebase_squash(
            &root,
            &[second.clone(), third],
            false,
            Some("combined".to_string()),
            true,
        )
        .expect("squash");
        assert_eq!(state.status, GitRebaseStatus::Completed);
        assert_eq!(git_ok(&root, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(git_ok(&root, &["rev-parse", "HEAD^{tree}"]), tree_before);
        assert_eq!(git_ok(&root, &["log", "-1", "--format=%s"]), "combined");
    }

    #[test]
    fn squash_without_a_custom_message_combines_commit_messages() {
        let root = temp_repo("squash-messages");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "a.txt", "two\n", "second");
        let third = commit_file(&root, "a.txt", "three\n", "third");
        git_rebase_squash(&root, &[second, third], false, None, true).expect("squash");
        let message = git_ok(&root, &["log", "-1", "--format=%B"]);
        assert!(message.contains("second"));
        assert!(message.contains("third"));
        assert_eq!(git_ok(&root, &["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn fixup_worktree_folds_changes_into_history_without_dirtying_tree() {
        let root = temp_repo("fixup-worktree");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "b.txt", "two\n", "second");
        commit_file(&root, "c.txt", "three\n", "third");
        std::fs::write(root.join("b.txt"), "two fixed\n").expect("edit b");
        let state = git_rebase_fixup_worktree(&root, &second, true).expect("fixup worktree");
        assert_eq!(state.status, GitRebaseStatus::Completed);
        assert_eq!(git_ok(&root, &["rev-list", "--count", "HEAD"]), "3");
        assert_eq!(git_ok(&root, &["show", "HEAD~1:b.txt"]), "two fixed");
        assert_eq!(git_ok(&root, &["status", "--porcelain"]), "");
    }

    #[test]
    fn edit_stop_continue_and_abort_work() {
        let root = temp_repo("edit");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "a.txt", "two\n", "second");
        commit_file(&root, "a.txt", "three\n", "third");
        let plan = plan_for(&root);
        let todo = plan
            .commits
            .iter()
            .map(|commit| GitRebaseTodo {
                commit: commit.hash.clone(),
                action: if commit.hash == second {
                    GitRebaseAction::Edit
                } else {
                    GitRebaseAction::Pick
                },
                message: None,
            })
            .collect();
        let spec = GitRebaseSpec {
            upstream: plan.upstream,
            onto: plan.onto,
            options: GitRebaseOptions::default(),
            todo,
        };
        let stopped = git_rebase(&root, &spec).expect("stop");
        assert_eq!(stopped.status, GitRebaseStatus::Stopped);
        assert_eq!(stopped.remaining, 1);
        let continued = git_rebase_continue(&root).expect("continue");
        assert_eq!(continued.status, GitRebaseStatus::Completed);
        assert_eq!(git_ok(&root, &["rev-list", "--count", "HEAD"]), "3");
        let after_continue = git_ok(&root, &["rev-parse", "HEAD"]);

        let plan = plan_for(&root);
        let edit_hash = plan
            .commits
            .first()
            .map(|commit| commit.hash.clone())
            .expect("rewritable commit");
        let todo = plan
            .commits
            .iter()
            .map(|commit| GitRebaseTodo {
                commit: commit.hash.clone(),
                action: if commit.hash == edit_hash {
                    GitRebaseAction::Edit
                } else {
                    GitRebaseAction::Pick
                },
                message: None,
            })
            .collect();
        let spec = GitRebaseSpec {
            upstream: plan.upstream,
            onto: plan.onto,
            options: GitRebaseOptions::default(),
            todo,
        };
        git_rebase(&root, &spec).expect("stop again");
        let aborted = git_rebase_abort(&root).expect("abort");
        assert_eq!(aborted.status, GitRebaseStatus::Aborted);
        assert_eq!(git_ok(&root, &["rev-parse", "HEAD"]), after_continue);
    }

    #[test]
    fn pushed_commits_require_confirmation() {
        let root = temp_repo("pushed");
        git_ok(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        commit_file(&root, "a.txt", "one\n", "first");
        let remote = root.with_extension("remote.git");
        std::fs::create_dir_all(&remote).expect("create remote");
        git_ok(&remote, &["init", "--bare", "-q"]);
        let remote_path = remote.to_string_lossy().to_string();
        git_ok(&root, &["remote", "add", "origin", remote_path.as_str()]);
        git_ok(&root, &["push", "-q", "-u", "origin", "main"]);
        git_ok(&root, &["switch", "-q", "-c", "feature"]);
        let second = commit_file(&root, "b.txt", "two\n", "second");
        git_ok(&root, &["push", "-q", "-u", "origin", "feature"]);
        git_ok(
            &root,
            &["branch", "--set-upstream-to=origin/main", "feature"],
        );

        let plan = plan_for(&root);
        assert!(plan.pushed_commits.contains(&second));

        let todo = vec![GitRebaseTodo {
            commit: second,
            action: GitRebaseAction::Pick,
            message: None,
        }];
        let blocked = git_rebase(
            &root,
            &GitRebaseSpec {
                upstream: plan.upstream.clone(),
                onto: plan.onto.clone(),
                options: GitRebaseOptions::default(),
                todo: todo.clone(),
            },
        )
        .expect_err("pushed commit needs confirmation");
        assert!(blocked.contains("已推送"));

        let completed = git_rebase(
            &root,
            &GitRebaseSpec {
                upstream: plan.upstream,
                onto: plan.onto,
                options: GitRebaseOptions {
                    allow_rewrite_pushed: true,
                    ..Default::default()
                },
                todo,
            },
        )
        .expect("confirmed rewrite");
        assert_eq!(completed.status, GitRebaseStatus::Completed);
    }

    #[test]
    fn protected_branch_is_rejected() {
        let root = temp_repo("protected");
        git_ok(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        commit_file(&root, "a.txt", "one\n", "first");
        commit_file(&root, "a.txt", "two\n", "second");
        let plan = plan_for(&root);
        let todo = plan
            .commits
            .iter()
            .map(|commit| GitRebaseTodo {
                commit: commit.hash.clone(),
                action: GitRebaseAction::Pick,
                message: None,
            })
            .collect();
        let spec = GitRebaseSpec {
            upstream: plan.upstream,
            onto: plan.onto,
            options: GitRebaseOptions::default(),
            todo,
        };
        let error = git_rebase(&root, &spec).expect_err("protected");
        assert!(error.contains("受保护"));
    }

    #[test]
    fn autosquash_moves_fixup_after_target() {
        let root = temp_repo("autosquash");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "a.txt", "two\n", "second");
        let fixup = commit_file(&root, "a.txt", "three\n", "fixup! second");
        let plan = plan_for(&root);
        let todo: Vec<GitRebaseTodo> = plan
            .commits
            .iter()
            .map(|commit| GitRebaseTodo {
                commit: commit.hash.clone(),
                action: GitRebaseAction::Pick,
                message: None,
            })
            .collect();
        let normalized = normalize_todo(&plan.commits, &todo, true).expect("normalize");
        let first_index = normalized
            .iter()
            .position(|item| item.commit == second)
            .expect("second");
        let fixup_index = normalized
            .iter()
            .position(|item| item.commit == fixup)
            .expect("fixup");
        assert_eq!(fixup_index, first_index + 1);
        assert_eq!(normalized[fixup_index].action, GitRebaseAction::Fixup);
    }

    #[cfg(unix)]
    #[test]
    fn custom_rebase_tree_matches_git_rebase_interactive() {
        let root = temp_repo("compare");
        commit_file(&root, "a.txt", "one\n", "first");
        let second = commit_file(&root, "b.txt", "two\n", "second");
        let third = commit_file(&root, "c.txt", "three\n", "third");
        let original = git_ok(&root, &["rev-parse", "HEAD"]);
        let plan = plan_for(&root);
        let todo = vec![
            GitRebaseTodo {
                commit: third.clone(),
                action: GitRebaseAction::Pick,
                message: None,
            },
            GitRebaseTodo {
                commit: second.clone(),
                action: GitRebaseAction::Pick,
                message: None,
            },
        ];
        let spec = GitRebaseSpec {
            upstream: plan.upstream.clone(),
            onto: plan.onto.clone(),
            options: GitRebaseOptions::default(),
            todo,
        };
        git_rebase(&root, &spec).expect("custom rebase");
        let custom_tree = git_ok(&root, &["rev-parse", "HEAD^{tree}"]);
        let custom_subjects = git_ok(&root, &["log", "--format=%s", "--reverse"]);

        git_ok(&root, &["reset", "--hard", &original]);
        let editor = root.join("sequence-editor.sh");
        std::fs::write(
            &editor,
            format!("#!/bin/sh\ncat > \"$1\" <<'EOF'\npick {third}\npick {second}\nEOF\n"),
        )
        .expect("write editor");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&editor).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&editor, permissions).expect("chmod");
        }
        let output = Command::new("git")
            .current_dir(&root)
            .args(["rebase", "-i", "--onto", &plan.onto, &plan.upstream])
            .env("GIT_SEQUENCE_EDITOR", &editor)
            .env("GIT_EDITOR", "true")
            .output()
            .expect("run interactive rebase");
        assert!(
            output.status.success(),
            "interactive rebase failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(git_ok(&root, &["rev-parse", "HEAD^{tree}"]), custom_tree);
        assert_eq!(
            git_ok(&root, &["log", "--format=%s", "--reverse"]),
            custom_subjects
        );
    }
}
