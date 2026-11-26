use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::{AgentProfile, WorkingDirectoryPolicy};

pub const MAX_AGENT_CONTEXT_PATHS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkingDirectoryStatus {
    Ready,
    NeedsSelection,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentWorkingDirectoryResolution {
    pub policy: WorkingDirectoryPolicy,
    pub status: AgentWorkingDirectoryStatus,
    pub path: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentContextMode {
    Fixed,
    FollowSelection,
}

impl AgentContextMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fixed => "固定上下文",
            Self::FollowSelection => "跟随当前选择",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSessionPhase {
    Empty,
    Unconfigured,
    Loading,
    Running,
    Error,
    Ended,
}

impl AgentSessionPhase {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "等待上下文",
            Self::Unconfigured => "未配置",
            Self::Loading => "正在连接",
            Self::Running => "运行中",
            Self::Error => "连接失败",
            Self::Ended => "已结束",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMenuCommand {
    Ask,
    ExplainFile,
    SummarizeFolder,
    ReviewChanges,
    GenerateCommitMessage,
    Configure,
}

impl AgentMenuCommand {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ask => "询问 Agent…",
            Self::ExplainFile => "解释此文件…",
            Self::SummarizeFolder => "总结此文件夹…",
            Self::ReviewChanges => "审查选中变更…",
            Self::GenerateCommitMessage => "生成提交说明…",
            Self::Configure => "配置 Agent…",
        }
    }

    pub const fn is_action(self) -> bool {
        !matches!(self, Self::Configure)
    }

    pub fn is_available_for(self, is_dir: bool, is_text_like: bool, git_available: bool) -> bool {
        match self {
            Self::Ask | Self::Configure => true,
            Self::ExplainFile => !is_dir && is_text_like,
            Self::SummarizeFolder => is_dir,
            Self::ReviewChanges | Self::GenerateCommitMessage => git_available,
        }
    }

    pub const fn prompt(self) -> &'static str {
        match self {
            Self::Ask => "",
            Self::ExplainFile => "请解释这个文件的作用、关键结构和需要注意的风险。不要修改文件。",
            Self::SummarizeFolder => "请总结这个文件夹的内容、结构和值得注意的事项。不要修改文件。",
            Self::ReviewChanges => {
                "请审查当前 Git 工作区中的变更，指出问题、风险和需要补充的测试。不要修改文件。"
            }
            Self::GenerateCommitMessage => {
                "请根据当前变更生成一条简洁、准确的提交说明。只生成准备内容，不要修改文件。"
            }
            Self::Configure => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPath {
    pub path: PathBuf,
    pub is_dir: bool,
}

impl AgentPath {
    pub fn new(path: impl Into<PathBuf>, is_dir: bool) -> Self {
        Self {
            path: path.into(),
            is_dir,
        }
    }

    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentContext {
    pub paths: Vec<AgentPath>,
    pub primary_path: PathBuf,
    pub working_directory: PathBuf,
    pub working_directory_status: AgentWorkingDirectoryStatus,
    pub working_directory_message: Option<String>,
}

impl AgentContext {
    pub fn new(
        primary_path: impl Into<PathBuf>,
        paths: Vec<AgentPath>,
        working_directory: Option<PathBuf>,
    ) -> Result<Self, String> {
        let (primary_path, paths) = normalize_context_paths(primary_path.into(), paths)?;
        let working_directory = working_directory
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| default_working_directory(&primary_path));

        Ok(Self {
            paths,
            primary_path,
            working_directory,
            working_directory_status: AgentWorkingDirectoryStatus::Ready,
            working_directory_message: None,
        })
    }

    pub fn new_for_profile(
        primary_path: impl Into<PathBuf>,
        paths: Vec<AgentPath>,
        profile: &AgentProfile,
        selected_working_directory: Option<&Path>,
    ) -> Result<Self, String> {
        let (primary_path, paths) = normalize_context_paths(primary_path.into(), paths)?;
        let mut context = Self {
            paths,
            primary_path,
            working_directory: PathBuf::new(),
            working_directory_status: AgentWorkingDirectoryStatus::Invalid,
            working_directory_message: None,
        };
        context.apply_profile_policy(profile, selected_working_directory);
        Ok(context)
    }

    pub fn apply_profile_policy(
        &mut self,
        profile: &AgentProfile,
        selected_working_directory: Option<&Path>,
    ) {
        let resolution = resolve_profile_working_directory(
            profile,
            &self.primary_path,
            selected_working_directory,
        );
        self.working_directory = resolution
            .path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_default();
        self.working_directory_status = resolution.status;
        self.working_directory_message = resolution.message;
    }

    pub fn has_resolved_working_directory(&self) -> bool {
        self.working_directory_status == AgentWorkingDirectoryStatus::Ready
            && !self.working_directory.as_os_str().is_empty()
    }

    pub fn primary(&self) -> Option<&AgentPath> {
        self.paths
            .iter()
            .find(|path| same_agent_path(&path.path, &self.primary_path))
            .or_else(|| self.paths.first())
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths
            .iter()
            .any(|candidate| same_agent_path(&candidate.path, path))
    }

    pub fn append(&mut self, paths: impl IntoIterator<Item = AgentPath>) -> usize {
        let mut added = 0;
        for path in paths {
            if self.paths.len() >= MAX_AGENT_CONTEXT_PATHS || path.path.as_os_str().is_empty() {
                break;
            }
            if self.contains(&path.path) {
                continue;
            }
            self.paths.push(path);
            added += 1;
        }
        added
    }

    pub fn remove(&mut self, path: &Path) -> bool {
        let Some(index) = self
            .paths
            .iter()
            .position(|candidate| same_agent_path(&candidate.path, path))
        else {
            return false;
        };
        self.paths.remove(index);
        if same_agent_path(&self.primary_path, path) {
            if let Some(next_primary) = self.paths.first() {
                self.primary_path = next_primary.path.clone();
            } else {
                self.primary_path.clear();
            }
        }
        true
    }

    pub fn replace_paths(&mut self, paths: Vec<AgentPath>) {
        let primary = self.primary_path.clone();
        if let Ok(context) = Self::new(primary, paths, Some(self.working_directory.clone())) {
            self.paths = context.paths;
            self.primary_path = context.primary_path;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunchRequest {
    pub command: AgentMenuCommand,
    pub prompt: String,
    pub context: AgentContext,
}

impl AgentLaunchRequest {
    pub fn new(
        command: AgentMenuCommand,
        primary_path: impl Into<PathBuf>,
        paths: Vec<AgentPath>,
        working_directory: Option<PathBuf>,
    ) -> Result<Self, String> {
        if !command.is_action() {
            return Err("配置命令不会启动 Agent 任务".to_string());
        }
        let context = AgentContext::new(primary_path, paths, working_directory)?;
        Ok(Self {
            command,
            prompt: command.prompt().to_string(),
            context,
        })
    }

    pub fn new_for_profile(
        command: AgentMenuCommand,
        primary_path: impl Into<PathBuf>,
        paths: Vec<AgentPath>,
        profile: &AgentProfile,
    ) -> Result<Self, String> {
        if !command.is_action() {
            return Err("配置命令不会启动 Agent 任务".to_string());
        }
        let context = AgentContext::new_for_profile(primary_path, paths, profile, None)?;
        Ok(Self {
            command,
            prompt: command.prompt().to_string(),
            context,
        })
    }
}

pub fn default_working_directory(primary_path: &Path) -> PathBuf {
    if primary_path.is_dir() {
        return primary_path.to_path_buf();
    }
    primary_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(primary_path)
        .to_path_buf()
}

pub fn resolve_profile_working_directory(
    profile: &AgentProfile,
    primary_path: &Path,
    selected_working_directory: Option<&Path>,
) -> AgentWorkingDirectoryResolution {
    let policy = profile.working_directory.clone();
    let result = match &profile.working_directory {
        WorkingDirectoryPolicy::SelectedItem => selected_item_directory(primary_path),
        WorkingDirectoryPolicy::FileParent => file_parent_directory(primary_path),
        WorkingDirectoryPolicy::RepositoryRoot => repository_root_directory(primary_path),
        WorkingDirectoryPolicy::Fixed { path } => {
            validate_directory(Path::new(path)).map_err(|message| {
                format!("固定的工作目录不可用：{message}。请在 Agent 设置中更新目录。")
            })
        }
        WorkingDirectoryPolicy::Ask => match selected_working_directory {
            Some(path) => validate_directory(path)
                .map_err(|message| format!("所选工作目录不可用：{message}。")),
            None => {
                return AgentWorkingDirectoryResolution {
                    policy,
                    status: AgentWorkingDirectoryStatus::NeedsSelection,
                    path: None,
                    message: Some("该 Agent 每次启动前都需要选择工作目录。".to_string()),
                };
            }
        },
    };

    match result {
        Ok(path) => AgentWorkingDirectoryResolution {
            policy,
            status: AgentWorkingDirectoryStatus::Ready,
            path: Some(path.to_string_lossy().into_owned()),
            message: None,
        },
        Err(message) => AgentWorkingDirectoryResolution {
            policy,
            status: AgentWorkingDirectoryStatus::Invalid,
            path: None,
            message: Some(message),
        },
    }
}

pub fn resolve_working_directory_for_context(
    profile: &AgentProfile,
    context_paths: &[String],
    requested_working_directory: Option<&Path>,
) -> AgentWorkingDirectoryResolution {
    let primary = context_paths
        .first()
        .map(PathBuf::from)
        .or_else(|| requested_working_directory.map(Path::to_path_buf));
    let Some(primary) = primary else {
        return AgentWorkingDirectoryResolution {
            policy: profile.working_directory.clone(),
            status: AgentWorkingDirectoryStatus::Invalid,
            path: None,
            message: Some("Agent 上下文缺少主目标，无法解析工作目录。".to_string()),
        };
    };
    resolve_profile_working_directory(profile, &primary, requested_working_directory)
}

fn normalize_context_paths(
    primary_path: PathBuf,
    paths: Vec<AgentPath>,
) -> Result<(PathBuf, Vec<AgentPath>), String> {
    if primary_path.as_os_str().is_empty() {
        return Err("Agent 上下文缺少主目标".to_string());
    }

    let mut normalized = Vec::new();
    for path in paths {
        if path.path.as_os_str().is_empty() {
            continue;
        }
        if normalized
            .iter()
            .any(|existing: &AgentPath| same_agent_path(&existing.path, &path.path))
        {
            continue;
        }
        normalized.push(path);
        if normalized.len() == MAX_AGENT_CONTEXT_PATHS {
            break;
        }
    }
    if let Some(index) = normalized
        .iter()
        .position(|path| same_agent_path(&path.path, &primary_path))
    {
        let primary = normalized.remove(index);
        normalized.insert(0, primary);
    } else {
        normalized.insert(
            0,
            AgentPath {
                path: primary_path.clone(),
                is_dir: primary_path.is_dir(),
            },
        );
    }
    if normalized.is_empty() {
        return Err("Agent 上下文为空".to_string());
    }
    Ok((primary_path, normalized))
}

fn selected_item_directory(primary_path: &Path) -> Result<PathBuf, String> {
    let target = validate_existing_target(primary_path)?;
    if target.is_dir() {
        validate_directory(&target)
    } else {
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| "所选文件没有可用的父目录。".to_string())?;
        validate_directory(parent)
    }
}

fn file_parent_directory(primary_path: &Path) -> Result<PathBuf, String> {
    let target = validate_existing_target(primary_path)?;
    // A selected directory has no file parent. Use its containing directory,
    // falling back to the directory itself only at a filesystem root.
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(&target);
    validate_directory(parent)
}

fn repository_root_directory(primary_path: &Path) -> Result<PathBuf, String> {
    validate_existing_target(primary_path)?;
    let root = crate::git::git_repository_root(primary_path)
        .map_err(|error| format!("无法解析 Git 仓库根目录：{error}"))?
        .ok_or_else(|| {
            "所选目标不在 Git 仓库中。请选择仓库内路径或更改工作目录策略。".to_string()
        })?;
    validate_directory(&root)
}

fn validate_existing_target(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("所选项目路径为空。".to_string());
    }
    if !path.is_absolute() {
        return Err(format!("所选项目必须是绝对路径：{}。", path.display()));
    }
    dunce::canonicalize(path)
        .map_err(|error| format!("所选项目不存在或不可访问：{}（{error}）。", path.display()))
}

fn validate_directory(path: &Path) -> Result<PathBuf, String> {
    validate_directory_with(path, |directory| fs::read_dir(directory).map(|_| ()))
}

fn validate_directory_with(
    path: &Path,
    probe: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("工作目录路径为空".to_string());
    }
    if !path.is_absolute() {
        return Err(format!("工作目录必须是绝对路径：{}", path.display()));
    }
    let canonical = dunce::canonicalize(path).map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied => {
            format!("工作目录不可访问：{}", path.display())
        }
        _ => format!("工作目录不存在或不可访问：{}", path.display()),
    })?;
    if !canonical.is_dir() {
        return Err(format!("目标不是目录：{}", canonical.display()));
    }
    probe(&canonical).map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied => {
            format!("工作目录权限不足：{}", canonical.display())
        }
        _ => format!("工作目录不可访问：{}（{error}）", canonical.display()),
    })?;
    Ok(canonical)
}

pub fn is_text_like_path(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    extension.is_empty()
        || matches!(
            extension.as_str(),
            "txt"
                | "md"
                | "markdown"
                | "mdx"
                | "mk"
                | "log"
                | "json"
                | "jsonc"
                | "toml"
                | "yaml"
                | "yml"
                | "xml"
                | "ini"
                | "conf"
                | "cfg"
                | "properties"
                | "env"
                | "csv"
                | "tsv"
                | "rs"
                | "py"
                | "pyw"
                | "js"
                | "mjs"
                | "cjs"
                | "jsx"
                | "ts"
                | "tsx"
                | "html"
                | "htm"
                | "css"
                | "scss"
                | "less"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "cxx"
                | "hpp"
                | "hxx"
                | "cs"
                | "java"
                | "go"
                | "kt"
                | "kts"
                | "sh"
                | "bash"
                | "zsh"
                | "bat"
                | "cmd"
                | "ps1"
                | "psm1"
                | "psd1"
                | "sql"
                | "swift"
                | "lua"
                | "rb"
                | "php"
                | "proto"
                | "cmake"
                | "diff"
                | "patch"
        )
}

fn same_agent_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentContext, AgentLaunchRequest, AgentMenuCommand, AgentPath, AgentWorkingDirectoryStatus,
        MAX_AGENT_CONTEXT_PATHS, default_working_directory, is_text_like_path,
        resolve_profile_working_directory, validate_directory_with,
    };
    use crate::agent::{AgentProfile, WorkingDirectoryPolicy};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn file_context_uses_parent_as_working_directory() {
        let root = std::env::temp_dir().join("kite-agent-working-directory");
        let file = root.join("src").join("main.rs");
        assert_eq!(default_working_directory(&file), root.join("src"));
    }

    #[test]
    fn directory_context_uses_itself_as_working_directory() {
        let root = std::env::temp_dir().join(format!("kite-agent-context-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create context directory");
        assert_eq!(default_working_directory(&root), root);
        std::fs::remove_dir_all(root).expect("remove context directory");
    }

    #[test]
    fn context_preserves_primary_and_deduplicates_paths() {
        let context = AgentContext::new(
            r"C:\repo\src",
            vec![
                AgentPath::new(r"C:\repo\src\main.rs", false),
                AgentPath::new(r"C:\repo\src", true),
                AgentPath::new(r"C:\repo\src\main.rs", false),
            ],
            Some(PathBuf::from(r"C:\repo")),
        )
        .expect("build context");

        assert_eq!(context.primary_path, PathBuf::from(r"C:\repo\src"));
        assert_eq!(
            context.paths.first().map(|path| &path.path),
            Some(&context.primary_path)
        );
        assert_eq!(context.working_directory, PathBuf::from(r"C:\repo"));
        assert_eq!(context.paths.len(), 2);
    }

    #[test]
    fn context_limits_path_count() {
        let mut paths = vec![AgentPath::new(r"C:\repo\root", true)];
        paths.extend(
            (0..MAX_AGENT_CONTEXT_PATHS + 5)
                .map(|index| AgentPath::new(format!(r"C:\repo\root\file-{index}.txt"), false)),
        );
        let context = AgentContext::new(r"C:\repo\root", paths, None).expect("build context");
        assert_eq!(context.paths.len(), MAX_AGENT_CONTEXT_PATHS);
    }

    #[test]
    fn removing_primary_promotes_the_next_context_path() {
        let mut context = AgentContext::new(
            r"C:\repo\src",
            vec![
                AgentPath::new(r"C:\repo\src", true),
                AgentPath::new(r"C:\repo\src\main.rs", false),
            ],
            None,
        )
        .expect("build context");
        assert!(context.remove(Path::new(r"C:\repo\src")));
        assert_eq!(context.primary_path, PathBuf::from(r"C:\repo\src\main.rs"));
    }

    #[test]
    fn menu_actions_only_launch_supported_contexts() {
        assert!(AgentMenuCommand::ExplainFile.is_available_for(false, true, false));
        assert!(!AgentMenuCommand::ExplainFile.is_available_for(false, false, false));
        assert!(!AgentMenuCommand::ExplainFile.is_available_for(true, true, false));
        assert!(AgentMenuCommand::SummarizeFolder.is_available_for(true, false, false));
        assert!(!AgentMenuCommand::ReviewChanges.is_available_for(false, true, false));
        assert!(AgentMenuCommand::ReviewChanges.is_available_for(false, true, true));
        assert!(!AgentMenuCommand::Configure.is_action());
    }

    #[test]
    fn text_file_detection_keeps_binary_actions_out_of_explain() {
        assert!(is_text_like_path(Path::new("notes.md")));
        assert!(is_text_like_path(Path::new("main.rs")));
        assert!(is_text_like_path(Path::new("Makefile")));
        assert!(!is_text_like_path(Path::new("Kite.exe")));
        assert!(!is_text_like_path(Path::new("photo.png")));
    }

    #[test]
    fn launch_request_prefills_prompt_without_sending() {
        let request = AgentLaunchRequest::new(
            AgentMenuCommand::ExplainFile,
            r"C:\repo\src\main.rs",
            vec![AgentPath::new(r"C:\repo\src\main.rs", false)],
            Some(PathBuf::from(r"C:\repo\src")),
        )
        .expect("build launch request");

        assert!(request.prompt.contains("不要修改文件"));
        assert_eq!(
            request.context.working_directory,
            PathBuf::from(r"C:\repo\src")
        );
    }

    fn test_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "kite-agent-working-directory-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        directory
    }

    fn profile_with_policy(policy: WorkingDirectoryPolicy) -> AgentProfile {
        AgentProfile {
            working_directory: policy,
            ..AgentProfile::default()
        }
    }

    #[test]
    fn launch_request_for_profile_uses_the_shared_working_directory_policy() {
        let root = test_directory("launch-request-profile");
        let nested = root.join("src");
        let file = nested.join("main.rs");
        fs::create_dir_all(&nested).expect("create nested directory");
        fs::write(&file, "fn main() {}").expect("write test file");
        let profile = profile_with_policy(WorkingDirectoryPolicy::SelectedItem);

        let request = AgentLaunchRequest::new_for_profile(
            AgentMenuCommand::ExplainFile,
            &file,
            vec![AgentPath::new(&file, false)],
            &profile,
        )
        .expect("build profile launch request");

        assert_eq!(
            request.context.working_directory,
            dunce::canonicalize(&nested).expect("canonical nested directory")
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn selected_item_resolves_file_parent_and_directory_itself() {
        let root = test_directory("selected-item");
        let nested = root.join("src");
        let file = nested.join("main.rs");
        fs::create_dir_all(&nested).expect("create nested directory");
        fs::write(&file, "fn main() {}").expect("write test file");
        let profile = profile_with_policy(WorkingDirectoryPolicy::SelectedItem);

        let file_resolution = resolve_profile_working_directory(&profile, &file, None);
        let directory_resolution = resolve_profile_working_directory(&profile, &nested, None);

        assert_eq!(file_resolution.status, AgentWorkingDirectoryStatus::Ready);
        assert_eq!(
            file_resolution.path.as_deref(),
            Some(dunce::canonicalize(&nested).unwrap().to_str().unwrap())
        );
        assert_eq!(
            directory_resolution.path.as_deref(),
            Some(dunce::canonicalize(&nested).unwrap().to_str().unwrap())
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn file_parent_uses_file_parent_and_directory_parent_as_documented_fallback() {
        let root = test_directory("file-parent");
        let nested = root.join("src");
        let file = nested.join("main.rs");
        fs::create_dir_all(&nested).expect("create nested directory");
        fs::write(&file, "fn main() {}").expect("write test file");
        let profile = profile_with_policy(WorkingDirectoryPolicy::FileParent);

        let file_resolution = resolve_profile_working_directory(&profile, &file, None);
        let directory_resolution = resolve_profile_working_directory(&profile, &nested, None);

        assert_eq!(
            file_resolution.path.as_deref(),
            Some(dunce::canonicalize(&nested).unwrap().to_str().unwrap())
        );
        assert_eq!(
            directory_resolution.path.as_deref(),
            Some(dunce::canonicalize(&root).unwrap().to_str().unwrap())
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn repository_root_resolves_from_nested_path_and_rejects_non_repository() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = test_directory("repository-root");
        let nested = root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create nested directory");
        let initialized = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("run git init");
        assert!(initialized.success());
        let profile = profile_with_policy(WorkingDirectoryPolicy::RepositoryRoot);

        let resolution = resolve_profile_working_directory(&profile, &nested, None);
        assert_eq!(resolution.status, AgentWorkingDirectoryStatus::Ready);
        assert_eq!(
            resolution.path.as_deref(),
            Some(dunce::canonicalize(&root).unwrap().to_str().unwrap())
        );

        let outside = test_directory("not-repository");
        let outside_resolution = resolve_profile_working_directory(&profile, &outside, None);
        assert_eq!(
            outside_resolution.status,
            AgentWorkingDirectoryStatus::Invalid
        );
        assert!(
            outside_resolution
                .message
                .as_deref()
                .is_some_and(|message| message.contains("不在 Git 仓库"))
        );
        fs::remove_dir_all(root).expect("remove repository");
        fs::remove_dir_all(outside).expect("remove non-repository");
    }

    #[test]
    fn fixed_directory_requires_existing_absolute_accessible_directory() {
        let root = test_directory("fixed");
        let valid = profile_with_policy(WorkingDirectoryPolicy::Fixed {
            path: root.to_string_lossy().into_owned(),
        });
        let missing = profile_with_policy(WorkingDirectoryPolicy::Fixed {
            path: root.join("missing").to_string_lossy().into_owned(),
        });
        let relative = profile_with_policy(WorkingDirectoryPolicy::Fixed {
            path: "relative/path".to_string(),
        });

        assert_eq!(
            resolve_profile_working_directory(&valid, &root, None).status,
            AgentWorkingDirectoryStatus::Ready
        );
        assert_eq!(
            resolve_profile_working_directory(&missing, &root, None).status,
            AgentWorkingDirectoryStatus::Invalid
        );
        assert_eq!(
            resolve_profile_working_directory(&relative, &root, None).status,
            AgentWorkingDirectoryStatus::Invalid
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn ask_requires_explicit_selection_and_accepts_selected_directory() {
        let root = test_directory("ask");
        let profile = profile_with_policy(WorkingDirectoryPolicy::Ask);

        let missing = resolve_profile_working_directory(&profile, &root, None);
        let selected = resolve_profile_working_directory(&profile, &root, Some(&root));

        assert_eq!(missing.status, AgentWorkingDirectoryStatus::NeedsSelection);
        assert_eq!(missing.path, None);
        assert_eq!(selected.status, AgentWorkingDirectoryStatus::Ready);
        assert_eq!(
            selected.path.as_deref(),
            Some(dunce::canonicalize(&root).unwrap().to_str().unwrap())
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn inaccessible_directory_reports_permission_failure() {
        let root = test_directory("permission");
        let error = validate_directory_with(&root, |_| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .expect_err("permission failure should be rejected");
        assert!(error.contains("权限不足"));
        fs::remove_dir_all(root).expect("remove test directory");
    }
}
