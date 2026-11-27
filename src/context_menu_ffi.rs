//! Minimal UniFFI adapter for the shared context-menu Composer.
//!
//! Domain types stay in `context_menu_model`; this module only converts the
//! frozen Swift request into that model and flattens the depth-limited plan into
//! UniFFI-safe records.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::archive;
use crate::context_menu_model::{
    ContainerSnapshot, ContextCapabilities, ContextPlatform, ContextPolicy, ContextSnapshot,
    ContextSurface, ContextTarget, MenuEntryPlan, MenuPlan, OpenWithApplication, OpenWithCatalog,
    OpenWithCatalogStatus, TargetKind, TargetSet, compose_menu_with_fallback,
};
use crate::tool_registry::{
    DiscoveredTool, ToolArgument, ToolAvailability, ToolCatalog, ToolCatalogStatus, ToolDefinition,
    ToolPlatform, ToolPolicy, builtin_tool_definitions,
};
use crate::vcs_menu::{
    SvnWorkingCopyMenuState, VcsChangeSummary, VcsHeadKind, VcsHeadState, VcsMenuRepository,
    VcsMenuSnapshot, VcsMenuTool, VcsOperationKind, VcsOperationMenuState, VcsTargetLink,
    VcsTargetMenuState, VcsTargetRelation,
};

static NEXT_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum VcsMenuError {
    #[error("{message}")]
    Operation { message: String },
}

impl VcsMenuError {
    fn operation(message: impl Into<String>) -> Self {
        Self::Operation {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuTargetDto {
    pub path: String,
    pub display_name: String,
    pub kind: String,
    pub exists: bool,
    pub readable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OpenWithCatalogStatusDto {
    Ready,
    Loading,
    Unavailable,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OpenWithApplicationDto {
    pub application_id: String,
    pub display_name: String,
    pub is_default: bool,
    pub is_recent: bool,
    pub is_available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OpenWithCatalogDto {
    pub status: OpenWithCatalogStatusDto,
    pub applications: Vec<OpenWithApplicationDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolArgumentDto {
    pub kind: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolRecipeDto {
    pub role: String,
    pub arguments: Vec<ToolArgumentDto>,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolCapabilityDto {
    pub target_shapes: Vec<String>,
    pub roles: Vec<String>,
    pub min_paths: u32,
    pub max_paths: Option<u32>,
    pub all_targets_required: bool,
    pub path_filter: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolDefinitionDto {
    pub tool_id: String,
    pub category: String,
    pub display_name: String,
    pub description: String,
    pub platform: String,
    pub launch_kind: String,
    pub executable_identity: Option<String>,
    pub bundle_identity: Option<String>,
    pub capabilities: Vec<ToolCapabilityDto>,
    pub recipes: Vec<ToolRecipeDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolDiscoveryDto {
    pub tool_id: String,
    pub status: String,
    pub executable_or_bundle: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolCatalogDto {
    pub status: String,
    pub tools: Vec<ToolDiscoveryDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ToolPolicyDto {
    pub pinned_tool_ids: Vec<String>,
    pub hidden_tool_ids: Vec<String>,
    pub user_order: Vec<String>,
    pub recent_tool_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsMenuToolDto {
    Git,
    Svn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsHeadKindDto {
    None,
    Branch,
    Detached,
    Unborn,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsHeadStateDto {
    pub kind: VcsHeadKindDto,
    pub name: Option<String>,
    pub oid: Option<String>,
    pub short_oid: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsChangeSummaryDto {
    pub staged: Option<u32>,
    pub unstaged: u32,
    pub untracked: u32,
    pub ignored: u32,
    pub conflicted: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsOperationKindDto {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    CleanupRequired,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsOperationControlDto {
    ContinueOperation,
    Skip,
    Abort,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsOperationMenuStateDto {
    pub kind: VcsOperationKindDto,
    pub label: String,
    pub continue_available: bool,
    pub skip_available: bool,
    pub abort_available: bool,
    pub resolve_available: bool,
    pub cleanup_available: bool,
    pub custom_rebase: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SvnWorkingCopyMenuStateDto {
    pub wc_root: String,
    pub url: Option<String>,
    pub revision: Option<u64>,
    pub switched: bool,
    pub property_modified: bool,
    pub missing: u32,
    pub conflicted: u32,
    pub unversioned: u32,
    pub cleanup_required: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsMenuRepositoryDto {
    pub id: String,
    pub tool: VcsMenuToolDto,
    pub repository_root: String,
    pub worktree_root: String,
    pub metadata_dir: Option<String>,
    pub common_dir: Option<String>,
    pub display_name: String,
    pub head: VcsHeadStateDto,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: VcsChangeSummaryDto,
    pub conflicts: Vec<String>,
    pub operation: Option<VcsOperationMenuStateDto>,
    pub svn: Option<SvnWorkingCopyMenuStateDto>,
    pub state_revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsTargetRelationDto {
    Outside,
    Root,
    InsideDirectory,
    Tracked,
    Untracked,
    Ignored,
    Submodule,
    SvnVersioned,
    SvnUnversioned,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsTargetLinkDto {
    pub repository_id: String,
    pub tool: VcsMenuToolDto,
    pub relative_path: Option<String>,
    pub relation: VcsTargetRelationDto,
    pub staged: Option<bool>,
    pub unstaged: bool,
    pub conflicted: bool,
    pub switched: bool,
    pub property_modified: bool,
    pub missing: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsTargetMenuStateDto {
    pub path: String,
    pub links: Vec<VcsTargetLinkDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VcsMenuSnapshotDto {
    pub schema_version: u32,
    pub observed_at_unix_ms: i64,
    pub git_available: bool,
    pub svn_available: bool,
    pub repositories: Vec<VcsMenuRepositoryDto>,
    pub targets: Vec<VcsTargetMenuStateDto>,
    pub state_revision: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuRequestDto {
    pub surface: String,
    pub platform: String,
    pub primary_path: Option<String>,
    pub targets: Vec<ContextMenuTargetDto>,
    pub working_directory: String,
    pub container_path: String,
    pub container_is_primary_target: bool,
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
    pub open_with: OpenWithCatalogDto,
    pub tools: ToolCatalogDto,
    pub tool_policy: ToolPolicyDto,
    pub vcs_menu: VcsMenuSnapshotDto,
    pub compact: bool,
    pub hide_unavailable: bool,
    pub confirm_high_risk: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuActionDto {
    pub action_id: String,
    pub variant_key: Option<String>,
    pub label: String,
    pub icon_key: String,
    pub enabled: bool,
    pub disabled_reason_code: Option<String>,
    pub disabled_reason: Option<String>,
    pub risk: String,
    pub confirmation_required: bool,
    pub target_paths: Vec<String>,
    pub target_kinds: Vec<String>,
    pub payload_kind: String,
    pub payload_value: Option<String>,
    pub payload_state_revision: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuEntryDto {
    pub kind: String,
    pub action: Option<ContextMenuActionDto>,
    pub submenu_id: Option<String>,
    pub label: String,
    pub icon_key: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuGroupDto {
    pub id: String,
    pub entries: Vec<ContextMenuEntryDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuSubmenuDto {
    pub id: String,
    pub label: String,
    pub icon_key: String,
    pub groups: Vec<ContextMenuGroupDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuPlanDto {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub title: String,
    pub groups: Vec<ContextMenuGroupDto>,
    pub submenus: Vec<ContextMenuSubmenuDto>,
    pub diagnostics: Vec<String>,
}

#[uniffi::export]
pub fn context_menu_tool_definitions(platform: String) -> Vec<ToolDefinitionDto> {
    builtin_tool_definitions(ToolPlatform::parse(&platform))
        .into_iter()
        .map(tool_definition_dto)
        .collect()
}

#[uniffi::export]
pub fn load_vcs_menu_snapshot(paths: Vec<String>, working_directory: String) -> VcsMenuSnapshotDto {
    let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    vcs_menu_snapshot_dto(crate::vcs_menu::load_vcs_menu_snapshot(
        &paths,
        Path::new(&working_directory),
    ))
}

#[uniffi::export]
pub fn revalidate_vcs_revision(
    path: String,
    tool: VcsMenuToolDto,
    expected_revision: String,
) -> Result<(), VcsMenuError> {
    crate::vcs_menu::revalidate_vcs_revision(
        Path::new(&path),
        vcs_menu_tool(tool),
        &expected_revision,
    )
    .map_err(VcsMenuError::operation)
}

#[uniffi::export]
pub fn run_git_menu_control(
    path: String,
    control: VcsOperationControlDto,
) -> Result<String, VcsMenuError> {
    let control = match control {
        VcsOperationControlDto::ContinueOperation => crate::vcs_menu::VcsOperationControl::Continue,
        VcsOperationControlDto::Skip => crate::vcs_menu::VcsOperationControl::Skip,
        VcsOperationControlDto::Abort => crate::vcs_menu::VcsOperationControl::Abort,
    };
    crate::vcs_menu::run_git_menu_control(Path::new(&path), control)
        .map_err(VcsMenuError::operation)
}

#[uniffi::export]
pub fn compose_context_menu(request: ContextMenuRequestDto) -> ContextMenuPlanDto {
    let context = snapshot_from_request(request);
    plan_dto(compose_menu_with_fallback(&context))
}

fn snapshot_from_request(request: ContextMenuRequestDto) -> ContextSnapshot {
    let targets = request
        .targets
        .into_iter()
        .map(|target| {
            let path_kind = if archive::is_supported_archive(Path::new(&target.path)) {
                TargetKind::Archive
            } else {
                TargetKind::parse(&target.kind)
            };
            let mut context_target =
                ContextTarget::new(target.path, target.display_name, path_kind);
            context_target.exists = target.exists;
            context_target.readable = target.readable;
            context_target
        })
        .collect::<Vec<_>>();
    let primary = request
        .primary_path
        .or_else(|| targets.first().map(|target| target.id.clone()));
    let target_set = TargetSet::new(primary, targets, request.working_directory);
    let mut container =
        ContainerSnapshot::for_path(request.container_path, request.container_is_primary_target);
    container.writable = request.can_create;
    ContextSnapshot {
        schema_version: crate::context_menu_model::CONTEXT_MENU_SCHEMA_VERSION,
        snapshot_id: new_snapshot_id(),
        platform: ContextPlatform::parse(&request.platform),
        surface: ContextSurface::parse(&request.surface),
        target_set,
        container,
        capabilities: ContextCapabilities {
            loading: request.loading,
            can_paste: request.can_paste,
            can_create: request.can_create,
            supports_transfer_ops: request.supports_transfer_ops,
            supports_cut: request.supports_cut,
            supports_rename: request.supports_rename,
            supports_delete: request.supports_delete,
            supports_favorite: request.supports_favorite,
            is_favorite: request.is_favorite,
            git_available: request.git_available,
            svn_available: request.svn_available,
            agent_available: request.agent_available,
            quick_look_available: request.quick_look_available,
            reveal_available: request.reveal_available,
            share_available: request.share_available,
            properties_available: request.properties_available,
            text_editor_available: request.text_editor_available,
            mindmap_available: request.mindmap_available,
            mindmap_import_available: request.mindmap_import_available,
            is_shell_namespace: request.is_shell_namespace,
            native_plane_available: request.native_plane_available,
        },
        tool_catalog: tool_catalog_from_dto(request.tools),
        tool_policy: tool_policy_from_dto(request.tool_policy),
        vcs_menu: vcs_menu_from_dto(request.vcs_menu),
        open_with: OpenWithCatalog {
            status: match request.open_with.status {
                OpenWithCatalogStatusDto::Ready => OpenWithCatalogStatus::Ready,
                OpenWithCatalogStatusDto::Loading => OpenWithCatalogStatus::Loading,
                OpenWithCatalogStatusDto::Unavailable => OpenWithCatalogStatus::Unavailable,
            },
            applications: request
                .open_with
                .applications
                .into_iter()
                .map(|application| OpenWithApplication {
                    application_id: application.application_id,
                    display_name: application.display_name,
                    is_default: application.is_default,
                    is_recent: application.is_recent,
                    is_available: application.is_available,
                    unavailable_reason: application.unavailable_reason,
                })
                .collect(),
        },
        policy: ContextPolicy {
            compact: request.compact,
            hide_unavailable: request.hide_unavailable,
            confirm_high_risk: request.confirm_high_risk,
        },
    }
}

fn tool_definition_dto(definition: ToolDefinition) -> ToolDefinitionDto {
    ToolDefinitionDto {
        tool_id: definition.tool_id,
        category: definition.category.as_str().to_string(),
        display_name: definition.display_name,
        description: definition.description,
        platform: definition.platform.as_str().to_string(),
        launch_kind: definition.launch_kind.as_str().to_string(),
        executable_identity: definition.executable_identity,
        bundle_identity: definition.bundle_identity,
        capabilities: definition
            .capabilities
            .into_iter()
            .map(|capability| ToolCapabilityDto {
                target_shapes: capability
                    .target_shapes
                    .into_iter()
                    .map(|shape| shape.as_str().to_string())
                    .collect(),
                roles: capability
                    .roles
                    .into_iter()
                    .map(|role| role.as_str().to_string())
                    .collect(),
                min_paths: u32::try_from(capability.min_paths).unwrap_or(u32::MAX),
                max_paths: capability
                    .max_paths
                    .map(|value| u32::try_from(value).unwrap_or(u32::MAX)),
                all_targets_required: capability.all_targets_required,
                path_filter: capability.path_filter.as_str().to_string(),
            })
            .collect(),
        recipes: definition
            .recipes
            .into_iter()
            .map(|recipe| ToolRecipeDto {
                role: recipe.role.as_str().to_string(),
                arguments: recipe
                    .arguments
                    .into_iter()
                    .map(|argument| match argument {
                        ToolArgument::Literal(value) => ToolArgumentDto {
                            kind: "literal".to_string(),
                            value: Some(value),
                        },
                        ToolArgument::PrimaryPath => ToolArgumentDto {
                            kind: "primary_path".to_string(),
                            value: None,
                        },
                        ToolArgument::TargetPaths => ToolArgumentDto {
                            kind: "target_paths".to_string(),
                            value: None,
                        },
                        ToolArgument::WorkingDirectory => ToolArgumentDto {
                            kind: "working_directory".to_string(),
                            value: None,
                        },
                        ToolArgument::RepositoryRoot => ToolArgumentDto {
                            kind: "repository_root".to_string(),
                            value: None,
                        },
                    })
                    .collect(),
                working_directory: recipe.working_directory.map(|directory| match directory {
                    crate::tool_registry::ToolWorkingDirectory::Context => "context".to_string(),
                    crate::tool_registry::ToolWorkingDirectory::PrimaryTarget => {
                        "primary_target".to_string()
                    }
                    crate::tool_registry::ToolWorkingDirectory::RepositoryRoot => {
                        "repository_root".to_string()
                    }
                }),
            })
            .collect(),
    }
}

fn tool_catalog_from_dto(catalog: ToolCatalogDto) -> ToolCatalog {
    ToolCatalog {
        status: match catalog.status.as_str() {
            "ready" => ToolCatalogStatus::Ready,
            "loading" => ToolCatalogStatus::Loading,
            _ => ToolCatalogStatus::Unavailable,
        },
        tools: catalog
            .tools
            .into_iter()
            .map(|tool| DiscoveredTool {
                tool_id: tool.tool_id,
                status: match tool.status.as_str() {
                    "available" => ToolAvailability::Available,
                    "not_installed" => ToolAvailability::NotInstalled,
                    "invalidated" => ToolAvailability::Invalidated,
                    "unsupported" => ToolAvailability::Unsupported,
                    "denied" => ToolAvailability::Denied,
                    _ => ToolAvailability::Unknown,
                },
                executable_or_bundle: tool.executable_or_bundle,
                version: tool.version,
            })
            .collect(),
    }
}

fn tool_policy_from_dto(policy: ToolPolicyDto) -> ToolPolicy {
    ToolPolicy {
        pinned_tool_ids: policy.pinned_tool_ids,
        hidden_tool_ids: policy.hidden_tool_ids,
        user_order: policy.user_order,
        recent_tool_ids: policy.recent_tool_ids,
    }
}

fn new_snapshot_id() -> String {
    let sequence = NEXT_SNAPSHOT_ID.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("context-menu-{timestamp}-{sequence}")
}

fn vcs_menu_tool(tool: VcsMenuToolDto) -> VcsMenuTool {
    match tool {
        VcsMenuToolDto::Git => VcsMenuTool::Git,
        VcsMenuToolDto::Svn => VcsMenuTool::Svn,
    }
}

fn vcs_menu_tool_dto(tool: VcsMenuTool) -> VcsMenuToolDto {
    match tool {
        VcsMenuTool::Git => VcsMenuToolDto::Git,
        VcsMenuTool::Svn => VcsMenuToolDto::Svn,
    }
}

fn vcs_head_kind_from_dto(kind: VcsHeadKindDto) -> VcsHeadKind {
    match kind {
        VcsHeadKindDto::None => VcsHeadKind::None,
        VcsHeadKindDto::Branch => VcsHeadKind::Branch,
        VcsHeadKindDto::Detached => VcsHeadKind::Detached,
        VcsHeadKindDto::Unborn => VcsHeadKind::Unborn,
    }
}

fn vcs_head_kind_dto(kind: VcsHeadKind) -> VcsHeadKindDto {
    match kind {
        VcsHeadKind::None => VcsHeadKindDto::None,
        VcsHeadKind::Branch => VcsHeadKindDto::Branch,
        VcsHeadKind::Detached => VcsHeadKindDto::Detached,
        VcsHeadKind::Unborn => VcsHeadKindDto::Unborn,
    }
}

fn vcs_operation_kind_from_dto(kind: VcsOperationKindDto) -> VcsOperationKind {
    match kind {
        VcsOperationKindDto::Merge => VcsOperationKind::Merge,
        VcsOperationKindDto::Rebase => VcsOperationKind::Rebase,
        VcsOperationKindDto::CherryPick => VcsOperationKind::CherryPick,
        VcsOperationKindDto::Revert => VcsOperationKind::Revert,
        VcsOperationKindDto::Bisect => VcsOperationKind::Bisect,
        VcsOperationKindDto::CleanupRequired => VcsOperationKind::CleanupRequired,
        VcsOperationKindDto::Busy => VcsOperationKind::Busy,
    }
}

fn vcs_operation_kind_dto(kind: VcsOperationKind) -> VcsOperationKindDto {
    match kind {
        VcsOperationKind::Merge => VcsOperationKindDto::Merge,
        VcsOperationKind::Rebase => VcsOperationKindDto::Rebase,
        VcsOperationKind::CherryPick => VcsOperationKindDto::CherryPick,
        VcsOperationKind::Revert => VcsOperationKindDto::Revert,
        VcsOperationKind::Bisect => VcsOperationKindDto::Bisect,
        VcsOperationKind::CleanupRequired => VcsOperationKindDto::CleanupRequired,
        VcsOperationKind::Busy => VcsOperationKindDto::Busy,
    }
}

fn vcs_target_relation_from_dto(relation: VcsTargetRelationDto) -> VcsTargetRelation {
    match relation {
        VcsTargetRelationDto::Outside => VcsTargetRelation::Outside,
        VcsTargetRelationDto::Root => VcsTargetRelation::Root,
        VcsTargetRelationDto::InsideDirectory => VcsTargetRelation::InsideDirectory,
        VcsTargetRelationDto::Tracked => VcsTargetRelation::Tracked,
        VcsTargetRelationDto::Untracked => VcsTargetRelation::Untracked,
        VcsTargetRelationDto::Ignored => VcsTargetRelation::Ignored,
        VcsTargetRelationDto::Submodule => VcsTargetRelation::Submodule,
        VcsTargetRelationDto::SvnVersioned => VcsTargetRelation::SvnVersioned,
        VcsTargetRelationDto::SvnUnversioned => VcsTargetRelation::SvnUnversioned,
    }
}

fn vcs_target_relation_dto(relation: VcsTargetRelation) -> VcsTargetRelationDto {
    match relation {
        VcsTargetRelation::Outside => VcsTargetRelationDto::Outside,
        VcsTargetRelation::Root => VcsTargetRelationDto::Root,
        VcsTargetRelation::InsideDirectory => VcsTargetRelationDto::InsideDirectory,
        VcsTargetRelation::Tracked => VcsTargetRelationDto::Tracked,
        VcsTargetRelation::Untracked => VcsTargetRelationDto::Untracked,
        VcsTargetRelation::Ignored => VcsTargetRelationDto::Ignored,
        VcsTargetRelation::Submodule => VcsTargetRelationDto::Submodule,
        VcsTargetRelation::SvnVersioned => VcsTargetRelationDto::SvnVersioned,
        VcsTargetRelation::SvnUnversioned => VcsTargetRelationDto::SvnUnversioned,
    }
}

fn vcs_menu_from_dto(snapshot: VcsMenuSnapshotDto) -> VcsMenuSnapshot {
    VcsMenuSnapshot {
        schema_version: snapshot.schema_version,
        observed_at_unix_ms: snapshot.observed_at_unix_ms,
        git_available: snapshot.git_available,
        svn_available: snapshot.svn_available,
        repositories: snapshot
            .repositories
            .into_iter()
            .map(|repository| VcsMenuRepository {
                id: repository.id,
                tool: vcs_menu_tool(repository.tool),
                repository_root: repository.repository_root,
                worktree_root: repository.worktree_root,
                metadata_dir: repository.metadata_dir,
                common_dir: repository.common_dir,
                display_name: repository.display_name,
                head: VcsHeadState {
                    kind: vcs_head_kind_from_dto(repository.head.kind),
                    name: repository.head.name,
                    oid: repository.head.oid,
                    short_oid: repository.head.short_oid,
                },
                upstream: repository.upstream,
                ahead: repository.ahead,
                behind: repository.behind,
                changes: VcsChangeSummary {
                    staged: repository.changes.staged,
                    unstaged: repository.changes.unstaged,
                    untracked: repository.changes.untracked,
                    ignored: repository.changes.ignored,
                    conflicted: repository.changes.conflicted,
                },
                conflicts: repository.conflicts,
                operation: repository.operation.map(|operation| VcsOperationMenuState {
                    kind: vcs_operation_kind_from_dto(operation.kind),
                    label: operation.label,
                    continue_available: operation.continue_available,
                    skip_available: operation.skip_available,
                    abort_available: operation.abort_available,
                    resolve_available: operation.resolve_available,
                    cleanup_available: operation.cleanup_available,
                    custom_rebase: operation.custom_rebase,
                }),
                svn: repository.svn.map(|svn| SvnWorkingCopyMenuState {
                    wc_root: svn.wc_root,
                    url: svn.url,
                    revision: svn.revision,
                    switched: svn.switched,
                    property_modified: svn.property_modified,
                    missing: svn.missing,
                    conflicted: svn.conflicted,
                    unversioned: svn.unversioned,
                    cleanup_required: svn.cleanup_required,
                }),
                state_revision: repository.state_revision,
            })
            .collect(),
        targets: snapshot
            .targets
            .into_iter()
            .map(|target| VcsTargetMenuState {
                path: target.path,
                links: target
                    .links
                    .into_iter()
                    .map(|link| VcsTargetLink {
                        repository_id: link.repository_id,
                        tool: vcs_menu_tool(link.tool),
                        relative_path: link.relative_path,
                        relation: vcs_target_relation_from_dto(link.relation),
                        staged: link.staged,
                        unstaged: link.unstaged,
                        conflicted: link.conflicted,
                        switched: link.switched,
                        property_modified: link.property_modified,
                        missing: link.missing,
                    })
                    .collect(),
            })
            .collect(),
        state_revision: snapshot.state_revision,
    }
}

fn vcs_menu_snapshot_dto(snapshot: VcsMenuSnapshot) -> VcsMenuSnapshotDto {
    VcsMenuSnapshotDto {
        schema_version: snapshot.schema_version,
        observed_at_unix_ms: snapshot.observed_at_unix_ms,
        git_available: snapshot.git_available,
        svn_available: snapshot.svn_available,
        repositories: snapshot
            .repositories
            .into_iter()
            .map(|repository| VcsMenuRepositoryDto {
                id: repository.id,
                tool: vcs_menu_tool_dto(repository.tool),
                repository_root: repository.repository_root,
                worktree_root: repository.worktree_root,
                metadata_dir: repository.metadata_dir,
                common_dir: repository.common_dir,
                display_name: repository.display_name,
                head: VcsHeadStateDto {
                    kind: vcs_head_kind_dto(repository.head.kind),
                    name: repository.head.name,
                    oid: repository.head.oid,
                    short_oid: repository.head.short_oid,
                },
                upstream: repository.upstream,
                ahead: repository.ahead,
                behind: repository.behind,
                changes: VcsChangeSummaryDto {
                    staged: repository.changes.staged,
                    unstaged: repository.changes.unstaged,
                    untracked: repository.changes.untracked,
                    ignored: repository.changes.ignored,
                    conflicted: repository.changes.conflicted,
                },
                conflicts: repository.conflicts,
                operation: repository
                    .operation
                    .map(|operation| VcsOperationMenuStateDto {
                        kind: vcs_operation_kind_dto(operation.kind),
                        label: operation.label,
                        continue_available: operation.continue_available,
                        skip_available: operation.skip_available,
                        abort_available: operation.abort_available,
                        resolve_available: operation.resolve_available,
                        cleanup_available: operation.cleanup_available,
                        custom_rebase: operation.custom_rebase,
                    }),
                svn: repository.svn.map(|svn| SvnWorkingCopyMenuStateDto {
                    wc_root: svn.wc_root,
                    url: svn.url,
                    revision: svn.revision,
                    switched: svn.switched,
                    property_modified: svn.property_modified,
                    missing: svn.missing,
                    conflicted: svn.conflicted,
                    unversioned: svn.unversioned,
                    cleanup_required: svn.cleanup_required,
                }),
                state_revision: repository.state_revision,
            })
            .collect(),
        targets: snapshot
            .targets
            .into_iter()
            .map(|target| VcsTargetMenuStateDto {
                path: target.path,
                links: target
                    .links
                    .into_iter()
                    .map(|link| VcsTargetLinkDto {
                        repository_id: link.repository_id,
                        tool: vcs_menu_tool_dto(link.tool),
                        relative_path: link.relative_path,
                        relation: vcs_target_relation_dto(link.relation),
                        staged: link.staged,
                        unstaged: link.unstaged,
                        conflicted: link.conflicted,
                        switched: link.switched,
                        property_modified: link.property_modified,
                        missing: link.missing,
                    })
                    .collect(),
            })
            .collect(),
        state_revision: snapshot.state_revision,
    }
}

fn plan_dto(plan: MenuPlan) -> ContextMenuPlanDto {
    let target_kinds = plan.target_kinds.clone();
    ContextMenuPlanDto {
        schema_version: plan.schema_version,
        snapshot_id: plan.snapshot_id,
        title: plan.title,
        groups: plan
            .groups
            .into_iter()
            .map(|group| ContextMenuGroupDto {
                id: group.id,
                entries: group
                    .entries
                    .into_iter()
                    .map(|entry| match entry {
                        MenuEntryPlan::Action(action) => ContextMenuEntryDto {
                            kind: "action".to_string(),
                            action: Some(ContextMenuActionDto {
                                action_id: action.action_id,
                                variant_key: action.variant_key,
                                label: action.label,
                                icon_key: action.icon_key,
                                enabled: action.enabled,
                                disabled_reason_code: action.disabled_reason_code,
                                disabled_reason: action.disabled_reason,
                                risk: action.risk.as_str().to_string(),
                                confirmation_required: action.requires_confirmation,
                                target_paths: action.targets,
                                target_kinds: target_kinds.clone(),
                                payload_kind: action.payload.kind().to_string(),
                                payload_value: action.payload.value().map(str::to_string),
                                payload_state_revision: action
                                    .payload
                                    .state_revision()
                                    .map(str::to_string),
                            }),
                            submenu_id: None,
                            label: String::new(),
                            icon_key: String::new(),
                            enabled: true,
                        },
                        MenuEntryPlan::Submenu(submenu) => ContextMenuEntryDto {
                            kind: "submenu".to_string(),
                            action: None,
                            submenu_id: Some(submenu.id.clone()),
                            label: submenu.label,
                            icon_key: submenu.icon_key,
                            enabled: true,
                        },
                    })
                    .collect(),
            })
            .collect(),
        submenus: plan
            .submenus
            .into_iter()
            .map(|submenu| ContextMenuSubmenuDto {
                id: submenu.id,
                label: submenu.label,
                icon_key: submenu.icon_key,
                groups: submenu
                    .sections
                    .into_iter()
                    .map(|section| ContextMenuGroupDto {
                        id: section.id,
                        entries: section
                            .entries
                            .into_iter()
                            .map(|action| ContextMenuEntryDto {
                                kind: "action".to_string(),
                                action: Some(ContextMenuActionDto {
                                    action_id: action.action_id,
                                    variant_key: action.variant_key,
                                    label: action.label,
                                    icon_key: action.icon_key,
                                    enabled: action.enabled,
                                    disabled_reason_code: action.disabled_reason_code,
                                    disabled_reason: action.disabled_reason,
                                    risk: action.risk.as_str().to_string(),
                                    confirmation_required: action.requires_confirmation,
                                    target_paths: action.targets,
                                    target_kinds: target_kinds.clone(),
                                    payload_kind: action.payload.kind().to_string(),
                                    payload_value: action.payload.value().map(str::to_string),
                                    payload_state_revision: action
                                        .payload
                                        .state_revision()
                                        .map(str::to_string),
                                }),
                                submenu_id: None,
                                label: String::new(),
                                icon_key: String::new(),
                                enabled: true,
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
        diagnostics: plan.diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ContextMenuRequestDto {
        ContextMenuRequestDto {
            surface: "browser.list".to_string(),
            platform: "macos".to_string(),
            primary_path: Some("/tmp/file.txt".to_string()),
            targets: vec![ContextMenuTargetDto {
                path: "/tmp/file.txt".to_string(),
                display_name: "file.txt".to_string(),
                kind: "file".to_string(),
                exists: true,
                readable: true,
            }],
            working_directory: "/tmp".to_string(),
            container_path: "/tmp".to_string(),
            container_is_primary_target: false,
            loading: false,
            can_paste: true,
            can_create: true,
            supports_transfer_ops: true,
            supports_cut: true,
            supports_rename: true,
            supports_delete: true,
            supports_favorite: true,
            is_favorite: false,
            git_available: false,
            svn_available: false,
            agent_available: true,
            quick_look_available: true,
            reveal_available: true,
            share_available: true,
            properties_available: true,
            text_editor_available: true,
            mindmap_available: false,
            mindmap_import_available: false,
            is_shell_namespace: false,
            native_plane_available: false,
            open_with: OpenWithCatalogDto {
                status: OpenWithCatalogStatusDto::Loading,
                applications: Vec::new(),
            },
            tools: ToolCatalogDto {
                status: "unavailable".to_string(),
                tools: Vec::new(),
            },
            tool_policy: ToolPolicyDto {
                pinned_tool_ids: Vec::new(),
                hidden_tool_ids: Vec::new(),
                user_order: Vec::new(),
                recent_tool_ids: Vec::new(),
            },
            vcs_menu: VcsMenuSnapshotDto {
                schema_version: crate::vcs_menu::VCS_MENU_SCHEMA_VERSION,
                observed_at_unix_ms: 0,
                git_available: false,
                svn_available: false,
                repositories: Vec::new(),
                targets: Vec::new(),
                state_revision: String::new(),
            },
            compact: true,
            hide_unavailable: true,
            confirm_high_risk: true,
        }
    }

    #[test]
    fn ffi_plan_is_flat_and_keeps_native_entry_last() {
        let plan = compose_context_menu(request());
        assert_eq!(plan.schema_version, 1);
        assert_eq!(plan.title, "file.txt");
        let last = plan.groups.last().expect("last group");
        assert_eq!(last.id, "G99");
        assert!(matches!(
            last.entries.last().and_then(|entry| entry.action.as_ref()),
            Some(action) if action.action_id == "system.native-menu" && action.enabled
        ));
        assert!(
            plan.submenus
                .iter()
                .any(|submenu| submenu.id == "file-actions")
        );
    }
}
