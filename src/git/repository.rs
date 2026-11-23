use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::command::{CancellationToken, GitCommandOptions, GitExecutor};
use super::error::{GitError, GitErrorKind};
use super::events::{GitOperationState, NoopGitEventSink};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHead {
    Branch { name: String, oid: Option<String> },
    Detached { oid: String },
    Unborn { name: String },
}

impl GitHead {
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch { name, .. } | Self::Unborn { name } => Some(name),
            Self::Detached { .. } => None,
        }
    }

    pub fn oid(&self) -> Option<&str> {
        match self {
            Self::Branch { oid, .. } => oid.as_deref(),
            Self::Detached { oid } => Some(oid),
            Self::Unborn { .. } => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Branch { name, .. } => name.clone(),
            Self::Detached { oid } => format!("detached {}", short_oid(oid)),
            Self::Unborn { name } => format!("{name} (尚无提交)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepository {
    pub work_tree: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

impl GitRepository {
    pub fn relative_path(&self, target: &Path) -> Result<PathBuf, GitError> {
        let target = dunce::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
        target
            .strip_prefix(&self.work_tree)
            .map(Path::to_path_buf)
            .map_err(|_| {
                GitError::new(GitErrorKind::NotRepository, "所选路径不在 Git 工作区中。")
                    .with_repository(self.work_tree.clone())
            })
    }

    pub fn relative_arg(&self, target: &Path) -> Result<OsString, GitError> {
        let relative = self.relative_path(target)?;
        if relative.as_os_str().is_empty() {
            Ok(OsString::from("."))
        } else {
            Ok(relative.into_os_string())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryState {
    pub repository: GitRepository,
    pub head: GitHead,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<GitOperationState>,
}

impl GitRepositoryState {
    pub fn branch(&self) -> Option<&str> {
        self.head.branch()
    }

    pub fn head_oid(&self) -> Option<&str> {
        self.head.oid()
    }
}

pub fn discover_repository(executor: &GitExecutor, path: &Path) -> Result<GitRepository, GitError> {
    let start = working_directory(path);
    let args = [
        "rev-parse",
        "--show-toplevel",
        "--absolute-git-dir",
        "--git-common-dir",
        "--is-inside-work-tree",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    let result = executor.run(
        start,
        args,
        GitCommandOptions::new("定位 Git 仓库"),
        &CancellationToken::default(),
        &NoopGitEventSink,
        None,
    )?;
    parse_discovery_output(&result.stdout, start).ok_or_else(|| {
        GitError::new(GitErrorKind::NotRepository, "所选路径不在 Git 工作区中。")
            .with_exit_code(result.exit_code)
            .with_repository(start)
            .with_detail(result.stdout)
    })
}

fn parse_discovery_output(output: &str, start: &Path) -> Option<GitRepository> {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let work_tree = lines.next()?;
    let git_dir = lines.next()?;
    let common_dir = lines.next()?;
    let inside_work_tree = lines.next()?;
    if inside_work_tree != "true" {
        return None;
    }
    let work_tree = absolute_path(start, work_tree);
    let git_dir = absolute_path(start, git_dir);
    let common_dir = absolute_path(start, common_dir);
    Some(GitRepository {
        work_tree: dunce::canonicalize(&work_tree).unwrap_or(work_tree),
        git_dir: dunce::canonicalize(&git_dir).unwrap_or(git_dir),
        common_dir: dunce::canonicalize(&common_dir).unwrap_or(common_dir),
    })
}

fn absolute_path(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

pub fn repository_state(
    executor: &GitExecutor,
    repository: &GitRepository,
) -> Result<GitRepositoryState, GitError> {
    let args = [
        "status",
        "--porcelain=v2",
        "--branch",
        "--untracked-files=no",
        "-z",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    let result = executor.run(
        &repository.work_tree,
        args,
        GitCommandOptions::new("读取 Git 仓库状态"),
        &CancellationToken::default(),
        &NoopGitEventSink,
        None,
    )?;
    let (head, upstream, ahead, behind) = parse_branch_headers(&result.stdout);
    let head = head.unwrap_or(GitHead::Detached { oid: String::new() });
    Ok(GitRepositoryState {
        repository: repository.clone(),
        head,
        upstream,
        ahead,
        behind,
        operation: operation_state(&repository.git_dir),
    })
}

fn parse_branch_headers(output: &str) -> (Option<GitHead>, Option<String>, u32, u32) {
    let mut oid = None;
    let mut head_name = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;

    for record in output.split('\0') {
        let record = record.trim_start_matches('\n');
        let Some(header) = record.strip_prefix("# ") else {
            continue;
        };
        if let Some(value) = header.strip_prefix("branch.oid ") {
            oid = (value != "(initial)").then(|| value.to_string());
        } else if let Some(value) = header.strip_prefix("branch.head ") {
            head_name = Some(value.to_string());
        } else if let Some(value) = header.strip_prefix("branch.upstream ") {
            upstream = Some(value.to_string());
        } else if let Some(value) = header.strip_prefix("branch.ab ") {
            for count in value.split_whitespace() {
                if let Some(value) = count.strip_prefix('+') {
                    ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = count.strip_prefix('-') {
                    behind = value.parse().unwrap_or(0);
                }
            }
        }
    }

    let head = match head_name.as_deref() {
        Some("(detached)") => oid.clone().map(|oid| GitHead::Detached { oid }),
        Some(name) if oid.is_none() => Some(GitHead::Unborn {
            name: name.to_string(),
        }),
        Some(name) => Some(GitHead::Branch {
            name: name.to_string(),
            oid,
        }),
        None => None,
    };
    (head, upstream, ahead, behind)
}

pub fn operation_state(git_dir: &Path) -> Option<GitOperationState> {
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

pub fn working_directory(path: &Path) -> &Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    }
}

fn short_oid(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
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
            .env("GIT_TERMINAL_PROMPT", "0")
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
            "kite-git-repository-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create temp repo");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "user.email", "kite@example.test"]);
        root
    }

    #[test]
    fn parses_branch_headers_and_detached_head() {
        let (head, upstream, ahead, behind) = parse_branch_headers(
            "# branch.oid abcdef1234567890\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0",
        );
        assert_eq!(
            head,
            Some(GitHead::Branch {
                name: "main".into(),
                oid: Some("abcdef1234567890".into())
            })
        );
        assert_eq!(upstream.as_deref(), Some("origin/main"));
        assert_eq!((ahead, behind), (2, 3));

        let (head, _, _, _) =
            parse_branch_headers("# branch.oid abcdef1234567890\0# branch.head (detached)\0");
        assert_eq!(
            head,
            Some(GitHead::Detached {
                oid: "abcdef1234567890".into()
            })
        );
    }

    fn current_branch(root: &Path) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(["branch", "--show-current"])
            .output()
            .expect("read current branch");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn discovers_a_nested_repository_and_reports_state() {
        if !git_available() {
            return;
        }
        let root = temp_repo("nested");
        std::fs::create_dir_all(root.join("src/deep")).expect("create nested dir");
        std::fs::write(root.join("src/deep/a.txt"), "one\n").expect("write file");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial"]);

        let executor = GitExecutor::new();
        let repository = discover_repository(&executor, &root.join("src/deep/a.txt"))
            .expect("discover repository");
        assert_eq!(dunce::canonicalize(&root).unwrap(), repository.work_tree);
        let state = repository_state(&executor, &repository).expect("repository state");
        let expected_branch = current_branch(&root);
        assert_eq!(state.branch(), Some(expected_branch.as_str()));
        assert!(state.head_oid().is_some());
        assert_eq!(state.operation, None);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_a_linked_worktree() {
        if !git_available() {
            return;
        }
        let root = temp_repo("linked-main");
        std::fs::write(root.join("a.txt"), "one\n").expect("write file");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial"]);
        let linked = root.with_extension("linked");
        git_ok(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "linked-branch",
                linked.to_str().unwrap(),
            ],
        );

        let executor = GitExecutor::new();
        let repository = discover_repository(&executor, &linked).expect("discover worktree");
        assert_ne!(repository.git_dir, repository.common_dir);
        assert!(
            repository
                .git_dir
                .starts_with(repository.common_dir.join("worktrees"))
        );
        let state = repository_state(&executor, &repository).expect("worktree state");
        assert_eq!(state.branch(), Some("linked-branch"));

        let _ = Command::new("git")
            .current_dir(&root)
            .args(["worktree", "remove", "--force", linked.to_str().unwrap()])
            .output();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn detects_in_progress_merge() {
        let root = std::env::temp_dir().join(format!(
            "kite-git-operation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".git")).expect("create git dir");
        std::fs::write(root.join(".git/MERGE_HEAD"), "abc\n").expect("write merge marker");
        assert_eq!(
            operation_state(&root.join(".git")),
            Some(GitOperationState::Merging)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
