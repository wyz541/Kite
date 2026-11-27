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
use kite_core::vcs_menu::load_vcs_menu_snapshot;

#[cfg(target_os = "windows")]
pub(crate) mod open_with_windows;
#[cfg(target_os = "windows")]
pub(crate) mod tool_registry_windows;
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
    pub supports_create: bool,
    pub supports_transfer_ops: bool,
    pub supports_cut: bool,
    pub supports_rename: bool,
    pub supports_delete: bool,
    pub supports_favorite: bool,
    pub supports_properties: bool,
    pub supports_reveal: bool,
    pub supports_share: bool,
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
        let shell = is_shell_namespace.then(|| crate::shell_fs::capability_snapshot(path));
        let mut can_paste = can_paste;
        let (
            supports_create,
            supports_transfer_ops,
            supports_cut,
            supports_rename,
            supports_delete,
            supports_properties,
            supports_reveal,
            supports_share,
        ) = if let Some(shell) = shell {
            can_paste = shell.can_paste;
            (
                shell.can_create,
                shell.can_copy || shell.can_paste,
                shell.can_move,
                shell.can_rename,
                shell.can_delete,
                shell.can_properties,
                false,
                false,
            )
        } else {
            let can_change = path.parent().is_some();
            (
                true, true, can_change, can_change, can_change, true, true, false,
            )
        };
        Self {
            loading,
            can_paste,
            git_available,
            svn_available,
            supports_create,
            supports_transfer_ops,
            supports_cut,
            supports_rename,
            supports_delete,
            supports_favorite: !is_shell_namespace,
            supports_properties,
            supports_reveal,
            supports_share,
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
    pub supports_create: bool,
    pub supports_transfer_ops: bool,
    pub supports_cut: bool,
    pub supports_rename: bool,
    pub supports_delete: bool,
    pub supports_favorite: bool,
    pub supports_properties: bool,
    pub supports_reveal: bool,
    pub supports_share: bool,
    pub is_shell_namespace: bool,
    pub agent_available: bool,
    pub container_is_primary_target: bool,
    pub surface: ContextSurface,
    pub is_favorite: bool,
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
            paths: Vec::new(),
            title,
            is_dir: true,
            loading: capabilities.loading,
            can_paste: capabilities.can_paste,
            git_available: capabilities.git_available,
            svn_available: capabilities.svn_available,
            supports_create: capabilities.supports_create,
            supports_transfer_ops: capabilities.supports_transfer_ops,
            supports_cut: capabilities.supports_cut,
            supports_rename: capabilities.supports_rename,
            supports_delete: capabilities.supports_delete,
            supports_favorite: capabilities.supports_favorite,
            supports_properties: capabilities.supports_properties,
            supports_reveal: capabilities.supports_reveal,
            supports_share: capabilities.supports_share,
            is_shell_namespace: capabilities.is_shell_namespace,
            agent_available: false,
            container_is_primary_target: true,
            surface: ContextSurface::Background,
            is_favorite: false,
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
            supports_create: capabilities.supports_create,
            supports_transfer_ops: capabilities.supports_transfer_ops,
            supports_cut: capabilities.supports_cut,
            supports_rename: capabilities.supports_rename,
            supports_delete: capabilities.supports_delete,
            supports_favorite: capabilities.supports_favorite,
            supports_properties: capabilities.supports_properties,
            supports_reveal: capabilities.supports_reveal,
            supports_share: capabilities.supports_share,
            is_shell_namespace: capabilities.is_shell_namespace,
            agent_available: false,
            container_is_primary_target: false,
            surface: ContextSurface::BrowserList,
            is_favorite: false,
        }
    }

    pub(crate) fn with_surface(mut self, surface: ContextSurface) -> Self {
        self.surface = surface;
        self
    }

    pub(crate) fn with_favorite(mut self, is_favorite: bool) -> Self {
        self.is_favorite = is_favorite;
        self
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

    fn action(
        &self,
        plan_action: &ResolvedAction,
        target_kinds: &[String],
    ) -> Option<ContextMenuAction> {
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
            kite_core::context_menu_model::ActionPayload::Vcs {
                repository_root,
                tool,
                state_revision,
            } => ContextMenuActionPayload::Vcs {
                repository_root: repository_root.clone(),
                tool: tool.clone(),
                state_revision: state_revision.clone(),
            },
            _ => ContextMenuActionPayload::None,
        };
        Some(ContextMenuAction {
            target: paths.first().cloned().unwrap_or_else(|| self.path.clone()),
            paths,
            working_directory: self.working_directory.clone(),
            command,
            action_id: plan_action.action_id.clone(),
            variant_key: plan_action.variant_key.clone(),
            target_kinds: target_kinds.to_vec(),
            payload,
        })
    }

    fn context_snapshot(&self, policy: ContextMenuPolicy) -> ContextSnapshot {
        let paths = self.paths.clone();
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
        )
        .with_kind(if self.is_shell_namespace {
            TargetKind::DeviceContainer
        } else {
            TargetKind::Directory
        });
        container.writable = self.supports_create;
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
        let vcs_menu = if self.is_shell_namespace {
            kite_core::vcs_menu::VcsMenuSnapshot::default()
        } else {
            let vcs_paths = if paths.is_empty() {
                vec![self.working_directory.clone()]
            } else {
                paths.clone()
            };
            load_vcs_menu_snapshot(&vcs_paths, &self.working_directory)
        };
        let single_file = paths.len() == 1 && !self.is_shell_namespace && !self.is_dir;
        let mindmap_available = single_file && is_xmind_path(&paths[0]);
        let mindmap_import_available = single_file && is_supported_mindmap_import_path(&paths[0]);
        ContextSnapshot {
            schema_version: kite_core::context_menu_model::CONTEXT_MENU_SCHEMA_VERSION,
            snapshot_id: format!("windows-context-menu:{}", self.path.display()),
            platform: ContextPlatform::Windows,
            surface: self.surface,
            target_set,
            container,
            capabilities: ContextCapabilities {
                loading: self.loading,
                can_paste: self.can_paste,
                can_create: self.supports_create && self.container_is_primary_target,
                supports_transfer_ops: self.supports_transfer_ops,
                supports_cut: self.supports_cut,
                supports_rename: self.supports_rename,
                supports_delete: self.supports_delete,
                supports_favorite: self.supports_favorite,
                is_favorite: self.is_favorite,
                git_available: self.git_available || vcs_menu.git_available,
                svn_available: self.svn_available || vcs_menu.svn_available,
                agent_available: self.agent_available,
                quick_look_available: false,
                reveal_available: self.supports_reveal,
                share_available: self.supports_share,
                properties_available: self.supports_properties,
                text_editor_available: false,
                mindmap_available,
                mindmap_import_available,
                is_shell_namespace: self.is_shell_namespace,
                native_plane_available: true,
            },
            open_with,
            tool_catalog,
            tool_policy: tool_registry_windows::policy(),
            vcs_menu,
            policy: policy.core_policy(),
        }
    }

    fn context_target(&self, path: &Path) -> ContextTarget {
        let kind = if self.is_shell_namespace {
            if self.path == path && self.is_dir || crate::shell_fs::is_directory(path) {
                TargetKind::DeviceContainer
            } else {
                TargetKind::DeviceItem
            }
        } else if archive::is_supported_archive(path) {
            TargetKind::Archive
        } else if path.is_dir() || (path == self.path && self.is_dir) {
            TargetKind::Directory
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
        target.exists = if self.is_shell_namespace {
            crate::shell_fs::exists(path)
        } else {
            path.exists()
        };
        target.readable = if self.is_shell_namespace {
            crate::shell_fs::is_readable(path)
        } else {
            path.is_dir() || std::fs::File::open(path).is_ok()
        };
        target.writable = if self.is_shell_namespace {
            kind == TargetKind::DeviceContainer
        } else {
            self.supports_create || self.supports_rename || self.supports_delete
        };
        target
    }
}

fn is_xmind_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xmind"))
}

fn is_supported_mindmap_import_path(path: &Path) -> bool {
    kite_core::mindmap_import::detect_source(path).is_some_and(|source| source.format.supported)
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
    target_kinds: &[String],
    target: &ContextMenuTarget,
    action_context: &FocusHandle,
    _window: &mut Window,
    _cx: &mut App,
) -> PopupMenu {
    let Some(dispatched) = target.action(action, target_kinds) else {
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
    target_kinds: &[String],
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
            menu = append_action(
                menu,
                action,
                target_kinds,
                target,
                action_context,
                window,
                cx,
            );
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
    let target_kinds = plan.target_kinds.clone();
    for (group_index, group) in plan.groups.iter().enumerate() {
        if group_index > 0 {
            menu = menu.separator();
        }
        for entry in &group.entries {
            match entry {
                MenuEntryPlan::Action(action) => {
                    menu = append_action(
                        menu,
                        action,
                        &target_kinds,
                        target,
                        action_context,
                        window,
                        cx,
                    );
                }
                MenuEntryPlan::Submenu(entry_submenu) => {
                    let Some(submenu) = plan.find_submenu(&entry_submenu.id) else {
                        continue;
                    };
                    let submenu_clone = submenu.clone();
                    let target = target.clone();
                    let target_kinds = target_kinds.clone();
                    let action_context = action_context.clone();
                    let popup = PopupMenu::build(window, cx, move |menu, window, cx| {
                        append_sections(
                            menu,
                            &submenu_clone.sections,
                            &target_kinds,
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
