use std::path::{Path, PathBuf};

use super::GitService;
use super::preview::run_git;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitSubmoduleState {
    Uninitialized,
    Initialized,
    OutOfDate,
    Conflicted,
}

impl GitSubmoduleState {
    pub fn code(self) -> &'static str {
        match self {
            Self::Uninitialized => "-",
            Self::Initialized => "✓",
            Self::OutOfDate => "+",
            Self::Conflicted => "!",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Uninitialized => "未初始化",
            Self::Initialized => "已初始化",
            Self::OutOfDate => "与索引不一致",
            Self::Conflicted => "有冲突",
        }
    }

    pub fn needs_attention(self) -> bool {
        !matches!(self, Self::Initialized)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSubmodule {
    pub name: String,
    pub relative_path: String,
    pub path: PathBuf,
    pub url: String,
    pub declared_commit: String,
    pub current_commit: Option<String>,
    pub branch: Option<String>,
    pub describe: Option<String>,
    pub state: GitSubmoduleState,
    pub has_uncommitted_changes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSubmoduleList {
    pub repository_root: PathBuf,
    pub submodules: Vec<GitSubmodule>,
}

pub fn git_submodules(path: &Path) -> Result<Option<GitSubmoduleList>, String> {
    let start = directory_start(path);
    let Ok(repository) = GitService::shared().discover_repository(start) else {
        return Ok(None);
    };
    let root = repository.work_tree;
    if !root.join(".gitmodules").is_file() {
        return Ok(Some(GitSubmoduleList {
            repository_root: root,
            submodules: Vec::new(),
        }));
    }
    let output = run_git(&root, &["submodule", "status", "--recursive"])
        .map_err(|error| error.to_string())?;
    let config = submodule_config(&root)?;
    let mut submodules = parse_submodule_status(&output.stdout, &config, &root);
    for submodule in &mut submodules {
        if !submodule.path.is_dir() {
            continue;
        }
        let output = run_git(
            &submodule.path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        );
        submodule.has_uncommitted_changes = output.is_ok_and(|output| {
            output
                .stdout
                .as_bytes()
                .iter()
                .any(|byte| !matches!(byte, 0 | b'\r' | b'\n'))
        });
        submodule.current_commit = run_git(&submodule.path, &["rev-parse", "HEAD"])
            .ok()
            .map(|output| output.stdout.trim().to_string());
        submodule.branch = run_git(&submodule.path, &["branch", "--show-current"])
            .ok()
            .map(|output| output.stdout.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    Ok(Some(GitSubmoduleList {
        repository_root: root,
        submodules,
    }))
}

pub fn git_init_submodules(path: &Path) -> Result<String, String> {
    run_submodule_command(path, &["submodule", "init"], "Git submodule 初始化")
}

pub fn git_update_submodules(path: &Path) -> Result<String, String> {
    run_submodule_command(
        path,
        &["submodule", "update", "--init", "--recursive"],
        "Git submodule 更新",
    )
}

fn run_submodule_command(path: &Path, args: &[&str], label: &str) -> Result<String, String> {
    let root = GitService::shared()
        .discover_repository(directory_start(path))
        .map_err(|error| error.to_string())?
        .work_tree;
    run_git(&root, args)
        .map(|_| format!("{label}完成"))
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Default)]
struct SubmoduleConfig {
    name: String,
    path: String,
    url: String,
}

fn submodule_config(root: &Path) -> Result<Vec<SubmoduleConfig>, String> {
    if !root.join(".gitmodules").is_file() {
        return Ok(Vec::new());
    }
    let output = run_git(
        root,
        &[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.(path|url)$",
        ],
    )
    .map_err(|error| error.to_string())?;
    let mut entries = Vec::<SubmoduleConfig>::new();
    for line in output.stdout.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let Some(rest) = key.strip_prefix("submodule.") else {
            continue;
        };
        let Some((name, property)) = rest.rsplit_once('.') else {
            continue;
        };
        let position = entries.iter().position(|entry| entry.name == name);
        let index = position.unwrap_or_else(|| {
            entries.push(SubmoduleConfig {
                name: name.to_string(),
                ..Default::default()
            });
            entries.len() - 1
        });
        match property {
            "path" => entries[index].path = value.to_string(),
            "url" => entries[index].url = value.to_string(),
            _ => {}
        }
    }
    Ok(entries)
}

fn parse_submodule_status(
    text: &str,
    config: &[SubmoduleConfig],
    root: &Path,
) -> Vec<GitSubmodule> {
    let mut paths = config
        .iter()
        .filter(|entry| !entry.path.is_empty())
        .collect::<Vec<_>>();
    paths.sort_by_key(|entry| std::cmp::Reverse(entry.path.len()));

    let mut output = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let marker = line.chars().next().unwrap_or(' ');
        let state = match marker {
            '-' => GitSubmoduleState::Uninitialized,
            '+' => GitSubmoduleState::OutOfDate,
            'U' => GitSubmoduleState::Conflicted,
            _ => GitSubmoduleState::Initialized,
        };
        let rest = line
            .get(marker.len_utf8()..)
            .unwrap_or_default()
            .trim_start();
        let Some((declared_commit, after_commit)) = rest.split_once(' ') else {
            continue;
        };
        let entry = paths.iter().copied().find(|entry| {
            after_commit == entry.path
                || after_commit
                    .strip_prefix(entry.path.as_str())
                    .is_some_and(|suffix| suffix.starts_with(" ("))
        });
        let (relative_path, describe) = if let Some(entry) = entry {
            let suffix = after_commit
                .strip_prefix(entry.path.as_str())
                .unwrap_or_default()
                .trim();
            (
                entry.path.clone(),
                suffix
                    .strip_prefix('(')
                    .and_then(|value| value.strip_suffix(')'))
                    .map(str::to_string),
            )
        } else {
            let mut fields = after_commit.splitn(2, " (");
            let path = fields.next().unwrap_or_default().to_string();
            let describe = fields
                .next()
                .and_then(|value| value.strip_suffix(')'))
                .map(str::to_string);
            (path, describe)
        };
        let config_entry = config.iter().find(|entry| entry.path == relative_path);
        output.push(GitSubmodule {
            name: config_entry
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| relative_path.clone()),
            path: root.join(&relative_path),
            relative_path,
            url: config_entry
                .map(|entry| entry.url.clone())
                .unwrap_or_default(),
            declared_commit: declared_commit.to_string(),
            current_commit: None,
            branch: None,
            describe,
            state,
            has_uncommitted_changes: false,
        });
    }
    output
}

fn directory_start(path: &Path) -> &Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
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

    fn init_repo(root: &Path) {
        std::fs::create_dir_all(root).expect("create repo root");
        git_ok(root, &["init", "--quiet", "-b", "main"]);
        git_ok(root, &["config", "user.email", "kite@example.com"]);
        git_ok(root, &["config", "user.name", "Kite Test"]);
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kite-git-submodule-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn parses_submodule_status_and_handles_lifecycle() {
        if !git_available() {
            return;
        }
        let base = temp_root("lifecycle");
        let child = base.join("child");
        let parent = base.join("parent");
        init_repo(&child);
        std::fs::write(child.join("child.txt"), b"child\n").expect("write child");
        git_ok(&child, &["add", "child.txt"]);
        git_ok(&child, &["commit", "--quiet", "-m", "child initial"]);

        init_repo(&parent);
        std::fs::write(parent.join("parent.txt"), b"parent\n").expect("write parent");
        let child_url = child.to_string_lossy().into_owned();
        git_ok(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &child_url,
                "modules/child",
            ],
        );
        git_ok(&parent, &["commit", "--quiet", "-m", "add submodule"]);

        let list = git_submodules(&parent)
            .expect("list submodules")
            .expect("inside repository");
        assert_eq!(list.submodules.len(), 1);
        assert_eq!(list.submodules[0].state, GitSubmoduleState::Initialized);
        assert!(!list.submodules[0].has_uncommitted_changes);
        assert!(
            list.submodules[0]
                .path
                .ends_with(Path::new("modules").join("child"))
        );

        let submodule_file = list.submodules[0].path.join("child.txt");
        std::fs::write(&submodule_file, b"dirty\n").expect("dirty submodule");
        let list = git_submodules(&parent)
            .expect("refresh submodules")
            .expect("inside repository");
        assert!(list.submodules[0].has_uncommitted_changes);

        git_ok(
            &parent,
            &["submodule", "deinit", "--force", "modules/child"],
        );
        let list = git_submodules(&parent)
            .expect("list deinitialized submodule")
            .expect("inside repository");
        assert_eq!(list.submodules[0].state, GitSubmoduleState::Uninitialized);
        git_init_submodules(&parent).expect("initialize submodule config");
        git_update_submodules(&parent).expect("update submodule");
        let list = git_submodules(&parent)
            .expect("list updated submodule")
            .expect("inside repository");
        assert_ne!(list.submodules[0].state, GitSubmoduleState::Uninitialized);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn parses_paths_with_spaces_from_gitmodules() {
        let root = Path::new("/repo");
        let config = vec![SubmoduleConfig {
            name: "module".to_string(),
            path: "modules/with space".to_string(),
            url: "https://example.invalid/repo.git".to_string(),
        }];
        let parsed = parse_submodule_status(
            " 1111111111111111111111111111111111111111 modules/with space (v1.0)\n",
            &config,
            root,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].relative_path, "modules/with space");
        assert_eq!(parsed[0].describe.as_deref(), Some("v1.0"));
    }
}
