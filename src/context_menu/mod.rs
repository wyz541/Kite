use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::StatefulInteractiveElement as _;
use gpui_kit::*;
use kite_core::context_menu_model::{
    ContainerSnapshot, ContextCapabilities, ContextPlatform, ContextPolicy, ContextSnapshot,
    ContextSurface, ContextTarget, MenuEntryPlan, MenuPlan, OpenWithCatalog, ResolvedAction,
    TargetKind, TargetSet, compose_menu_with_fallback,
};

use crate::actions::{ContextMenuAction, ContextMenuActionPayload, ContextMenuCommand};
use crate::archive;
use crate::platform::HostPlatform;

#[cfg(target_os = "windows")]
mod open_with_windows;
#[cfg(target_os = "windows")]
mod tool_registry_windows;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub(crate) use open_with_windows as open_with;
#[cfg(target_os = "windows")]
pub(crate) use windows::{request_open_with_dialog, request_system_menu, show_properties};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextMenuPolicy {
    pub platform: HostPlatform,
    pub compact: bool,
    pub hide_unavailable: bool,
}

impl ContextMenuPolicy {
    pub(crate) const fn current() -> Self {
        Self::for_platform(crate::platform::host_platform())
    }

    pub(crate) const fn for_platform(platform: HostPlatform) -> Self {
        match platform {
            HostPlatform::Windows => Self {
                platform,
                compact: false,
                hide_unavailable: false,
            },
            HostPlatform::MacOS | HostPlatform::Other => Self {
                platform,
                compact: true,
                hide_unavailable: true,
            },
        }
    }

    fn core_policy(self) -> ContextPolicy {
        ContextPolicy {
            compact: self.compact,
            hide_unavailable: self.hide_unavailable,
            confirm_high_risk: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ContextMenuCapabilities {
    pub loading: bool,
    pub can_paste: bool,
    pub git_available: bool,
    pub svn_available: bool,
    pub supports_transfer_ops: bool,
    pub supports_cut: bool,
    pub supports_rename: bool,
    pub supports_delete: bool,
    pub supports_favorite: bool,
    pub is_shell_namespace: bool,
}

impl ContextMenuCapabilities {
    pub(crate) fn for_path(
        path: &Path,
        loading: bool,
        can_paste: bool,
        git_available: bool,
        svn_available: bool,
    ) -> Self {
        let is_shell_namespace = crate::shell_fs::is_shell_namespace_path(path);
        let (supports_cut, supports_rename, supports_delete) = if is_shell_namespace {
            let (can_rename, can_delete, can_move) = crate::shell_fs::item_capabilities(path);
            (can_move, can_rename, can_delete)
        } else {
            let can_change = path.parent().is_some();
            (can_change, can_change, can_change)
        };
        Self {
            loading,
            can_paste,
            git_available,
            svn_available,
            supports_transfer_ops: true,
            supports_cut,
            supports_rename,
            supports_delete,
            supports_favorite: !is_shell_namespace,
            is_shell_namespace,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ContextMenuTarget {
    pub path: PathBuf,
    pub paths: Vec<PathBuf>,
    pub working_directory: PathBuf,
    pub title: String,
    pub is_dir: bool,
    pub loading: bool,
    pub can_paste: bool,
    pub git_available: bool,
    pub svn_available: bool,
    pub supports_transfer_ops: bool,
    pub supports_cut: bool,
    pub supports_rename: bool,
    pub supports_delete: bool,
    pub supports_favorite: bool,
    pub is_shell_namespace: bool,
    pub agent_available: bool,
    pub container_is_primary_target: bool,
}

impl ContextMenuTarget {
    pub(crate) fn directory(
        path: PathBuf,
        title: String,
        capabilities: ContextMenuCapabilities,
    ) -> Self {
        Self {
            working_directory: path.clone(),
            path: path.clone(),
            paths: vec![path],
            title,
            is_dir: true,
            loading: capabilities.loading,
            can_paste: capabilities.can_paste,
            git_available: capabilities.git_available,
            svn_available: capabilities.svn_available,
            supports_transfer_ops: capabilities.supports_transfer_ops,
            supports_cut: capabilities.supports_cut,
            supports_rename: capabilities.supports_rename,
            supports_delete: capabilities.supports_delete,
            supports_favorite: capabilities.supports_favorite,
            is_shell_namespace: capabilities.is_shell_namespace,
            agent_available: false,
            container_is_primary_target: true,
        }
    }

    pub(crate) fn entry(
        path: PathBuf,
        title: String,
        is_dir: bool,
        working_directory: PathBuf,
        capabilities: ContextMenuCapabilities,
    ) -> Self {
        Self {
            path: path.clone(),
            paths: vec![path],
            working_directory,
            title,
            is_dir,
            loading: capabilities.loading,
            can_paste: capabilities.can_paste,
            git_available: capabilities.git_available,
            svn_available: capabilities.svn_available,
            supports_transfer_ops: capabilities.supports_transfer_ops,
            supports_cut: capabilities.supports_cut,
            supports_rename: capabilities.supports_rename,
            supports_delete: capabilities.supports_delete,
            supports_favorite: capabilities.supports_favorite,
            is_shell_namespace: capabilities.is_shell_namespace,
            agent_available: false,
            container_is_primary_target: false,
        }
    }

    pub(crate) fn with_agent_available(mut self, available: bool) -> Self {
        self.agent_available = available;
        self
    }

    pub(crate) fn with_paths(mut self, paths: Vec<PathBuf>) -> Self {
        if !paths.is_empty() {
            self.paths = paths;
        }
        self
    }

    fn action(&self, plan_action: &ResolvedAction) -> Option<ContextMenuAction> {
        let command = ContextMenuCommand::from_action_id(
            &plan_action.action_id,
            plan_action.variant_key.as_deref(),
        )?;
        let paths = plan_action
            .targets
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let payload = match &plan_action.payload {
            kite_core::context_menu_model::ActionPayload::Application { application_id } => {
                ContextMenuActionPayload::Application(application_id.clone())
            }
            kite_core::context_menu_model::ActionPayload::Tool { tool_id } => {
                ContextMenuActionPayload::Tool(tool_id.clone())
            }
            _ => ContextMenuActionPayload::None,
        };
        Some(ContextMenuAction {
            target: paths.first().cloned().unwrap_or_else(|| self.path.clone()),
            paths,
            working_directory: self.working_directory.clone(),
            command,
            action_id: plan_action.action_id.clone(),
            variant_key: plan_action.variant_key.clone(),
            payload,
        })
    }

    fn context_snapshot(&self, policy: ContextMenuPolicy) -> ContextSnapshot {
        let paths = if self.paths.is_empty() {
            vec![self.path.clone()]
        } else {
            self.paths.clone()
        };
        let targets = paths
            .iter()
            .map(|path| self.context_target(path))
            .collect::<Vec<_>>();
        let primary = targets
            .iter()
            .find(|target| target.path == self.path.display().to_string())
            .or_else(|| targets.first())
            .map(|target| target.id.clone());
        let mut target_set = TargetSet::new(
            primary,
            targets,
            self.working_directory.display().to_string(),
        );
        target_set.selection_relation = if paths.len() > 1 {
            kite_core::context_menu_model::SelectionRelation::ClickedInSelection
        } else {
            kite_core::context_menu_model::SelectionRelation::None
        };
        let mut container = ContainerSnapshot::for_path(
            self.working_directory.display().to_string(),
            self.container_is_primary_target,
        );
        container.writable = self.supports_transfer_ops && !self.is_shell_namespace;
        let open_with = if paths.len() == 1 && !self.is_shell_namespace {
            open_with_windows::catalog(&paths[0])
        } else {
            OpenWithCatalog::default()
        };
        let tool_catalog = if self.is_shell_namespace {
            kite_core::tool_registry::ToolCatalog::default()
        } else {
            tool_registry_windows::catalog(self)
        };
        ContextSnapshot {
            schema_version: kite_core::context_menu_model::CONTEXT_MENU_SCHEMA_VERSION,
            snapshot_id: format!("windows-context-menu:{}", self.path.display()),
            platform: ContextPlatform::Windows,
            surface: if self.container_is_primary_target {
                ContextSurface::BrowserColumns
            } else {
                ContextSurface::BrowserList
            },
            target_set,
            container,
            capabilities: ContextCapabilities {
                loading: self.loading,
                can_paste: self.can_paste,
                can_create: self.supports_transfer_ops
                    && !self.is_shell_namespace
                    && self.container_is_primary_target,
                supports_transfer_ops: self.supports_transfer_ops,
                supports_cut: self.supports_cut,
                supports_rename: self.supports_rename,
                supports_delete: self.supports_delete,
                supports_favorite: self.supports_favorite,
                is_favorite: false,
                git_available: self.git_available,
                svn_available: self.svn_available,
                agent_available: self.agent_available,
                quick_look_available: false,
                reveal_available: false,
                share_available: false,
                properties_available: true,
                text_editor_available: false,
                mindmap_available: false,
                mindmap_import_available: false,
                is_shell_namespace: self.is_shell_namespace,
                native_plane_available: true,
            },
            open_with,
            tool_catalog,
            tool_policy: tool_registry_windows::policy(),
            policy: policy.core_policy(),
        }
    }

    fn context_target(&self, path: &Path) -> ContextTarget {
        let kind = if archive::is_supported_archive(path) {
            TargetKind::Archive
        } else if path.is_dir() || (path == self.path && self.is_dir) {
            TargetKind::Directory
        } else if self.is_shell_namespace {
            TargetKind::DeviceItem
        } else {
            TargetKind::File
        };
        let display_name = if path == self.path {
            self.title.clone()
        } else {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        };
        let mut target = ContextTarget::new(path.display().to_string(), display_name, kind);
        target.exists = path.exists();
        target.readable =
            !self.is_shell_namespace && (path.is_dir() || std::fs::File::open(path).is_ok());
        target
    }
}

fn build_plan(target: &ContextMenuTarget, policy: ContextMenuPolicy) -> MenuPlan {
    compose_menu_with_fallback(&target.context_snapshot(policy))
}

fn popup_icon(icon_key: &str) -> IconName {
    match icon_key {
        "refresh" => IconName::RotateCw,
        "open" => IconName::FolderOpen,
        "open-with" => IconName::AppWindow,
        "open-in" => IconName::AppWindow,
        "compare-with" => IconName::AppWindow,
        "quick-look" => IconName::Eye,
        "copy" => IconName::Copy,
        "cut" => IconName::Scissors,
        "copy-to" => IconName::FolderInput,
        "copy-path" => IconName::Link,
        "paste" => IconName::ClipboardPaste,
        "new-folder" => IconName::FolderPlus,
        "new-file" => IconName::FileText,
        "favorite" => IconName::StarPlus,
        "rename" => IconName::Pencil,
        "trash" => IconName::Trash,
        "archive" | "extract" => IconName::FileArchive,
        "agent" => IconName::Sparkles,
        "vcs" => IconName::GitBranch,
        "system" | "properties" => IconName::Info,
        "reveal" => IconName::FolderOpen,
        "share" => IconName::Share2,
        "editor" => IconName::FileText,
        "mindmap" => IconName::Network,
        "native" => IconName::Ellipsis,
        _ => IconName::Ellipsis,
    }
}

fn append_action(
    mut menu: PopupMenu,
    action: &ResolvedAction,
    target: &ContextMenuTarget,
    action_context: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let Some(dispatched) = target.action(action) else {
        return menu;
    };
    let item = if action.action_id == "system.native-menu" {
        let hover_action = dispatched.clone();
        let hover_context = action_context.clone();
        let hover_triggered = Rc::new(Cell::new(false));
        let label = action.label.clone();
        let icon = popup_icon(&action.icon_key);
        PopupMenuItem::element(move |_, cx| {
            let hover_context = hover_context.clone();
            let hover_action = hover_action.clone();
            let hover_triggered = hover_triggered.clone();
            h_flex()
                .id("context-menu-show-more-hover")
                .w_full()
                .min_w_0()
                .items_center()
                .justify_between()
                .gap_2()
                .on_hover(move |hovered, window, cx| {
                    if !*hovered {
                        hover_triggered.set(false);
                        return;
                    }
                    if hover_triggered.replace(true) {
                        return;
                    }
                    hover_context.focus(window, cx);
                    window.dispatch_action(Box::new(hover_action.clone()), cx);
                })
                .child(label.clone())
                .child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
        })
        .icon(icon)
        .disabled(!action.enabled)
        .action(Box::new(dispatched))
    } else {
        PopupMenuItem::new(action.label.clone())
            .icon(popup_icon(&action.icon_key))
            .disabled(!action.enabled)
            .action(Box::new(dispatched))
    };
    menu = menu.item(item);
    menu
}

fn append_sections(
    mut menu: PopupMenu,
    sections: &[kite_core::context_menu_model::MenuSectionPlan],
    target: &ContextMenuTarget,
    action_context: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            menu = menu.separator();
        }
        for action in &section.entries {
            menu = append_action(menu, action, target, action_context, window, cx);
        }
    }
    menu
}

fn append_plan(
    mut menu: PopupMenu,
    plan: &MenuPlan,
    target: &ContextMenuTarget,
    action_context: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    for (group_index, group) in plan.groups.iter().enumerate() {
        if group_index > 0 {
            menu = menu.separator();
        }
        for entry in &group.entries {
            match entry {
                MenuEntryPlan::Action(action) => {
                    menu = append_action(menu, action, target, action_context, window, cx);
                }
                MenuEntryPlan::Submenu(entry_submenu) => {
                    let Some(submenu) = plan.find_submenu(&entry_submenu.id) else {
                        continue;
                    };
                    let submenu_clone = submenu.clone();
                    let target = target.clone();
                    let action_context = action_context.clone();
                    let popup = PopupMenu::build(window, cx, move |menu, window, cx| {
                        append_sections(
                            menu,
                            &submenu_clone.sections,
                            &target,
                            &action_context,
                            window,
                            cx,
                        )
                    });
                    menu = menu.item(
                        PopupMenuItem::submenu(submenu.label.clone(), popup)
                            .icon(popup_icon(&submenu.icon_key)),
                    );
                }
            }
        }
    }
    menu
}

pub(crate) fn render_context_menu(
    menu: PopupMenu,
    target: &ContextMenuTarget,
    action_context: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let policy = ContextMenuPolicy::current();
    let plan = build_plan(target, policy);
    let mut menu = menu.label(plan.title.clone());
    if !plan.groups.is_empty() {
        menu = menu.separator();
        menu = append_plan(menu, &plan, target, &action_context, window, cx);
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ContextMenuCommand;
    use kite_core::context_menu_model::ActionRisk;

    fn target(is_dir: bool) -> ContextMenuTarget {
        let path = if is_dir {
            PathBuf::from(r"C:\repo\src")
        } else {
            PathBuf::from(r"C:\repo\src\main.rs")
        };
        let working_directory = if is_dir {
            path.clone()
        } else {
            PathBuf::from(r"C:\repo\src")
        };
        ContextMenuTarget::entry(
            path,
            if is_dir {
                "src".to_string()
            } else {
                "main.rs".to_string()
            },
            is_dir,
            working_directory,
            ContextMenuCapabilities {
                loading: false,
                can_paste: true,
                git_available: true,
                svn_available: true,
                supports_transfer_ops: true,
                supports_cut: true,
                supports_rename: true,
                supports_delete: true,
                supports_favorite: true,
                is_shell_namespace: false,
            },
        )
        .with_agent_available(true)
    }

    fn windows_policy() -> ContextMenuPolicy {
        ContextMenuPolicy::for_platform(HostPlatform::Windows)
    }

    #[test]
    fn local_root_keeps_transfer_actions_and_disables_destructive_actions() {
        let capabilities = ContextMenuCapabilities::for_path(
            std::path::Path::new(r"C:\"),
            false,
            true,
            false,
            false,
        );

        assert!(capabilities.supports_transfer_ops);
        assert!(capabilities.supports_favorite);
        assert!(!capabilities.supports_cut);
        assert!(!capabilities.supports_rename);
        assert!(!capabilities.supports_delete);
    }

    #[test]
    fn plan_uses_stable_root_order_and_native_tail() {
        let plan = build_plan(&target(false), windows_policy());
        let groups = plan
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(groups.first().copied(), Some("G10"));
        assert_eq!(groups.last().copied(), Some("G99"));
        let last = plan.groups.last().and_then(|group| group.entries.last());
        assert!(matches!(
            last,
            Some(MenuEntryPlan::Action(action)) if action.action_id == "system.native-menu"
        ));
    }

    #[test]
    fn every_windows_visible_action_has_a_stable_command_mapping() {
        let mut target = target(true);
        target.git_available = true;
        target.svn_available = true;
        let plan = build_plan(&target, windows_policy());
        for action_id in plan.visible_action_ids() {
            assert!(
                ContextMenuCommand::from_action_id(action_id, None).is_some(),
                "missing Windows dispatch mapping for {action_id}"
            );
        }
    }

    #[test]
    fn git_and_agent_are_declared_once_in_central_submenus() {
        let plan = build_plan(&target(false), windows_policy());
        let ids = plan.visible_action_ids();
        assert_eq!(ids.iter().filter(|id| **id == "open.in").count(), 1);
        assert_eq!(ids.iter().filter(|id| **id == "agent.ask").count(), 1);
        assert!(plan.find_submenu("git").is_some());
        assert!(plan.find_submenu("agent").is_some());
    }

    #[test]
    fn multi_select_promotes_delete_to_destructive_group() {
        let mut target = target(false);
        target.paths = vec![
            PathBuf::from(r"C:\repo\src\main.rs"),
            PathBuf::from(r"C:\repo\src\lib.rs"),
        ];
        let plan = build_plan(&target, windows_policy());
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
        assert_eq!(
            trash.root_group,
            kite_core::context_menu_model::RootGroup::Destructive
        );
        assert!(!plan.visible_action_ids().contains(&"file.rename"));
    }
}
