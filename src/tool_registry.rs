//! Cross-platform tool registry and launch recipes for `open.in` / `compare.with`.
//!
//! Platform adapters discover the stable tool identities defined here. This
//! module owns capability matching, policy ordering, stale filtering, and typed
//! argv expansion. It never builds a shell command line.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::context_menu_model::{ContextPlatform, ContextTarget, TargetKind, TargetSet};

pub const MAX_RECENT_TOOLS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolPlatform {
    Windows,
    MacOs,
    Any,
}

impl ToolPlatform {
    pub fn parse(value: &str) -> Self {
        match value {
            "windows" => Self::Windows,
            "macos" | "mac" => Self::MacOs,
            _ => Self::Any,
        }
    }

    pub const fn from_context(platform: ContextPlatform) -> Self {
        match platform {
            ContextPlatform::Windows => Self::Windows,
            ContextPlatform::MacOs => Self::MacOs,
            ContextPlatform::Other => Self::Any,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Any => "any",
        }
    }

    pub fn matches(self, platform: ContextPlatform) -> bool {
        self == Self::Any || self == Self::from_context(platform)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolCategory {
    KiteInternal,
    Terminal,
    EditorIde,
    DiffMerge,
    Vcs,
}

impl ToolCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KiteInternal => "kite_internal",
            Self::Terminal => "terminal",
            Self::EditorIde => "editor_ide",
            Self::DiffMerge => "diff_merge",
            Self::Vcs => "vcs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolTargetShape {
    File,
    Directory,
    MultiPath,
    Repository,
    Workspace,
}

impl ToolTargetShape {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::MultiPath => "multi",
            Self::Repository => "repository",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolRole {
    Open,
    Compare,
    Merge,
    Execute,
}

impl ToolRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Compare => "compare",
            Self::Merge => "merge",
            Self::Execute => "execute",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolLaunchKind {
    Process,
    WorkspaceApplication,
    KiteInternal,
}

impl ToolLaunchKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::WorkspaceApplication => "workspace_application",
            Self::KiteInternal => "kite_internal",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolPathFilter {
    #[default]
    Any,
    TextFile,
    MindmapFile,
}

impl ToolPathFilter {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::TextFile => "text_file",
            Self::MindmapFile => "mindmap_file",
        }
    }

    fn matches(self, target: &ContextTarget) -> bool {
        match self {
            Self::Any => true,
            Self::TextFile => {
                target.kind.is_file_like()
                    && crate::agent::is_text_like_path(Path::new(&target.path))
            }
            Self::MindmapFile => {
                target.kind == TargetKind::File
                    && Path::new(&target.path)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("xmind"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolAvailability {
    Available,
    NotInstalled,
    Invalidated,
    Unsupported,
    Denied,
    #[default]
    Unknown,
}

impl ToolAvailability {
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NotInstalled => "not_installed",
            Self::Invalidated => "invalidated",
            Self::Unsupported => "unsupported",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolCatalogStatus {
    Ready,
    Loading,
    #[default]
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredTool {
    pub tool_id: String,
    pub status: ToolAvailability,
    /// Resolved executable path or application bundle path. This is a
    /// discovery result and must be revalidated before launch when it can stale.
    pub executable_or_bundle: Option<String>,
    pub version: Option<String>,
}

impl DiscoveredTool {
    pub fn available(tool_id: impl Into<String>, executable_or_bundle: impl Into<String>) -> Self {
        Self {
            tool_id: tool_id.into(),
            status: ToolAvailability::Available,
            executable_or_bundle: Some(executable_or_bundle.into()),
            version: None,
        }
    }

    pub fn unavailable(tool_id: impl Into<String>, status: ToolAvailability) -> Self {
        Self {
            tool_id: tool_id.into(),
            status,
            executable_or_bundle: None,
            version: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCatalog {
    pub status: ToolCatalogStatus,
    pub tools: Vec<DiscoveredTool>,
}

impl ToolCatalog {
    pub fn ready(tools: Vec<DiscoveredTool>) -> Self {
        Self {
            status: ToolCatalogStatus::Ready,
            tools,
        }
    }

    fn available_tool(&self, tool_id: &str) -> Option<&DiscoveredTool> {
        self.tools
            .iter()
            .find(|tool| tool.tool_id.eq_ignore_ascii_case(tool_id) && tool.status.is_available())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPolicy {
    pub pinned_tool_ids: Vec<String>,
    pub hidden_tool_ids: Vec<String>,
    pub user_order: Vec<String>,
    pub recent_tool_ids: Vec<String>,
}

impl ToolPolicy {
    pub fn is_hidden(&self, tool_id: &str) -> bool {
        contains_case_insensitive(&self.hidden_tool_ids, tool_id)
    }

    pub fn is_pinned(&self, tool_id: &str) -> bool {
        contains_case_insensitive(&self.pinned_tool_ids, tool_id)
    }

    pub fn set_pinned(&mut self, tool_id: &str, pinned: bool) {
        self.pinned_tool_ids
            .retain(|id| !id.eq_ignore_ascii_case(tool_id));
        if pinned {
            self.pinned_tool_ids.push(tool_id.to_string());
        }
    }

    pub fn set_hidden(&mut self, tool_id: &str, hidden: bool) {
        self.hidden_tool_ids
            .retain(|id| !id.eq_ignore_ascii_case(tool_id));
        if hidden {
            self.hidden_tool_ids.push(tool_id.to_string());
        }
    }

    pub fn record_recent(&mut self, tool_id: &str) {
        let tool_id = tool_id.trim();
        if tool_id.is_empty() {
            return;
        }
        self.recent_tool_ids
            .retain(|id| !id.eq_ignore_ascii_case(tool_id));
        self.recent_tool_ids.insert(0, tool_id.to_string());
        self.recent_tool_ids.truncate(MAX_RECENT_TOOLS);
    }

    fn user_order_index(&self, tool_id: &str) -> Option<usize> {
        self.user_order
            .iter()
            .position(|id| id.eq_ignore_ascii_case(tool_id))
    }

    fn pinned_index(&self, tool_id: &str) -> Option<usize> {
        self.pinned_tool_ids
            .iter()
            .position(|id| id.eq_ignore_ascii_case(tool_id))
    }

    fn recent_index(&self, tool_id: &str) -> Option<usize> {
        self.recent_tool_ids
            .iter()
            .position(|id| id.eq_ignore_ascii_case(tool_id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolArgument {
    Literal(String),
    PrimaryPath,
    TargetPaths,
    WorkingDirectory,
    RepositoryRoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolWorkingDirectory {
    Context,
    PrimaryTarget,
    RepositoryRoot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRecipe {
    pub role: ToolRole,
    pub arguments: Vec<ToolArgument>,
    pub working_directory: Option<ToolWorkingDirectory>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCapability {
    pub target_shapes: Vec<ToolTargetShape>,
    pub roles: Vec<ToolRole>,
    pub min_paths: usize,
    /// `None` means no upper bound.
    pub max_paths: Option<usize>,
    pub all_targets_required: bool,
    pub path_filter: ToolPathFilter,
}

impl ToolCapability {
    pub fn matches(&self, targets: &TargetSet, role: ToolRole) -> bool {
        if !self.roles.contains(&role) {
            return false;
        }
        let count = targets.targets.len();
        if count < self.min_paths {
            return false;
        }
        if self.max_paths.is_some_and(|max| count > max) {
            return false;
        }
        if count > 1 && !self.target_shapes.contains(&ToolTargetShape::MultiPath) {
            return false;
        }
        let candidates = if self.all_targets_required {
            targets.targets.as_slice()
        } else {
            targets
                .primary_target()
                .map(std::slice::from_ref)
                .unwrap_or_default()
        };
        if candidates.is_empty() {
            return false;
        }
        candidates.iter().all(|target| {
            self.path_filter.matches(target)
                && target_shape(target).is_some_and(|shape| {
                    self.target_shapes.contains(&shape)
                        || (shape == ToolTargetShape::Directory
                            && (self.target_shapes.contains(&ToolTargetShape::Repository)
                                || self.target_shapes.contains(&ToolTargetShape::Workspace)))
                })
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDefinition {
    pub tool_id: String,
    pub category: ToolCategory,
    pub display_name: String,
    pub description: String,
    pub platform: ToolPlatform,
    pub launch_kind: ToolLaunchKind,
    pub executable_identity: Option<String>,
    pub bundle_identity: Option<String>,
    pub capabilities: Vec<ToolCapability>,
    pub recipes: Vec<ToolRecipe>,
}

impl ToolDefinition {
    pub fn supports(&self, targets: &TargetSet, role: ToolRole) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability.matches(targets, role))
    }

    pub fn recipe(&self, role: ToolRole) -> Option<&ToolRecipe> {
        self.recipes.iter().find(|recipe| recipe.role == role)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInvocation {
    pub tool_id: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
}

pub fn target_shape(target: &ContextTarget) -> Option<ToolTargetShape> {
    match target.kind {
        TargetKind::File | TargetKind::Archive => Some(ToolTargetShape::File),
        TargetKind::Directory => Some(ToolTargetShape::Directory),
        TargetKind::RepositoryRoot => Some(ToolTargetShape::Repository),
        TargetKind::WorkspaceRoot => Some(ToolTargetShape::Workspace),
        TargetKind::DeviceContainer | TargetKind::DeviceItem | TargetKind::VirtualItem => None,
    }
}

fn contains_case_insensitive(values: &[String], value: &str) -> bool {
    values
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(value))
}

/// Returns available, visible, capability-matching tools in menu order.
///
/// Pinned tools come first, followed by MRU tools, then the remaining tools in
/// the user's explicit order. MRU never changes root action order; the Composer
/// uses this order only inside an `open.in` or `compare.with` submenu.
pub fn available_tools(
    platform: ContextPlatform,
    definitions: &[ToolDefinition],
    catalog: &ToolCatalog,
    policy: &ToolPolicy,
    targets: &TargetSet,
    role: ToolRole,
) -> Vec<ToolDefinition> {
    if catalog.status != ToolCatalogStatus::Ready {
        return Vec::new();
    }
    let mut tools = definitions
        .iter()
        .filter(|definition| definition.platform.matches(platform))
        .filter(|definition| definition.supports(targets, role))
        .filter(|definition| !policy.is_hidden(&definition.tool_id))
        .filter(|definition| catalog.available_tool(&definition.tool_id).is_some())
        .cloned()
        .collect::<Vec<_>>();

    tools.sort_by(|left, right| {
        let left_pinned = policy.pinned_index(&left.tool_id);
        let right_pinned = policy.pinned_index(&right.tool_id);
        match (left_pinned, right_pinned) {
            (Some(left_index), Some(right_index)) => {
                let left_order = policy.user_order_index(&left.tool_id);
                let right_order = policy.user_order_index(&right.tool_id);
                left_order
                    .cmp(&right_order)
                    .then_with(|| left_index.cmp(&right_index))
                    .then_with(|| left.display_name.cmp(&right.display_name))
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => {
                let left_recent = policy.recent_index(&left.tool_id);
                let right_recent = policy.recent_index(&right.tool_id);
                match (left_recent, right_recent) {
                    (Some(left_index), Some(right_index)) => left_index.cmp(&right_index),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => {
                        let left_order = policy.user_order_index(&left.tool_id);
                        let right_order = policy.user_order_index(&right.tool_id);
                        left_order
                            .cmp(&right_order)
                            .then_with(|| left.display_name.cmp(&right.display_name))
                    }
                }
            }
        }
    });
    tools
}

pub fn discovered_identity<'a>(
    catalog: &'a ToolCatalog,
    tool_id: &str,
) -> Option<&'a DiscoveredTool> {
    catalog.available_tool(tool_id)
}

pub fn stale_or_unavailable_ids(catalog: &ToolCatalog) -> Vec<&str> {
    catalog
        .tools
        .iter()
        .filter(|tool| !tool.status.is_available())
        .map(|tool| tool.tool_id.as_str())
        .collect()
}

pub fn unexpected_discovery_ids<'a>(
    definitions: &[ToolDefinition],
    catalog: &'a ToolCatalog,
) -> Vec<&'a str> {
    let known = definitions
        .iter()
        .map(|definition| definition.tool_id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    catalog
        .tools
        .iter()
        .filter(|tool| !known.contains(&tool.tool_id.to_ascii_lowercase()))
        .map(|tool| tool.tool_id.as_str())
        .collect()
}

pub fn build_tool_invocation(
    definition: &ToolDefinition,
    role: ToolRole,
    targets: &TargetSet,
    repository_root: Option<&str>,
) -> Result<ToolInvocation, String> {
    if !definition.supports(targets, role) {
        return Err(format!(
            "工具 {} 不支持当前目标或角色 {}",
            definition.tool_id,
            role.as_str()
        ));
    }
    let recipe = definition.recipe(role).ok_or_else(|| {
        format!(
            "工具 {} 缺少 {} 启动配方",
            definition.tool_id,
            role.as_str()
        )
    })?;
    let executable = definition
        .executable_identity
        .as_deref()
        .or(definition.bundle_identity.as_deref())
        .ok_or_else(|| format!("工具 {} 缺少可执行文件或应用标识", definition.tool_id))?;
    let mut arguments = Vec::new();
    for argument in &recipe.arguments {
        match argument {
            ToolArgument::Literal(value) => arguments.push(value.clone()),
            ToolArgument::PrimaryPath => arguments.push(
                targets
                    .ordered_paths
                    .first()
                    .cloned()
                    .ok_or_else(|| "工具启动缺少主目标".to_string())?,
            ),
            ToolArgument::TargetPaths => arguments.extend(targets.ordered_paths.iter().cloned()),
            ToolArgument::WorkingDirectory => {
                arguments.push(targets.working_directory.clone());
            }
            ToolArgument::RepositoryRoot => arguments.push(
                repository_root
                    .map(str::to_string)
                    .unwrap_or_else(|| targets.working_directory.clone()),
            ),
        }
    }
    let working_directory = match recipe.working_directory {
        Some(ToolWorkingDirectory::Context) => Some(targets.working_directory.clone()),
        Some(ToolWorkingDirectory::PrimaryTarget) => targets.primary_target().map(|target| {
            if target.kind == TargetKind::Directory
                || target.kind == TargetKind::RepositoryRoot
                || target.kind == TargetKind::WorkspaceRoot
            {
                target.path.clone()
            } else {
                targets.working_directory.clone()
            }
        }),
        Some(ToolWorkingDirectory::RepositoryRoot) => repository_root
            .map(str::to_string)
            .or_else(|| Some(targets.working_directory.clone())),
        None => None,
    };
    Ok(ToolInvocation {
        tool_id: definition.tool_id.clone(),
        executable: executable.to_string(),
        arguments,
        working_directory,
    })
}

fn capability(
    shapes: &[ToolTargetShape],
    roles: &[ToolRole],
    min_paths: usize,
    max_paths: Option<usize>,
) -> ToolCapability {
    ToolCapability {
        target_shapes: shapes.to_vec(),
        roles: roles.to_vec(),
        min_paths,
        max_paths,
        all_targets_required: true,
        path_filter: ToolPathFilter::Any,
    }
}

fn recipe(role: ToolRole, arguments: &[ToolArgument]) -> ToolRecipe {
    ToolRecipe {
        role,
        arguments: arguments.to_vec(),
        working_directory: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn definition(
    tool_id: &str,
    category: ToolCategory,
    display_name: &str,
    description: &str,
    platform: ToolPlatform,
    launch_kind: ToolLaunchKind,
    executable_identity: Option<&str>,
    bundle_identity: Option<&str>,
    capabilities: Vec<ToolCapability>,
    recipes: Vec<ToolRecipe>,
) -> ToolDefinition {
    ToolDefinition {
        tool_id: tool_id.to_string(),
        category,
        display_name: display_name.to_string(),
        description: description.to_string(),
        platform,
        launch_kind,
        executable_identity: executable_identity.map(str::to_string),
        bundle_identity: bundle_identity.map(str::to_string),
        capabilities,
        recipes,
    }
}

/// Stable built-in identities. Discovery adapters decide which entries are
/// installed; this list remains deterministic across menu builds and machines.
pub fn builtin_tool_definitions(platform: ToolPlatform) -> Vec<ToolDefinition> {
    let any = ToolPlatform::Any;
    let macos = ToolPlatform::MacOs;
    let windows = ToolPlatform::Windows;
    let open_file = capability(
        &[ToolTargetShape::File, ToolTargetShape::MultiPath],
        &[ToolRole::Open],
        1,
        None,
    );
    let open_paths = capability(
        &[
            ToolTargetShape::File,
            ToolTargetShape::Directory,
            ToolTargetShape::MultiPath,
            ToolTargetShape::Repository,
            ToolTargetShape::Workspace,
        ],
        &[ToolRole::Open],
        1,
        None,
    );
    let compare_files = capability(
        &[ToolTargetShape::File, ToolTargetShape::MultiPath],
        &[ToolRole::Compare, ToolRole::Merge],
        2,
        Some(2),
    );
    let open_repository = capability(
        &[
            ToolTargetShape::Directory,
            ToolTargetShape::Repository,
            ToolTargetShape::Workspace,
        ],
        &[ToolRole::Open],
        1,
        Some(1),
    );

    let text_file = ToolCapability {
        path_filter: ToolPathFilter::TextFile,
        ..open_file.clone()
    };
    let mindmap_file = ToolCapability {
        path_filter: ToolPathFilter::MindmapFile,
        ..open_file.clone()
    };

    let mut tools = vec![
        definition(
            "kite.text-editor",
            ToolCategory::KiteInternal,
            "Kite 编辑器",
            "在 Kite 的编辑器中打开文本文件。",
            any,
            ToolLaunchKind::KiteInternal,
            Some("kite-internal:text-editor"),
            None,
            vec![text_file],
            vec![recipe(ToolRole::Open, &[ToolArgument::PrimaryPath])],
        ),
        definition(
            "kite.git-client",
            ToolCategory::KiteInternal,
            "Kite Git 客户端",
            "在 Kite Git 客户端中打开仓库。",
            any,
            ToolLaunchKind::KiteInternal,
            Some("kite-internal:git-client"),
            None,
            vec![open_repository],
            vec![recipe(ToolRole::Open, &[ToolArgument::PrimaryPath])],
        ),
        definition(
            "terminal.apple",
            ToolCategory::Terminal,
            "Terminal",
            "在系统终端中打开目录或工作区。",
            macos,
            ToolLaunchKind::WorkspaceApplication,
            None,
            Some("com.apple.Terminal"),
            vec![capability(
                &[
                    ToolTargetShape::Directory,
                    ToolTargetShape::Repository,
                    ToolTargetShape::Workspace,
                ],
                &[ToolRole::Open, ToolRole::Execute],
                1,
                Some(1),
            )],
            vec![recipe(ToolRole::Open, &[ToolArgument::PrimaryPath])],
        ),
        definition(
            "terminal.iterm2",
            ToolCategory::Terminal,
            "iTerm2",
            "在 iTerm2 中打开目录或工作区。",
            macos,
            ToolLaunchKind::WorkspaceApplication,
            None,
            Some("com.googlecode.iterm2"),
            vec![capability(
                &[
                    ToolTargetShape::Directory,
                    ToolTargetShape::Repository,
                    ToolTargetShape::Workspace,
                ],
                &[ToolRole::Open, ToolRole::Execute],
                1,
                Some(1),
            )],
            vec![recipe(ToolRole::Open, &[ToolArgument::PrimaryPath])],
        ),
        definition(
            "terminal.windows-terminal",
            ToolCategory::Terminal,
            "Windows Terminal",
            "在 Windows Terminal 中打开目录。",
            windows,
            ToolLaunchKind::Process,
            Some("wt.exe"),
            None,
            vec![capability(
                &[
                    ToolTargetShape::Directory,
                    ToolTargetShape::Repository,
                    ToolTargetShape::Workspace,
                ],
                &[ToolRole::Open, ToolRole::Execute],
                1,
                Some(1),
            )],
            vec![ToolRecipe {
                role: ToolRole::Open,
                arguments: vec![
                    ToolArgument::Literal("-d".to_string()),
                    ToolArgument::PrimaryPath,
                ],
                working_directory: Some(ToolWorkingDirectory::PrimaryTarget),
            }],
        ),
        definition(
            "editor.vscode",
            ToolCategory::EditorIde,
            "Visual Studio Code",
            "在 VS Code 中打开文件、目录、工作区或比较两个文件。",
            any,
            ToolLaunchKind::Process,
            Some(if platform == windows {
                "Code.exe"
            } else {
                "code"
            }),
            Some("com.microsoft.VSCode"),
            vec![open_paths.clone(), compare_files.clone()],
            vec![
                recipe(ToolRole::Open, &[ToolArgument::TargetPaths]),
                recipe(
                    ToolRole::Compare,
                    &[
                        ToolArgument::Literal("--diff".to_string()),
                        ToolArgument::TargetPaths,
                    ],
                ),
            ],
        ),
        definition(
            "editor.cursor",
            ToolCategory::EditorIde,
            "Cursor",
            "在 Cursor 中打开文件、目录、工作区或比较两个文件。",
            any,
            ToolLaunchKind::Process,
            Some(if platform == windows {
                "Cursor.exe"
            } else {
                "cursor"
            }),
            Some("com.todesktop.230313mzl4w4u92"),
            vec![open_paths.clone(), compare_files.clone()],
            vec![
                recipe(ToolRole::Open, &[ToolArgument::TargetPaths]),
                recipe(
                    ToolRole::Compare,
                    &[
                        ToolArgument::Literal("--diff".to_string()),
                        ToolArgument::TargetPaths,
                    ],
                ),
            ],
        ),
        definition(
            "editor.zed",
            ToolCategory::EditorIde,
            "Zed",
            "在 Zed 中打开文件、目录或工作区。",
            any,
            ToolLaunchKind::Process,
            Some(if platform == windows {
                "Zed.exe"
            } else {
                "zed"
            }),
            Some("dev.zed.Zed"),
            vec![open_paths.clone()],
            vec![recipe(ToolRole::Open, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "editor.sublime",
            ToolCategory::EditorIde,
            "Sublime Text",
            "在 Sublime Text 中打开文件、目录或工作区。",
            any,
            ToolLaunchKind::Process,
            Some(if platform == windows {
                "subl.exe"
            } else {
                "subl"
            }),
            Some("com.sublimetext.4"),
            vec![open_paths.clone()],
            vec![recipe(ToolRole::Open, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "editor.xcode",
            ToolCategory::EditorIde,
            "Xcode",
            "在 Xcode 中打开文件或项目目录。",
            macos,
            ToolLaunchKind::WorkspaceApplication,
            None,
            Some("com.apple.dt.Xcode"),
            vec![open_paths.clone()],
            vec![recipe(ToolRole::Open, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "diff.filemerge",
            ToolCategory::DiffMerge,
            "FileMerge",
            "使用 Xcode 的 FileMerge 比较两个文件。",
            macos,
            ToolLaunchKind::Process,
            Some("xcrun"),
            None,
            vec![compare_files.clone()],
            vec![recipe(
                ToolRole::Compare,
                &[
                    ToolArgument::Literal("opendiff".to_string()),
                    ToolArgument::TargetPaths,
                ],
            )],
        ),
        definition(
            "diff.beyond-compare",
            ToolCategory::DiffMerge,
            "Beyond Compare",
            "使用 Beyond Compare 比较两个文件。",
            any,
            ToolLaunchKind::Process,
            Some(if platform == windows {
                "BCompare.exe"
            } else {
                "bcomp"
            }),
            Some("com.ScooterSoftware.BeyondCompare"),
            vec![compare_files.clone()],
            vec![recipe(ToolRole::Compare, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "diff.kaleidoscope",
            ToolCategory::DiffMerge,
            "Kaleidoscope",
            "使用 Kaleidoscope 比较两个文件。",
            macos,
            ToolLaunchKind::WorkspaceApplication,
            None,
            Some("com.blackpixel.kaleidoscope"),
            vec![compare_files.clone()],
            vec![recipe(ToolRole::Compare, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "diff.winmerge",
            ToolCategory::DiffMerge,
            "WinMerge",
            "使用 WinMerge 比较两个文件。",
            windows,
            ToolLaunchKind::Process,
            Some("WinMergeU.exe"),
            None,
            vec![compare_files.clone()],
            vec![recipe(ToolRole::Compare, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "diff.kdiff3",
            ToolCategory::DiffMerge,
            "KDiff3",
            "使用 KDiff3 比较或合并两个文件。",
            any,
            ToolLaunchKind::Process,
            Some(if platform == windows {
                "kdiff3.exe"
            } else {
                "kdiff3"
            }),
            None,
            vec![compare_files],
            vec![recipe(ToolRole::Compare, &[ToolArgument::TargetPaths])],
        ),
        definition(
            "diff.meld",
            ToolCategory::DiffMerge,
            "Meld",
            "使用 Meld 比较两个文件。",
            macos,
            ToolLaunchKind::Process,
            Some("meld"),
            None,
            vec![capability(
                &[ToolTargetShape::File, ToolTargetShape::MultiPath],
                &[ToolRole::Compare, ToolRole::Merge],
                2,
                Some(2),
            )],
            vec![recipe(ToolRole::Compare, &[ToolArgument::TargetPaths])],
        ),
    ];

    if platform == ToolPlatform::MacOs {
        tools.push(definition(
            "kite.mindmap",
            ToolCategory::KiteInternal,
            "Kite Mindmap",
            "使用 Kite Mindmap 打开思维导图文件。",
            macos,
            ToolLaunchKind::KiteInternal,
            Some("kite-internal:mindmap"),
            None,
            vec![mindmap_file],
            vec![recipe(ToolRole::Open, &[ToolArgument::PrimaryPath])],
        ));
    }

    if platform != ToolPlatform::Any {
        tools.retain(|tool| tool.platform == ToolPlatform::Any || tool.platform == platform);
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_menu_model::{ContextTarget, TargetSet};

    fn target(path: &str, kind: TargetKind) -> ContextTarget {
        ContextTarget::new(path, path, kind)
    }

    fn targets(items: Vec<ContextTarget>) -> TargetSet {
        TargetSet::new(
            items.first().map(|item| item.id.clone()),
            items,
            "/tmp/work",
        )
    }

    fn custom_definition(
        id: &str,
        shapes: &[ToolTargetShape],
        roles: &[ToolRole],
        min: usize,
        max: Option<usize>,
        all: bool,
    ) -> ToolDefinition {
        ToolDefinition {
            tool_id: id.to_string(),
            category: ToolCategory::EditorIde,
            display_name: id.to_string(),
            description: "test".to_string(),
            platform: ToolPlatform::Any,
            launch_kind: ToolLaunchKind::Process,
            executable_identity: Some(id.to_string()),
            bundle_identity: None,
            capabilities: vec![ToolCapability {
                target_shapes: shapes.to_vec(),
                roles: roles.to_vec(),
                min_paths: min,
                max_paths: max,
                all_targets_required: all,
                path_filter: ToolPathFilter::Any,
            }],
            recipes: vec![
                recipe(ToolRole::Open, &[ToolArgument::TargetPaths]),
                recipe(ToolRole::Compare, &[ToolArgument::TargetPaths]),
            ],
        }
    }

    #[test]
    fn builtins_have_stable_unique_ids_per_platform() {
        for platform in [ToolPlatform::Windows, ToolPlatform::MacOs] {
            let tools = builtin_tool_definitions(platform);
            let ids = tools
                .iter()
                .map(|tool| tool.tool_id.as_str())
                .collect::<Vec<_>>();
            assert!(!ids.is_empty());
            for tool in &tools {
                assert_eq!(ids.iter().filter(|id| **id == tool.tool_id).count(), 1);
                assert!(
                    tool.capabilities[0].min_paths
                        <= tool.capabilities[0].max_paths.unwrap_or(usize::MAX)
                );
            }
            assert!(tools.iter().any(|tool| tool.tool_id == "editor.vscode"));
            assert!(tools.iter().any(|tool| tool.tool_id == "kite.git-client"));
        }
    }

    #[test]
    fn capability_matches_shapes_and_path_count() {
        let file_tool = custom_definition(
            "file",
            &[ToolTargetShape::File, ToolTargetShape::MultiPath],
            &[ToolRole::Open],
            1,
            None,
            true,
        );
        let one_file = targets(vec![target("/tmp/a.txt", TargetKind::File)]);
        let two_files = targets(vec![
            target("/tmp/a.txt", TargetKind::File),
            target("/tmp/b.txt", TargetKind::File),
        ]);
        let directory = targets(vec![target("/tmp", TargetKind::Directory)]);
        assert!(file_tool.capabilities[0].matches(&one_file, ToolRole::Open));
        assert!(file_tool.capabilities[0].matches(&two_files, ToolRole::Open));
        assert!(!file_tool.capabilities[0].matches(&directory, ToolRole::Open));
        assert!(!file_tool.capabilities[0].matches(&two_files, ToolRole::Compare));
    }

    #[test]
    fn target_shapes_cover_directory_workspace_and_repository() {
        let tool = custom_definition(
            "workspace",
            &[
                ToolTargetShape::Directory,
                ToolTargetShape::Repository,
                ToolTargetShape::Workspace,
            ],
            &[ToolRole::Open],
            1,
            Some(1),
            true,
        );
        for kind in [
            TargetKind::Directory,
            TargetKind::RepositoryRoot,
            TargetKind::WorkspaceRoot,
        ] {
            let context = targets(vec![target("/tmp/workspace", kind)]);
            assert!(tool.supports(&context, ToolRole::Open));
        }
        let file = targets(vec![target("/tmp/file.txt", TargetKind::File)]);
        assert!(!tool.supports(&file, ToolRole::Open));
    }

    #[test]
    fn builtin_editor_has_role_specific_capabilities() {
        let vscode = builtin_tool_definitions(ToolPlatform::MacOs)
            .into_iter()
            .find(|tool| tool.tool_id == "editor.vscode")
            .expect("vscode definition");
        let one_file = targets(vec![target("/tmp/a.txt", TargetKind::File)]);
        let two_files = targets(vec![
            target("/tmp/a.txt", TargetKind::File),
            target("/tmp/b.txt", TargetKind::File),
        ]);
        assert!(vscode.supports(&one_file, ToolRole::Open));
        assert!(!vscode.supports(&one_file, ToolRole::Compare));
        assert!(vscode.supports(&two_files, ToolRole::Compare));
    }

    #[test]
    fn all_targets_requirement_rejects_mixed_selection() {
        let all_required = custom_definition(
            "all",
            &[ToolTargetShape::File, ToolTargetShape::MultiPath],
            &[ToolRole::Open],
            1,
            None,
            true,
        );
        let primary_only = custom_definition(
            "primary",
            &[ToolTargetShape::File, ToolTargetShape::MultiPath],
            &[ToolRole::Open],
            1,
            None,
            false,
        );
        let mixed = targets(vec![
            target("/tmp/a.txt", TargetKind::File),
            target("/tmp/sub", TargetKind::Directory),
        ]);
        assert!(!all_required.capabilities[0].matches(&mixed, ToolRole::Open));
        assert!(primary_only.capabilities[0].matches(&mixed, ToolRole::Open));
    }

    #[test]
    fn policy_orders_pinned_then_recent_then_user_order_and_filters_hidden() {
        let file = target("/tmp/a.txt", TargetKind::File);
        let context = targets(vec![file]);
        let definitions = vec![
            custom_definition(
                "alpha",
                &[ToolTargetShape::File],
                &[ToolRole::Open],
                1,
                Some(1),
                true,
            ),
            custom_definition(
                "beta",
                &[ToolTargetShape::File],
                &[ToolRole::Open],
                1,
                Some(1),
                true,
            ),
            custom_definition(
                "gamma",
                &[ToolTargetShape::File],
                &[ToolRole::Open],
                1,
                Some(1),
                true,
            ),
            custom_definition(
                "hidden",
                &[ToolTargetShape::File],
                &[ToolRole::Open],
                1,
                Some(1),
                true,
            ),
        ];
        let catalog = ToolCatalog::ready(vec![
            DiscoveredTool::available("alpha", "a"),
            DiscoveredTool::available("beta", "b"),
            DiscoveredTool::available("gamma", "c"),
            DiscoveredTool::available("hidden", "d"),
        ]);
        let policy = ToolPolicy {
            pinned_tool_ids: vec!["gamma".to_string()],
            hidden_tool_ids: vec!["hidden".to_string()],
            user_order: vec!["beta".to_string(), "alpha".to_string()],
            recent_tool_ids: vec!["alpha".to_string()],
        };
        let ordered = available_tools(
            ContextPlatform::MacOs,
            &definitions,
            &catalog,
            &policy,
            &context,
            ToolRole::Open,
        );
        assert_eq!(
            ordered
                .iter()
                .map(|tool| tool.tool_id.as_str())
                .collect::<Vec<_>>(),
            vec!["gamma", "alpha", "beta"]
        );
    }

    #[test]
    fn stale_and_unavailable_tools_are_filtered() {
        let context = targets(vec![target("/tmp/a.txt", TargetKind::File)]);
        let definitions = vec![custom_definition(
            "editor",
            &[ToolTargetShape::File],
            &[ToolRole::Open],
            1,
            Some(1),
            true,
        )];
        for status in [
            ToolAvailability::NotInstalled,
            ToolAvailability::Invalidated,
            ToolAvailability::Unsupported,
            ToolAvailability::Denied,
            ToolAvailability::Unknown,
        ] {
            let catalog = ToolCatalog::ready(vec![DiscoveredTool::unavailable("editor", status)]);
            assert!(
                available_tools(
                    ContextPlatform::Windows,
                    &definitions,
                    &catalog,
                    &ToolPolicy::default(),
                    &context,
                    ToolRole::Open
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn recent_tools_are_capped_and_deduplicated() {
        let mut policy = ToolPolicy::default();
        for id in ["a", "b", "a", "c", "d", "e", "f"] {
            policy.record_recent(id);
        }
        assert_eq!(policy.recent_tool_ids, vec!["f", "e", "d", "c", "a"]);
    }

    #[test]
    fn compare_recipe_is_exactly_two_files_and_uses_argv_tokens() {
        let definition = custom_definition(
            "diff",
            &[ToolTargetShape::File, ToolTargetShape::MultiPath],
            &[ToolRole::Compare],
            2,
            Some(2),
            true,
        );
        let two_files = targets(vec![
            target("/tmp/a.txt", TargetKind::File),
            target("/tmp/b.txt", TargetKind::File),
        ]);
        let invocation = build_tool_invocation(&definition, ToolRole::Compare, &two_files, None)
            .expect("invocation");
        assert_eq!(
            invocation.arguments,
            vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()]
        );

        let three_files = targets(vec![
            target("/tmp/a.txt", TargetKind::File),
            target("/tmp/b.txt", TargetKind::File),
            target("/tmp/c.txt", TargetKind::File),
        ]);
        assert!(build_tool_invocation(&definition, ToolRole::Compare, &three_files, None).is_err());
    }

    #[test]
    fn literal_arguments_are_never_shell_interpolated() {
        let mut definition = custom_definition(
            "editor",
            &[ToolTargetShape::File],
            &[ToolRole::Open],
            1,
            Some(1),
            true,
        );
        definition.recipes = vec![ToolRecipe {
            role: ToolRole::Open,
            arguments: vec![
                ToolArgument::Literal("--new-window".to_string()),
                ToolArgument::PrimaryPath,
            ],
            working_directory: None,
        }];
        let context = targets(vec![target("/tmp/a b; rm -rf x.txt", TargetKind::File)]);
        let invocation =
            build_tool_invocation(&definition, ToolRole::Open, &context, None).expect("invocation");
        assert_eq!(invocation.arguments[0], "--new-window");
        assert_eq!(invocation.arguments[1], "/tmp/a b; rm -rf x.txt");
    }

    #[test]
    fn unknown_discovery_ids_are_reported_without_entering_menu() {
        let definitions = vec![custom_definition(
            "known",
            &[ToolTargetShape::File],
            &[ToolRole::Open],
            1,
            Some(1),
            true,
        )];
        let catalog = ToolCatalog::ready(vec![DiscoveredTool::available("unknown", "x")]);
        assert_eq!(
            unexpected_discovery_ids(&definitions, &catalog),
            vec!["unknown"]
        );
        let context = targets(vec![target("/tmp/a.txt", TargetKind::File)]);
        assert!(
            available_tools(
                ContextPlatform::Windows,
                &definitions,
                &catalog,
                &ToolPolicy::default(),
                &context,
                ToolRole::Open
            )
            .is_empty()
        );
    }
}
