use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::fs::{FileEntry, VcsStatus, worse_status};
use crate::git::{GitPathStatus, GitService, apply_git_statuses, git_statuses};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
struct VcsPathStatus {
    path: PathBuf,
    status: VcsStatus,
}

#[derive(Clone, Debug, Default)]
pub struct VcsSnapshot {
    git: Vec<GitPathStatus>,
    svn: Vec<VcsPathStatus>,
    git_available: bool,
    svn_available: bool,
}

impl VcsSnapshot {
    pub fn git_available(&self) -> bool {
        self.git_available
    }

    pub fn svn_available(&self) -> bool {
        self.svn_available
    }

    #[cfg(feature = "performance")]
    pub fn git_status_count(&self) -> usize {
        self.git.len()
    }

    #[cfg(feature = "performance")]
    pub fn svn_status_count(&self) -> usize {
        self.svn.len()
    }

    #[cfg(test)]
    pub(crate) fn git_entries(&self) -> impl Iterator<Item = (&Path, VcsStatus)> {
        self.git
            .iter()
            .map(|item| (item.path.as_path(), item.status))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcsTool {
    Git,
    Svn,
}

impl VcsTool {
    pub fn label(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::Svn => "SVN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VcsOperation {
    Add,
    Unstage,
    Commit(String),
    Pull,
    Push,
    Fetch,
    Revert,
    Clean,
    Update,
    Lock,
    Unlock,
    Cleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcsQuery {
    Diff,
    Log,
}

impl VcsQuery {
    pub fn label(self) -> &'static str {
        match self {
            Self::Diff => "差异",
            Self::Log => "日志",
        }
    }
}

pub fn load_vcs_snapshot(view_root: &Path) -> VcsSnapshot {
    let (git_available, git) = git_statuses(view_root);
    let (svn_available, svn) = svn_statuses(view_root);
    VcsSnapshot {
        git,
        svn,
        git_available,
        svn_available,
    }
}

pub fn apply_statuses(entries: &mut [FileEntry], view_root: &Path, snapshot: &VcsSnapshot) {
    apply_git_statuses(entries, view_root, &snapshot.git);

    let (exact, directories) = index_svn_statuses(&snapshot.svn, view_root);
    for entry in entries {
        let key = path_key(&entry.path);
        let aggregate = entry
            .is_dir()
            .then(|| directories.get(&key).copied())
            .flatten();
        entry.svn_status = worse_status(exact.get(&key).copied(), aggregate);
    }
}

fn index_svn_statuses(
    statuses: &[VcsPathStatus],
    view_root: &Path,
) -> (HashMap<String, VcsStatus>, HashMap<String, VcsStatus>) {
    let mut exact = HashMap::new();
    let mut directories = HashMap::new();

    for item in statuses {
        let Ok(relative) = item.path.strip_prefix(view_root) else {
            continue;
        };
        let mut components = relative.components();
        let Some(first) = components.next() else {
            continue;
        };
        if !matches!(first, Component::Normal(_)) {
            continue;
        }

        merge_status(&mut exact, path_key(&item.path), item.status);
        if components.next().is_some() {
            let directory = view_root.join(first.as_os_str());
            merge_status(&mut directories, path_key(&directory), item.status);
        }
    }

    (exact, directories)
}

fn merge_status(map: &mut HashMap<String, VcsStatus>, key: String, status: VcsStatus) {
    let merged = worse_status(map.get(&key).copied(), Some(status));
    if let Some(merged) = merged {
        map.insert(key, merged);
    }
}

fn svn_statuses(view_root: &Path) -> (bool, Vec<VcsPathStatus>) {
    if !has_svn_working_copy(view_root) {
        return (false, Vec::new());
    }
    let Some(output) = run_command(
        "svn",
        view_root,
        &["status", "--no-ignore", "--non-interactive"],
    ) else {
        return (false, Vec::new());
    };
    if !output.status.success() {
        return (false, Vec::new());
    }
    (true, parse_svn_status(&output.stdout, view_root))
}

fn has_svn_working_copy(path: &Path) -> bool {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    start
        .ancestors()
        .any(|ancestor| ancestor.join(".svn").is_dir())
}

fn parse_svn_status(output: &[u8], view_root: &Path) -> Vec<VcsPathStatus> {
    let mut statuses = Vec::new();
    let text = String::from_utf8_lossy(output);

    for line in text.lines() {
        if line.len() < 8 {
            continue;
        }
        let item_code = line.as_bytes()[0] as char;
        let property_code = line.as_bytes()[1] as char;
        let path = line[8..].trim();
        if path.is_empty() {
            continue;
        }
        if let Some(status) = svn_status_from_codes(item_code, property_code) {
            statuses.push(VcsPathStatus {
                path: resolve_path(view_root, path),
                status,
            });
        }
    }

    statuses
}

fn svn_status_from_codes(item_code: char, property_code: char) -> Option<VcsStatus> {
    match item_code {
        'M' => Some(VcsStatus::Modified),
        'A' => Some(VcsStatus::Added),
        'D' => Some(VcsStatus::Deleted),
        'R' => Some(VcsStatus::Renamed),
        'C' => Some(VcsStatus::Conflicted),
        '?' => Some(VcsStatus::Untracked),
        '!' => Some(VcsStatus::Missing),
        'I' => Some(VcsStatus::Ignored),
        'X' => Some(VcsStatus::External),
        '~' => Some(VcsStatus::Missing),
        ' ' if property_code == 'M' => Some(VcsStatus::PropertyModified),
        _ if property_code == 'M' => Some(VcsStatus::PropertyModified),
        _ => None,
    }
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        value.replace('/', "\\").to_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        value.into_owned()
    }
}

pub fn run_vcs_operation(
    tool: VcsTool,
    operation: VcsOperation,
    target: &Path,
) -> Result<String, String> {
    match tool {
        VcsTool::Git => GitService::shared()
            .run_legacy_operation(target, &operation)
            .map_err(|error| error.to_string()),
        VcsTool::Svn => run_svn_operation(operation, target),
    }
}

pub fn run_vcs_query(tool: VcsTool, query: VcsQuery, target: &Path) -> Result<String, String> {
    match tool {
        VcsTool::Git => GitService::shared()
            .run_legacy_query(target, query)
            .map_err(|error| error.to_string()),
        VcsTool::Svn => {
            let start = working_directory(target);
            let mut command = configured_command("svn", start);
            match query {
                VcsQuery::Diff => {
                    command.arg("diff").arg(target);
                    run_query_command("SVN 差异", command)
                }
                VcsQuery::Log => {
                    command.args(["log", "--limit", "100"]).arg(target);
                    run_query_command("SVN 日志", command)
                }
            }
        }
    }
}

pub fn open_vcs_log_window(tool: VcsTool, target: &Path) -> Result<String, String> {
    let target = dunce::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    match tool {
        VcsTool::Git => crate::git::open_log_window(&target),
        VcsTool::Svn => open_svn_log_window(&target),
    }
}

fn open_svn_log_window(target: &Path) -> Result<String, String> {
    if spawn_tortoise_log("TortoiseProc.exe", "TortoiseSVN", target) {
        Ok("SVN 日志窗口已打开".to_string())
    } else {
        Err("未找到 TortoiseProc.exe。请安装 TortoiseSVN。".to_string())
    }
}

fn spawn_tortoise_log(executable: &str, product_dir: &str, target: &Path) -> bool {
    gui_executable_candidates(executable, product_dir)
        .into_iter()
        .any(|candidate| {
            let mut command = Command::new(candidate);
            command
                .arg("/command:log")
                .arg(format!("/path:{}", target.display()));
            #[cfg(target_os = "windows")]
            command.creation_flags(CREATE_NO_WINDOW);
            command.spawn().is_ok()
        })
}

fn gui_executable_candidates(executable: &str, product_dir: &str) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(executable)];
    if let Some(base) = std::env::var_os("ProgramFiles") {
        candidates.push(
            PathBuf::from(base)
                .join(product_dir)
                .join("bin")
                .join(executable),
        );
    }
    if let Some(base) = std::env::var_os("ProgramFiles(x86)") {
        candidates.push(
            PathBuf::from(base)
                .join(product_dir)
                .join("bin")
                .join(executable),
        );
    }
    if let Some(base) = std::env::var_os("LocalAppData") {
        candidates.push(
            PathBuf::from(base)
                .join("Programs")
                .join(product_dir)
                .join("bin")
                .join(executable),
        );
    }
    candidates
}

fn run_svn_operation(operation: VcsOperation, target: &Path) -> Result<String, String> {
    if matches!(
        &operation,
        VcsOperation::Unstage
            | VcsOperation::Pull
            | VcsOperation::Push
            | VcsOperation::Fetch
            | VcsOperation::Clean
    ) {
        return Err("SVN 不支持此操作".to_string());
    }
    let start = working_directory(target);
    let mut command = configured_command("svn", start);

    let label = match operation {
        VcsOperation::Add => {
            command.args(["add", "--force"]).arg(target);
            "SVN 添加"
        }
        VcsOperation::Commit(message) => {
            command
                .args(["commit", "--non-interactive", "-m"])
                .arg(message)
                .arg(target);
            "SVN 提交"
        }
        VcsOperation::Revert => {
            command.args(["revert", "--depth=infinity"]).arg(target);
            "SVN 还原"
        }
        VcsOperation::Update => {
            command.args(["update", "--non-interactive"]).arg(target);
            "SVN 更新"
        }
        VcsOperation::Lock => {
            command.args(["lock", "--non-interactive"]).arg(target);
            "SVN 锁定"
        }
        VcsOperation::Unlock => {
            command.args(["unlock", "--non-interactive"]).arg(target);
            "SVN 解锁"
        }
        VcsOperation::Cleanup => {
            command.arg("cleanup").arg(target);
            "SVN 清理"
        }
        VcsOperation::Unstage
        | VcsOperation::Pull
        | VcsOperation::Push
        | VcsOperation::Fetch
        | VcsOperation::Clean => unreachable!(),
    };
    run_operation_command(label, command)
}

fn working_directory(target: &Path) -> &Path {
    if target.is_dir() {
        target
    } else {
        target.parent().unwrap_or(target)
    }
}

pub(crate) fn configured_command(program: &str, current_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(current_dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .stdin(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn run_query_command(label: &str, mut command: Command) -> Result<String, String> {
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("未找到 {label} 所需命令。请确认命令已安装并位于 PATH 中。")
        } else {
            format!("{label}读取失败：{error}")
        }
    })?;
    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            Ok("没有可显示内容。".to_string())
        } else {
            Ok(value)
        }
    } else {
        Err(output_error(label, &output))
    }
}

fn run_operation_command(label: &str, mut command: Command) -> Result<String, String> {
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("未找到 {}。请确认命令已安装并位于 PATH 中。", label)
        } else {
            format!("{label}失败：{error}")
        }
    })?;
    if output.status.success() {
        Ok(format!("{label}完成"))
    } else {
        Err(output_error(label, &output))
    }
}

fn output_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let detail = detail
        .replace(['\r', '\n'], " ")
        .chars()
        .take(500)
        .collect::<String>();
    if detail.is_empty() {
        format!(
            "{label}操作失败，退出码 {}",
            output.status.code().unwrap_or(-1)
        )
    } else {
        format!("{label}操作失败：{detail}")
    }
}

fn run_command(program: &str, current_dir: &Path, args: &[&str]) -> Option<Output> {
    let mut command = configured_command(program, current_dir);
    command.args(args);
    command.output().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::EntryKind;
    use std::time::SystemTime;

    fn entry_at(path: PathBuf, kind: EntryKind) -> FileEntry {
        FileEntry {
            path,
            name: String::new(),
            kind,
            size: 0,
            modified: None,
            extension: String::new(),
            git_status: None,
            submodule_status: None,
            svn_status: None,
        }
    }

    #[test]
    fn parses_svn_statuses_and_property_changes() {
        let root = Path::new("/repo");
        let output = b" M       file.txt\n?        new.txt\nC        conflict.txt\nA        added.txt\n M       prop.txt\n";
        let statuses = parse_svn_status(output, root);
        assert_eq!(statuses.len(), 5);
        assert_eq!(statuses[0].status, VcsStatus::PropertyModified);
        assert_eq!(statuses[1].status, VcsStatus::Untracked);
        assert_eq!(statuses[2].status, VcsStatus::Conflicted);
        assert_eq!(statuses[3].status, VcsStatus::Added);
        assert_eq!(statuses[4].status, VcsStatus::PropertyModified);
    }

    #[test]
    fn aggregates_nested_svn_changes_on_parent_directory() {
        let root = std::env::temp_dir().join("kite-vcs-aggregate");
        let child = root.join("src").join("a.rs");
        let snapshot = VcsSnapshot {
            git: Vec::new(),
            svn: vec![VcsPathStatus {
                path: child,
                status: VcsStatus::Modified,
            }],
            git_available: false,
            svn_available: true,
        };
        let mut entries = vec![
            entry_at(root.join("src"), EntryKind::Directory),
            entry_at(root.join("other"), EntryKind::Directory),
        ];
        apply_statuses(&mut entries, &root, &snapshot);
        assert_eq!(entries[0].svn_status, Some(VcsStatus::Modified));
        assert_eq!(entries[1].svn_status, None);
    }

    fn command_available(program: &str, args: &[&str]) -> bool {
        configured_command(program, Path::new("."))
            .args(args)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn command_ok(program: &str, root: &Path, args: &[&str]) {
        let status = configured_command(program, root)
            .args(args)
            .status()
            .expect("run command");
        assert!(status.success(), "{program} {args:?} failed");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kite-vcs-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn does_not_report_svn_for_a_regular_directory() {
        if !command_available("svn", &["--version", "--quiet"]) {
            return;
        }
        let root = unique_temp_dir("not-svn");
        std::fs::create_dir_all(&root).expect("create regular directory");
        let snapshot = load_vcs_snapshot(&root);
        assert!(!snapshot.svn_available());
        std::fs::remove_dir_all(root).expect("clean regular directory");
    }

    #[test]
    fn detects_svn_status_in_a_working_copy() {
        if !command_available("svn", &["--version", "--quiet"])
            || !command_available("svnadmin", &["--version", "--quiet"])
        {
            return;
        }
        let base = unique_temp_dir("svn");
        let repository = base.join("repository");
        let working_copy = base.join("working-copy");
        std::fs::create_dir_all(&base).expect("create svn test base");
        command_ok(
            "svnadmin",
            &base,
            &["create", repository.to_string_lossy().as_ref()],
        );
        let url = format!(
            "file:///{}",
            repository.to_string_lossy().replace('\\', "/")
        );
        command_ok(
            "svn",
            &base,
            &[
                "checkout",
                "--quiet",
                &url,
                working_copy.to_string_lossy().as_ref(),
            ],
        );
        std::fs::write(working_copy.join("tracked.txt"), b"before").expect("write svn file");
        std::fs::create_dir_all(working_copy.join("src")).expect("create svn subdirectory");
        std::fs::write(working_copy.join("src").join("nested.txt"), b"before")
            .expect("write svn nested file");
        command_ok("svn", &working_copy, &["add", "--quiet", "tracked.txt"]);
        command_ok(
            "svn",
            &working_copy,
            &["add", "--parents", "--quiet", "src/nested.txt"],
        );
        command_ok(
            "svn",
            &working_copy,
            &["commit", "--quiet", "-m", "initial"],
        );
        std::fs::write(working_copy.join("tracked.txt"), b"after").expect("modify svn file");
        std::fs::write(working_copy.join("src").join("nested.txt"), b"after")
            .expect("modify svn nested file");
        std::fs::write(working_copy.join("new.txt"), b"new").expect("write svn untracked file");

        let diff = run_vcs_query(VcsTool::Svn, VcsQuery::Diff, &working_copy).expect("svn diff");
        assert!(diff.contains("+after"));
        run_vcs_query(VcsTool::Svn, VcsQuery::Log, &working_copy).expect("svn log");

        let nested_root = working_copy.join("src");
        let nested_snapshot = load_vcs_snapshot(&nested_root);
        let expected_nested = nested_root.join("nested.txt");
        assert!(nested_snapshot.svn.iter().any(|item| {
            path_key(&item.path) == path_key(&expected_nested) && item.status == VcsStatus::Modified
        }));

        let snapshot = load_vcs_snapshot(&working_copy);
        assert!(snapshot.svn.iter().any(|item| {
            item.path.ends_with("tracked.txt") && item.status == VcsStatus::Modified
        }));
        assert!(
            snapshot.svn.iter().any(|item| {
                item.path.ends_with("new.txt") && item.status == VcsStatus::Untracked
            })
        );
        let new_file = working_copy.join("new.txt");
        assert!(
            run_vcs_operation(VcsTool::Svn, VcsOperation::Add, &new_file)
                .expect("svn add")
                .contains("完成")
        );
        assert!(
            run_vcs_operation(VcsTool::Svn, VcsOperation::Revert, &new_file)
                .expect("svn revert")
                .contains("完成")
        );
        assert!(
            run_vcs_operation(VcsTool::Svn, VcsOperation::Cleanup, &working_copy)
                .expect("svn cleanup")
                .contains("完成")
        );
        std::fs::remove_dir_all(base).expect("clean svn test base");
    }

    #[test]
    fn detects_git_status_in_a_working_tree() {
        if !command_available("git", &["--version"]) {
            return;
        }
        let root = unique_temp_dir("git");
        std::fs::create_dir_all(&root).expect("create git test root");
        command_ok("git", &root, &["init", "--quiet"]);
        command_ok("git", &root, &["config", "user.name", "Kite Test"]);
        command_ok("git", &root, &["config", "user.email", "kite@example.test"]);
        std::fs::write(root.join("tracked.txt"), "initial\n").expect("write tracked file");
        std::fs::create_dir_all(root.join("src")).expect("create git subdirectory");
        std::fs::write(root.join("src/nested.txt"), "initial\n").expect("write nested file");
        command_ok("git", &root, &["add", "tracked.txt", "src/nested.txt"]);
        command_ok("git", &root, &["commit", "--quiet", "-m", "initial"]);

        std::fs::write(root.join("tracked.txt"), "changed\n").expect("modify tracked file");
        std::fs::write(root.join("new.txt"), "fresh\n").expect("write new file");
        let snapshot = load_vcs_snapshot(&root);
        assert!(snapshot.git_available);
        assert!(snapshot.git.iter().any(|item| {
            item.path.ends_with("tracked.txt") && item.status == VcsStatus::Modified
        }));
        assert!(
            snapshot.git.iter().any(|item| {
                item.path.ends_with("new.txt") && item.status == VcsStatus::Untracked
            })
        );

        std::fs::remove_dir_all(root).expect("clean git test root");
    }

    #[test]
    fn runs_git_add_commit_and_revert_operations() {
        if !command_available("git", &["--version"]) {
            return;
        }
        let root = unique_temp_dir("git-operations");
        std::fs::create_dir_all(&root).expect("create git operations root");
        command_ok("git", &root, &["init", "--quiet"]);
        command_ok("git", &root, &["config", "user.name", "Kite Test"]);
        command_ok("git", &root, &["config", "user.email", "kite@example.test"]);
        let file = root.join("a.txt");
        std::fs::write(&file, "one\n").expect("write file");

        run_vcs_operation(VcsTool::Git, VcsOperation::Add, &file).expect("git add");
        run_vcs_operation(
            VcsTool::Git,
            VcsOperation::Commit("initial".to_string()),
            &file,
        )
        .expect("git commit");
        std::fs::write(&file, "two\n").expect("edit file");
        let diff = run_vcs_query(VcsTool::Git, VcsQuery::Diff, &file).expect("git diff");
        assert!(diff.contains("-one"));
        assert!(diff.contains("+two"));
        run_vcs_operation(VcsTool::Git, VcsOperation::Revert, &file).expect("git revert");

        std::fs::remove_dir_all(root).expect("clean git operations root");
    }
}
