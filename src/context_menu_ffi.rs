//! Minimal UniFFI adapter for the shared context-menu Composer.
//!
//! Domain types stay in `context_menu_model`; this module only converts the
//! frozen Swift request into that model and flattens the depth-limited plan into
//! UniFFI-safe records.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::archive;
use crate::context_menu_model::{
    ContainerSnapshot, ContextCapabilities, ContextPlatform, ContextPolicy, ContextSnapshot,
    ContextSurface, ContextTarget, MenuEntryPlan, MenuPlan, TargetKind, TargetSet,
    compose_menu_with_fallback,
};

static NEXT_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContextMenuTargetDto {
    pub path: String,
    pub display_name: String,
    pub kind: String,
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
    pub payload_kind: String,
    pub payload_value: Option<String>,
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
            ContextTarget::new(target.path, target.display_name, path_kind)
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
        policy: ContextPolicy {
            compact: request.compact,
            hide_unavailable: request.hide_unavailable,
            confirm_high_risk: request.confirm_high_risk,
        },
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

fn plan_dto(plan: MenuPlan) -> ContextMenuPlanDto {
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
                                payload_kind: action.payload.kind().to_string(),
                                payload_value: action.payload.value().map(str::to_string),
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
                                    payload_kind: action.payload.kind().to_string(),
                                    payload_value: action.payload.value().map(str::to_string),
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
            Some(action) if action.action_id == "system.native-menu" && !action.enabled
        ));
        assert!(
            plan.submenus
                .iter()
                .any(|submenu| submenu.id == "file-actions")
        );
    }
}
