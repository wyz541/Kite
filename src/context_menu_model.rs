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

pub const CONTEXT_MENU_SCHEMA_VERSION: u32 = 1;
pub const FILE_ACTIONS_SUBMENU: &str = "file-actions";
pub const ARCHIVE_SUBMENU: &str = "archive";
pub const AGENT_SUBMENU: &str = "agent";
pub const GIT_SUBMENU: &str = "git";
pub const SVN_SUBMENU: &str = "svn";
pub const SYSTEM_SUBMENU: &str = "system";
pub const OPEN_WITH_SUBMENU: &str = "open-with";

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
    Sidebar,
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
            "sidebar" => Self::Sidebar,
            "device.namespace" => Self::DeviceNamespace,
            _ => Self::Background,
        }
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
            is_primary_target,
        }
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
    Primary,
    Changes,
    Sync,
    History,
    Advanced,
    Recovery,
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
    Application { application_id: String },
    Tool { tool_id: String },
    Repository { path: String, vcs: String },
}

impl ActionPayload {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Application { .. } => "application",
            Self::Tool { .. } => "tool",
            Self::Repository { .. } => "repository",
        }
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Application { application_id } => Some(application_id),
            Self::Tool { tool_id } => Some(tool_id),
            Self::Repository { path, .. } => Some(path),
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

                let targets = if context.target_set.ordered_paths.is_empty() {
                    vec![context.container.path.clone()]
                } else {
                    context.target_set.ordered_paths.clone()
                };
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
                let label = candidate.label.unwrap_or(descriptor.label);
                let icon_key = candidate.icon_key.unwrap_or(descriptor.icon_key);
                let order = candidate.order.unwrap_or(descriptor.order);
                resolved.push(ResolvedAction {
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
                });
            }
        }

        let mut deduplicated: HashMap<String, (u8, usize, ResolvedAction)> = HashMap::new();
        for (provider_order, action) in resolved.into_iter().enumerate() {
            let key = action.instance_key();
            let enabled_rank = u8::from(action.enabled);
            let replacement = (enabled_rank, usize::MAX - provider_order, action);
            match deduplicated.get_mut(&key) {
                Some(existing)
                    if replacement.0 > existing.0
                        || (replacement.0 == existing.0 && replacement.1 > existing.1) => {}
                Some(_) => {
                    deduplicated.insert(key, replacement);
                }
                None => {
                    deduplicated.insert(key, replacement);
                }
            }
        }
        let mut actions = deduplicated
            .into_values()
            .map(|(_, _, action)| action)
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

fn validate_plan(context: &ContextSnapshot, plan: &MenuPlan) -> Result<(), ComposerError> {
    let mut previous_group_order = None;
    let mut native_count = 0;
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
        let mut keys = HashSet::new();
        for entry in &group.entries {
            match entry {
                MenuEntryPlan::Action(action) => {
                    validate_action(context, action)?;
                    if !keys.insert(action.instance_key()) {
                        return Err(ComposerError {
                            message: format!(
                                "duplicate root action instance: {}",
                                action.instance_key()
                            ),
                        });
                    }
                    if action.action_id == "system.native-menu" {
                        native_count += 1;
                    }
                }
                MenuEntryPlan::Submenu(submenu) => {
                    if submenu.sections.is_empty() {
                        return Err(ComposerError {
                            message: format!("empty submenu: {}", submenu.id),
                        });
                    }
                    let mut submenu_keys = HashSet::new();
                    for section in &submenu.sections {
                        if section.entries.is_empty() {
                            return Err(ComposerError {
                                message: format!("empty submenu section: {}", section.id),
                            });
                        }
                        for action in &section.entries {
                            validate_action(context, action)?;
                            if !submenu_keys.insert(action.instance_key()) {
                                return Err(ComposerError {
                                    message: format!(
                                        "duplicate submenu action instance: {}",
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
                "当前平台没有可用的完整原生菜单平面",
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

fn default_providers() -> Vec<Box<dyn ActionProvider>> {
    vec![
        Box::new(RefreshActionProvider),
        Box::new(CoreActionProvider),
        Box::new(OpenWithActionProvider),
        Box::new(ArchiveActionProvider),
        Box::new(AgentActionProvider),
        Box::new(VcsActionProvider),
        Box::new(SystemActionProvider),
    ]
}

struct RefreshActionProvider;
struct CoreActionProvider;
struct OpenWithActionProvider;
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
        let mut candidates = Vec::new();
        let loading = if context.capabilities.loading {
            CandidateAvailability::disabled("TARGET_LOADING", "正在读取仓库状态")
        } else {
            CandidateAvailability::Available
        };
        if context.capabilities.git_available {
            for action_id in [
                "git.open-client",
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
            ] {
                candidates.push(ActionCandidate::with_availability(
                    action_id,
                    loading.clone(),
                ));
            }
        }
        if context.container.is_primary_target {
            candidates.push(ActionCandidate::with_availability(
                "git.clone",
                if context.capabilities.loading {
                    loading.clone()
                } else {
                    CandidateAvailability::Available
                },
            ));
            if !context.capabilities.git_available {
                candidates.push(ActionCandidate::with_availability(
                    "git.init",
                    if context.capabilities.loading {
                        loading.clone()
                    } else {
                        CandidateAvailability::Available
                    },
                ));
            }
        }
        if context.capabilities.svn_available {
            for action_id in [
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
                    loading.clone(),
                ));
            }
        }
        candidates
    }
}

impl ActionProvider for SystemActionProvider {
    fn candidates(&self, context: &ContextSnapshot) -> Vec<ActionCandidate> {
        let mut candidates = Vec::new();
        if context.capabilities.quick_look_available && !context.target_set.targets.is_empty() {
            candidates.push(ActionCandidate::available("open.quick-look"));
        }
        if context.capabilities.reveal_available && !context.target_set.targets.is_empty() {
            candidates.push(ActionCandidate::available("system.reveal"));
        }
        if context.capabilities.share_available && !context.target_set.targets.is_empty() {
            candidates.push(ActionCandidate::available("system.share"));
        }
        if context.capabilities.properties_available && context.target_set.targets.len() == 1 {
            candidates.push(ActionCandidate::available("system.properties"));
        }
        if context.capabilities.text_editor_available
            && context.target_set.targets.len() == 1
            && context.all_files_text_like()
        {
            candidates.push(ActionCandidate::available("tool.kite.text-editor"));
        }
        if context.capabilities.mindmap_available && context.target_set.targets.len() == 1 {
            candidates.push(ActionCandidate::available("tool.kite.mindmap"));
        }
        if context.capabilities.mindmap_import_available && context.target_set.targets.len() == 1 {
            candidates.push(ActionCandidate::available("tool.kite.mindmap-import"));
        }
        let native_availability = if context.capabilities.native_plane_available {
            CandidateAvailability::Available
        } else {
            CandidateAvailability::disabled(
                "NATIVE_PLANE_UNAVAILABLE",
                "当前平台没有可用的完整原生菜单平面",
            )
        };
        candidates.push(ActionCandidate::with_availability(
            "system.native-menu",
            native_availability,
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
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
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
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
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
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
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
                &[TargetKind::File, TargetKind::Directory, TargetKind::Archive],
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
            TargetCardinality::One,
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
            TargetCardinality::One,
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
            policy: ContextPolicy {
                hide_unavailable: true,
                ..ContextPolicy::default()
            },
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
    fn duplicate_candidates_are_resolved_by_precedence_independent_of_provider_order() {
        struct DuplicateProvider {
            first: bool,
        }
        impl ActionProvider for DuplicateProvider {
            fn candidates(&self, _: &ContextSnapshot) -> Vec<ActionCandidate> {
                if self.first {
                    vec![
                        ActionCandidate::with_availability(
                            "clipboard.copy",
                            CandidateAvailability::disabled("TEST", "disabled"),
                        ),
                        ActionCandidate::available("system.native-menu"),
                    ]
                } else {
                    vec![
                        ActionCandidate::available("clipboard.copy").precedence(10),
                        ActionCandidate::available("system.native-menu"),
                    ]
                }
            }
        }
        let context = snapshot(ContextSurface::BrowserList, &[TargetKind::File], 1);
        let left = ContextMenuComposer::new(vec![
            Box::new(DuplicateProvider { first: true }),
            Box::new(DuplicateProvider { first: false }),
        ])
        .compose(&context)
        .expect("left plan");
        let right = ContextMenuComposer::new(vec![
            Box::new(DuplicateProvider { first: false }),
            Box::new(DuplicateProvider { first: true }),
        ])
        .compose(&context)
        .expect("right plan");
        assert_eq!(left, right);
        let copy = left
            .visible_action_ids()
            .into_iter()
            .find(|id| *id == "clipboard.copy")
            .expect("copy action");
        assert_eq!(copy, "clipboard.copy");
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
        assert!(!ids.iter().any(|id| *id == "open.default"));
        assert!(!ids.iter().any(|id| *id == "file.rename"));
        assert!(ids.iter().any(|id| *id == "clipboard.paste"));
        assert!(ids.iter().any(|id| *id == "file.new-folder"));
        assert_eq!(ids.last().copied(), Some("system.native-menu"));
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
            "git.open-client",
            "git.commit",
            "svn.update",
            "svn.commit",
            "system.native-menu",
        ] {
            assert!(ids.contains(&id), "missing stable action id {id}: {ids:?}");
        }
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
    fn native_entry_survives_unavailable_policy() {
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
            }
            _ => panic!("native entry must be an action"),
        }
    }
}
