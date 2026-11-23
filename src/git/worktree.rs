use std::path::{Path, PathBuf};

use super::GitService;
use super::preview::run_git;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked_reason: Option<String>,
    pub prunable_reason: Option<String>,
    pub is_current: bool,
    pub is_primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktreeList {
    pub repository_root: PathBuf,
    pub worktrees: Vec<GitWorktree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitWorktreeCreateMode {
    NewBranch {
        branch: String,
        start_point: Option<String>,
    },
    ExistingBranch {
        branch: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktreeRemovalCheck {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_primary: bool,
    pub has_uncommitted_changes: bool,
    pub change_count: u32,
    pub summary: String,
}

pub fn git_worktrees(path: &Path) -> Result<Option<GitWorktreeList>, String> {
    let start = directory_start(path);
    let Ok(repository) = GitService::shared().discover_repository(start) else {
        return Ok(None);
    };
    let root = repository.work_tree;
    let output = run_git(&root, &["worktree", "list", "--porcelain", "-z"])
        .map_err(|error| error.to_string())?;
    let mut worktrees = parse_worktrees(output.stdout.as_bytes(), &root);
    if let Some(primary) = worktrees.first_mut() {
        primary.is_primary = true;
    }
    Ok(Some(GitWorktreeList {
        repository_root: root,
        worktrees,
    }))
}

pub fn create_git_worktree(
    path: &Path,
    destination: &Path,
    mode: GitWorktreeCreateMode,
) -> Result<GitWorktree, String> {
    let start = directory_start(path);
    let root = GitService::shared()
        .discover_repository(start)
        .map_err(|error| error.to_string())?
        .work_tree;
    let destination = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        root.join(destination)
    };
    let destination_text = destination.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add"];
    match &mode {
        GitWorktreeCreateMode::NewBranch {
            branch,
            start_point,
        } => {
            if branch.trim().is_empty() {
                return Err("新分支名称不能为空".to_string());
            }
            args.extend(["-b", branch.as_str(), destination_text.as_str()]);
            if let Some(start_point) = start_point
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                args.push(start_point);
            }
        }
        GitWorktreeCreateMode::ExistingBranch { branch } => {
            if branch.trim().is_empty() {
                return Err("请选择已有分支".to_string());
            }
            args.extend([destination_text.as_str(), branch.as_str()]);
        }
    }
    run_git(&root, &args).map_err(|error| error.to_string())?;

    let target = dunce::canonicalize(&destination).unwrap_or(destination);
    if let Some(list) = git_worktrees(&root)?
        && let Some(worktree) = list
            .worktrees
            .into_iter()
            .find(|worktree| same_path(&worktree.path, &target))
    {
        return Ok(worktree);
    }
    Ok(GitWorktree {
        path: target,
        head: None,
        branch: match mode {
            GitWorktreeCreateMode::NewBranch { branch, .. }
            | GitWorktreeCreateMode::ExistingBranch { branch } => Some(branch),
        },
        detached: false,
        bare: false,
        locked_reason: None,
        prunable_reason: None,
        is_current: false,
        is_primary: false,
    })
}

pub fn worktree_removal_check(path: &Path) -> Result<GitWorktreeRemovalCheck, String> {
    let start = directory_start(path);
    let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let list = git_worktrees(start)?.ok_or_else(|| "所选项目不在 Git 工作区中".to_string())?;
    let worktree = list
        .worktrees
        .iter()
        .find(|worktree| same_path(&worktree.path, &target))
        .ok_or_else(|| "该目录不是已注册的 Git worktree".to_string())?;

    let output = run_git(
        &target,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .map_err(|error| error.to_string())?;
    let mut change_count = 0u32;
    let mut modified = 0u32;
    let mut untracked = 0u32;
    for record in output.stdout.as_bytes().split(|byte| *byte == 0) {
        if !record.iter().any(|byte| !matches!(byte, 0 | b'\r' | b'\n')) {
            continue;
        }
        change_count = change_count.saturating_add(1);
        if record.starts_with(b"??") {
            untracked = untracked.saturating_add(1);
        } else {
            modified = modified.saturating_add(1);
        }
    }
    let summary = if change_count == 0 {
        "没有未提交的改动".to_string()
    } else if untracked == 0 {
        format!("{modified} 个已跟踪文件有未提交改动")
    } else if modified == 0 {
        format!("{untracked} 个未跟踪文件")
    } else {
        format!("{modified} 个已跟踪文件有改动，{untracked} 个未跟踪文件")
    };
    Ok(GitWorktreeRemovalCheck {
        path: worktree.path.clone(),
        branch: worktree.branch.clone(),
        is_primary: worktree.is_primary,
        has_uncommitted_changes: change_count > 0,
        change_count,
        summary,
    })
}

pub fn remove_git_worktree(path: &Path, force: bool) -> Result<(), String> {
    let check = worktree_removal_check(path)?;
    if check.is_primary {
        return Err("不能删除主 worktree".to_string());
    }
    if check.has_uncommitted_changes && !force {
        return Err(format!("worktree 有未提交的改动：{}", check.summary));
    }
    let root = GitService::shared()
        .discover_repository(path)
        .map_err(|error| error.to_string())?
        .work_tree;
    let path_text = path.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path_text.as_str());
    run_git(&root, &args)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn parse_worktrees(output: &[u8], current_root: &Path) -> Vec<GitWorktree> {
    let text = String::from_utf8_lossy(output);
    text.split("\0\0")
        .filter(|record| !record.trim_matches('\0').is_empty())
        .map(|record| {
            let mut path = None;
            let mut head = None;
            let mut branch = None;
            let mut detached = false;
            let mut bare = false;
            let mut locked_reason = None;
            let mut prunable_reason = None;
            for field in record.trim_matches('\0').split('\0') {
                if let Some(value) = field.strip_prefix("worktree ") {
                    path =
                        Some(dunce::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value)));
                } else if let Some(value) = field.strip_prefix("HEAD ") {
                    head = Some(value.to_string());
                } else if let Some(value) = field.strip_prefix("branch ") {
                    branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_string(),
                    );
                } else if field == "detached" {
                    detached = true;
                } else if field == "bare" {
                    bare = true;
                } else if let Some(value) = field.strip_prefix("locked") {
                    locked_reason = Some(value.trim().to_string());
                } else if let Some(value) = field.strip_prefix("prunable") {
                    prunable_reason = Some(value.trim().to_string());
                }
            }
            let path = path.unwrap_or_default();
            GitWorktree {
                is_current: same_path(&path, current_root),
                path,
                head,
                branch,
                detached,
                bare,
                locked_reason,
                prunable_reason,
                is_primary: false,
            }
        })
        .filter(|worktree| !worktree.path.as_os_str().is_empty())
        .collect()
}

fn directory_start(path: &Path) -> &Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = dunce::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = dunce::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    #[cfg(target_os = "windows")]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(target_os = "windows"))]
    {
        left == right
    }
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
            "kite-git-worktree-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo root");
        git_ok(&root, &["init", "--quiet", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "kite@example.com"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        std::fs::write(root.join("tracked.txt"), b"before\n").expect("write fixture");
        git_ok(&root, &["add", "tracked.txt"]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial"]);
        root
    }

    #[test]
    fn creates_lists_checks_and_removes_a_worktree() {
        if !git_available() {
            return;
        }
        let root = temp_repo("lifecycle");
        let destination = root
            .parent()
            .expect("repo parent")
            .join(format!("kite-worktree-peer-{}", std::process::id()));
        let created = create_git_worktree(
            &root,
            &destination,
            GitWorktreeCreateMode::NewBranch {
                branch: "feature/worktree".to_string(),
                start_point: Some("HEAD".to_string()),
            },
        )
        .expect("create worktree");
        assert!(created.path.exists());
        assert_eq!(created.branch.as_deref(), Some("feature/worktree"));

        let listed = git_worktrees(&root)
            .expect("list worktrees")
            .expect("inside repository");
        assert_eq!(listed.worktrees.len(), 2);
        assert!(listed.worktrees[0].is_primary);
        assert!(listed.worktrees[0].is_current);
        assert!(!listed.worktrees[1].is_current);

        std::fs::write(created.path.join("tracked.txt"), b"dirty\n").expect("dirty worktree");
        let check = worktree_removal_check(&created.path).expect("check removal");
        assert!(check.has_uncommitted_changes);
        assert_eq!(check.change_count, 1);
        assert!(remove_git_worktree(&created.path, false).is_err());
        remove_git_worktree(&created.path, true).expect("force remove worktree");
        assert!(!created.path.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_removing_the_primary_worktree() {
        if !git_available() {
            return;
        }
        let root = temp_repo("primary");
        let check = worktree_removal_check(&root).expect("check primary worktree");
        assert!(check.is_primary);
        assert!(remove_git_worktree(&root, true).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_porcelain_worktree_records() {
        let output = b"worktree /repo/main\0HEAD 111111111111\0branch refs/heads/main\0\0worktree /repo/peer\0HEAD 222222222222\0detached\0locked maintenance\0\0";
        let parsed = parse_worktrees(output, Path::new("/repo/main"));
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].is_current);
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert!(parsed[1].detached);
        assert_eq!(parsed[1].locked_reason.as_deref(), Some("maintenance"));
    }
}
