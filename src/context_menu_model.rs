//! Canonical, UI-independent context-menu composition.
//!
//! Providers declare action candidates only. This module owns target matching,
//! visibility, deduplication, risk placement, grouping, ordering, and invariant
//! validation. GPUI and SwiftUI are projections of the resulting `MenuPlan`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;

use crate::agent::is_text_like_path;
use crate::archive;
use crate::tool_registry::{
    ToolCatalog, ToolCatalogStatus, ToolPlatform, ToolPolicy, ToolRole, available_tools,
    builtin_tool_definitions,
};
use crate::vcs_menu::{
    VcsMenuRepository, VcsMenuSnapshot, VcsMenuTool, VcsTargetLink, VcsTargetRelation,
};

pub const CONTEXT_MENU_SCHEMA_VERSION: u32 = 1;
pub const FILE_ACTIONS_SUBMENU: &str = "file-actions";
pub const ARCHIVE_SUBMENU: &str = "archive";
pub const AGENT_SUBMENU: &str = "agent";
pub const GIT_SUBMENU: &str = "git";
pub const SVN_SUBMENU: &str = "svn";
pub const SYSTEM_SUBMENU: &str = "system";
pub const OPEN_WITH_SUBMENU: &str = "open-with";
pub const OPEN_IN_SUBMENU: &str = "open-in";
pub const COMPARE_WITH_SUBMENU: &str = "compare-with";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextPlatform {
    Windows,
    MacOs,
    Other,
}

impl ContextPlatform {
    pub fn parse(value: &str) -> Self {
        match value {
            "windows" => Self::Windows,
            "macos" => Self::MacOs,
            _ => Self::Other,
        }
    }

    pub const fn is_macos(self) -> bool {
        matches!(self, Self::MacOs)
    }

    pub const fn is_windows(self) -> bool {
        matches!(self, Self::Windows)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextSurface {
    BrowserList,
    BrowserColumns,
    Preview,
    SearchResults,
    SearchColumn,
    SearchGlobal,
    SearchContent,
    Sidebar,
    SidebarLocations,
    SidebarFavorites,
    SidebarVolumes,
    Background,
    DeviceNamespace,
}

impl ContextSurface {
    pub fn parse(value: &str) -> Self {
        match value {
            "browser.list" => Self::BrowserList,
            "browser.columns" => Self::BrowserColumns,
            "preview" => Self::Preview,
            "search.results" => Self::SearchResults,
            "search.column" => Self::SearchColumn,
            "search.global" => Self::SearchGlobal,
            "search.content" => Self::SearchContent,
            "sidebar" => Self::Sidebar,
            "sidebar.locations" => Self::SidebarLocations,
            "sidebar.favorites" => Self::SidebarFavorites,
            "sidebar.volumes" => Self::SidebarVolumes,
            "device.namespace" => Self::DeviceNamespace,
            _ => Self::Background,
        }
    }

    pub const fn is_search(self) -> bool {
        matches!(
            self,
            Self::SearchResults | Self::SearchColumn | Self::SearchGlobal | Self::SearchContent
        )
    }

    pub const fn is_sidebar(self) -> bool {
        matches!(
            self,
            Self::Sidebar | Self::SidebarLocations | Self::SidebarFavorites | Self::SidebarVolumes
        )
    }

    pub const fn is_device_namespace(self) -> bool {
        matches!(self, Self::DeviceNamespace)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TargetKind {
    File,
    Directory,
    Archive,
    RepositoryRoot,
    WorkspaceRoot,
    DeviceContainer,
    DeviceItem,
    VirtualItem,
}

impl TargetKind {
    pub fn parse(value: &str) -> Self {
        match value {
            "directory" => Self::Directory,
            "archive" => Self::Archive,
            "repository-root" | "repository_root" => Self::RepositoryRoot,
            "workspace-root" | "workspace_root" => Self::WorkspaceRoot,
            "device-container" | "device_container" => Self::DeviceContainer,
            "device-item" | "device_item" => Self::DeviceItem,
            "virtual-item" | "virtual_item" => Self::VirtualItem,
            _ => Self::File,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Archive => "archive",
            Self::RepositoryRoot => "repository-root",
            Self::WorkspaceRoot => "workspace-root",
            Self::DeviceContainer => "device-container",
            Self::DeviceItem => "device-item",
            Self::VirtualItem => "virtual-item",
        }
    }

    pub const fn is_file_like(self) -> bool {
        matches!(self, Self::File | Self::Archive)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SelectionRelation {
    None,
    ClickedUnselected,
    ClickedInSelection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetOrder {
    VisibleOrder,
    ExplicitOrder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextTarget {
    pub id: String,
    pub path: String,
    pub display_name: String,
    pub kind: TargetKind,
    pub exists: bool,
    pub readable: bool,
    pub writable: bool,
}

impl ContextTarget {
    pub fn new(path: impl Into<String>, display_name: impl Into<String>, kind: TargetKind) -> Self {
        let path = path.into();
        Self {
            id: path.clone(),
            path,
            display_name: display_name.into(),
            kind,
            exists: true,
            readable: true,
            writable: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetSet {
    pub primary: Option<String>,
    pub targets: Vec<ContextTarget>,
    pub selection_relation: SelectionRelation,
    pub order: TargetOrder,
    pub homogeneous: bool,
    /// Frozen ordered paths used by dispatch. Never re-read selection state.
    pub ordered_paths: Vec<String>,
    /// Frozen working directory used by container fallback actions.
    pub working_directory: String,
}

impl TargetSet {
    pub fn new(
        primary: Option<String>,
        targets: Vec<ContextTarget>,
        working_directory: impl Into<String>,
    ) -> Self {
        let ordered_paths = targets.iter().map(|target| target.path.clone()).collect();
        let homogeneous = targets
            .first()
            .map(|first| targets.iter().all(|target| target.kind == first.kind))
            .unwrap_or(true);
        let selection_relation = if targets.is_empty() {
            SelectionRelation::None
        } else {
            SelectionRelation::ClickedInSelection
        };
        Self {
            primary,
            targets,
            selection_relation,
            order: TargetOrder::VisibleOrder,
            homogeneous,
            ordered_paths,
            working_directory: working_directory.into(),
        }
    }

    pub fn cardinality(&self) -> TargetCardinality {
        match self.targets.len() {
            0 => TargetCardinality::None,
            1 => TargetCardinality::One,
            _ => TargetCardinality::OneOrMore,
        }
    }

    pub fn primary_target(&self) -> Option<&ContextTarget> {
        let primary = self.primary.as_deref()?;
        self.targets
            .iter()
            .find(|target| target.id == primary || target.path == primary)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerSnapshot {
    pub path: String,
    pub display_name: String,
    pub writable: bool,
    pub kind: TargetKind,
    /// True when the container itself is the object being activated, as in a
    /// directory/background menu. File menus use their parent only as fallback.
    pub is_primary_target: bool,
}

impl ContainerSnapshot {
    pub fn for_path(path: impl Into<String>, is_primary_target: bool) -> Self {
        let path = path.into();
        let display_name = Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.clone());
        Self {
            path,
            display_name,
            writable: true,
            kind: TargetKind::Directory,
            is_primary_target,
        }
    }

    pub fn with_kind(mut self, kind: TargetKind) -> Self {
        self.kind = kind;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextCapabilities {
    pub loading: bool,
    pub can_paste: bool,
    pub can_create: bool,
    pub supports_transfer_ops: bool,
    pub supports_cut: bool,
    pub supports_rename: bool,
    pub supports_delete: bool,
    pub supports_favorite: bool,
    pub is_favorite: bool,
    pub git_available: bool,
    pub svn_available: bool,
    pub agent_available: bool,
    pub quick_look_available: bool,
    pub reveal_available: bool,
    pub share_available: bool,
    pub properties_available: bool,
    pub text_editor_available: bool,
    pub mindmap_available: bool,
    pub mindmap_import_available: bool,
    pub is_shell_namespace: bool,
    pub native_plane_available: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenWithCatalogStatus {
    Ready,
    Loading,
    #[default]
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWithApplication {
    /// Stable platform identity: bundle ID on macOS, canonical shell-handler
    /// key on Windows. Never a transient COM object or UI object.
    pub application_id: String,
    pub display_name: String,
    pub is_default: bool,
    pub is_recent: bool,
    pub is_available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenWithCatalog {
    pub status: OpenWithCatalogStatus,
    pub applications: Vec<OpenWithApplication>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPolicy {
    pub compact: bool,
    pub hide_unavailable: bool,
    pub confirm_high_risk: bool,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            compact: false,
            hide_unavailable: false,
            confirm_high_risk: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSnapshot {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub platform: ContextPlatform,
    pub surface: ContextSurface,
    pub target_set: TargetSet,
    pub container: ContainerSnapshot,
    pub capabilities: ContextCapabilities,
    pub open_with: OpenWithCatalog,
    pub tool_catalog: ToolCatalog,
    pub tool_policy: ToolPolicy,
    pub vcs_menu: VcsMenuSnapshot,
    pub policy: ContextPolicy,
}

impl ContextSnapshot {
    pub fn title(&self) -> String {
        self.target_set
            .primary_target()
            .map(|target| target.display_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.container.display_name.clone())
    }

    pub fn target_count(&self) -> usize {
        self.target_set.targets.len()
    }

    pub fn primary_kind(&self) -> Option<TargetKind> {
        self.target_set.primary_target().map(|target| target.kind)
    }

    pub fn all_targets_match(&self, predicate: impl Fn(&ContextTarget) -> bool) -> bool {
        !self.target_set.targets.is_empty() && self.target_set.targets.iter().all(predicate)
    }

    pub fn all_files_text_like(&self) -> bool {
        self.all_targets_match(|target| {
            target.kind.is_file_like() && is_text_like_path(Path::new(&target.path))
        })
    }

    pub fn single_target_is_archive(&self) -> bool {
        self.target_set.targets.len() == 1
            && archive::is_supported_archive(Path::new(&self.target_set.targets[0].path))
    }

    pub fn single_target_can_compress(&self) -> bool {
        if self.target_set.targets.len() != 1 || self.capabilities.is_shell_namespace {
            return false;
        }
        let path = Path::new(&self.target_set.targets[0].path);
        path.parent().is_some()
    }

    pub fn single_open_with_target(&self) -> Option<&ContextTarget> {
        if self.capabilities.is_shell_namespace || self.target_set.targets.len() != 1 {
            return None;
        }
        self.target_set.targets.first().filter(|target| {
            target.kind.is_file_like()
                && target.kind != TargetKind::DeviceItem
                && target.exists
                && target.readable
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionScope {
    Entity,
    Collection,
    Container,
    Workspace,
    Repository,
    System,
    Extension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionPlane {
    KiteSemantic,
    OsNativeExtension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionOwner {
    Kite,
    Os,
    Tool,
    ExternalExtension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionEffect {
    Navigation,
    Read,
    Clipboard,
    FileMutation,
    ProcessLaunch,
    VcsRead,
    VcsWrite,
    SystemUi,
    ExternalUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionRisk {
    None,
    Low,
    Medium,
    High,
    ExternalUnknown,
}

impl ActionRisk {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExternalUnknown => "external_unknown",
        }
    }

    pub const fn requires_confirmation(self) -> bool {
        matches!(self, Self::High)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confirmation {
    None,
    Normal,
    Dangerous,
    SystemOwned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feedback {
    None,
    Notice,
    Progress,
    Dialog,
    OsOwned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetCardinality {
    None,
    One,
    OneOrMore,
    ExactlyTwo,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetRule {
    pub kinds: Vec<TargetKind>,
    pub cardinality: TargetCardinality,
    pub container_required: bool,
    pub container_fallback: bool,
}

impl TargetRule {
    fn new(kinds: &[TargetKind], cardinality: TargetCardinality) -> Self {
        Self {
            kinds: kinds.to_vec(),
            cardinality,
            container_required: false,
            container_fallback: false,
        }
    }

    fn container() -> Self {
        Self {
            kinds: vec![TargetKind::Directory],
            cardinality: TargetCardinality::Any,
            container_required: true,
            container_fallback: true,
        }
    }

    fn any() -> Self {
        Self {
            kinds: Vec::new(),
            cardinality: TargetCardinality::Any,
            container_required: false,
            container_fallback: false,
        }
    }

    fn matches(&self, context: &ContextSnapshot) -> bool {
        if self.container_required && !context.container.is_primary_target {
            return false;
        }
        if context.target_set.targets.is_empty() {
            return !self.container_required || !context.container.path.is_empty();
        }
        if !self.kinds.is_empty()
            && !context
                .target_set
                .targets
                .iter()
                .all(|target| self.kinds.contains(&target.kind))
        {
            return false;
        }
        match self.cardinality {
            TargetCardinality::None => context.target_set.targets.is_empty(),
            TargetCardinality::One => context.target_set.targets.len() == 1,
            TargetCardinality::OneOrMore => !context.target_set.targets.is_empty(),
            TargetCardinality::ExactlyTwo => context.target_set.targets.len() == 2,
            TargetCardinality::Any => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RootGroup {
    Refresh,
    OpenAndLaunch,
    Transfer,
    CreateAndOrganize,
    Archive,
    AgentAndTools,
    Vcs,
    System,
    Destructive,
    Native,
}

impl RootGroup {
    pub const fn order(self) -> u16 {
        match self {
            Self::Refresh => 0,
            Self::OpenAndLaunch => 10,
            Self::Transfer => 20,
            Self::CreateAndOrganize => 30,
            Self::Archive => 40,
            Self::AgentAndTools => 50,
            Self::Vcs => 60,
            Self::System => 70,
            Self::Destructive => 90,
            Self::Native => 99,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Refresh => "G00",
            Self::OpenAndLaunch => "G10",
            Self::Transfer => "G20",
            Self::CreateAndOrganize => "G30",
            Self::Archive => "G40",
            Self::AgentAndTools => "G50",
            Self::Vcs => "G60",
            Self::System => "G70",
            Self::Destructive => "G90",
            Self::Native => "G99",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MenuSection {
    Recovery,
    Primary,
    Changes,
    Sync,
    History,
    Advanced,
}

impl MenuSection {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Changes => "changes",
            Self::Sync => "sync",
            Self::History => "history",
            Self::Advanced => "advanced",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubmenuId {
    FileActions,
    Archive,
    Agent,
    Git,
    Svn,
    OpenWith,
    OpenIn,
    CompareWith,
    System,
}

impl SubmenuId {
    pub const fn id(self) -> &'static str {
        match self {
            Self::FileActions => FILE_ACTIONS_SUBMENU,
            Self::Archive => ARCHIVE_SUBMENU,
            Self::Agent => AGENT_SUBMENU,
            Self::Git => GIT_SUBMENU,
            Self::Svn => SVN_SUBMENU,
            Self::OpenWith => OPEN_WITH_SUBMENU,
            Self::OpenIn => OPEN_IN_SUBMENU,
            Self::CompareWith => COMPARE_WITH_SUBMENU,
            Self::System => SYSTEM_SUBMENU,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            FILE_ACTIONS_SUBMENU => Some(Self::FileActions),
            ARCHIVE_SUBMENU => Some(Self::Archive),
            AGENT_SUBMENU => Some(Self::Agent),
            GIT_SUBMENU => Some(Self::Git),
            SVN_SUBMENU => Some(Self::Svn),
            OPEN_WITH_SUBMENU => Some(Self::OpenWith),
            OPEN_IN_SUBMENU => Some(Self::OpenIn),
            COMPARE_WITH_SUBMENU => Some(Self::CompareWith),
            SYSTEM_SUBMENU => Some(Self::System),
            _ => None,
        }
    }

    pub fn label(self, platform: ContextPlatform) -> &'static str {
        match self {
            Self::FileActions => "文件操作",
            Self::Archive => "压缩与解压",
            Self::Agent => "使用 Agent",
            Self::Git => "Git",
            Self::Svn => "SVN",
            Self::OpenWith => "打开方式",
            Self::OpenIn => "在工具中打开",
            Self::CompareWith => "比较…",
            Self::System if platform.is_macos() => "系统",
            Self::System if platform.is_windows() => "更多系统操作",
            Self::System => "系统",
        }
    }

    pub const fn icon_key(self) -> &'static str {
        match self {
            Self::FileActions => "file-actions",
            Self::Archive => "archive",
            Self::Agent => "agent",
            Self::Git | Self::Svn => "vcs",
            Self::OpenWith => "open-with",
            Self::OpenIn => "open-in",
            Self::CompareWith => "compare-with",
            Self::System => "system",
        }
    }

    pub const fn root_group(self) -> RootGroup {
        match self {
            Self::FileActions => RootGroup::Transfer,
            Self::Archive => RootGroup::Archive,
            Self::Agent => RootGroup::AgentAndTools,
            Self::Git | Self::Svn => RootGroup::Vcs,
            Self::OpenWith => RootGroup::OpenAndLaunch,
            Self::OpenIn => RootGroup::OpenAndLaunch,
            Self::CompareWith => RootGroup::OpenAndLaunch,
            Self::System => RootGroup::System,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub action_id: String,
    pub label: String,
    pub scope: ActionScope,
    pub plane: ActionPlane,
    pub owner: ActionOwner,
    pub effect: ActionEffect,
    pub risk: ActionRisk,
    pub target_rule: TargetRule,
    pub confirmation: Confirmation,
    pub feedback: Feedback,
    pub icon_key: String,
    pub root_group: RootGroup,
    pub section: MenuSection,
    pub submenu: Option<SubmenuId>,
    pub order: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateAvailability {
    Available,
    Disabled { reason_code: String, reason: String },
    Hidden { reason_code: String },
}

impl CandidateAvailability {
    pub fn disabled(reason_code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Disabled {
            reason_code: reason_code.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionPayload {
    None,
    Application {
        application_id: String,
    },
    Tool {
        tool_id: String,
    },
    Repository {
        path: String,
        vcs: String,
    },
    Vcs {
        repository_root: String,
        tool: String,
        state_revision: String,
    },
}

impl ActionPayload {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Application { .. } => "application",
            Self::Tool { .. } => "tool",
            Self::Repository { .. } => "repository",
            Self::Vcs { .. } => "vcs",
        }
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Application { application_id } => Some(application_id),
            Self::Tool { tool_id } => Some(tool_id),
            Self::Repository { path, .. } => Some(path),
            Self::Vcs {
                repository_root, ..
            } => Some(repository_root),
        }
    }

    pub fn state_revision(&self) -> Option<&str> {
        match self {
            Self::Vcs { state_revision, .. } => Some(state_revision),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionCandidate {
    pub action_id: String,
    pub variant_key: Option<String>,
    pub availability: CandidateAvailability,
    /// Optional provider-owned presentation for dynamic variants.
    pub label: Option<String>,
    pub icon_key: Option<String>,
    /// Optional submenu-local ordering for dynamic variants. Static actions use
    /// the canonical descriptor order.
    pub order: Option<u16>,
    /// Higher values win duplicate resolution. Providers should use this only
    /// to express evidence specificity, never to express layout.
    pub precedence: u8,
    pub payload: ActionPayload,
}

impl ActionCandidate {
    pub fn available(action_id: &str) -> Self {
        Self {
            action_id: action_id.to_string(),
            variant_key: None,
            availability: CandidateAvailability::Available,
            label: None,
            icon_key: None,
            order: None,
            precedence: 0,
            payload: ActionPayload::None,
        }
    }

    pub fn with_availability(action_id: &str, availability: CandidateAvailability) -> Self {
        Self {
            action_id: action_id.to_string(),
            variant_key: None,
            availability,
            label: None,
            icon_key: None,
            order: None,
            precedence: 0,
            payload: ActionPayload::None,
        }
    }

    pub fn variant(mut self, variant_key: impl Into<String>) -> Self {
        self.variant_key = Some(variant_key.into());
        self
    }

    pub fn precedence(mut self, precedence: u8) -> Self {
        self.precedence = precedence;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon_key(mut self, icon_key: impl Into<String>) -> Self {
        self.icon_key = Some(icon_key.into());
        self
    }

    pub fn order(mut self, order: u16) -> Self {
        self.order = Some(order);
        self
    }

    pub fn payload(mut self, payload: ActionPayload) -> Self {
        self.payload = payload;
        self
    }

    pub fn instance_key(&self) -> String {
        match self.variant_key.as_deref() {
            Some(variant) => format!("{}::{variant}", self.action_id),
            None => self.action_id.clone(),
        }
    }
}

pub trait ActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAction {
    pub action_id: String,
    pub variant_key: Option<String>,
    pub label: String,
    pub icon_key: String,
    pub scope: ActionScope,
    pub plane: ActionPlane,
    pub owner: ActionOwner,
    pub effect: ActionEffect,
    pub risk: ActionRisk,
    pub target_rule: TargetRule,
    pub confirmation: Confirmation,
    pub requires_confirmation: bool,
    pub feedback: Feedback,
    pub root_group: RootGroup,
    pub section: MenuSection,
    pub order: u16,
    pub targets: Vec<String>,
    pub enabled: bool,
    pub disabled_reason_code: Option<String>,
    pub disabled_reason: Option<String>,
    pub payload: ActionPayload,
}

impl ResolvedAction {
    pub fn instance_key(&self) -> String {
        match self.variant_key.as_deref() {
            Some(variant) => format!("{}::{variant}", self.action_id),
            None => self.action_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEntryPlan {
    Action(ResolvedAction),
    Submenu(SubmenuPlan),
}

impl MenuEntryPlan {
    pub fn order(&self) -> u16 {
        match self {
            Self::Action(action) => action.order,
            Self::Submenu(submenu) => submenu.order,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuSectionPlan {
    pub id: String,
    pub entries: Vec<ResolvedAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuGroupPlan {
    pub id: String,
    pub entries: Vec<MenuEntryPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmenuPlan {
    pub id: String,
    pub label: String,
    pub icon_key: String,
    pub order: u16,
    pub sections: Vec<MenuSectionPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuPlan {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub title: String,
    pub target_kinds: Vec<String>,
    pub groups: Vec<MenuGroupPlan>,
    pub submenus: Vec<SubmenuPlan>,
    pub diagnostics: Vec<String>,
}

impl MenuPlan {
    pub fn find_submenu(&self, id: &str) -> Option<&SubmenuPlan> {
        self.submenus.iter().find(|submenu| submenu.id == id)
    }

    pub fn visible_action_ids(&self) -> Vec<&str> {
        let mut ids = Vec::new();
        for group in &self.groups {
            for entry in &group.entries {
                match entry {
                    MenuEntryPlan::Action(action) => ids.push(action.action_id.as_str()),
                    MenuEntryPlan::Submenu(submenu) => {
                        for section in &submenu.sections {
                            for action in &section.entries {
                                ids.push(action.action_id.as_str());
                            }
                        }
                    }
                }
            }
        }
        ids
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerError {
    pub message: String,
}

impl fmt::Display for ComposerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ComposerError {}

pub struct ContextMenuComposer {
    providers: Vec<Box<dyn ActionProvider>>,
}

impl ContextMenuComposer {
    pub fn new(providers: Vec<Box<dyn ActionProvider>>) -> Self {
        Self { providers }
    }

    pub fn compose(&self, context: &ContextSnapshot) -> Result<MenuPlan, ComposerError> {
        let mut diagnostics = Vec::new();
        let mut resolved = Vec::new();
        let (_, target_kinds) = context_target_paths_and_kinds(context);
        for provider in &self.providers {
            for candidate in provider.candidates(context) {
                let Some(descriptor) = descriptor(context, &candidate.action_id) else {
                    diagnostics.push(format!(
                        "unknown context-menu action candidate: {}",
                        candidate.action_id
                    ));
                    continue;
                };
                if matches!(candidate.availability, CandidateAvailability::Hidden { .. }) {
                    continue;
                }
                if !descriptor.target_rule.matches(context) {
                    continue;
                }
                if !surface_allows_action(context, &candidate.action_id) {
                    continue;
                }
                let (enabled, reason_code, reason) = match &candidate.availability {
                    CandidateAvailability::Available => (true, None, None),
                    CandidateAvailability::Disabled {
                        reason_code,
                        reason,
                    } => (false, Some(reason_code.clone()), Some(reason.clone())),
                    CandidateAvailability::Hidden { reason_code } => {
                        diagnostics.push(format!(
                            "hidden candidate {} unexpectedly reached resolution ({reason_code})",
                            candidate.action_id
                        ));
                        continue;
                    }
                };
                if !enabled
                    && context.policy.hide_unavailable
                    && !matches!(
                        candidate.action_id.as_str(),
                        "system.native-menu" | "open.with" | "open.with.choose-other"
                    )
                {
                    continue;
                }

                let (targets, _) = context_target_paths_and_kinds(context);
                let mut risk = descriptor.risk;
                if descriptor.action_id == "file.move-to-trash" && targets.len() > 1 {
                    risk = ActionRisk::High;
                }
                let requires_confirmation = context.policy.confirm_high_risk
                    && (risk.requires_confirmation()
                        || descriptor.confirmation == Confirmation::Dangerous
                        || descriptor.confirmation == Confirmation::SystemOwned);
                let mut root_group = descriptor.root_group;
                if risk == ActionRisk::High || descriptor.confirmation == Confirmation::SystemOwned
                {
                    root_group = if matches!(descriptor.plane, ActionPlane::OsNativeExtension) {
                        RootGroup::Native
                    } else {
                        RootGroup::Destructive
                    };
                }
                let precedence = candidate.precedence;
                let label = candidate.label.unwrap_or(descriptor.label);
                let icon_key = candidate.icon_key.unwrap_or(descriptor.icon_key);
                let order = candidate.order.unwrap_or(descriptor.order);
                resolved.push((
                    ResolvedAction {
                        action_id: descriptor.action_id,
                        variant_key: candidate.variant_key,
                        label,
                        icon_key,
                        scope: descriptor.scope,
                        plane: descriptor.plane,
                        owner: descriptor.owner,
                        effect: descriptor.effect,
                        risk,
                        target_rule: descriptor.target_rule,
                        confirmation: descriptor.confirmation,
                        requires_confirmation,
                        feedback: descriptor.feedback,
                        root_group,
                        section: descriptor.section,
                        order,
                        targets,
                        enabled,
                        disabled_reason_code: reason_code,
                        disabled_reason: reason,
                        payload: candidate.payload,
                    },
                    precedence,
                ));
            }
        }

        // Enabled candidates win first. Among candidates in the same state,
        // explicit evidence precedence wins before provider registration order.
        let mut deduplicated: HashMap<String, (u8, u8, usize, ResolvedAction)> = HashMap::new();
        for (provider_order, (action, precedence)) in resolved.into_iter().enumerate() {
            let key = action.instance_key();
            let enabled_rank = u8::from(action.enabled);
            let replacement = (
                enabled_rank,
                precedence,
                usize::MAX - provider_order,
                action,
            );
            match deduplicated.get_mut(&key) {
                Some(existing)
                    if replacement.0 > existing.0
                        || (replacement.0 == existing.0 && replacement.1 > existing.1)
                        || (replacement.0 == existing.0
                            && replacement.1 == existing.1
                            && replacement.2 > existing.2) =>
                {
                    deduplicated.insert(key, replacement);
                }
                Some(_) => {}
                None => {
                    deduplicated.insert(key, replacement);
                }
            }
        }
        let mut actions = deduplicated
            .into_values()
            .map(|(_, _, _, action)| action)
            .collect::<Vec<_>>();
        actions.sort_by(|left, right| {
            left.root_group
                .order()
                .cmp(&right.root_group.order())
                .then_with(|| left.order.cmp(&right.order))
                .then_with(|| left.action_id.cmp(&right.action_id))
                .then_with(|| left.variant_key.cmp(&right.variant_key))
        });

        let mut root_actions: BTreeMap<RootGroup, Vec<ResolvedAction>> = BTreeMap::new();
        let mut submenu_actions: BTreeMap<SubmenuId, Vec<ResolvedAction>> = BTreeMap::new();
        for action in actions {
            let mut descriptor_submenu = submenu_for_action(&action.action_id);
            if action.root_group == RootGroup::Destructive || action.root_group == RootGroup::Native
            {
                descriptor_submenu = None;
            }
            if let Some(submenu) = descriptor_submenu {
                submenu_actions.entry(submenu).or_default().push(action);
            } else {
                root_actions
                    .entry(action.root_group)
                    .or_default()
                    .push(action);
            }
        }

        let mut submenus = Vec::new();
        for (submenu_id, mut actions) in submenu_actions {
            actions.sort_by(|left, right| {
                left.section
                    .cmp(&right.section)
                    .then_with(|| left.order.cmp(&right.order))
                    .then_with(|| left.action_id.cmp(&right.action_id))
            });
            if actions.len() == 1 {
                let only = actions.pop().expect("single submenu action");
                root_actions.entry(only.root_group).or_default().push(only);
                continue;
            }
            let order = actions.iter().map(|action| action.order).min().unwrap_or(0);
            let mut sections: BTreeMap<MenuSection, Vec<ResolvedAction>> = BTreeMap::new();
            for action in actions {
                sections.entry(action.section).or_default().push(action);
            }
            let sections = sections
                .into_iter()
                .map(|(section, entries)| MenuSectionPlan {
                    id: section.id().to_string(),
                    entries,
                })
                .collect::<Vec<_>>();
            submenus.push(SubmenuPlan {
                id: submenu_id.id().to_string(),
                label: submenu_id.label(context.platform).to_string(),
                icon_key: submenu_id.icon_key().to_string(),
                order,
                sections,
            });
        }

        let mut root_entries: BTreeMap<RootGroup, Vec<MenuEntryPlan>> = BTreeMap::new();
        for (group, mut actions) in root_actions {
            actions.sort_by(|left, right| {
                left.order
                    .cmp(&right.order)
                    .then_with(|| left.action_id.cmp(&right.action_id))
                    .then_with(|| left.variant_key.cmp(&right.variant_key))
            });
            root_entries
                .entry(group)
                .or_default()
                .extend(actions.into_iter().map(MenuEntryPlan::Action));
        }
        for submenu in &submenus {
            let group = SubmenuId::parse(&submenu.id)
                .map(SubmenuId::root_group)
                .unwrap_or(RootGroup::System);
            root_entries
                .entry(group)
                .or_default()
                .push(MenuEntryPlan::Submenu(submenu.clone()));
        }
        for entries in root_entries.values_mut() {
            entries.sort_by_key(MenuEntryPlan::order);
        }
        let groups = root_entries
            .into_iter()
            .filter(|(_, entries)| !entries.is_empty())
            .map(|(group, entries)| MenuGroupPlan {
                id: group.id().to_string(),
                entries,
            })
            .collect::<Vec<_>>();
        let plan = MenuPlan {
            schema_version: CONTEXT_MENU_SCHEMA_VERSION,
            snapshot_id: context.snapshot_id.clone(),
            title: context.title(),
            target_kinds,
            groups,
            submenus,
            diagnostics,
        };
        validate_plan(context, &plan)?;
        Ok(plan)
    }
}

fn submenu_for_action(action_id: &str) -> Option<SubmenuId> {
    if action_id == "open.with" || action_id == "open.with.choose-other" {
        return Some(SubmenuId::OpenWith);
    }
    if action_id == "open.in" {
        return Some(SubmenuId::OpenIn);
    }
    if action_id == "compare.with" {
        return Some(SubmenuId::CompareWith);
    }
    if action_id.starts_with("git.") {
        return Some(SubmenuId::Git);
    }
    if action_id.starts_with("svn.") {
        return Some(SubmenuId::Svn);
    }
    if action_id.starts_with("archive.") {
        return Some(SubmenuId::Archive);
    }
    if action_id.starts_with("agent.") {
        return Some(SubmenuId::Agent);
    }
    if matches!(
        action_id,
        "clipboard.copy"
            | "clipboard.cut"
            | "clipboard.copy-to"
            | "clipboard.copy-path"
            | "clipboard.paste"
            | "file.new-folder"
            | "file.new-file"
            | "favorite.add"
            | "favorite.remove"
            | "file.rename"
    ) {
        return Some(SubmenuId::FileActions);
    }
    if matches!(
        action_id,
        "system.reveal" | "system.share" | "system.properties"
    ) {
        return Some(SubmenuId::System);
    }
    None
}

fn surface_allows_action(context: &ContextSnapshot, action_id: &str) -> bool {
    if action_id == "system.native-menu" {
        return true;
    }

    match context.surface {
        ContextSurface::BrowserList | ContextSurface::BrowserColumns => true,
        ContextSurface::Preview => !matches!(
            action_id,
            "navigation.refresh" | "clipboard.paste" | "file.new-folder" | "file.new-file"
        ),
        ContextSurface::SearchResults
        | ContextSurface::SearchColumn
        | ContextSurface::SearchGlobal
        | ContextSurface::SearchContent => matches!(
            action_id,
            "open.default"
                | "open.with"
                | "open.with.choose-other"
                | "open.in"
                | "open.quick-look"
                | "clipboard.copy-path"
                | "system.reveal"
        ),
        ContextSurface::Sidebar | ContextSurface::SidebarLocations => matches!(
            action_id,
            "open.default" | "open.in" | "favorite.add" | "system.reveal" | "system.properties"
        ),
        ContextSurface::SidebarFavorites => matches!(
            action_id,
            "open.default"
                | "open.in"
                | "clipboard.copy-path"
                | "favorite.remove"
                | "system.reveal"
                | "system.properties"
        ),
        ContextSurface::SidebarVolumes => matches!(
            action_id,
            "open.default" | "system.reveal" | "system.properties"
        ),
        ContextSurface::Background => {
            matches!(
                action_id,
                "navigation.refresh"
                    | "open.in"
                    | "clipboard.paste"
                    | "file.new-folder"
                    | "file.new-file"
                    | "archive.compress.zip"
                    | "archive.compress.tar-gz"
                    | "agent.ask"
                    | "agent.summarize-folder"
                    | "agent.review-changes"
                    | "agent.generate-commit-message"
                    | "agent.configure"
            ) || action_id.starts_with("git.")
                || action_id.starts_with("svn.")
        }
        ContextSurface::DeviceNamespace => {
            let targets = &context.target_set.targets;
            let container = !targets.is_empty()
                && targets
                    .iter()
                    .all(|target| target.kind == TargetKind::DeviceContainer);
            if container {
                matches!(
                    action_id,
                    "navigation.refresh"
                        | "open.default"
                        | "clipboard.paste"
                        | "file.new-folder"
                        | "file.new-file"
                        | "system.properties"
                )
            } else {
                matches!(
                    action_id,
                    "open.default"
                        | "clipboard.copy"
                        | "clipboard.copy-path"
                        | "device.delete"
                        | "system.properties"
                )
            }
        }
    }
}

fn validate_plan(context: &ContextSnapshot, plan: &MenuPlan) -> Result<(), ComposerError> {
    let mut previous_group_order = None;
    let mut native_count = 0;
    let mut action_instances = HashSet::new();
    let mut ordinary_root_count = 0usize;
    for group in &plan.groups {
        let group_order = group
            .id
            .strip_prefix('G')
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| ComposerError {
                message: format!("invalid context-menu group id: {}", group.id),
            })?;
        if previous_group_order.is_some_and(|previous| group_order <= previous) {
            return Err(ComposerError {
                message: "context-menu groups are not in stable ascending order".to_string(),
            });
        }
        previous_group_order = Some(group_order);
        if group.entries.is_empty() {
            return Err(ComposerError {
                message: format!("empty context-menu group: {}", group.id),
            });
        }
        if group.id != RootGroup::Destructive.id() && group.id != RootGroup::Native.id() {
            ordinary_root_count = ordinary_root_count.saturating_add(group.entries.len());
        }
        for entry in &group.entries {
            match entry {
                MenuEntryPlan::Action(action) => {
                    validate_action(context, action)?;
                    if !action_instances.insert(action.instance_key()) {
                        return Err(ComposerError {
                            message: format!(
                                "duplicate action instance: {}",
                                action.instance_key()
                            ),
                        });
                    }
                    if action.action_id == "system.native-menu" {
                        native_count += 1;
                    }
                }
                MenuEntryPlan::Submenu(submenu) => {
                    if submenu.label.trim().is_empty() {
                        return Err(ComposerError {
                            message: format!("submenu has no accessible label: {}", submenu.id),
                        });
                    }
                    if submenu.sections.is_empty() {
                        return Err(ComposerError {
                            message: format!("empty submenu: {}", submenu.id),
                        });
                    }
                    for section in &submenu.sections {
                        if section.entries.is_empty() {
                            return Err(ComposerError {
                                message: format!("empty submenu section: {}", section.id),
                            });
                        }
                        for action in &section.entries {
                            validate_action(context, action)?;
                            if !action_instances.insert(action.instance_key()) {
                                return Err(ComposerError {
                                    message: format!(
                                        "duplicate action instance: {}",
                                        action.instance_key()
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    if ordinary_root_count > 16 {
        return Err(ComposerError {
            message: format!(
                "ordinary root context-menu entries exceed the hard limit: {ordinary_root_count} > 16"
            ),
        });
    }
    if native_count != 1 {
        return Err(ComposerError {
            message: format!("expected exactly one system.native-menu, found {native_count}"),
        });
    }
    let last_is_native = plan
        .groups
        .last()
        .and_then(|group| group.entries.last())
        .is_some_and(|entry| {
            matches!(entry, MenuEntryPlan::Action(action) if action.action_id == "system.native-menu")
        });
    if !last_is_native {
        return Err(ComposerError {
            message: "system.native-menu must be the final root entry".to_string(),
        });
    }
    Ok(())
}

fn validate_action(
    context: &ContextSnapshot,
    action: &ResolvedAction,
) -> Result<(), ComposerError> {
    if action.label.trim().is_empty() {
        return Err(ComposerError {
            message: format!(
                "context-menu action has no accessible label: {}",
                action.action_id
            ),
        });
    }
    if !action.enabled
        && (action
            .disabled_reason_code
            .as_deref()
            .is_none_or(|reason| reason.trim().is_empty())
            || action
                .disabled_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty()))
    {
        return Err(ComposerError {
            message: format!(
                "disabled context-menu action has no accessible reason: {}",
                action.action_id
            ),
        });
    }
    if context.target_set.targets.is_empty()
        && matches!(action.scope, ActionScope::Entity | ActionScope::Collection)
    {
        return Err(ComposerError {
            message: format!(
                "entity/collection action leaked into background menu: {}",
                action.action_id
            ),
        });
    }
    if context.target_set.targets.len() > 1
        && action.target_rule.cardinality == TargetCardinality::One
    {
        return Err(ComposerError {
            message: format!(
                "single-target action leaked into multi-select menu: {}",
                action.action_id
            ),
        });
    }
    if action.risk == ActionRisk::High && action.root_group != RootGroup::Destructive {
        return Err(ComposerError {
            message: format!(
                "high-risk action outside destructive group: {}",
                action.action_id
            ),
        });
    }
    if action.action_id == "system.native-menu" && action.root_group != RootGroup::Native {
        return Err(ComposerError {
            message: "system.native-menu outside native group".to_string(),
        });
    }
    Ok(())
}

pub fn compose_default_menu(context: &ContextSnapshot) -> Result<MenuPlan, ComposerError> {
    ContextMenuComposer::new(default_providers()).compose(context)
}

pub fn compose_menu_with_fallback(context: &ContextSnapshot) -> MenuPlan {
    match compose_default_menu(context) {
        Ok(plan) => plan,
        Err(error) => {
            let mut plan = fallback_plan(context);
            plan.diagnostics.push(format!(
                "context-menu composer failed; using safe fallback: {error}"
            ));
            plan
        }
    }
}

fn fallback_plan(context: &ContextSnapshot) -> MenuPlan {
    let (_, target_kinds) = context_target_paths_and_kinds(context);
    let mut groups = Vec::new();
    let mut open_group = Vec::new();
    if context.target_set.targets.len() == 1
        && let Some(descriptor) = descriptor(context, "open.default")
    {
        open_group.push(MenuEntryPlan::Action(resolved_action(
            descriptor,
            context,
            CandidateAvailability::Available,
            ActionPayload::None,
        )));
    }
    if context.capabilities.reveal_available
        && let Some(descriptor) = descriptor(context, "system.reveal")
    {
        open_group.push(MenuEntryPlan::Action(resolved_action(
            descriptor,
            context,
            CandidateAvailability::Available,
            ActionPayload::None,
        )));
    }
    if !open_group.is_empty() {
        groups.push(MenuGroupPlan {
            id: RootGroup::OpenAndLaunch.id().to_string(),
            entries: open_group,
        });
    }
    if let Some(descriptor) = descriptor(context, "system.native-menu") {
        let availability = if context.capabilities.native_plane_available {
            CandidateAvailability::Available
        } else {
            CandidateAvailability::disabled(
                "NATIVE_PLANE_UNAVAILABLE",
                "系统原生菜单在当前环境不可用",
            )
        };
        groups.push(MenuGroupPlan {
            id: RootGroup::Native.id().to_string(),
            entries: vec![MenuEntryPlan::Action(resolved_action(
                descriptor,
                context,
                availability,
                ActionPayload::None,
            ))],
        });
    }
    MenuPlan {
        schema_version: CONTEXT_MENU_SCHEMA_VERSION,
        snapshot_id: context.snapshot_id.clone(),
        title: context.title(),
        target_kinds,
        groups,
        submenus: Vec::new(),
        diagnostics: vec!["safe fallback menu".to_string()],
    }
}

fn resolved_action(
    descriptor: ActionDescriptor,
    context: &ContextSnapshot,
    availability: CandidateAvailability,
    payload: ActionPayload,
) -> ResolvedAction {
    let (enabled, disabled_reason_code, disabled_reason) = match availability {
        CandidateAvailability::Available => (true, None, None),
        CandidateAvailability::Disabled {
            reason_code,
            reason,
        } => (false, Some(reason_code), Some(reason)),
        CandidateAvailability::Hidden { reason_code } => (
            false,
            Some(reason_code),
            Some("该动作在当前上下文不可用".to_string()),
        ),
    };
    let targets = if context.target_set.ordered_paths.is_empty() {
        vec![context.container.path.clone()]
    } else {
        context.target_set.ordered_paths.clone()
    };
    let risk = if descriptor.action_id == "file.move-to-trash" && targets.len() > 1 {
        ActionRisk::High
    } else {
        descriptor.risk
    };
    let root_group =
        if risk == ActionRisk::High || descriptor.confirmation == Confirmation::SystemOwned {
            if matches!(descriptor.plane, ActionPlane::OsNativeExtension) {
                RootGroup::Native
            } else {
                RootGroup::Destructive
            }
        } else {
            descriptor.root_group
        };
    ResolvedAction {
        action_id: descriptor.action_id,
        variant_key: None,
        label: descriptor.label,
        icon_key: descriptor.icon_key,
        scope: descriptor.scope,
        plane: descriptor.plane,
        owner: descriptor.owner,
        effect: descriptor.effect,
        risk,
        target_rule: descriptor.target_rule,
        confirmation: descriptor.confirmation,
        requires_confirmation: context.policy.confirm_high_risk
            && (risk.requires_confirmation()
                || descriptor.confirmation == Confirmation::Dangerous
                || descriptor.confirmation == Confirmation::SystemOwned),
        feedback: descriptor.feedback,
        root_group,
        section: descriptor.section,
        order: descriptor.order,
        targets,
        enabled,
        disabled_reason_code,
        disabled_reason,
        payload,
    }
}

fn context_target_paths_and_kinds(context: &ContextSnapshot) -> (Vec<String>, Vec<String>) {
    if context.target_set.ordered_paths.is_empty() {
        (
            vec![context.container.path.clone()],
            vec![context.container.kind.as_str().to_string()],
        )
    } else {
        (
            context.target_set.ordered_paths.clone(),
            context
                .target_set
                .targets
                .iter()
                .map(|target| target.kind.as_str().to_string())
                .collect(),
        )
    }
}

fn default_providers() -> Vec<Box<dyn ActionProvider>> {
    vec![
        Box::new(RefreshActionProvider),
        Box::new(CoreActionProvider),
        Box::new(OpenWithActionProvider),
        Box::new(ToolActionProvider),
        Box::new(ArchiveActionProvider),
        Box::new(AgentActionProvider),
        Box::new(VcsActionProvider),
        Box::new(SystemActionProvider),
    ]
}

struct RefreshActionProvider;
struct CoreActionProvider;
struct OpenWithActionProvider;
struct ToolActionProvider;
struct ArchiveActionProvider;
struct AgentActionProvider;
struct VcsActionProvider;
struct SystemActionProvider;

impl ActionProvider for RefreshActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        if !context.container.is_primary_target {
            return Vec::new();
        }
        vec![ActionCandidate::with_availability(
            "navigation.refresh",
            if context.capabilities.loading {
                CandidateAvailability::disabled("TARGET_LOADING", "正在加载当前目录")
            } else {
                CandidateAvailability::Available
            },
        )]
    }
}

impl ActionProvider for CoreActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        let mut candidates = Vec::new();
        if context.target_set.targets.len() == 1 {
            let target = &context.target_set.targets[0];
            let availability = if context.capabilities.loading {
                CandidateAvailability::disabled("TARGET_LOADING", "目标仍在加载")
            } else if !target.exists {
                CandidateAvailability::disabled("TARGET_MISSING", "目标已不存在")
            } else {
                CandidateAvailability::Available
            };
            candidates.push(ActionCandidate::with_availability(
                "open.default",
                availability,
            ));
        }
        if !context.target_set.targets.is_empty() {
            candidates.push(if context.capabilities.supports_transfer_ops {
                ActionCandidate::available("clipboard.copy")
            } else {
                ActionCandidate::with_availability(
                    "clipboard.copy",
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标不支持文件复制"),
                )
            });
            candidates.push(if context.capabilities.supports_transfer_ops {
                ActionCandidate::available("clipboard.copy-to")
            } else {
                ActionCandidate::with_availability(
                    "clipboard.copy-to",
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标不支持复制到"),
                )
            });
            candidates.push(ActionCandidate::available("clipboard.copy-path"));
            candidates.push(
                if context.capabilities.supports_transfer_ops && context.capabilities.supports_cut {
                    ActionCandidate::available("clipboard.cut")
                } else {
                    ActionCandidate::with_availability(
                        "clipboard.cut",
                        CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标不支持剪切"),
                    )
                },
            );
        }
        if context.container.is_primary_target {
            candidates.push(
                if context.capabilities.can_paste && context.capabilities.supports_transfer_ops {
                    ActionCandidate::available("clipboard.paste")
                } else {
                    ActionCandidate::with_availability(
                        "clipboard.paste",
                        CandidateAvailability::disabled(
                            "CLIPBOARD_EMPTY",
                            "剪贴板中没有可粘贴的文件",
                        ),
                    )
                },
            );
            let create_availability =
                if context.capabilities.can_create && context.capabilities.supports_transfer_ops {
                    CandidateAvailability::Available
                } else {
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前目录不可写")
                };
            candidates.push(ActionCandidate::with_availability(
                "file.new-folder",
                create_availability.clone(),
            ));
            candidates.push(ActionCandidate::with_availability(
                "file.new-file",
                create_availability,
            ));
        }
        if context.target_set.targets.len() == 1 {
            candidates.push(if context.capabilities.supports_favorite {
                if context.capabilities.is_favorite {
                    ActionCandidate::available("favorite.remove")
                } else {
                    ActionCandidate::available("favorite.add")
                }
            } else {
                ActionCandidate::with_availability(
                    "favorite.add",
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标不支持收藏"),
                )
            });
            let rename_availability = if context.capabilities.supports_rename {
                CandidateAvailability::Available
            } else {
                CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标不支持重命名")
            };
            candidates.push(ActionCandidate::with_availability(
                "file.rename",
                rename_availability,
            ));
            if context.capabilities.is_shell_namespace {
                candidates.push(if context.capabilities.supports_delete {
                    ActionCandidate::available("device.delete")
                } else {
                    ActionCandidate::with_availability(
                        "device.delete",
                        CandidateAvailability::disabled(
                            "PERMISSION_DENIED",
                            "此设备项目不支持删除",
                        ),
                    )
                });
            }
        }
        if !context.target_set.targets.is_empty() && !context.capabilities.is_shell_namespace {
            candidates.push(if context.capabilities.supports_delete {
                ActionCandidate::available("file.move-to-trash")
            } else {
                ActionCandidate::with_availability(
                    "file.move-to-trash",
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标不支持删除"),
                )
            });
        }
        candidates
    }
}

impl ActionProvider for OpenWithActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        if context.single_open_with_target().is_none() {
            return Vec::new();
        }

        let mut candidates = Vec::new();
        match context.open_with.status {
            OpenWithCatalogStatus::Loading => {
                candidates.push(
                    ActionCandidate::with_availability(
                        "open.with",
                        CandidateAvailability::disabled(
                            "OPEN_WITH_LOADING",
                            "正在查找可用于打开此文件的应用",
                        ),
                    )
                    .variant("status:loading")
                    .label("正在查找兼容应用…")
                    .icon_key("open-with")
                    .order(10),
                );
            }
            OpenWithCatalogStatus::Unavailable => {
                candidates.push(
                    ActionCandidate::with_availability(
                        "open.with",
                        CandidateAvailability::disabled(
                            "OPEN_WITH_NO_CANDIDATES",
                            "当前文件类型没有已发现的兼容应用",
                        ),
                    )
                    .variant("status:unavailable")
                    .label("没有可用的兼容应用")
                    .icon_key("open-with")
                    .order(10),
                );
            }
            OpenWithCatalogStatus::Ready => {
                let applications =
                    normalized_open_with_applications(&context.open_with.applications);
                if applications.is_empty() {
                    candidates.push(
                        ActionCandidate::with_availability(
                            "open.with",
                            CandidateAvailability::disabled(
                                "OPEN_WITH_NO_CANDIDATES",
                                "当前文件类型没有已发现的兼容应用",
                            ),
                        )
                        .variant("status:unavailable")
                        .label("没有可用的兼容应用")
                        .icon_key("open-with")
                        .order(10),
                    );
                } else {
                    for (index, application) in applications.into_iter().enumerate() {
                        let label = if application.is_default {
                            format!("默认应用 — {}", application.display_name)
                        } else {
                            application.display_name.clone()
                        };
                        let order = 10u16.saturating_add(
                            u16::try_from(index).unwrap_or(u16::MAX).saturating_mul(10),
                        );
                        candidates.push(
                            ActionCandidate::available("open.with")
                                .variant(format!("application:{}", application.application_id))
                                .label(label)
                                .icon_key("open-with")
                                .order(order)
                                .payload(ActionPayload::Application {
                                    application_id: application.application_id,
                                }),
                        );
                    }
                }
            }
        }

        candidates.push(
            ActionCandidate::available("open.with.choose-other")
                .label("选择其他应用…")
                .icon_key("open-with")
                .order(10_000),
        );
        candidates
    }
}

fn normalized_open_with_applications(
    applications: &[OpenWithApplication],
) -> Vec<OpenWithApplication> {
    let mut unique = Vec::<OpenWithApplication>::new();
    for application in applications {
        let application_id = application.application_id.trim();
        let display_name = application.display_name.trim();
        if application_id.is_empty() || display_name.is_empty() || !application.is_available {
            continue;
        }
        if let Some(existing) = unique
            .iter_mut()
            .find(|existing| existing.application_id.eq_ignore_ascii_case(application_id))
        {
            existing.is_default |= application.is_default;
            existing.is_recent |= application.is_recent;
            continue;
        }
        unique.push(OpenWithApplication {
            application_id: application_id.to_string(),
            display_name: display_name.to_string(),
            is_default: application.is_default,
            is_recent: application.is_recent,
            is_available: true,
            unavailable_reason: None,
        });
    }

    let default_index = unique.iter().position(|application| application.is_default);
    let default = default_index.map(|index| unique.remove(index));
    let recent_limit = 5usize;
    let mut recent_indexes = Vec::new();
    for (index, application) in unique.iter().enumerate() {
        if application.is_recent && recent_indexes.len() < recent_limit {
            recent_indexes.push(index);
        }
    }
    let recent_ids = recent_indexes
        .iter()
        .map(|index| unique[*index].application_id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut ordered = Vec::with_capacity(unique.len() + usize::from(default.is_some()));
    if let Some(mut default) = default {
        default.is_default = true;
        default.is_recent = false;
        ordered.push(default);
    }
    for index in recent_indexes {
        let mut application = unique[index].clone();
        application.is_recent = true;
        ordered.push(application);
    }
    for application in unique {
        if recent_ids.contains(&application.application_id.to_ascii_lowercase()) {
            continue;
        }
        let mut application = application;
        application.is_recent = false;
        ordered.push(application);
    }
    ordered
}

impl ActionProvider for ToolActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        if context.capabilities.is_shell_namespace {
            return Vec::new();
        }

        let definitions = builtin_tool_definitions(ToolPlatform::from_context(context.platform));
        let background_target_set = context.target_set.targets.is_empty().then(|| {
            let target = ContextTarget::new(
                context.container.path.clone(),
                context.container.display_name.clone(),
                context.container.kind,
            );
            TargetSet::new(
                Some(target.id.clone()),
                vec![target],
                context.container.path.clone(),
            )
        });
        let tool_target_set = background_target_set
            .as_ref()
            .unwrap_or(&context.target_set);
        let mut candidates = Vec::new();
        if context.tool_catalog.status == ToolCatalogStatus::Loading {
            candidates.push(
                ActionCandidate::with_availability(
                    "open.in",
                    CandidateAvailability::disabled(
                        "TOOL_DISCOVERY_LOADING",
                        "正在查找已安装的工具",
                    ),
                )
                .variant("status:loading")
                .label("正在查找已安装的工具…")
                .icon_key("open-in")
                .order(15),
            );
        } else {
            let tools = available_tools(
                context.platform,
                &definitions,
                &context.tool_catalog,
                &context.tool_policy,
                tool_target_set,
                ToolRole::Open,
            );
            for (index, tool) in tools.into_iter().enumerate() {
                candidates.push(
                    ActionCandidate::available("open.in")
                        .variant(format!("tool:{}:open", tool.tool_id))
                        .label(format!("在 {} 中打开", tool.display_name))
                        .icon_key("open-in")
                        .order(100u16.saturating_add(
                            u16::try_from(index).unwrap_or(u16::MAX).saturating_mul(10),
                        ))
                        .payload(ActionPayload::Tool {
                            tool_id: tool.tool_id,
                        }),
                );
            }
        }

        if compare_targets_eligible(context) {
            if context.tool_catalog.status == ToolCatalogStatus::Loading {
                candidates.push(
                    ActionCandidate::with_availability(
                        "compare.with",
                        CandidateAvailability::disabled(
                            "TOOL_DISCOVERY_LOADING",
                            "正在查找已安装的比较工具",
                        ),
                    )
                    .variant("status:loading")
                    .label("正在查找比较工具…")
                    .icon_key("compare-with")
                    .order(25),
                );
            } else {
                let tools = available_tools(
                    context.platform,
                    &definitions,
                    &context.tool_catalog,
                    &context.tool_policy,
                    tool_target_set,
                    ToolRole::Compare,
                );
                for (index, tool) in tools.into_iter().enumerate() {
                    candidates.push(
                        ActionCandidate::available("compare.with")
                            .variant(format!("tool:{}:compare", tool.tool_id))
                            .label(format!("使用 {} 比较", tool.display_name))
                            .icon_key("compare-with")
                            .order(100u16.saturating_add(
                                u16::try_from(index).unwrap_or(u16::MAX).saturating_mul(10),
                            ))
                            .payload(ActionPayload::Tool {
                                tool_id: tool.tool_id,
                            }),
                    );
                }
            }
        }
        candidates
    }
}

fn compare_targets_eligible(context: &ContextSnapshot) -> bool {
    context.target_set.targets.len() == 2
        && context
            .target_set
            .targets
            .iter()
            .all(|target| target.kind.is_file_like() && target.exists && target.readable)
}

impl ActionProvider for ArchiveActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        let mut candidates = Vec::new();
        if context.target_set.targets.len() == 1 {
            if context.single_target_can_compress() {
                let availability = if context.capabilities.supports_transfer_ops {
                    CandidateAvailability::Available
                } else {
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前目标无法压缩")
                };
                candidates.push(ActionCandidate::with_availability(
                    "archive.compress.zip",
                    availability.clone(),
                ));
                candidates.push(ActionCandidate::with_availability(
                    "archive.compress.tar-gz",
                    availability,
                ));
            }
            if context.single_target_is_archive() {
                let availability = if context.capabilities.supports_transfer_ops {
                    CandidateAvailability::Available
                } else {
                    CandidateAvailability::disabled("PERMISSION_DENIED", "当前压缩包无法解压")
                };
                candidates.push(ActionCandidate::with_availability(
                    "archive.extract-here",
                    availability.clone(),
                ));
                candidates.push(ActionCandidate::with_availability(
                    "archive.extract-to",
                    availability,
                ));
            }
        }
        candidates
    }
}

impl ActionProvider for AgentActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        let mut candidates = Vec::new();
        let agent_availability = if context.capabilities.agent_available {
            CandidateAvailability::Available
        } else {
            CandidateAvailability::disabled("AGENT_NOT_CONFIGURED", "尚未配置 Agent")
        };
        if !context.target_set.targets.is_empty() {
            candidates.push(ActionCandidate::with_availability(
                "agent.ask",
                agent_availability.clone(),
            ));
        }
        if context.all_files_text_like() {
            candidates.push(ActionCandidate::with_availability(
                "agent.explain-file",
                agent_availability.clone(),
            ));
        }
        if context.all_targets_match(|target| target.kind == TargetKind::Directory) {
            candidates.push(ActionCandidate::with_availability(
                "agent.summarize-folder",
                agent_availability.clone(),
            ));
        }
        if context.capabilities.git_available && !context.target_set.targets.is_empty() {
            candidates.push(ActionCandidate::with_availability(
                "agent.review-changes",
                agent_availability.clone(),
            ));
            candidates.push(ActionCandidate::with_availability(
                "agent.generate-commit-message",
                agent_availability,
            ));
        }
        candidates.push(ActionCandidate::available("agent.configure"));
        candidates
    }
}

impl ActionProvider for VcsActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        if context.capabilities.is_shell_namespace {
            return Vec::new();
        }

        let mut candidates = Vec::new();
        if context.capabilities.loading {
            for action_id in [
                "git.add",
                "git.unstage",
                "git.commit",
                "git.pull",
                "git.push",
                "git.fetch-current",
                "git.fetch-all",
                "git.remotes.manage",
                "git.diff",
                "git.log",
                "git.rebase.interactive",
                "git.discard",
                "git.clean-untracked",
                "svn.update",
                "svn.add",
                "svn.commit",
                "svn.lock",
                "svn.unlock",
                "svn.diff",
                "svn.log",
                "svn.revert",
                "svn.cleanup",
            ] {
                candidates.push(ActionCandidate::with_availability(
                    action_id,
                    CandidateAvailability::disabled("TARGET_LOADING", "正在读取仓库状态"),
                ));
            }
            return candidates;
        }

        if let Some(repository) = common_vcs_repository(context, VcsMenuTool::Git) {
            let links = vcs_links(context, VcsMenuTool::Git);
            let operation_active = repository.operation.is_some();
            if let Some(operation) = repository.operation.as_ref() {
                if operation.resolve_available || repository.has_conflicts() {
                    candidates.push(vcs_candidate("git.conflicts.resolve", repository));
                }
                if operation.continue_available {
                    let mut candidate = ActionCandidate::available("git.operation.continue");
                    if operation.custom_rebase {
                        candidate = candidate.label("继续 Kite 变基");
                    }
                    candidates.push(with_vcs_payload(candidate, repository));
                }
                if operation.skip_available {
                    candidates.push(vcs_candidate("git.operation.skip", repository));
                }
                if operation.abort_available {
                    candidates.push(vcs_candidate("git.operation.abort", repository));
                }
            } else if repository.has_conflicts() {
                candidates.push(vcs_candidate("git.conflicts.resolve", repository));
            }

            if !operation_active {
                let has_staged = repository.has_staged_changes();
                let has_unstaged = links.iter().any(|link| link.unstaged);
                let has_selected_staged = links.iter().any(|link| link.staged == Some(true));
                let has_selected_unstaged = links.iter().any(|link| link.unstaged);
                let has_selected_changes = has_selected_staged || has_selected_unstaged;
                let is_container = context.container.is_primary_target;
                let is_repository_root = links
                    .iter()
                    .any(|link| link.relation == VcsTargetRelation::Root);
                let is_inside_directory = links
                    .iter()
                    .any(|link| link.relation == VcsTargetRelation::InsideDirectory);

                if has_unstaged {
                    candidates.push(vcs_candidate("git.add", repository));
                }
                if has_selected_staged {
                    candidates.push(vcs_candidate("git.unstage", repository));
                }
                if has_staged {
                    candidates.push(vcs_candidate("git.commit", repository));
                }
                if has_selected_unstaged {
                    candidates.push(vcs_candidate("git.discard", repository));
                }
                if has_selected_changes || repository.has_conflicts() {
                    candidates.push(vcs_candidate("git.diff", repository));
                }
                candidates.push(vcs_candidate("git.log", repository));

                if repository.has_upstream() {
                    candidates.push(vcs_candidate("git.pull", repository));
                    candidates.push(vcs_candidate("git.push", repository));
                    candidates.push(vcs_candidate("git.fetch-current", repository));
                } else {
                    candidates.push(with_vcs_payload(
                        ActionCandidate::with_availability(
                            "git.pull",
                            CandidateAvailability::disabled(
                                "UPSTREAM_NOT_CONFIGURED",
                                "当前分支没有配置上游",
                            ),
                        ),
                        repository,
                    ));
                    candidates.push(with_vcs_payload(
                        ActionCandidate::with_availability(
                            "git.push",
                            CandidateAvailability::disabled(
                                "UPSTREAM_NOT_CONFIGURED",
                                "当前分支没有配置上游",
                            ),
                        ),
                        repository,
                    ));
                }
                candidates.push(vcs_candidate("git.fetch-all", repository));
                candidates.push(vcs_candidate("git.remotes.manage", repository));
                candidates.push(vcs_candidate("git.rebase.interactive", repository));
                if (is_container || is_repository_root || is_inside_directory)
                    && repository.changes.untracked > 0
                {
                    candidates.push(vcs_candidate("git.clean-untracked", repository));
                }
            } else {
                // Keep read-only navigation available while an operation is
                // active; recovery actions above are the primary controls.
                candidates.push(vcs_candidate("git.log", repository));
                if repository.has_staged_changes() || repository.has_unstaged_changes() {
                    candidates.push(vcs_candidate("git.diff", repository));
                }
            }
        } else if context.vcs_menu.git_available && context.container.is_primary_target {
            candidates.push(ActionCandidate::available("git.clone"));
            candidates.push(ActionCandidate::available("git.init"));
        }

        if let Some(repository) = common_vcs_repository(context, VcsMenuTool::Svn) {
            let links = vcs_links(context, VcsMenuTool::Svn);
            let cleanup_required = repository
                .svn
                .as_ref()
                .is_some_and(|svn| svn.cleanup_required);
            if cleanup_required {
                candidates.push(vcs_candidate("svn.cleanup", repository));
            }
            if repository.has_conflicts() {
                candidates.push(vcs_candidate("svn.resolve-conflicts", repository));
            }
            candidates.push(vcs_candidate("svn.update", repository));
            if links
                .iter()
                .any(|link| link.relation == VcsTargetRelation::SvnUnversioned)
            {
                candidates.push(vcs_candidate("svn.add", repository));
            }
            if repository.changes.unstaged > 0
                || repository
                    .svn
                    .as_ref()
                    .is_some_and(|svn| svn.property_modified)
            {
                candidates.push(vcs_candidate("svn.commit", repository));
                candidates.push(vcs_candidate("svn.diff", repository));
                candidates.push(vcs_candidate("svn.revert", repository));
            }
            candidates.push(vcs_candidate("svn.log", repository));
            candidates.push(vcs_candidate("svn.lock", repository));
            candidates.push(vcs_candidate("svn.unlock", repository));
        }
        candidates
    }
}

fn context_vcs_paths(context: &ContextSnapshot) -> Vec<String> {
    if context.target_set.targets.is_empty() {
        vec![context.container.path.clone()]
    } else {
        context.target_set.ordered_paths.clone()
    }
}

fn vcs_target_for_path<'a>(
    context: &'a ContextSnapshot,
    path: &str,
) -> Option<&'a crate::vcs_menu::VcsTargetMenuState> {
    let key = context_path_key(Path::new(path));
    context
        .vcs_menu
        .targets
        .iter()
        .find(|target| context_path_key(Path::new(&target.path)) == key)
}

fn vcs_links(context: &ContextSnapshot, tool: VcsMenuTool) -> Vec<&VcsTargetLink> {
    context_vcs_paths(context)
        .iter()
        .filter_map(|path| vcs_target_for_path(context, path))
        .flat_map(|target| target.links.iter().filter(move |link| link.tool == tool))
        .collect()
}

fn common_vcs_repository(
    context: &ContextSnapshot,
    tool: VcsMenuTool,
) -> Option<&VcsMenuRepository> {
    let paths = context_vcs_paths(context);
    let mut repository_id: Option<&str> = None;
    for path in paths {
        let target = vcs_target_for_path(context, &path)?;
        let links = target
            .links
            .iter()
            .filter(|link| link.tool == tool)
            .collect::<Vec<_>>();
        if links.is_empty() {
            return None;
        }
        for link in links {
            match repository_id {
                Some(existing) if existing != link.repository_id => return None,
                None => repository_id = Some(&link.repository_id),
                _ => {}
            }
        }
    }
    repository_id.and_then(|id| context.vcs_menu.repository(id))
}

fn vcs_candidate(action_id: &str, repository: &VcsMenuRepository) -> ActionCandidate {
    with_vcs_payload(ActionCandidate::available(action_id), repository)
}

fn with_vcs_payload(candidate: ActionCandidate, repository: &VcsMenuRepository) -> ActionCandidate {
    candidate.payload(ActionPayload::Vcs {
        repository_root: repository.repository_root.clone(),
        tool: repository.tool.as_str().to_string(),
        state_revision: repository.state_revision.clone(),
    })
}

fn context_path_key(path: &Path) -> String {
    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let value = canonical.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        value.replace('/', "\\").to_ascii_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        value.into_owned()
    }
}

impl ActionProvider for SystemActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        let mut candidates = Vec::new();
        if context.capabilities.quick_look_available && !context.target_set.targets.is_empty() {
            candidates.push(ActionCandidate::available("open.quick-look"));
        }
        if context.capabilities.reveal_available
            && (!context.target_set.targets.is_empty() || context.container.is_primary_target)
        {
            candidates.push(ActionCandidate::available("system.reveal"));
        }
        if context.capabilities.share_available
            && (!context.target_set.targets.is_empty() || context.container.is_primary_target)
        {
            candidates.push(ActionCandidate::available("system.share"));
        }
        if context.capabilities.properties_available && context.target_set.targets.len() == 1 {
            candidates.push(ActionCandidate::available("system.properties"));
        }
        if context.capabilities.mindmap_available && context.target_set.targets.len() == 1 {
            candidates.push(ActionCandidate::available("tool.kite.mindmap"));
        }
        if context.capabilities.mindmap_import_available && context.target_set.targets.len() == 1 {
            candidates.push(ActionCandidate::available("tool.kite.mindmap-import"));
        }
        candidates.push(ActionCandidate::with_availability(
            "system.native-menu",
            if context.capabilities.native_plane_available {
                CandidateAvailability::Available
            } else {
                CandidateAvailability::disabled(
                    "NATIVE_PLANE_UNAVAILABLE",
                    "系统原生菜单在当前环境不可用",
                )
            },
        ));
        candidates
    }
}

fn descriptor(context: &ContextSnapshot, action_id: &str) -> Option<ActionDescriptor> {
    let platform = context.platform;
    let file_label = "打开";
    let reveal_label = if platform.is_macos() {
        "在 Finder 中显示"
    } else {
        "在 Explorer 中显示"
    };
    let native_label = if platform.is_macos() {
        "系统原生操作…"
    } else {
        "显示更多选项 (Show more options)"
    };
    let log_label = match context.primary_kind() {
        Some(TargetKind::Directory) => "查看目录历史",
        _ => "查看文件历史",
    };
    let action = match action_id {
        "navigation.refresh" => d(
            action_id,
            "刷新",
            ActionScope::Container,
            ActionEffect::Read,
            ActionRisk::None,
            TargetRule::container(),
            RootGroup::Refresh,
            MenuSection::Primary,
            None,
            0,
            "refresh",
        ),
        "open.default" => d(
            action_id,
            file_label,
            ActionScope::Entity,
            ActionEffect::Navigation,
            ActionRisk::Low,
            TargetRule::new(
                &[
                    TargetKind::File,
                    TargetKind::Directory,
                    TargetKind::Archive,
                    TargetKind::DeviceContainer,
                    TargetKind::DeviceItem,
                ],
                TargetCardinality::One,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            None,
            0,
            "open",
        ),
        "open.with" => d(
            action_id,
            "打开方式",
            ActionScope::Entity,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            Some(SubmenuId::OpenWith),
            10,
            "open-with",
        ),
        "open.with.choose-other" => d(
            action_id,
            "选择其他应用…",
            ActionScope::Entity,
            ActionEffect::SystemUi,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            Some(SubmenuId::OpenWith),
            10_000,
            "open-with",
        ),
        "open.in" => d(
            action_id,
            "在工具中打开",
            ActionScope::Workspace,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(
                &[
                    TargetKind::File,
                    TargetKind::Directory,
                    TargetKind::Archive,
                    TargetKind::RepositoryRoot,
                    TargetKind::WorkspaceRoot,
                ],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            Some(SubmenuId::OpenIn),
            15,
            "open-in",
        ),
        "compare.with" => d(
            action_id,
            "比较…",
            ActionScope::Collection,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Archive],
                TargetCardinality::ExactlyTwo,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            Some(SubmenuId::CompareWith),
            25,
            "compare-with",
        ),
        "open.quick-look" => d(
            action_id,
            "快速查看",
            ActionScope::Collection,
            ActionEffect::SystemUi,
            ActionRisk::None,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            None,
            10,
            "quick-look",
        ),
        "clipboard.copy" => d(
            action_id,
            "复制",
            ActionScope::Collection,
            ActionEffect::Clipboard,
            ActionRisk::Low,
            TargetRule::new(
                &[
                    TargetKind::File,
                    TargetKind::Directory,
                    TargetKind::Archive,
                    TargetKind::DeviceItem,
                ],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::Transfer,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            0,
            "copy",
        ),
        "clipboard.cut" => d(
            action_id,
            "剪切",
            ActionScope::Collection,
            ActionEffect::Clipboard,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::Transfer,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            10,
            "cut",
        ),
        "clipboard.copy-to" => d(
            action_id,
            "拷贝到…",
            ActionScope::Collection,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::Transfer,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            20,
            "copy-to",
        ),
        "clipboard.paste" => d(
            action_id,
            "粘贴",
            ActionScope::Container,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::container(),
            RootGroup::Transfer,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            30,
            "paste",
        ),
        "clipboard.copy-path" => d(
            action_id,
            "复制完整路径",
            ActionScope::Collection,
            ActionEffect::Clipboard,
            ActionRisk::None,
            TargetRule::new(
                &[
                    TargetKind::File,
                    TargetKind::Directory,
                    TargetKind::Archive,
                    TargetKind::DeviceItem,
                ],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::Transfer,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            40,
            "copy-path",
        ),
        "file.new-folder" => d(
            action_id,
            "新建文件夹…",
            ActionScope::Container,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::container(),
            RootGroup::CreateAndOrganize,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            50,
            "new-folder",
        ),
        "file.new-file" => d(
            action_id,
            "新建文件…",
            ActionScope::Container,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::container(),
            RootGroup::CreateAndOrganize,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            60,
            "new-file",
        ),
        "favorite.add" => d(
            action_id,
            "添加到收藏",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::CreateAndOrganize,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            70,
            "favorite",
        ),
        "favorite.remove" => d(
            action_id,
            "从收藏移除",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::CreateAndOrganize,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            70,
            "favorite",
        ),
        "file.rename" => d(
            action_id,
            "重命名…",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Medium,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::CreateAndOrganize,
            MenuSection::Primary,
            Some(SubmenuId::FileActions),
            80,
            "rename",
        ),
        "file.move-to-trash" => d(
            action_id,
            "移到废纸篓",
            ActionScope::Collection,
            ActionEffect::FileMutation,
            ActionRisk::Medium,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::Destructive,
            MenuSection::Primary,
            None,
            0,
            "trash",
        ),
        "device.delete" => d(
            action_id,
            "删除设备项目",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::High,
            TargetRule::new(&[TargetKind::DeviceItem], TargetCardinality::One),
            RootGroup::Destructive,
            MenuSection::Primary,
            None,
            0,
            "trash",
        ),
        "archive.compress.zip" => d(
            action_id,
            "ZIP (.zip)",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::Archive,
            MenuSection::Primary,
            Some(SubmenuId::Archive),
            0,
            "archive",
        ),
        "archive.compress.tar-gz" => d(
            action_id,
            "TAR.GZ (.tar.gz)",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::Archive,
            MenuSection::Primary,
            Some(SubmenuId::Archive),
            10,
            "archive",
        ),
        "archive.extract-here" => d(
            action_id,
            "解压到当前文件夹",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(&[TargetKind::Archive], TargetCardinality::One),
            RootGroup::Archive,
            MenuSection::Primary,
            Some(SubmenuId::Archive),
            20,
            "extract",
        ),
        "archive.extract-to" => d(
            action_id,
            "解压到…",
            ActionScope::Entity,
            ActionEffect::FileMutation,
            ActionRisk::Low,
            TargetRule::new(&[TargetKind::Archive], TargetCardinality::One),
            RootGroup::Archive,
            MenuSection::Primary,
            Some(SubmenuId::Archive),
            30,
            "extract",
        ),
        "agent.ask" => d(
            action_id,
            "询问 Agent…",
            ActionScope::Collection,
            ActionEffect::Read,
            ActionRisk::None,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::AgentAndTools,
            MenuSection::Primary,
            Some(SubmenuId::Agent),
            0,
            "agent",
        ),
        "agent.explain-file" => d(
            action_id,
            "解释此文件…",
            ActionScope::Collection,
            ActionEffect::Read,
            ActionRisk::None,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::AgentAndTools,
            MenuSection::Primary,
            Some(SubmenuId::Agent),
            10,
            "agent",
        ),
        "agent.summarize-folder" => d(
            action_id,
            "总结此文件夹…",
            ActionScope::Collection,
            ActionEffect::Read,
            ActionRisk::None,
            TargetRule::new(&[TargetKind::Directory], TargetCardinality::OneOrMore),
            RootGroup::AgentAndTools,
            MenuSection::Primary,
            Some(SubmenuId::Agent),
            20,
            "agent",
        ),
        "agent.review-changes" => d(
            action_id,
            "审查选中变更…",
            ActionScope::Collection,
            ActionEffect::Read,
            ActionRisk::None,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::AgentAndTools,
            MenuSection::Changes,
            Some(SubmenuId::Agent),
            30,
            "agent",
        ),
        "agent.generate-commit-message" => d(
            action_id,
            "生成提交说明…",
            ActionScope::Collection,
            ActionEffect::Read,
            ActionRisk::None,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::AgentAndTools,
            MenuSection::Changes,
            Some(SubmenuId::Agent),
            40,
            "agent",
        ),
        "agent.configure" => d(
            action_id,
            "配置 Agent…",
            ActionScope::System,
            ActionEffect::SystemUi,
            ActionRisk::None,
            TargetRule::any(),
            RootGroup::AgentAndTools,
            MenuSection::Primary,
            Some(SubmenuId::Agent),
            90,
            "settings",
        ),
        "git.open-client" => git_d(
            action_id,
            "打开 Git 客户端",
            ActionRisk::None,
            MenuSection::Primary,
            0,
            "vcs",
        ),
        "git.clone" => git_container_d(
            action_id,
            "克隆仓库…",
            ActionRisk::Low,
            MenuSection::Primary,
            10,
        ),
        "git.init" => git_container_d(
            action_id,
            "初始化仓库",
            ActionRisk::Low,
            MenuSection::Primary,
            20,
        ),
        "git.conflicts.resolve" => git_d(
            action_id,
            "解决冲突…",
            ActionRisk::Low,
            MenuSection::Recovery,
            0,
            "vcs",
        ),
        "git.operation.continue" => git_d(
            action_id,
            "继续当前 Git 操作",
            ActionRisk::Medium,
            MenuSection::Recovery,
            10,
            "vcs",
        ),
        "git.operation.skip" => git_d(
            action_id,
            "跳过当前 Git 操作步骤",
            ActionRisk::Medium,
            MenuSection::Recovery,
            20,
            "vcs",
        ),
        "git.operation.abort" => git_d(
            action_id,
            "中止当前 Git 操作",
            ActionRisk::Medium,
            MenuSection::Recovery,
            30,
            "vcs",
        ),
        "git.add" => git_d(
            action_id,
            "添加",
            ActionRisk::Medium,
            MenuSection::Changes,
            0,
            "vcs",
        ),
        "git.unstage" => git_d(
            action_id,
            "取消暂存",
            ActionRisk::Medium,
            MenuSection::Changes,
            10,
            "vcs",
        ),
        "git.commit" => git_d(
            action_id,
            "提交…",
            ActionRisk::Medium,
            MenuSection::Changes,
            20,
            "vcs",
        ),
        "git.pull" => git_d(
            action_id,
            "拉取…",
            ActionRisk::Medium,
            MenuSection::Sync,
            0,
            "vcs",
        ),
        "git.push" => git_d(
            action_id,
            "推送…",
            ActionRisk::Medium,
            MenuSection::Sync,
            10,
            "vcs",
        ),
        "git.fetch-current" => git_d(
            action_id,
            "获取当前远程",
            ActionRisk::Medium,
            MenuSection::Sync,
            20,
            "vcs",
        ),
        "git.fetch-all" => git_d(
            action_id,
            "获取全部远程",
            ActionRisk::Medium,
            MenuSection::Sync,
            30,
            "vcs",
        ),
        "git.remotes.manage" => git_d(
            action_id,
            "管理远程…",
            ActionRisk::Medium,
            MenuSection::Sync,
            40,
            "vcs",
        ),
        "git.diff" => git_d(
            action_id,
            "显示差异",
            ActionRisk::None,
            MenuSection::History,
            0,
            "vcs",
        ),
        "git.log" => d(
            action_id,
            log_label,
            ActionScope::Repository,
            ActionEffect::VcsRead,
            ActionRisk::None,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::Vcs,
            MenuSection::History,
            Some(SubmenuId::Git),
            10,
            "vcs",
        ),
        "git.rebase.interactive" => git_d(
            action_id,
            "交互式 Rebase…",
            ActionRisk::High,
            MenuSection::Advanced,
            0,
            "vcs",
        ),
        "git.discard" => git_d(
            action_id,
            "放弃更改…",
            ActionRisk::High,
            MenuSection::Recovery,
            0,
            "vcs",
        ),
        "git.clean-untracked" => git_d(
            action_id,
            "清理未跟踪文件…",
            ActionRisk::High,
            MenuSection::Recovery,
            10,
            "vcs",
        ),
        "svn.update" => svn_d(action_id, "更新", ActionRisk::Medium, MenuSection::Sync, 0),
        "svn.add" => svn_d(
            action_id,
            "添加",
            ActionRisk::Medium,
            MenuSection::Changes,
            0,
        ),
        "svn.commit" => svn_d(
            action_id,
            "提交…",
            ActionRisk::Medium,
            MenuSection::Changes,
            10,
        ),
        "svn.lock" => svn_d(
            action_id,
            "锁定",
            ActionRisk::Medium,
            MenuSection::Changes,
            20,
        ),
        "svn.unlock" => svn_d(
            action_id,
            "解锁",
            ActionRisk::Medium,
            MenuSection::Changes,
            30,
        ),
        "svn.diff" => svn_d(
            action_id,
            "显示差异",
            ActionRisk::None,
            MenuSection::History,
            0,
        ),
        "svn.log" => svn_d(
            action_id,
            "显示日志",
            ActionRisk::None,
            MenuSection::History,
            10,
        ),
        "svn.revert" => svn_d(
            action_id,
            "还原…",
            ActionRisk::High,
            MenuSection::Recovery,
            0,
        ),
        "svn.cleanup" => svn_d(
            action_id,
            "清理工作副本",
            ActionRisk::Medium,
            MenuSection::Advanced,
            0,
        ),
        "svn.resolve-conflicts" => svn_d(
            action_id,
            "解决冲突…",
            ActionRisk::Low,
            MenuSection::Recovery,
            5,
        ),
        "system.reveal" => d(
            action_id,
            reveal_label,
            ActionScope::Collection,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::System,
            MenuSection::Primary,
            Some(SubmenuId::System),
            0,
            "reveal",
        ),
        "system.share" => d(
            action_id,
            "分享…",
            ActionScope::Collection,
            ActionEffect::SystemUi,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
                TargetCardinality::OneOrMore,
            ),
            RootGroup::System,
            MenuSection::Primary,
            Some(SubmenuId::System),
            10,
            "share",
        ),
        "system.properties" => d(
            action_id,
            if platform.is_macos() {
                "显示简介"
            } else {
                "属性"
            },
            ActionScope::Entity,
            ActionEffect::SystemUi,
            ActionRisk::None,
            TargetRule::new(
                &[
                    TargetKind::File,
                    TargetKind::Directory,
                    TargetKind::Archive,
                    TargetKind::DeviceContainer,
                    TargetKind::DeviceItem,
                ],
                TargetCardinality::One,
            ),
            RootGroup::System,
            MenuSection::Primary,
            Some(SubmenuId::System),
            20,
            "properties",
        ),
        "tool.kite.text-editor" => d(
            action_id,
            "在 Kite 编辑器中打开",
            ActionScope::Entity,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(
                &[TargetKind::File, TargetKind::Archive],
                TargetCardinality::One,
            ),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            None,
            20,
            "editor",
        ),
        "tool.kite.mindmap" => d(
            action_id,
            "使用 Mindmap 编辑",
            ActionScope::Entity,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(&[TargetKind::File], TargetCardinality::One),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            None,
            30,
            "mindmap",
        ),
        "tool.kite.mindmap-import" => d(
            action_id,
            "转换为 Mindmap…",
            ActionScope::Entity,
            ActionEffect::ProcessLaunch,
            ActionRisk::Low,
            TargetRule::new(&[TargetKind::File], TargetCardinality::One),
            RootGroup::OpenAndLaunch,
            MenuSection::Primary,
            None,
            40,
            "mindmap",
        ),
        "system.native-menu" => d(
            action_id,
            native_label,
            ActionScope::System,
            ActionEffect::ExternalUnknown,
            ActionRisk::ExternalUnknown,
            TargetRule::any(),
            RootGroup::Native,
            MenuSection::Primary,
            None,
            0,
            "native",
        ),
        _ => return None,
    };
    Some(action)
}

#[allow(clippy::too_many_arguments)]
fn d(
    action_id: &str,
    label: &str,
    scope: ActionScope,
    effect: ActionEffect,
    risk: ActionRisk,
    target_rule: TargetRule,
    root_group: RootGroup,
    section: MenuSection,
    submenu: Option<SubmenuId>,
    order: u16,
    icon_key: &str,
) -> ActionDescriptor {
    ActionDescriptor {
        action_id: action_id.to_string(),
        label: label.to_string(),
        scope,
        plane: ActionPlane::KiteSemantic,
        owner: ActionOwner::Kite,
        effect,
        risk,
        target_rule,
        confirmation: if risk == ActionRisk::High {
            Confirmation::Dangerous
        } else {
            Confirmation::None
        },
        feedback: Feedback::Notice,
        icon_key: icon_key.to_string(),
        root_group,
        section,
        submenu,
        order,
    }
}

fn git_d(
    action_id: &str,
    label: &str,
    risk: ActionRisk,
    section: MenuSection,
    order: u16,
    icon_key: &str,
) -> ActionDescriptor {
    d(
        action_id,
        label,
        ActionScope::Repository,
        if matches!(risk, ActionRisk::None) {
            ActionEffect::VcsRead
        } else {
            ActionEffect::VcsWrite
        },
        risk,
        TargetRule::new(
            &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
            TargetCardinality::OneOrMore,
        ),
        RootGroup::Vcs,
        section,
        Some(SubmenuId::Git),
        order,
        icon_key,
    )
}

fn git_container_d(
    action_id: &str,
    label: &str,
    risk: ActionRisk,
    section: MenuSection,
    order: u16,
) -> ActionDescriptor {
    d(
        action_id,
        label,
        ActionScope::Container,
        if matches!(risk, ActionRisk::None) {
            ActionEffect::VcsRead
        } else {
            ActionEffect::VcsWrite
        },
        risk,
        TargetRule::container(),
        RootGroup::Vcs,
        section,
        Some(SubmenuId::Git),
        order,
        "vcs",
    )
}

fn svn_d(
    action_id: &str,
    label: &str,
    risk: ActionRisk,
    section: MenuSection,
    order: u16,
) -> ActionDescriptor {
    d(
        action_id,
        label,
        ActionScope::Repository,
        if matches!(risk, ActionRisk::None) {
            ActionEffect::VcsRead
        } else {
            ActionEffect::VcsWrite
        },
        risk,
        TargetRule::new(
            &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
            TargetCardinality::OneOrMore,
        ),
        RootGroup::Vcs,
        section,
        Some(SubmenuId::Svn),
        order,
        "vcs",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        surface: ContextSurface,
        kinds: &[TargetKind],
        path_count: usize,
    ) -> ContextSnapshot {
        let targets = (0..path_count)
            .map(|index| {
                let kind = kinds.get(index).copied().unwrap_or(kinds[0]);
                let extension = if kind == TargetKind::Archive {
                    ".zip"
                } else {
                    ""
                };
                ContextTarget::new(
                    format!("/tmp/item-{index}{extension}"),
                    format!("item-{index}{extension}"),
                    kind,
                )
            })
            .collect::<Vec<_>>();
        let primary = targets.first().map(|target| target.id.clone());
        let working_directory = if target_count_is_directory(&targets) {
            targets
                .first()
                .map(|target| target.path.clone())
                .unwrap_or_else(|| "/tmp".to_string())
        } else {
            "/tmp".to_string()
        };
        ContextSnapshot {
            schema_version: CONTEXT_MENU_SCHEMA_VERSION,
            snapshot_id: "test-snapshot".to_string(),
            platform: ContextPlatform::MacOs,
            surface,
            target_set: TargetSet::new(primary, targets, working_directory.clone()),
            container: ContainerSnapshot::for_path(
                working_directory,
                target_count_is_directory(&context_snapshot_targets(kinds, path_count)),
            ),
            capabilities: ContextCapabilities {
                can_paste: true,
                can_create: true,
                supports_transfer_ops: true,
                supports_cut: true,
                supports_rename: true,
                supports_delete: true,
                supports_favorite: true,
                agent_available: true,
                quick_look_available: true,
                reveal_available: true,
                share_available: true,
                properties_available: true,
                text_editor_available: true,
                native_plane_available: true,
                ..ContextCapabilities::default()
            },
            open_with: OpenWithCatalog::default(),
            tool_catalog: ToolCatalog::default(),
            tool_policy: ToolPolicy::default(),
            vcs_menu: VcsMenuSnapshot::default(),
            policy: ContextPolicy {
                hide_unavailable: true,
                ..ContextPolicy::default()
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn vcs_snapshot(
        path: &str,
        tool: crate::vcs_menu::VcsMenuTool,
        staged: Option<u32>,
        unstaged: u32,
        untracked: u32,
        conflicted: u32,
        upstream: Option<&str>,
        ahead: u32,
        behind: u32,
        operation: Option<crate::vcs_menu::VcsOperationMenuState>,
    ) -> VcsMenuSnapshot {
        let tool_id = tool.as_str();
        let repository_id = format!("{tool_id}:{path}");
        let relation = match tool {
            crate::vcs_menu::VcsMenuTool::Git => VcsTargetRelation::Root,
            crate::vcs_menu::VcsMenuTool::Svn => VcsTargetRelation::SvnVersioned,
        };
        let repository = VcsMenuRepository {
            id: repository_id.clone(),
            tool,
            repository_root: path.to_string(),
            worktree_root: path.to_string(),
            metadata_dir: None,
            common_dir: None,
            display_name: "repo".to_string(),
            head: crate::vcs_menu::VcsHeadState::default(),
            upstream: upstream.map(str::to_string),
            ahead,
            behind,
            changes: crate::vcs_menu::VcsChangeSummary {
                staged,
                unstaged,
                untracked,
                ignored: 0,
                conflicted,
            },
            conflicts: (0..conflicted)
                .map(|index| format!("conflict-{index}"))
                .collect(),
            operation,
            svn: None,
            state_revision: format!("{tool_id}-revision"),
        };
        VcsMenuSnapshot {
            schema_version: crate::vcs_menu::VCS_MENU_SCHEMA_VERSION,
            observed_at_unix_ms: 0,
            git_available: tool == crate::vcs_menu::VcsMenuTool::Git,
            svn_available: tool == crate::vcs_menu::VcsMenuTool::Svn,
            repositories: vec![repository],
            targets: vec![crate::vcs_menu::VcsTargetMenuState {
                path: path.to_string(),
                links: vec![VcsTargetLink {
                    repository_id,
                    tool,
                    relative_path: Some(String::new()),
                    relation,
                    staged: staged.map(|count| count > 0),
                    unstaged: unstaged > 0 || untracked > 0,
                    conflicted: conflicted > 0,
                    switched: false,
                    property_modified: false,
                    missing: false,
                }],
            }],
            state_revision: format!("{tool_id}-snapshot"),
        }
    }

    fn context_snapshot_targets(kinds: &[TargetKind], path_count: usize) -> Vec<ContextTarget> {
        (0..path_count)
            .map(|index| {
                ContextTarget::new(
                    format!("/tmp/item-{index}"),
                    "item",
                    kinds.get(index).copied().unwrap_or(kinds[0]),
                )
            })
            .collect()
    }

    fn target_count_is_directory(targets: &[ContextTarget]) -> bool {
        targets.len() == 1 && targets[0].kind == TargetKind::Directory
    }

    fn root_action_ids(plan: &MenuPlan) -> Vec<String> {
        let mut ids = Vec::new();
        for group in &plan.groups {
            for entry in &group.entries {
                match entry {
                    MenuEntryPlan::Action(action) => ids.push(action.action_id.clone()),
                    MenuEntryPlan::Submenu(submenu) => {
                        ids.push(format!("submenu:{}", submenu.id));
                    }
                }
            }
        }
        ids
    }

    fn all_actions(plan: &MenuPlan) -> Vec<&ResolvedAction> {
        let mut actions = plan
            .groups
            .iter()
            .flat_map(|group| &group.entries)
            .filter_map(|entry| match entry {
                MenuEntryPlan::Action(action) => Some(action),
                MenuEntryPlan::Submenu(_) => None,
            })
            .collect::<Vec<_>>();
        actions.extend(
            plan.submenus
                .iter()
                .flat_map(|submenu| &submenu.sections)
                .flat_map(|section| &section.entries),
        );
        actions
    }

    #[test]
    fn mindmap_actions_require_explicit_platform_capabilities() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.capabilities.mindmap_available = true;
        context.capabilities.mindmap_import_available = true;

        let plan = compose_default_menu(&context).expect("compose mindmap menu");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"tool.kite.mindmap"));
        assert!(ids.contains(&"tool.kite.mindmap-import"));

        let multi = snapshot(
            ContextSurface::BrowserList,
            &[TargetKind::File, TargetKind::File],
            2,
        );
        let mut multi = multi;
        multi.capabilities.mindmap_available = true;
        multi.capabilities.mindmap_import_available = true;
        let multi_plan = compose_default_menu(&multi).expect("compose multi-selection menu");
        let ids = multi_plan.visible_action_ids();
        assert!(!ids.contains(&"tool.kite.mindmap"));
        assert!(!ids.contains(&"tool.kite.mindmap-import"));
    }

    #[test]
    fn file_menu_is_deduplicated_and_ordered() {
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        let plan = compose_default_menu(&context).expect("compose file menu");
        let ids = root_action_ids(&plan);
        assert_eq!(ids.first().map(String::as_str), Some("open.default"));
        assert!(ids.contains(&"submenu:file-actions".to_string()));
        assert_eq!(ids.last().map(String::as_str), Some("system.native-menu"));
        let file_actions = plan
            .find_submenu(FILE_ACTIONS_SUBMENU)
            .expect("file actions");
        let ordered = file_actions
            .sections
            .iter()
            .flat_map(|section| section.entries.iter())
            .map(|action| action.action_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                "clipboard.copy",
                "clipboard.cut",
                "clipboard.copy-to",
                "clipboard.copy-path",
                "favorite.add",
                "file.rename",
            ]
        );
    }

    #[test]
    fn directory_entity_menu_keeps_directory_actions() {
        let context = snapshot(ContextSurface::BrowserColumns, &[TargetKind::Directory], 1);
        let plan = compose_default_menu(&context).expect("directory entity menu");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"open.default"));
        assert!(ids.contains(&"file.rename"));
        assert!(ids.contains(&"file.move-to-trash"));
        assert!(!ids.contains(&"archive.extract-here"));
        assert_eq!(ids.last().copied(), Some("system.native-menu"));
    }

    #[test]
    fn duplicate_candidates_prefer_enabled_then_precedence_independent_of_provider_order() {
        struct SingleCandidateProvider(ActionCandidate);
        impl ActionProvider for SingleCandidateProvider {
            fn candidates(&self, _: &ContextSnapshot) -> Vec<ActionCandidate> {
                vec![self.0.clone()]
            }
        }

        let candidates = [
            ActionCandidate::with_availability(
                "clipboard.copy",
                CandidateAvailability::disabled("TEST", "disabled"),
            )
            .precedence(255)
            .label("disabled"),
            ActionCandidate::available("clipboard.copy")
                .precedence(1)
                .label("low"),
            ActionCandidate::available("clipboard.copy")
                .precedence(10)
                .label("high"),
            ActionCandidate::available("system.native-menu"),
        ];
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        for order in [[0, 1, 2, 3], [2, 0, 3, 1], [1, 3, 2, 0]] {
            let providers = order
                .into_iter()
                .map(|index| {
                    Box::new(SingleCandidateProvider(candidates[index].clone()))
                        as Box<dyn ActionProvider>
                })
                .collect::<Vec<_>>();
            let plan = ContextMenuComposer::new(providers)
                .compose(&context)
                .expect("deduplicated plan");
            let copy = all_actions(&plan)
                .into_iter()
                .find(|action| action.action_id == "clipboard.copy")
                .expect("copy action");
            assert!(copy.enabled);
            assert_eq!(copy.label, "high");
            assert_eq!(
                plan.visible_action_ids().last().copied(),
                Some("system.native-menu")
            );
        }
    }

    #[test]
    fn typed_variants_remain_distinct_under_the_same_action_id() {
        struct VariantProvider;
        impl ActionProvider for VariantProvider {
            fn candidates(&self, _: &ContextSnapshot) -> Vec<ActionCandidate> {
                vec![
                    ActionCandidate::available("clipboard.copy").variant("application:a"),
                    ActionCandidate::available("clipboard.copy").variant("application:b"),
                    ActionCandidate::available("system.native-menu"),
                ]
            }
        }
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        let plan = ContextMenuComposer::new(vec![Box::new(VariantProvider)])
            .compose(&context)
            .expect("variant plan");
        let copies = plan
            .visible_action_ids()
            .into_iter()
            .filter(|id| *id == "clipboard.copy")
            .count();
        assert_eq!(copies, 2);
    }

    #[test]
    fn provider_input_order_does_not_change_layout() {
        struct OrderedProvider {
            reverse: bool,
        }
        impl ActionProvider for OrderedProvider {
            fn candidates(&self, _: &ContextSnapshot) -> Vec<ActionCandidate> {
                let mut candidates = vec![
                    ActionCandidate::available("clipboard.copy"),
                    ActionCandidate::available("clipboard.cut"),
                    ActionCandidate::available("system.native-menu"),
                    ActionCandidate::available("file.move-to-trash"),
                ];
                if self.reverse {
                    candidates.reverse();
                }
                candidates
            }
        }
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        let left = ContextMenuComposer::new(vec![Box::new(OrderedProvider { reverse: false })])
            .compose(&context)
            .expect("left plan");
        let right = ContextMenuComposer::new(vec![Box::new(OrderedProvider { reverse: true })])
            .compose(&context)
            .expect("right plan");
        assert_eq!(left, right);
    }

    #[test]
    fn background_menu_has_no_entity_or_selection_actions() {
        let mut context = snapshot(ContextSurface::Background, &[TargetKind::Directory], 0);
        context.target_set.targets.clear();
        context.target_set.ordered_paths.clear();
        context.target_set.primary = None;
        context.target_set.working_directory = "/tmp/background".to_string();
        context.container = ContainerSnapshot::for_path("/tmp/background", true);
        let plan = compose_default_menu(&context).expect("background plan");
        let ids = plan.visible_action_ids();
        assert!(!ids.contains(&"open.default"));
        assert!(!ids.contains(&"file.rename"));
        assert!(ids.contains(&"clipboard.paste"));
        assert!(ids.contains(&"file.new-folder"));
        assert_eq!(ids.last().copied(), Some("system.native-menu"));
    }

    #[test]
    fn search_result_menu_is_identity_only_and_never_container_mutating() {
        for surface in [
            ContextSurface::SearchResults,
            ContextSurface::SearchColumn,
            ContextSurface::SearchGlobal,
            ContextSurface::SearchContent,
        ] {
            let context = snapshot(surface, &[TargetKind::File], 1);
            let plan = compose_default_menu(&context).expect("search result menu");
            let ids = plan.visible_action_ids();
            assert!(ids.contains(&"open.default"));
            assert!(ids.contains(&"clipboard.copy-path"));
            assert!(ids.contains(&"system.reveal"));
            assert!(!ids.contains(&"clipboard.paste"));
            assert!(!ids.contains(&"file.new-folder"));
            assert!(!ids.contains(&"file.new-file"));
            assert!(!ids.contains(&"file.rename"));
            assert!(!ids.contains(&"file.move-to-trash"));
            assert!(
                !ids.iter()
                    .any(|id| id.starts_with("git.") || id.starts_with("svn."))
            );
        }
    }

    #[test]
    fn sidebar_favorite_menu_has_remove_without_file_content_mutations() {
        let mut context = snapshot(
            ContextSurface::SidebarFavorites,
            &[TargetKind::Directory],
            1,
        );
        context.capabilities.is_favorite = true;
        let plan = compose_default_menu(&context).expect("sidebar favorite menu");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"open.default"));
        assert!(ids.contains(&"clipboard.copy-path"));
        assert!(ids.contains(&"favorite.remove"));
        assert!(!ids.contains(&"file.rename"));
        assert!(!ids.contains(&"file.move-to-trash"));
        assert!(!ids.contains(&"clipboard.cut"));
        assert!(
            !ids.iter()
                .any(|id| id.starts_with("git.") || id.starts_with("svn."))
        );
    }

    #[test]
    fn preview_menu_reuses_file_context_without_container_actions() {
        let context = snapshot(ContextSurface::Preview, &[TargetKind::File], 1);
        let plan = compose_default_menu(&context).expect("preview menu");
        let ids = plan.visible_action_ids();
        assert!(!ids.contains(&"navigation.refresh"));
        assert!(!ids.contains(&"clipboard.paste"));
        assert!(!ids.contains(&"file.new-folder"));
        assert!(!ids.contains(&"file.new-file"));
        assert!(ids.contains(&"open.default"));
        assert!(ids.contains(&"clipboard.copy"));
        assert!(ids.contains(&"file.move-to-trash"));
    }

    #[test]
    fn device_item_menu_uses_namespace_capabilities_and_never_file_trash() {
        let mut context = snapshot(
            ContextSurface::DeviceNamespace,
            &[TargetKind::DeviceItem],
            1,
        );
        context.capabilities.is_shell_namespace = true;
        context.capabilities.supports_transfer_ops = true;
        context.capabilities.supports_cut = false;
        context.capabilities.supports_rename = false;
        context.capabilities.supports_delete = true;
        context.capabilities.supports_favorite = false;
        context.capabilities.properties_available = true;
        let plan = compose_default_menu(&context).expect("device item menu");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"open.default"));
        assert!(ids.contains(&"clipboard.copy"));
        assert!(ids.contains(&"clipboard.copy-path"));
        assert!(ids.contains(&"device.delete"));
        assert!(ids.contains(&"system.properties"));
        assert!(!ids.contains(&"file.move-to-trash"));
        assert!(!ids.contains(&"file.rename"));
        assert!(!ids.contains(&"open.with"));
        assert!(!ids.contains(&"open.in"));
    }

    #[test]
    fn device_container_background_can_create_and_paste_without_entity_leaks() {
        let mut context = snapshot(
            ContextSurface::Background,
            &[TargetKind::DeviceContainer],
            0,
        );
        context.target_set.targets.clear();
        context.target_set.ordered_paths.clear();
        context.target_set.primary = None;
        context.container = ContainerSnapshot::for_path("/device/root", true)
            .with_kind(TargetKind::DeviceContainer);
        context.capabilities.is_shell_namespace = true;
        context.capabilities.can_paste = true;
        context.capabilities.can_create = true;
        context.capabilities.supports_transfer_ops = true;
        let plan = compose_default_menu(&context).expect("device container background menu");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"clipboard.paste"));
        assert!(ids.contains(&"file.new-folder"));
        assert!(ids.contains(&"file.new-file"));
        assert!(!ids.contains(&"open.default"));
        assert!(!ids.contains(&"device.delete"));
        assert!(!ids.contains(&"file.rename"));
        assert_eq!(plan.target_kinds, vec!["device-container"]);
    }

    #[test]
    fn multi_select_hides_single_target_actions_and_promotes_destructive_risk() {
        let context = snapshot(
            ContextSurface::BrowserList,
            &[TargetKind::File, TargetKind::File],
            2,
        );
        let plan = compose_default_menu(&context).expect("multi plan");
        let ids = plan.visible_action_ids();
        assert!(!ids.contains(&"file.rename"));
        assert!(!ids.contains(&"system.properties"));
        assert!(ids.contains(&"clipboard.copy"));
        assert!(ids.contains(&"file.move-to-trash"));
        assert!(!ids.contains(&"git.add"));
        assert!(!ids.contains(&"svn.add"));
        let trash = plan
            .groups
            .iter()
            .flat_map(|group| &group.entries)
            .find_map(|entry| match entry {
                MenuEntryPlan::Action(action) if action.action_id == "file.move-to-trash" => {
                    Some(action)
                }
                _ => None,
            })
            .expect("trash action");
        assert_eq!(trash.risk, ActionRisk::High);
        assert_eq!(trash.root_group, RootGroup::Destructive);
    }

    #[test]
    fn plan_freezes_ordered_target_paths_and_working_directory() {
        let context = snapshot(
            ContextSurface::BrowserList,
            &[TargetKind::File, TargetKind::File],
            2,
        );
        let expected_paths = context.target_set.ordered_paths.clone();
        assert_eq!(context.target_set.working_directory, "/tmp".to_string());
        let plan = compose_default_menu(&context).expect("frozen target plan");
        let copy = plan
            .find_submenu(FILE_ACTIONS_SUBMENU)
            .and_then(|submenu| {
                submenu
                    .sections
                    .iter()
                    .flat_map(|section| &section.entries)
                    .find(|action| action.action_id == "clipboard.copy")
            })
            .expect("copy action");
        assert_eq!(copy.targets, expected_paths);
    }

    #[test]
    fn visibility_filter_hides_scope_inapplicable_actions() {
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::Directory], 1);
        let plan = compose_default_menu(&context).expect("directory plan");
        let ids = plan.visible_action_ids();
        assert!(!ids.contains(&"archive.extract-here"));
        assert!(!ids.contains(&"archive.extract-to"));
    }

    #[test]
    fn stable_action_ids_cover_core_agent_vcs_archive_and_native() {
        let context = snapshot(ContextSurface::BrowserColumns, &[TargetKind::Directory], 1);
        let mut context = context;
        context.capabilities.git_available = true;
        context.capabilities.svn_available = true;
        let path = context.target_set.targets[0].path.clone();
        context.vcs_menu = vcs_snapshot(
            &path,
            VcsMenuTool::Git,
            Some(1),
            1,
            0,
            0,
            Some("origin/main"),
            0,
            0,
            None,
        );
        let mut svn = vcs_snapshot(&path, VcsMenuTool::Svn, None, 1, 0, 0, None, 0, 0, None);
        context.vcs_menu.repositories.append(&mut svn.repositories);
        context.vcs_menu.targets[0]
            .links
            .append(&mut svn.targets[0].links);
        context.vcs_menu.svn_available = true;
        context.tool_catalog = crate::tool_registry::ToolCatalog::ready(vec![
            crate::tool_registry::DiscoveredTool::available(
                "kite.git-client",
                "kite-internal:git-client",
            ),
        ]);
        let plan = compose_default_menu(&context).expect("rich plan");
        let ids = plan.visible_action_ids();
        for id in [
            "navigation.refresh",
            "open.default",
            "clipboard.copy",
            "clipboard.paste",
            "file.new-folder",
            "archive.compress.zip",
            "agent.ask",
            "agent.summarize-folder",
            "open.in",
            "git.commit",
            "svn.update",
            "svn.commit",
            "system.native-menu",
        ] {
            assert!(ids.contains(&id), "missing stable action id {id}: {ids:?}");
        }
    }

    #[test]
    fn vcs_operation_controls_are_promoted_and_regular_writes_are_hidden() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::Directory], 1);
        let path = context.target_set.targets[0].path.clone();
        let operation = crate::vcs_menu::VcsOperationMenuState {
            kind: crate::vcs_menu::VcsOperationKind::Rebase,
            label: "Kite 变基已暂停".to_string(),
            continue_available: true,
            skip_available: true,
            abort_available: true,
            resolve_available: true,
            cleanup_available: false,
            custom_rebase: true,
        };
        context.vcs_menu = vcs_snapshot(
            &path,
            VcsMenuTool::Git,
            Some(1),
            1,
            0,
            1,
            Some("origin/main"),
            1,
            0,
            Some(operation),
        );
        let plan = compose_default_menu(&context).expect("recovery plan");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"git.conflicts.resolve"));
        assert!(ids.contains(&"git.operation.continue"));
        assert!(ids.contains(&"git.operation.skip"));
        assert!(ids.contains(&"git.operation.abort"));
        assert!(!ids.contains(&"git.commit"));
        assert!(!ids.contains(&"git.clean-untracked"));
        let git = plan.find_submenu(GIT_SUBMENU).expect("git submenu");
        assert_eq!(git.sections[0].id, MenuSection::Recovery.id());
        let continue_action = git
            .sections
            .iter()
            .flat_map(|section| &section.entries)
            .find(|action| action.action_id == "git.operation.continue")
            .expect("continue action");
        assert_eq!(continue_action.label, "继续 Kite 变基");
    }

    #[test]
    fn vcs_upstream_missing_is_not_rendered_as_upstream_equal() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::Directory], 1);
        let path = context.target_set.targets[0].path.clone();
        context.vcs_menu =
            vcs_snapshot(&path, VcsMenuTool::Git, Some(0), 0, 0, 0, None, 0, 0, None);
        context.policy.hide_unavailable = false;
        let plan = compose_default_menu(&context).expect("no upstream plan");
        let pull = plan
            .find_submenu(GIT_SUBMENU)
            .and_then(|submenu| {
                submenu
                    .sections
                    .iter()
                    .flat_map(|section| &section.entries)
                    .find(|action| action.action_id == "git.pull")
            })
            .expect("pull action");
        assert!(!pull.enabled);
        assert_eq!(
            pull.disabled_reason_code.as_deref(),
            Some("UPSTREAM_NOT_CONFIGURED")
        );

        context.vcs_menu = vcs_snapshot(
            &path,
            VcsMenuTool::Git,
            Some(0),
            0,
            0,
            0,
            Some("origin/main"),
            0,
            0,
            None,
        );
        let plan = compose_default_menu(&context).expect("equal upstream plan");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"git.pull"));
        assert!(ids.contains(&"git.push"));
    }

    #[test]
    fn vcs_actions_are_hidden_when_targets_span_repositories() {
        let mut context = snapshot(
            ContextSurface::BrowserList,
            &[TargetKind::File, TargetKind::File],
            2,
        );
        let path_a = context.target_set.targets[0].path.clone();
        let path_b = context.target_set.targets[1].path.clone();
        context.vcs_menu = vcs_snapshot(
            &path_a,
            VcsMenuTool::Git,
            Some(1),
            0,
            0,
            0,
            Some("origin/main"),
            0,
            0,
            None,
        );
        let mut other = vcs_snapshot(
            &path_b,
            VcsMenuTool::Git,
            Some(1),
            0,
            0,
            0,
            Some("origin/main"),
            0,
            0,
            None,
        );
        context
            .vcs_menu
            .repositories
            .append(&mut other.repositories);
        context.vcs_menu.targets.append(&mut other.targets);
        let plan = compose_default_menu(&context).expect("cross repository plan");
        let ids = plan.visible_action_ids();
        assert!(!ids.iter().any(|id| id.starts_with("git.")));
    }

    #[test]
    fn svn_never_implies_staged_content() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::Directory], 1);
        let path = context.target_set.targets[0].path.clone();
        context.vcs_menu = vcs_snapshot(&path, VcsMenuTool::Svn, None, 2, 0, 0, None, 0, 0, None);
        let plan = compose_default_menu(&context).expect("svn plan");
        let ids = plan.visible_action_ids();
        assert!(ids.contains(&"svn.commit"));
        assert!(!ids.contains(&"git.commit"));
        assert!(!ids.contains(&"git.unstage"));
    }

    fn tool_catalog(ids: &[&str]) -> crate::tool_registry::ToolCatalog {
        crate::tool_registry::ToolCatalog::ready(
            ids.iter()
                .map(|id| {
                    crate::tool_registry::DiscoveredTool::available(*id, format!("/tmp/{id}"))
                })
                .collect(),
        )
    }

    fn tool_action<'a>(plan: &'a MenuPlan, action_id: &str) -> Vec<&'a ResolvedAction> {
        let mut actions = Vec::new();
        for group in &plan.groups {
            for entry in &group.entries {
                match entry {
                    MenuEntryPlan::Action(action) if action.action_id == action_id => {
                        actions.push(action)
                    }
                    MenuEntryPlan::Submenu(submenu) => {
                        for section in &submenu.sections {
                            actions.extend(
                                section
                                    .entries
                                    .iter()
                                    .filter(|action| action.action_id == action_id),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        actions
    }

    #[test]
    fn tools_use_policy_order_only_inside_submenus() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.tool_catalog =
            tool_catalog(&["kite.text-editor", "editor.vscode", "editor.cursor"]);
        let mut first_policy = crate::tool_registry::ToolPolicy {
            pinned_tool_ids: vec!["editor.cursor".to_string()],
            user_order: vec!["editor.vscode".to_string(), "editor.cursor".to_string()],
            recent_tool_ids: vec!["editor.vscode".to_string()],
            ..Default::default()
        };
        context.tool_policy = first_policy.clone();
        let first = compose_default_menu(&context).expect("ordered tools");
        let submenu = first
            .find_submenu(OPEN_IN_SUBMENU)
            .expect("open-in submenu");
        let first_ids = submenu
            .sections
            .iter()
            .flat_map(|section| &section.entries)
            .map(|action| action.payload.value().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            first_ids,
            vec!["editor.cursor", "editor.vscode", "kite.text-editor"]
        );
        let root_position = first
            .groups
            .iter()
            .flat_map(|group| &group.entries)
            .position(|entry| matches!(entry, MenuEntryPlan::Submenu(submenu) if submenu.id == OPEN_IN_SUBMENU))
            .expect("open-in root position");

        first_policy.recent_tool_ids = vec!["kite.text-editor".to_string()];
        context.tool_policy = first_policy;
        let second = compose_default_menu(&context).expect("reordered tools");
        let second_submenu = second
            .find_submenu(OPEN_IN_SUBMENU)
            .expect("open-in submenu after MRU change");
        let second_ids = second_submenu
            .sections
            .iter()
            .flat_map(|section| &section.entries)
            .map(|action| action.payload.value().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            second_ids,
            vec!["editor.cursor", "kite.text-editor", "editor.vscode"]
        );
        let second_position = second
            .groups
            .iter()
            .flat_map(|group| &group.entries)
            .position(|entry| matches!(entry, MenuEntryPlan::Submenu(submenu) if submenu.id == OPEN_IN_SUBMENU))
            .expect("open-in root position");
        assert_eq!(root_position, second_position);
    }

    #[test]
    fn single_tool_is_promoted_without_duplicate_submenu() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.tool_catalog = tool_catalog(&["editor.vscode"]);
        let plan = compose_default_menu(&context).expect("single tool");
        assert!(plan.find_submenu(OPEN_IN_SUBMENU).is_none());
        let actions = tool_action(&plan, "open.in");
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0].payload,
            ActionPayload::Tool {
                tool_id: "editor.vscode".to_string()
            }
        );
    }

    #[test]
    fn compare_with_requires_exactly_two_files_and_preserves_target_order() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 2);
        context.tool_catalog = tool_catalog(&["editor.vscode"]);
        let plan = compose_default_menu(&context).expect("compare plan");
        let actions = tool_action(&plan, "compare.with");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].targets, context.target_set.ordered_paths);
        assert_eq!(
            actions[0].payload,
            ActionPayload::Tool {
                tool_id: "editor.vscode".to_string()
            }
        );

        context.tool_catalog = tool_catalog(&["editor.vscode"]);
        context.target_set = TargetSet::new(
            Some("/tmp/a.txt".to_string()),
            vec![
                ContextTarget::new("/tmp/a.txt", "a.txt", TargetKind::File),
                ContextTarget::new("/tmp/b.txt", "b.txt", TargetKind::File),
                ContextTarget::new("/tmp/c.txt", "c.txt", TargetKind::File),
            ],
            "/tmp",
        );
        let plan = compose_default_menu(&context).expect("three-file plan");
        assert!(tool_action(&plan, "compare.with").is_empty());
    }

    fn open_with_actions(plan: &MenuPlan) -> Vec<&ResolvedAction> {
        plan.find_submenu(OPEN_WITH_SUBMENU)
            .map(|submenu| {
                submenu
                    .sections
                    .iter()
                    .flat_map(|section| &section.entries)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn open_with_is_limited_to_one_existing_readable_file() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 2);
        context.open_with.status = OpenWithCatalogStatus::Ready;
        assert!(
            !compose_default_menu(&context)
                .expect("multi-file plan")
                .visible_action_ids()
                .iter()
                .any(|id| id.starts_with("open.with"))
        );

        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::Directory], 1);
        assert!(
            !compose_default_menu(&context)
                .expect("directory plan")
                .visible_action_ids()
                .iter()
                .any(|id| id.starts_with("open.with"))
        );

        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.target_set.targets[0].exists = false;
        assert!(
            !compose_default_menu(&context)
                .expect("missing file plan")
                .visible_action_ids()
                .iter()
                .any(|id| id.starts_with("open.with"))
        );

        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.target_set.targets[0].readable = false;
        assert!(
            !compose_default_menu(&context)
                .expect("unreadable file plan")
                .visible_action_ids()
                .iter()
                .any(|id| id.starts_with("open.with"))
        );

        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.capabilities.is_shell_namespace = true;
        assert!(
            !compose_default_menu(&context)
                .expect("shell namespace plan")
                .visible_action_ids()
                .iter()
                .any(|id| id.starts_with("open.with"))
        );
    }

    #[test]
    fn open_with_orders_default_recent_then_system_and_filters_stale_duplicates() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.open_with = OpenWithCatalog {
            status: OpenWithCatalogStatus::Ready,
            applications: vec![
                OpenWithApplication {
                    application_id: "com.example.recent-b".to_string(),
                    display_name: "Recent B".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.default".to_string(),
                    display_name: "Default".to_string(),
                    is_default: true,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.recent-a".to_string(),
                    display_name: "Recent A".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.recent-b".to_string(),
                    display_name: "Recent B Duplicate".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.stale".to_string(),
                    display_name: "Stale".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: false,
                    unavailable_reason: Some("application is no longer installed".to_string()),
                },
                OpenWithApplication {
                    application_id: "com.example.recent-c".to_string(),
                    display_name: "Recent C".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.recent-d".to_string(),
                    display_name: "Recent D".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.recent-e".to_string(),
                    display_name: "Recent E".to_string(),
                    is_default: false,
                    is_recent: true,
                    is_available: true,
                    unavailable_reason: None,
                },
                OpenWithApplication {
                    application_id: "com.example.remaining".to_string(),
                    display_name: "Remaining".to_string(),
                    is_default: false,
                    is_recent: false,
                    is_available: true,
                    unavailable_reason: None,
                },
            ],
        };

        let plan = compose_default_menu(&context).expect("open with plan");
        let actions = open_with_actions(&plan);
        let labels = actions
            .iter()
            .map(|action| action.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "默认应用 — Default",
                "Recent B",
                "Recent A",
                "Recent C",
                "Recent D",
                "Recent E",
                "Remaining",
                "选择其他应用…",
            ]
        );
        assert_eq!(
            actions[0].variant_key.as_deref(),
            Some("application:com.example.default")
        );
        assert_eq!(
            actions[0].payload,
            ActionPayload::Application {
                application_id: "com.example.default".to_string()
            }
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.variant_key.as_deref()
                    == Some("application:com.example.recent-b"))
                .count(),
            1
        );
        assert!(!actions.iter().any(|action| action.label.contains("Stale")));
    }

    #[test]
    fn open_with_loading_keeps_chooser_usable_and_variant_identity_stable() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.open_with.status = OpenWithCatalogStatus::Loading;
        let plan = compose_default_menu(&context).expect("loading plan");
        let actions = open_with_actions(&plan);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].action_id, "open.with");
        assert_eq!(actions[0].variant_key.as_deref(), Some("status:loading"));
        assert!(!actions[0].enabled);
        assert_eq!(actions[1].action_id, "open.with.choose-other");
        assert!(actions[1].enabled);
    }

    #[test]
    fn unknown_action_candidates_are_reported_without_breaking_the_plan() {
        struct UnknownProvider;
        impl ActionProvider for UnknownProvider {
            fn candidates(&self, _: &ContextSnapshot) -> Vec<ActionCandidate> {
                vec![
                    ActionCandidate::available("open.with"),
                    ActionCandidate::available("not.a.real.action"),
                    ActionCandidate::available("system.native-menu"),
                ]
            }
        }
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.open_with.status = OpenWithCatalogStatus::Loading;
        let plan = ContextMenuComposer::new(vec![Box::new(UnknownProvider)])
            .compose(&context)
            .expect("plan with unknown candidate");
        assert!(
            plan.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("not.a.real.action"))
        );
    }

    #[test]
    fn native_entry_remains_visible_with_reason_when_platform_is_unavailable() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.capabilities.native_plane_available = false;
        context.policy.hide_unavailable = true;
        let plan = compose_default_menu(&context).expect("native fallback plan");
        let native = plan
            .groups
            .last()
            .and_then(|group| group.entries.last())
            .expect("native entry");
        match native {
            MenuEntryPlan::Action(action) => {
                assert_eq!(action.action_id, "system.native-menu");
                assert!(!action.enabled);
                assert_eq!(
                    action.disabled_reason_code.as_deref(),
                    Some("NATIVE_PLANE_UNAVAILABLE")
                );
                assert!(
                    action
                        .disabled_reason
                        .as_deref()
                        .is_some_and(|reason| !reason.trim().is_empty())
                );
            }
            _ => panic!("native entry must be an action"),
        }
    }

    #[test]
    fn safe_fallback_keeps_native_entry_last_when_platform_is_unavailable() {
        let mut context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        context.capabilities.native_plane_available = false;
        let plan = fallback_plan(&context);
        let native = plan
            .groups
            .last()
            .and_then(|group| group.entries.last())
            .and_then(|entry| match entry {
                MenuEntryPlan::Action(action) => Some(action),
                MenuEntryPlan::Submenu(_) => None,
            })
            .expect("native fallback entry");
        assert_eq!(native.action_id, "system.native-menu");
        assert!(!native.enabled);
        assert_eq!(
            native.disabled_reason_code.as_deref(),
            Some("NATIVE_PLANE_UNAVAILABLE")
        );
        assert!(
            plan.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.contains("safe fallback") })
        );
    }

    #[test]
    fn ordinary_root_menu_entries_have_a_hard_limit() {
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        let descriptor = descriptor(&context, "clipboard.copy").expect("copy descriptor");
        let entries = (0..17)
            .map(|index| {
                let mut action = resolved_action(
                    descriptor.clone(),
                    &context,
                    CandidateAvailability::Available,
                    ActionPayload::None,
                );
                action.variant_key = Some(format!("test-{index}"));
                MenuEntryPlan::Action(action)
            })
            .collect::<Vec<_>>();
        let plan = MenuPlan {
            schema_version: CONTEXT_MENU_SCHEMA_VERSION,
            snapshot_id: context.snapshot_id.clone(),
            title: context.title(),
            target_kinds: vec!["file".to_string()],
            groups: vec![MenuGroupPlan {
                id: RootGroup::Transfer.id().to_string(),
                entries,
            }],
            submenus: Vec::new(),
            diagnostics: Vec::new(),
        };
        let error = validate_plan(&context, &plan).expect_err("root hard limit");
        assert!(error.message.contains("exceed the hard limit"));
    }

    #[test]
    fn all_visible_actions_have_accessible_labels_and_disabled_reasons() {
        let contexts = [
            snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1),
            snapshot(ContextSurface::BrowserColumns, &[TargetKind::Directory], 1),
            snapshot(
                ContextSurface::BrowserList,
                &[TargetKind::File, TargetKind::File],
                2,
            ),
            snapshot(ContextSurface::Background, &[TargetKind::Directory], 0),
            snapshot(ContextSurface::SearchResults, &[TargetKind::File], 1),
            snapshot(
                ContextSurface::SidebarFavorites,
                &[TargetKind::Directory],
                1,
            ),
            snapshot(ContextSurface::Preview, &[TargetKind::File], 1),
            snapshot(
                ContextSurface::DeviceNamespace,
                &[TargetKind::DeviceItem],
                1,
            ),
        ];
        for context in contexts {
            let plan = compose_default_menu(&context).expect("accessible plan");
            for action in all_actions(&plan) {
                assert!(
                    !action.label.trim().is_empty(),
                    "empty label for {}",
                    action.action_id
                );
                if !action.enabled {
                    assert!(
                        action
                            .disabled_reason
                            .as_deref()
                            .is_some_and(|reason| !reason.trim().is_empty()),
                        "missing disabled reason for {}",
                        action.action_id
                    );
                }
            }
            for submenu in &plan.submenus {
                assert!(!submenu.label.trim().is_empty());
            }
        }
    }
}
