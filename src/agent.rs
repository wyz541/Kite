use std::path::{Path, PathBuf};

pub const MAX_AGENT_CONTEXT_PATHS: usize = 64;

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
}

impl AgentContext {
    pub fn new(
        primary_path: impl Into<PathBuf>,
        paths: Vec<AgentPath>,
        working_directory: Option<PathBuf>,
    ) -> Result<Self, String> {
        let primary_path = primary_path.into();
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

        let working_directory = working_directory
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| default_working_directory(&primary_path));

        Ok(Self {
            paths: normalized,
            primary_path,
            working_directory,
        })
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
            *self = context;
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
        AgentContext, AgentLaunchRequest, AgentMenuCommand, AgentPath, MAX_AGENT_CONTEXT_PATHS,
        default_working_directory, is_text_like_path,
    };
    use std::path::{Path, PathBuf};

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
}
