use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::actions::*;
use crate::archive::{self, ArchiveFormat};
use crate::archive_progress_window;
use crate::context_menu::{
    ContextMenuCapabilities, ContextMenuTarget, render_context_menu, request_system_menu,
    show_properties,
};
use crate::devices::{DeviceSnapshot, PortableDevice, StorageVolume, VolumeKind};
use crate::diff_viewer_window;
use crate::favorites::{self as favorites_store, FavoriteEntryDto};
use crate::file_clipboard::{self, ClipboardMode, FileClipboard};
use crate::fs::{
    DirectorySnapshot, FileEntry, SortKey, SortSpec, VcsStatus, create_file, create_folder,
    format_size, load_directory, open_file, recycle_entry, rename_entry, search_directory,
    sort_entries, transfer_entry, validate_name, worse_status,
};
use crate::git_branches::GitHeadSummary;
use crate::git_branches_window;
use crate::git_client_window;
use crate::git_log_window;
use crate::global_search_window;
use crate::platform::{default_root_path, root_directory};
use crate::preview_window::{PreviewWindow, preview_render_image};
use crate::settings_window;
use crate::shell_fs;
use crate::table::{
    FileIconKind, FileTableDelegate, column_icon_for, folder_icon_color, icon_color, icon_for,
    vcs_icon_badge, vcs_status_color,
};
use crate::text_editor;
use crate::ui_preferences::{
    self, AppearancePreference, ColumnGroupPreference as ColumnGroup, ColumnViewRule, UiPreferences,
};
use crate::vcs::{
    VcsOperation, VcsQuery, VcsTool, apply_statuses, load_vcs_snapshot, open_vcs_log_window,
    run_vcs_operation, run_vcs_query,
};
use crate::workspace_window::WorkspaceWindow;
use gpui_kit::assets::IconName;
use gpui_kit::base::{Selectable as _, SelectableText, TextSelectionHandle};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::scroll::{ScrollableMask, Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, Size, StyledExt as _, Theme, ThemeMode,
    h_flex, v_flex,
};
use gpui_kit::prelude::{FluentBuilder as _, StatefulInteractiveElement as _};
use gpui_kit::*;
use kite_core::diff::DiffEndpoint;
use kite_core::file_watcher::{DirectoryWatcher, trace_directory_refresh};
use kite_core::git::{
    GitChangeCounts, GitPathSummary, GitService, git_path_summary, git_repository_root,
};
pub(crate) use kite_core::preview::read_log_preview;
use kite_core::preview::{PreviewImageData, load_image_preview};

const SIDEBAR_WIDTH: f32 = 208.;
const TOOLBAR_CONTROL_HEIGHT: f32 = 36.;
const TOOLBAR_VERTICAL_PADDING: f32 = 4.;
const STORAGE_SIDEBAR_ROW_HEIGHT: f32 = 48.;
const COLUMN_WIDTH: f32 = 252.;
const COLUMN_STRIP_GAP: f32 = 4.;
const COLUMN_STRIP_PADDING: f32 = 4.;
const COLUMN_HEADER_HEIGHT: f32 = 32.;
const COLUMN_SEARCH_HEIGHT: f32 = 40.;
const PREVIEW_WIDTH: f32 = 300.;
const PREVIEW_LOAD_DEBOUNCE: Duration = Duration::from_millis(45);
const PREVIEW_IMAGE_CACHE_CAPACITY: usize = 12;
const COLUMN_PREVIEW_IMAGE_MAX_WIDTH: u32 = 640;
const COLUMN_PREVIEW_IMAGE_MAX_HEIGHT: u32 = 960;
const ISLAND_GAP: f32 = 4.;
const COMPACT_ISLAND_PADDING: f32 = 6.;
const STATUS_BAR_HEIGHT: f32 = 28.;
const DETAILED_COLUMN_MIN_WIDTH: f32 = 250.;
const DETAILED_COLUMN_MAX_WIDTH: f32 = 640.;
const DETAILED_COLUMN_NAME_OVERHEAD: f32 = 150.;
const DETAILED_DATE_WIDTH: f32 = 84.;
const DETAILED_KIND_WIDTH: f32 = 70.;
const DETAILED_SIZE_WIDTH: f32 = 64.;
const COLUMN_SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
const COLUMN_AUTO_REFRESH_DEBOUNCE: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, Debug)]
enum NavigationIntent {
    Initial,
    Push,
    ReplaceIndex(usize),
    Refresh,
}

struct PathSegment {
    label: SharedString,
    path: PathBuf,
    clickable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StorageSummary {
    drive_letter: Option<SharedString>,
    free_bytes: Option<u64>,
    total_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoragePressure {
    Normal,
    Low,
    Critical,
}

impl StorageSummary {
    fn free_percent(&self) -> Option<f32> {
        let free_bytes = self.free_bytes?;
        let total_bytes = self.total_bytes?;
        if total_bytes == 0 {
            return None;
        }
        Some((free_bytes as f64 / total_bytes as f64 * 100.0).clamp(0.0, 100.0) as f32)
    }

    fn pressure(&self) -> StoragePressure {
        match self.free_percent() {
            Some(percent) if percent < 5.0 => StoragePressure::Critical,
            Some(percent) if percent < 10.0 => StoragePressure::Low,
            _ => StoragePressure::Normal,
        }
    }

    fn capacity_label(&self) -> Option<SharedString> {
        storage_capacity_label(self.free_bytes, self.total_bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QuickLink {
    label: SharedString,
    path: PathBuf,
    icon: IconName,
    detail: Option<SharedString>,
    storage: Option<StorageSummary>,
}

#[derive(Clone)]
struct SidebarItem {
    id: SharedString,
    label: SharedString,
    path: PathBuf,
    icon: IconName,
    detail: Option<SharedString>,
    storage: Option<StorageSummary>,
    active: bool,
    is_directory: bool,
    favorite: bool,
}

#[derive(Clone)]
struct SearchState {
    query: String,
    root: PathBuf,
}

#[derive(Clone)]
enum ModalKind {
    NewFolder(PathBuf),
    NewFile(PathBuf),
    Rename(PathBuf),

    GitCommit(PathBuf),
    SvnCommit(PathBuf),
    GitUsername(GitNetworkRequest),
    GitPassword {
        request: GitNetworkRequest,
        username: String,
    },
}

struct ModalState {
    kind: ModalKind,
    input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Clone)]
enum GitOperationCommand {
    Fetch {
        path: PathBuf,
        all_remotes: bool,
    },
    Pull {
        path: PathBuf,
        options: kite_core::git::remote::GitPullOptions,
    },
    Push {
        path: PathBuf,
        options: kite_core::git::remote::GitPushOptions,
    },
    Clone {
        url: String,
        target: PathBuf,
        shallow_depth: Option<u32>,
    },
    PullThenPush {
        path: PathBuf,
        pull: kite_core::git::remote::GitPullOptions,
        push: kite_core::git::remote::GitPushOptions,
    },
}

impl GitOperationCommand {
    fn label(&self) -> &'static str {
        match self {
            Self::Fetch { .. } => "获取远程更新",
            Self::Pull { .. } | Self::PullThenPush { .. } => "拉取远程更新",
            Self::Push { .. } => "推送提交",
            Self::Clone { .. } => "克隆仓库",
        }
    }
}

#[derive(Clone)]
struct GitNetworkRequest {
    command: GitOperationCommand,
    credentials: Option<kite_core::git::remote::GitCredentials>,
}

struct GitOperationOutcome {
    message: String,
    open_path: Option<PathBuf>,
}

impl GitNetworkRequest {
    fn new(command: GitOperationCommand) -> Self {
        Self {
            command,
            credentials: None,
        }
    }

    fn with_credentials(mut self, username: String, password: String) -> Self {
        self.credentials = Some(kite_core::git::remote::GitCredentials { username, password });
        self
    }

    fn execute(
        &self,
        monitor: &kite_core::git::remote::GitOperationMonitor,
    ) -> Result<GitOperationOutcome, kite_core::git::remote::GitFailure> {
        use kite_core::git::remote::{self as git_remote, GitFetchOptions};
        let credentials = self.credentials.as_ref();
        match &self.command {
            GitOperationCommand::Fetch { path, all_remotes } => {
                let message = git_remote::fetch(
                    path,
                    &GitFetchOptions {
                        remote: None,
                        all_remotes: *all_remotes,
                        prune: true,
                    },
                    credentials,
                    monitor,
                )?;
                Ok(GitOperationOutcome {
                    message,
                    open_path: None,
                })
            }
            GitOperationCommand::Pull { path, options } => {
                let message = git_remote::pull(path, options, credentials, monitor)?;
                Ok(GitOperationOutcome {
                    message,
                    open_path: None,
                })
            }
            GitOperationCommand::Push { path, options } => {
                let message = git_remote::push(path, options, credentials, monitor)?;
                Ok(GitOperationOutcome {
                    message,
                    open_path: None,
                })
            }
            GitOperationCommand::Clone {
                url,
                target,
                shallow_depth,
            } => {
                let result = git_remote::clone_repository(
                    url,
                    target,
                    &git_remote::GitCloneOptions {
                        shallow_depth: *shallow_depth,
                    },
                    credentials,
                    monitor,
                )?;
                Ok(GitOperationOutcome {
                    message: format!("已克隆到 {}", result.path.display()),
                    open_path: Some(result.path),
                })
            }
            GitOperationCommand::PullThenPush { path, pull, push } => {
                git_remote::pull(path, pull, credentials, monitor)?;
                let message = git_remote::push(path, push, credentials, monitor)?;
                Ok(GitOperationOutcome {
                    message,
                    open_path: None,
                })
            }
        }
    }
}

struct ActiveGitOperation {
    label: SharedString,
    monitor: Arc<kite_core::git::remote::GitOperationMonitor>,
    progress: kite_core::git::remote::GitOperationProgress,
    _task: Task<()>,
}

struct CloneCleanupPrompt {
    partial_path: PathBuf,
    target: PathBuf,
}

struct GitRecoveryPrompt {
    path: PathBuf,
    push: kite_core::git::remote::GitPushOptions,
    credentials: Option<kite_core::git::remote::GitCredentials>,
}

struct GitPushDialogState {
    path: PathBuf,
    remote_input: Entity<InputState>,
    branch_input: Entity<InputState>,
    preview: Option<kite_core::git::remote::GitPushPreview>,
    selected_commit: Option<usize>,
    files: Vec<kite_core::git::remote::GitFileChange>,
    include_tags: bool,
    force_with_lease: bool,
    loading: bool,
    error: Option<SharedString>,
    _preview_task: Task<()>,
    _files_task: Task<()>,
}

struct GitCloneDialogState {
    directory: PathBuf,
    url_input: Entity<InputState>,
    target_input: Entity<InputState>,
    depth_input: Entity<InputState>,
    shallow: bool,
    error: Option<SharedString>,
}

struct GitRemotesDialogState {
    path: PathBuf,
    remotes: Vec<kite_core::git::remote::GitRemote>,
    name_input: Entity<InputState>,
    fetch_url_input: Entity<InputState>,
    push_url_input: Entity<InputState>,
    selected: Option<String>,
    loading: bool,
    error: Option<SharedString>,
    _task: Task<()>,
}

enum GitDialogState {
    Push(GitPushDialogState),
    Clone(GitCloneDialogState),
    Remotes(GitRemotesDialogState),
}

#[derive(Clone)]
struct AvailableUpdate {
    info: crate::updater::UpdateInfo,
    notes: crate::update_notes::UpdateNotesContent,
}

#[derive(Clone)]
pub(crate) struct SettingsUpdateState {
    pub in_progress: bool,
    pub status: Option<SharedString>,
    pub status_is_error: bool,
    pub available_version: Option<SharedString>,
    pub current_version: Option<SharedString>,
    pub notes: Option<SharedString>,
    pub download_progress: Option<crate::updater::UpdateDownloadProgress>,
}

pub(crate) type SharedClipboard = Rc<RefCell<Option<FileClipboard>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Columns,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabRequest {
    New,
    Close,
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ColumnFilter {
    #[default]
    All,
    Folders,
    Files,
    Media,
    Documents,
}

#[derive(Clone, Copy)]
struct ColumnViewRules {
    sort: SortSpec,
    filter: ColumnFilter,
    group: ColumnGroup,
}

#[derive(Clone, Copy)]
struct ColumnLayoutState {
    show_date_column: bool,
    show_kind_column: bool,
    show_size_column: bool,
    preferred_width: u16,
}

#[derive(Clone)]
enum ColumnDisplayRow {
    Group {
        label: SharedString,
        count: usize,
        collapsed: bool,
    },
    Entry(FileEntry),
}

#[derive(Clone)]
struct KiteFileDrag {
    paths: Vec<(PathBuf, bool)>,
    label: SharedString,
}

struct KiteFileDragPreview {
    label: SharedString,
    count: usize,
    position: Point<Pixels>,
}

impl Render for KiteFileDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = if self.count > 1 {
            format!("{} · {} 项", self.label, self.count)
        } else {
            self.label.to_string()
        };
        div()
            .pl(self.position.x - px(72.))
            .pt(self.position.y - px(18.))
            .child(
                h_flex()
                    .h(px(36.))
                    .max_w(px(180.))
                    .gap_2()
                    .px_3()
                    .items_center()
                    .rounded_md()
                    .bg(cx.theme().accent)
                    .text_sm()
                    .text_color(cx.theme().accent_foreground)
                    .shadow_md()
                    .child(Icon::new(IconName::Folder).size_4())
                    .child(div().min_w_0().truncate().child(label)),
            )
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ColumnSelection {
    selected_path: Option<PathBuf>,
    selected_paths: Vec<PathBuf>,
    anchor_path: Option<PathBuf>,
}

#[derive(Clone)]
struct ColumnPane {
    path: PathBuf,
    directory_entries: Vec<FileEntry>,
    search_entries: Option<Vec<FileEntry>>,
    entries: Vec<FileEntry>,
    selected_path: Option<PathBuf>,
    selected_paths: Vec<PathBuf>,
    selection_anchor_path: Option<PathBuf>,
    sort: SortSpec,
    filter: ColumnFilter,
    group: ColumnGroup,
    collapsed_groups: BTreeSet<String>,
    search_query: Option<String>,
    search_truncated: bool,
    /// Results came from the local listing, not the recursive walk.
    search_preview: bool,
    /// Selection from before the search started; restored when it ends so a
    /// query that hides the selected file does not lose it.
    pre_search_selection: Option<ColumnSelection>,
    loading: bool,
    skipped: usize,
    git_available: bool,
    svn_available: bool,
}

impl ColumnPane {
    fn new(
        path: PathBuf,
        directory_entries: Vec<FileEntry>,
        skipped: usize,
        loading: bool,
        git_available: bool,
        svn_available: bool,
        rule: ColumnViewRule,
    ) -> Self {
        let mut pane = Self {
            path,
            directory_entries,
            search_entries: None,
            entries: Vec::new(),
            selected_path: None,
            selected_paths: Vec::new(),
            selection_anchor_path: None,
            sort: rule.sort,
            filter: ColumnFilter::All,
            group: rule.group,
            collapsed_groups: rule.collapsed_groups,
            search_query: None,
            search_truncated: false,
            search_preview: false,
            pre_search_selection: None,
            loading,
            skipped,
            git_available,
            svn_available,
        };
        pane.refresh_view();
        pane
    }

    fn source_entries(&self) -> &[FileEntry] {
        self.search_entries
            .as_deref()
            .unwrap_or(self.directory_entries.as_slice())
    }

    fn refresh_view(&mut self) {
        let mut entries = self
            .source_entries()
            .iter()
            .filter(|entry| self.filter.matches(entry))
            .cloned()
            .collect::<Vec<_>>();
        // Search results arrive ranked by relevance; keep that order.
        if self.search_entries.is_none() {
            sort_entries(&mut entries, self.sort);
        }
        self.entries = entries;
        self.selected_paths.retain(|selected| {
            self.entries
                .iter()
                .any(|entry| same_path(&entry.path, selected))
        });
        if self.selection_anchor_path.as_ref().is_some_and(|selected| {
            !self
                .entries
                .iter()
                .any(|entry| same_path(&entry.path, selected))
        }) {
            self.selection_anchor_path = None;
        }
        if self.selected_path.as_ref().is_some_and(|selected| {
            !self
                .selected_paths
                .iter()
                .any(|path| same_path(path, selected))
        }) {
            self.selected_path = None;
        }
        if self.selected_path.is_none() {
            self.selected_path = self.selected_paths.first().cloned();
        }
    }

    fn is_selected(&self, path: &Path) -> bool {
        self.selected_paths
            .iter()
            .any(|selected| same_path(selected, path))
    }

    fn set_single_selection(&mut self, path: PathBuf) {
        self.selected_path = Some(path.clone());
        self.selected_paths = vec![path.clone()];
        self.selection_anchor_path = Some(path);
    }

    fn toggle_selection(&mut self, path: PathBuf) {
        if let Some(index) = self
            .selected_paths
            .iter()
            .position(|selected| same_path(selected, &path))
        {
            self.selected_paths.remove(index);
        } else {
            self.selected_paths.push(path.clone());
        }
        self.selected_path = self.selected_paths.last().cloned();
        self.selection_anchor_path = Some(path);
    }

    fn select_range_to(&mut self, path: PathBuf) {
        let visible_entries = self
            .display_rows()
            .into_iter()
            .filter_map(|row| match row {
                ColumnDisplayRow::Entry(entry) => Some(entry.path),
                ColumnDisplayRow::Group { .. } => None,
            })
            .collect::<Vec<_>>();
        let anchor = self
            .selection_anchor_path
            .as_ref()
            .and_then(|anchor| {
                visible_entries
                    .iter()
                    .position(|path| same_path(path, anchor))
            })
            .or_else(|| {
                self.selected_path.as_ref().and_then(|selected| {
                    visible_entries
                        .iter()
                        .position(|path| same_path(path, selected))
                })
            });
        let target = visible_entries
            .iter()
            .position(|candidate| same_path(candidate, &path));
        let (Some(anchor), Some(target)) = (anchor, target) else {
            self.set_single_selection(path);
            return;
        };
        let range = if anchor <= target {
            anchor..=target
        } else {
            target..=anchor
        };
        self.selected_paths = visible_entries[range].to_vec();
        self.selected_path = Some(path.clone());
        if self.selection_anchor_path.is_none() {
            self.selection_anchor_path = Some(path);
        }
    }

    fn clear_selection(&mut self) {
        self.selected_path = None;
        self.selected_paths.clear();
        self.selection_anchor_path = None;
    }

    fn select_all(&mut self) {
        self.selected_paths = self
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        self.selected_path = self.selected_paths.first().cloned();
        self.selection_anchor_path = self.selected_paths.first().cloned();
    }

    fn toggle_sort_order(&mut self) {
        self.sort.ascending = !self.sort.ascending;
        self.refresh_view();
    }

    fn toggle_sort_for_header(&mut self, key: SortKey) {
        let ascending = if self.sort.key == key {
            !self.sort.ascending
        } else {
            default_column_header_sort_ascending(key)
        };
        self.sort = SortSpec { key, ascending };
        self.refresh_view();
    }

    fn set_directory_entries(&mut self, entries: Vec<FileEntry>, skipped: usize) {
        self.directory_entries = entries;
        self.search_entries = None;
        self.search_query = None;
        self.search_truncated = false;
        self.search_preview = false;
        self.pre_search_selection = None;
        self.skipped = skipped;
        self.loading = false;
        self.refresh_view();
    }

    fn apply_directory_refresh(&mut self, refreshed: ColumnPane) -> bool {
        let changed = self.directory_entries != refreshed.directory_entries
            || self.skipped != refreshed.skipped
            || self.git_available != refreshed.git_available
            || self.svn_available != refreshed.svn_available;
        if !changed {
            return false;
        }

        self.directory_entries = refreshed.directory_entries;
        self.skipped = refreshed.skipped;
        self.git_available = refreshed.git_available;
        self.svn_available = refreshed.svn_available;
        self.loading = false;
        self.refresh_view();
        true
    }

    fn set_search_results(&mut self, query: String, entries: Vec<FileEntry>, truncated: bool) {
        self.remember_pre_search_selection();
        self.search_entries = Some(entries);
        self.search_query = Some(query);
        self.search_truncated = truncated;
        self.search_preview = false;
        self.refresh_view();
    }

    #[cfg(test)]
    fn set_search_preview(&mut self, query: String, mut entries: Vec<FileEntry>) {
        self.remember_pre_search_selection();
        sort_entries(&mut entries, self.sort);
        self.search_entries = Some(entries);
        self.search_query = Some(query);
        self.search_truncated = false;
        self.search_preview = true;
        self.refresh_view();
    }

    fn clear_search_results(&mut self) {
        let was_searching = self.search_entries.is_some();
        self.search_entries = None;
        self.search_query = None;
        self.search_truncated = false;
        self.search_preview = false;
        if let Some(saved) = self.pre_search_selection.take()
            && was_searching
        {
            self.selected_path = saved.selected_path;
            self.selected_paths = saved.selected_paths;
            self.selection_anchor_path = saved.anchor_path;
        }
        self.refresh_view();
    }

    /// Captures the selection the first time results replace the listing.
    fn remember_pre_search_selection(&mut self) {
        if self.search_entries.is_none() && self.pre_search_selection.is_none() {
            self.pre_search_selection = Some(ColumnSelection {
                selected_path: self.selected_path.clone(),
                selected_paths: self.selected_paths.clone(),
                anchor_path: self.selection_anchor_path.clone(),
            });
        }
    }

    #[cfg(test)]
    fn first_search_row_index(&self) -> Option<usize> {
        let first_match = self.entries.first()?.path.as_path();
        self.display_rows().iter().position(|row| match row {
            ColumnDisplayRow::Entry(entry) => same_path(&entry.path, first_match),
            ColumnDisplayRow::Group { .. } => false,
        })
    }

    fn display_rows(&self) -> Vec<ColumnDisplayRow> {
        if self.group == ColumnGroup::None {
            return self
                .entries
                .iter()
                .cloned()
                .map(ColumnDisplayRow::Entry)
                .collect();
        }

        let now = SystemTime::now();
        let mut groups = std::collections::BTreeMap::<(u8, String), Vec<FileEntry>>::new();
        for entry in &self.entries {
            let (rank, label) = self.group_key(entry, now);
            groups.entry((rank, label)).or_default().push(entry.clone());
        }

        let mut rows = Vec::new();
        for ((_, label), mut entries) in groups {
            // Search results arrive ranked; keep that order inside each group.
            if self.search_entries.is_none() {
                sort_entries(&mut entries, self.sort);
            }
            let collapsed = self.collapsed_groups.contains(&label);
            rows.push(ColumnDisplayRow::Group {
                label: SharedString::from(label),
                count: entries.len(),
                collapsed,
            });
            if !collapsed {
                rows.extend(entries.into_iter().map(ColumnDisplayRow::Entry));
            }
        }
        rows
    }

    fn visible_row_count(&self) -> usize {
        self.display_rows().len()
    }

    fn entry_at_visible_row(&self, row: usize) -> Option<FileEntry> {
        self.display_rows().get(row).and_then(|row| match row {
            ColumnDisplayRow::Entry(entry) => Some(entry.clone()),
            ColumnDisplayRow::Group { .. } => None,
        })
    }

    fn view_rule(&self) -> ColumnViewRule {
        ColumnViewRule {
            sort: self.sort,
            group: self.group,
            collapsed_groups: self.collapsed_groups.clone(),
        }
    }

    fn toggle_group_collapse(&mut self, label: &str) {
        if !self.collapsed_groups.remove(label) {
            self.collapsed_groups.insert(label.to_string());
            self.prune_selection_to_visible_rows();
        }
    }

    fn prune_selection_to_visible_rows(&mut self) {
        let visible_paths = self
            .display_rows()
            .into_iter()
            .filter_map(|row| match row {
                ColumnDisplayRow::Entry(entry) => Some(entry.path),
                ColumnDisplayRow::Group { .. } => None,
            })
            .collect::<Vec<_>>();
        self.selected_paths.retain(|selected| {
            visible_paths
                .iter()
                .any(|visible| same_path(visible, selected))
        });
        if self.selected_path.as_ref().is_some_and(|selected| {
            !visible_paths
                .iter()
                .any(|visible| same_path(visible, selected))
        }) {
            self.selected_path = None;
        }
        if self.selection_anchor_path.as_ref().is_some_and(|selected| {
            !visible_paths
                .iter()
                .any(|visible| same_path(visible, selected))
        }) {
            self.selection_anchor_path = None;
        }
        if self.selected_path.is_none() {
            self.selected_path = self.selected_paths.first().cloned();
        }
    }

    fn group_key(&self, entry: &FileEntry, now: SystemTime) -> (u8, String) {
        match self.group {
            ColumnGroup::None => (0, String::new()),
            ColumnGroup::Kind => {
                if entry.is_dir() {
                    (0, "文件夹".to_string())
                } else {
                    (1, "文件".to_string())
                }
            }
            ColumnGroup::Extension => {
                if entry.is_dir() {
                    (0, "文件夹".to_string())
                } else if entry.extension.is_empty() {
                    (2, "无扩展名".to_string())
                } else {
                    (1, entry.extension.to_uppercase())
                }
            }
            ColumnGroup::Modified => {
                let Some(modified) = entry.modified else {
                    return (4, "未知日期".to_string());
                };
                let age = now.duration_since(modified).unwrap_or_default();
                if age < Duration::from_secs(24 * 60 * 60) {
                    (0, "今天".to_string())
                } else if age < Duration::from_secs(7 * 24 * 60 * 60) {
                    (1, "最近 7 天".to_string())
                } else if age < Duration::from_secs(30 * 24 * 60 * 60) {
                    (2, "最近 30 天".to_string())
                } else {
                    (3, "更早".to_string())
                }
            }
        }
    }
}

impl ColumnFilter {
    fn matches(self, entry: &FileEntry) -> bool {
        match self {
            Self::All => true,
            Self::Folders => entry.is_dir(),
            Self::Files => !entry.is_dir(),
            Self::Media => {
                !entry.is_dir()
                    && matches!(
                        entry.extension.as_str(),
                        "png"
                            | "jpg"
                            | "jpeg"
                            | "gif"
                            | "webp"
                            | "svg"
                            | "bmp"
                            | "ico"
                            | "mp3"
                            | "wav"
                            | "flac"
                            | "aac"
                            | "ogg"
                            | "mp4"
                            | "mkv"
                            | "mov"
                            | "avi"
                            | "webm"
                    )
            }
            Self::Documents => {
                !entry.is_dir()
                    && matches!(
                        entry.extension.as_str(),
                        "txt"
                            | "md"
                            | "log"
                            | "ini"
                            | "toml"
                            | "yaml"
                            | "yml"
                            | "json"
                            | "pdf"
                            | "doc"
                            | "docx"
                            | "xls"
                            | "xlsx"
                            | "ppt"
                            | "pptx"
                    )
            }
        }
    }
}

#[derive(Clone)]
struct ColumnListScroll {
    path: PathBuf,
    handle: UniformListScrollHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreviewImageCacheKey {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

impl PreviewImageCacheKey {
    fn for_entry(entry: &FileEntry) -> Self {
        Self {
            path: entry.path.clone(),
            size: entry.size,
            modified: entry.modified,
        }
    }
}

struct PreviewImageCache {
    entries: VecDeque<(PreviewImageCacheKey, Arc<RenderImage>)>,
}

impl PreviewImageCache {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &PreviewImageCacheKey) -> Option<Arc<RenderImage>> {
        let index = self.entries.iter().position(|(cached, _)| cached == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_front(entry);
        Some(image)
    }

    fn insert(&mut self, key: PreviewImageCacheKey, image: Arc<RenderImage>) {
        if let Some(index) = self.entries.iter().position(|(cached, _)| cached == &key) {
            self.entries.remove(index);
        }
        self.entries.push_front((key, image));
        self.entries.truncate(PREVIEW_IMAGE_CACHE_CAPACITY);
    }
}

enum PreviewLoadResult {
    Text(Result<String, String>),
    Image(Result<PreviewImageData, String>),
}

#[derive(Clone)]
struct ColumnPreviewState {
    path: PathBuf,
    // Fixed-height rows make the scroll extent deterministic for large files.
    lines: Arc<Vec<SharedString>>,
    selection: Option<TextSelectionHandle>,
    markdown_view: Option<Entity<TextViewState>>,
    image: Option<Arc<RenderImage>>,
    error: Option<SharedString>,
    loading: bool,
    scroll: UniformListScrollHandle,
    markdown_scroll: ScrollHandle,
}

/// Git section of the column preview. Loaded after the content preview so
/// the git CLI never delays the file itself.
#[derive(Clone)]
struct ColumnGitPreview {
    path: PathBuf,
    summary: Option<GitPathSummary>,
    loading: bool,
}

enum PreviewScrollbar {
    Lines(UniformListScrollHandle),
    Markdown(ScrollHandle),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppLaunchTarget {
    pub directory: PathBuf,
    pub file: Option<PathBuf>,
}

impl AppLaunchTarget {
    fn from_existing_path(path: PathBuf) -> Option<Self> {
        let path = absolute_path(path);
        let metadata = std::fs::metadata(&path).ok()?;
        if metadata.is_dir() {
            return Some(Self {
                directory: path,
                file: None,
            });
        }
        if metadata.is_file() {
            return Some(Self {
                directory: path.parent()?.to_path_buf(),
                file: Some(path),
            });
        }
        None
    }
}

pub struct KiteApp {
    current_dir: PathBuf,
    history: Vec<PathBuf>,
    history_index: usize,
    quick_links: Vec<QuickLink>,
    favorites: Vec<FavoriteEntryDto>,
    connected_devices: Vec<QuickLink>,
    drives: Vec<QuickLink>,
    portable_devices: Vec<PortableDevice>,
    address_input: Entity<InputState>,
    search_input: Entity<InputState>,
    column_search_input: Entity<InputState>,
    table: Entity<TableState<FileTableDelegate>>,
    focus_handle: FocusHandle,
    active_search: Option<SearchState>,
    search_truncated: bool,
    loading: bool,
    skipped: usize,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    update_in_progress: bool,
    update_download_progress: Option<crate::updater::UpdateDownloadProgress>,
    available_update: Option<AvailableUpdate>,
    modal: Option<ModalState>,
    clipboard: SharedClipboard,
    load_generation: u64,
    search_generation: u64,
    vcs_generation: u64,
    search_cancel: Option<Arc<AtomicBool>>,
    column_search_path: Option<PathBuf>,
    last_operated_column: Option<PathBuf>,
    column_search_loading: bool,
    column_search_generation: u64,
    column_search_cancel: Option<Arc<AtomicBool>>,
    _load_task: Task<()>,
    _search_task: Task<()>,
    _column_search_task: Task<()>,
    _vcs_task: Task<()>,
    git_head: Option<GitHeadSummary>,
    pending_git_refresh: bool,
    pending_git_open: Option<PathBuf>,
    view_mode: ViewMode,
    show_path_bar: bool,
    show_sidebar: bool,
    address_editing: bool,
    search_visible: bool,
    show_preview: bool,
    show_column_details: bool,
    show_date_column: bool,
    show_kind_column: bool,
    show_size_column: bool,
    preferred_column_width: u16,
    column_panes: Vec<ColumnPane>,
    column_selected_path: Option<PathBuf>,
    column_scroll: ScrollHandle,
    column_vertical_scrolls: Vec<ColumnListScroll>,
    column_scroll_pending: bool,
    column_preview: Option<ColumnPreviewState>,
    pending_file_selection: Option<PathBuf>,
    preview_generation: u64,
    preview_image_cache: PreviewImageCache,
    _preview_task: Task<()>,
    column_git_preview: Option<ColumnGitPreview>,
    _git_preview_task: Task<()>,
    path_bar_scroll: ScrollHandle,
    path_bar_scroll_pending: bool,
    column_generation: u64,
    _column_task: Task<()>,
    directory_watcher: Option<DirectoryWatcher>,
    _directory_watch_task: Task<()>,
    _column_refresh_task: Task<()>,
    column_refresh_generation: u64,
    _device_task: Task<()>,
    _git_auto_fetch_task: Task<()>,
    git_dialog: Option<GitDialogState>,
    active_git_operation: Option<ActiveGitOperation>,
    clone_cleanup_prompt: Option<CloneCleanupPrompt>,
    git_recovery_prompt: Option<GitRecoveryPrompt>,
    _subscriptions: Vec<Subscription>,
}

impl KiteApp {
    pub(crate) fn launch_target_override() -> Option<AppLaunchTarget> {
        std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .and_then(AppLaunchTarget::from_existing_path)
    }

    pub(crate) fn launch_directory_override() -> Option<PathBuf> {
        Self::launch_target_override().map(|target| target.directory)
    }

    pub(crate) fn default_directory() -> PathBuf {
        Self::launch_directory_override()
            .or_else(dirs::home_dir)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(default_root_path)
    }

    pub(crate) fn new_at(
        home: PathBuf,
        clipboard: SharedClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_at_with_state(home, None, ViewMode::Columns, true, clipboard, window, cx)
    }

    pub(crate) fn new_launch_target(
        target: AppLaunchTarget,
        clipboard: SharedClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_at_with_state(
            target.directory,
            target.file,
            ViewMode::Columns,
            true,
            clipboard,
            window,
            cx,
        )
    }

    pub(crate) fn new_restored(
        state: &crate::session::SessionTabDto,
        clipboard: SharedClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let restored_path = PathBuf::from(&state.current_directory);
        let path = if restored_path.is_dir() {
            restored_path
        } else {
            Self::default_directory()
        };
        let view_mode = if state.view_mode == "list" {
            ViewMode::List
        } else {
            ViewMode::Columns
        };
        Self::new_at_with_state(
            path,
            None,
            view_mode,
            state.show_path_bar,
            clipboard,
            window,
            cx,
        )
    }

    pub(crate) fn session_tab(&self) -> crate::session::SessionTabDto {
        crate::session::SessionTabDto {
            current_directory: self.current_dir.to_string_lossy().into_owned(),
            view_mode: match self.view_mode {
                ViewMode::Columns => "columns".to_string(),
                ViewMode::List => "list".to_string(),
            },
            show_path_bar: self.show_path_bar,
        }
    }

    fn new_at_with_state(
        home: PathBuf,
        initial_file: Option<PathBuf>,
        view_mode: ViewMode,
        show_path_bar: bool,
        clipboard: SharedClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let quick_links = build_quick_links();
        let favorites = favorites_store::load_favorites();
        let snapshot = crate::devices::snapshot();
        let (connected_devices, drives) = volume_quick_links(&snapshot.volumes);
        let portable_devices = snapshot.portable_devices;
        let address_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入文件夹或文件路径，按 Enter 打开")
                .clean_on_escape()
        });
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("全局搜索当前文件夹")
                .clean_on_escape()
        });
        let column_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索此分栏")
                .clean_on_escape()
        });
        let ui_preferences = ui_preferences::load_ui_preferences();
        let table = cx.new(|cx| {
            TableState::new(
                FileTableDelegate::new(
                    Vec::new(),
                    ui_preferences.default_column_rule.sort,
                    home.clone(),
                ),
                window,
                cx,
            )
            .col_movable(false)
            .col_resizable(true)
            .row_selectable(true)
            .sortable(true)
        });

        let address_subscription = cx.subscribe_in(
            &address_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let value = this.address_input.read(cx).value().to_string();
                    this.navigate_from_text(value, window, cx);
                }
                InputEvent::Focus => {
                    this.address_editing = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.address_editing = false;
                    cx.notify();
                }
                InputEvent::Change => {}
            },
        );
        let search_subscription = cx.subscribe_in(
            &search_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let value = this.search_input.read(cx).value().to_string();
                    this.search_visible = true;
                    this.submit_search(value, window, cx);
                }
                InputEvent::Focus => {
                    this.search_visible = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    if this.active_search.is_none() && this.search_input.read(cx).value().is_empty()
                    {
                        this.search_visible = false;
                        cx.notify();
                    }
                }
                InputEvent::Change => {}
            },
        );
        let column_search_subscription = cx.subscribe_in(
            &column_search_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let value = this.column_search_input.read(cx).value().to_string();
                    this.submit_column_search(value, window, cx);
                }
                InputEvent::Change => {
                    let value = this.column_search_input.read(cx).value().to_string();
                    if value.trim().is_empty() {
                        this.clear_column_search_results(window, cx);
                    } else {
                        this.schedule_column_search(value, window, cx);
                    }
                }
                _ => {}
            },
        );
        let table_subscription = cx.subscribe_in(&table, window, Self::on_table_event);

        let mut app = Self {
            current_dir: home.clone(),
            history: Vec::new(),
            history_index: 0,
            quick_links,
            favorites,
            connected_devices,
            drives,
            portable_devices,
            address_input,
            search_input,
            column_search_input,
            table,
            focus_handle: cx.focus_handle(),
            active_search: None,
            search_truncated: false,
            loading: false,
            skipped: 0,
            error: None,
            notice: None,
            update_in_progress: false,
            update_download_progress: None,
            available_update: None,
            modal: None,
            clipboard,
            load_generation: 0,
            search_generation: 0,
            vcs_generation: 0,
            search_cancel: None,
            column_search_path: None,
            last_operated_column: None,
            column_search_loading: false,
            column_search_generation: 0,
            column_search_cancel: None,
            _load_task: Task::ready(()),
            _search_task: Task::ready(()),
            _column_search_task: Task::ready(()),
            _vcs_task: Task::ready(()),
            git_head: None,
            pending_git_refresh: false,
            pending_git_open: None,
            view_mode,
            show_path_bar,
            show_sidebar: ui_preferences.sidebar_visible,
            address_editing: false,
            search_visible: false,
            show_preview: ui_preferences.preview_visible,
            show_column_details: ui_preferences.column_details_visible,
            show_date_column: ui_preferences.date_column_visible,
            show_kind_column: ui_preferences.kind_column_visible,
            show_size_column: ui_preferences.size_column_visible,
            preferred_column_width: ui_preferences.column_width,
            column_panes: Vec::new(),
            column_selected_path: None,
            column_scroll: ScrollHandle::new(),
            column_vertical_scrolls: Vec::new(),
            column_scroll_pending: false,
            column_preview: None,
            pending_file_selection: initial_file.clone(),
            preview_generation: 0,
            preview_image_cache: PreviewImageCache::new(),
            _preview_task: Task::ready(()),
            column_git_preview: None,
            _git_preview_task: Task::ready(()),
            path_bar_scroll: ScrollHandle::new(),
            path_bar_scroll_pending: false,
            column_generation: 0,
            _column_task: Task::ready(()),
            directory_watcher: None,
            _directory_watch_task: Task::ready(()),
            _column_refresh_task: Task::ready(()),
            column_refresh_generation: 0,
            _device_task: Task::ready(()),
            _git_auto_fetch_task: Task::ready(()),
            git_dialog: None,
            active_git_operation: None,
            clone_cleanup_prompt: None,
            git_recovery_prompt: None,
            _subscriptions: vec![
                address_subscription,
                search_subscription,
                column_search_subscription,
                table_subscription,
            ],
        };
        app.start_device_watcher(cx);
        app.start_directory_watcher(cx);
        app.start_git_auto_fetch(
            ui_preferences.git_auto_fetch,
            ui_preferences.git_auto_fetch_minutes,
            cx,
        );
        app.request_load_with_file_selection(
            home,
            NavigationIntent::Initial,
            initial_file,
            window,
            cx,
        );
        kite_core::perf_trace::mark("windows.app_model_ready");
        app
    }

    pub(crate) fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    pub(crate) fn preview_visible(&self) -> bool {
        self.show_preview
    }

    pub(crate) fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.show_preview = !self.show_preview;
        if self.show_preview {
            if let Some(entry) = self.selected_last_column_file() {
                self.start_column_preview(entry, cx);
            }
        } else {
            self.clear_column_preview();
        }
        cx.notify();
    }

    fn toggle_column_sort_order(&mut self, index: usize, cx: &mut Context<Self>) {
        let (path, rule) = {
            let Some(pane) = self.column_panes.get_mut(index) else {
                return;
            };
            pane.toggle_sort_order();
            (pane.path.clone(), pane.view_rule())
        };
        if let Err(error) = ui_preferences::save_column_view_rule(&path, rule) {
            self.notice = Some(error.into());
        }
        self.clear_hidden_column_selection();
        self.column_scroll_pending = true;
        cx.notify();
    }

    fn sort_column_from_header(&mut self, index: usize, key: SortKey, cx: &mut Context<Self>) {
        let (path, rule) = {
            let Some(pane) = self.column_panes.get_mut(index) else {
                return;
            };
            pane.toggle_sort_for_header(key);
            (pane.path.clone(), pane.view_rule())
        };
        if let Err(error) = ui_preferences::save_column_view_rule(&path, rule) {
            self.notice = Some(error.into());
        }
        self.clear_hidden_column_selection();
        self.column_scroll_pending = true;
        cx.notify();
    }

    fn toggle_column_details(&mut self, cx: &mut Context<Self>) {
        self.show_column_details = !self.show_column_details;
        self.column_scroll_pending = true;
        if let Err(error) = self.save_layout_preferences() {
            self.notice = Some(error.into());
        }
        cx.notify();
    }

    fn save_layout_preferences(&self) -> Result<(), String> {
        let mut preferences = ui_preferences::load_ui_preferences();
        preferences.column_details_visible = self.show_column_details;
        preferences.date_column_visible = self.show_date_column;
        preferences.kind_column_visible = self.show_kind_column;
        preferences.size_column_visible = self.show_size_column;
        preferences.column_width = self.preferred_column_width;
        ui_preferences::save_ui_preferences(preferences)
    }

    fn start_device_watcher(&mut self, cx: &mut Context<Self>) {
        self._device_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let snapshot = cx
                    .background_spawn(async { crate::devices::snapshot() })
                    .await;
                if this
                    .update(cx, |this, cx| this.apply_device_snapshot(snapshot, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    fn start_directory_watcher(&mut self, cx: &mut Context<Self>) {
        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        let watcher = match DirectoryWatcher::new(move |_| {
            let _ = sender.unbounded_send(());
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("failed to start directory watcher: {error}");
                return;
            }
        };
        self.directory_watcher = Some(watcher);
        self._directory_watch_task = cx.spawn(async move |this, cx| {
            while receiver.recv().await.is_ok() {
                cx.background_executor()
                    .timer(COLUMN_AUTO_REFRESH_DEBOUNCE)
                    .await;
                while receiver.try_recv().is_ok() {}
                if this
                    .update(cx, |this, cx| {
                        this.refresh_columns_after_directory_change(cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.sync_directory_watchers();
    }

    fn sync_directory_watchers(&mut self) {
        let paths = if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            self.column_panes
                .iter()
                .map(|pane| pane.path.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if let Some(watcher) = self.directory_watcher.as_mut() {
            watcher.set_paths(paths);
        }
    }

    fn refresh_columns_after_directory_change(&mut self, cx: &mut Context<Self>) {
        if self.view_mode != ViewMode::Columns
            || self.active_search.is_some()
            || self.column_panes.is_empty()
        {
            return;
        }

        if let Some(watcher) = self.directory_watcher.as_mut() {
            watcher.rearm_paths(self.column_panes.iter().map(|pane| pane.path.clone()));
        }

        self.column_refresh_generation = self.column_refresh_generation.wrapping_add(1);
        let generation = self.column_refresh_generation;
        let load_paths = self
            .column_panes
            .iter()
            .enumerate()
            .map(|(index, pane)| (index, pane.path.clone(), pane.view_rule()))
            .collect::<Vec<_>>();
        self._column_refresh_task = cx.spawn(async move |this, cx| {
            let refreshed = cx
                .background_spawn(async move {
                    load_paths
                        .into_iter()
                        .map(|(index, path, rule)| {
                            (index, path.clone(), try_load_column_pane(path, rule))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.column_refresh_generation != generation {
                    return;
                }
                this.apply_column_refresh(refreshed, cx);
            });
        });
    }

    fn apply_column_refresh(
        &mut self,
        refreshed: Vec<(usize, PathBuf, Result<ColumnPane, String>)>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        let mut refreshed_paths = Vec::new();
        for (index, path, result) in refreshed {
            let Ok(refreshed_pane) = result else {
                continue;
            };
            let Some(pane) = self.column_panes.get_mut(index) else {
                continue;
            };
            if !same_path(&pane.path, &path) || !same_path(&pane.path, &refreshed_pane.path) {
                continue;
            }
            if pane.apply_directory_refresh(refreshed_pane) {
                changed = true;
                refreshed_paths.push(path);
            }
        }
        if !changed {
            return;
        }
        trace_directory_refresh(&format!("visible columns refreshed: {refreshed_paths:?}"));

        if self.truncate_columns_after_missing_selection() {
            self.column_scroll_pending = true;
        }
        self.clear_hidden_column_selection();
        if self.show_preview {
            self.clear_column_preview();
            if let Some(entry) = self.selected_last_column_file() {
                self.start_column_preview(entry, cx);
            }
        }
        self.sync_directory_watchers();
        cx.notify();
    }

    fn truncate_columns_after_missing_selection(&mut self) -> bool {
        let mut keep = 1;
        for index in 1..self.column_panes.len() {
            let child_path = self.column_panes[index].path.clone();
            let parent_keeps_child = self.column_panes[index - 1]
                .selected_path
                .as_ref()
                .is_some_and(|selected| same_path(selected, &child_path))
                && self.column_panes[index - 1]
                    .entries
                    .iter()
                    .any(|entry| entry.is_dir() && same_path(&entry.path, &child_path));
            if !parent_keeps_child {
                break;
            }
            keep = index + 1;
        }
        if keep == self.column_panes.len() {
            return false;
        }
        self.column_panes.truncate(keep);
        true
    }

    fn apply_device_snapshot(&mut self, snapshot: DeviceSnapshot, cx: &mut Context<Self>) {
        let (connected_devices, drives) = volume_quick_links(&snapshot.volumes);
        let volumes_changed = !same_quick_links(&self.connected_devices, &connected_devices)
            || !same_quick_links(&self.drives, &drives);
        let portable_changed = self.portable_devices != snapshot.portable_devices;

        if volumes_changed {
            self.connected_devices = connected_devices;
            self.drives = drives;
        }
        if portable_changed {
            shell_fs::clear_shell_caches();
            self.portable_devices = snapshot.portable_devices;
        }
        if volumes_changed || portable_changed {
            cx.notify();
        }
    }

    fn request_tab(&mut self, request: TabRequest, cx: &mut Context<Self>) {
        cx.emit(request);
    }

    pub fn bind_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("alt-left", NavigateBack, Some("Kite")),
            KeyBinding::new("alt-right", NavigateForward, Some("Kite")),
            KeyBinding::new("alt-up", NavigateUp, Some("Kite")),
            KeyBinding::new("secondary-r", RefreshFolder, Some("Kite")),
            KeyBinding::new("secondary-l", FocusAddress, Some("Kite")),
            KeyBinding::new("secondary-f", FocusSearch, Some("Kite")),
            KeyBinding::new("secondary-shift-f", OpenGlobalSearch, Some("Kite")),
            KeyBinding::new("secondary-shift-g", OpenGitClient, Some("Kite")),
            KeyBinding::new("secondary-1", ShowListView, Some("Kite")),
            KeyBinding::new("secondary-2", ShowColumnsView, Some("Kite")),
            KeyBinding::new("secondary-t", NewTab, Some("Kite")),
            KeyBinding::new("secondary-w", CloseTab, Some("Kite")),
            KeyBinding::new("secondary-tab", NextTab, Some("Kite")),
            KeyBinding::new("secondary-shift-tab", PreviousTab, Some("Kite")),
            KeyBinding::new("alt-p", TogglePathBar, Some("Kite")),
            KeyBinding::new("secondary-shift-n", CreateFolder, Some("Kite")),
            KeyBinding::new("secondary-c", CopySelection, Some("Kite")),
            KeyBinding::new("secondary-x", CutSelection, Some("Kite")),
            KeyBinding::new("secondary-v", PasteSelection, Some("Kite")),
            KeyBinding::new("secondary-a", SelectAll, Some("Kite")),
            KeyBinding::new("f2", RenameSelection, Some("Kite")),
            KeyBinding::new("delete", RecycleSelection, Some("Kite")),
            KeyBinding::new("enter", OpenSelection, Some("Kite")),
            KeyBinding::new("escape", ClearSearchOverlay, Some("Kite")),
            KeyBinding::new("secondary-shift-r", GitClientRefresh, Some("GitClient")),
            KeyBinding::new("secondary-shift-f", GitClientFetch, Some("GitClient")),
            KeyBinding::new("secondary-shift-p", GitClientPull, Some("GitClient")),
            KeyBinding::new("secondary-shift-k", GitClientPush, Some("GitClient")),
            KeyBinding::new("secondary-k", GitClientCommit, Some("GitClient")),
            KeyBinding::new("secondary-alt-s", GitClientStash, Some("GitClient")),
            KeyBinding::new("secondary-j", GitClientToggleConsole, Some("GitClient")),
            KeyBinding::new("f1", GitClientShowShortcuts, Some("GitClient")),
            KeyBinding::new("escape", GitClientDismiss, Some("GitClient")),
            KeyBinding::new("secondary-s", SaveEditor, Some("KiteEditor")),
            KeyBinding::new("f7", DiffNextChange, Some("KiteDiff")),
            KeyBinding::new("shift-f7", DiffPreviousChange, Some("KiteDiff")),
        ]);
    }
    pub fn install_menus(cx: &mut App) {
        cx.set_menus([
            Menu::new("文件").items([
                MenuItem::action("新建页签", NewTab),
                MenuItem::action("关闭页签", CloseTab),
                MenuItem::separator(),
                MenuItem::action("下一个页签", NextTab),
                MenuItem::action("上一个页签", PreviousTab),
            ]),
            Menu::new("导航").items([
                MenuItem::action("聚焦路径栏", FocusAddress),
                MenuItem::separator(),
                MenuItem::action("全局搜索…", OpenGlobalSearch),
            ]),
            Menu::new("Git").items([MenuItem::action("打开 Git 客户端…", OpenGitClient)]),
            Menu::new("显示").items([
                MenuItem::action("列表视图", ShowListView),
                MenuItem::action("分栏视图", ShowColumnsView),
                MenuItem::separator(),
                MenuItem::action("显示或隐藏路径栏", TogglePathBar),
            ]),
            Menu::new("Kite").items([
                MenuItem::action("设置…", OpenSettings),
                MenuItem::action("工作区编辑器…", OpenWorkspaceEditor),
                MenuItem::separator(),
                MenuItem::action("检查更新…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("设为默认资源管理器…", SetDefaultExplorer),
                MenuItem::action("恢复之前的文件夹打开方式…", RestoreDefaultExplorer),
                MenuItem::separator(),
                MenuItem::action("创建开始菜单快捷方式…", CreateStartMenuShortcut),
            ]),
        ]);
    }

    fn request_load(
        &mut self,
        path: PathBuf,
        intent: NavigationIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_load_with_file_selection(path, intent, None, window, cx);
    }

    fn request_load_with_file_selection(
        &mut self,
        path: PathBuf,
        intent: NavigationIntent,
        pending_file_selection: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_column_search(window, cx);
        self.pending_file_selection = pending_file_selection;
        self.clear_column_preview();
        self.cancel_search();
        self.invalidate_vcs();
        self.invalidate_columns();
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let initial_load = matches!(intent, NavigationIntent::Initial);
        let sort = self.table.read(cx).delegate().sort_spec();
        let load_path = path.clone();

        self.loading = true;
        self.error = None;
        self.active_search = None;
        self.search_truncated = false;
        self.address_editing = false;
        self.search_visible = false;
        self.table.update(cx, |state, cx| {
            state.delegate_mut().set_loading(true);
            state.refresh(cx);
            cx.notify();
        });
        cx.notify();

        self._load_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { load_directory_snapshot(&load_path, sort) })
                .await;

            let _ = this.update_in(cx, |this, window, cx| {
                if this.load_generation != generation {
                    return;
                }
                this.loading = false;
                let pending_file_selection = this.pending_file_selection.take();
                this.table.update(cx, |state, cx| {
                    state.delegate_mut().set_loading(false);
                    cx.notify();
                });

                match result {
                    Ok(snapshot) => {
                        let column_entries =
                            (this.view_mode == ViewMode::Columns).then(|| snapshot.entries.clone());
                        let column_skipped = snapshot.skipped;
                        this.apply_history(snapshot.path.clone(), intent);
                        this.current_dir = snapshot.path.clone();
                        this.skipped = snapshot.skipped;
                        this.error = None;
                        this.address_editing = false;
                        this.search_visible = false;
                        this.address_input.update(cx, |input, cx| {
                            input.set_value(path_text(&snapshot.path), window, cx);
                        });
                        this.search_input.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                        });
                        let table_directory = snapshot.path.clone();
                        this.table.update(cx, |state, cx| {
                            state.delegate_mut().set_working_directory(table_directory);
                            state.delegate_mut().replace_entries(snapshot.entries, None);
                            state.clear_selection(cx);
                            state.refresh(cx);
                            cx.notify();
                        });
                        if this.view_mode == ViewMode::Columns
                            && let Some(entries) = column_entries
                        {
                            this.update_columns_for_loaded_path(
                                this.current_dir.clone(),
                                entries,
                                column_skipped,
                                cx,
                            );
                        }
                        if let Some(path) = pending_file_selection.as_deref()
                            && this.select_loaded_file(path, cx)
                        {
                            this.open_path(path.to_path_buf(), window, cx);
                        }
                        this.refresh_vcs_statuses(this.current_dir.clone(), cx);
                        this.path_bar_scroll_pending = true;
                        if initial_load {
                            kite_core::perf_trace::mark_once("windows.first_directory_loaded");
                        }
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                        this.table.update(cx, |state, cx| {
                            state.refresh(cx);
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            });
        });
    }

    fn apply_history(&mut self, path: PathBuf, intent: NavigationIntent) {
        match intent {
            NavigationIntent::Initial => {
                self.history.clear();
                self.history.push(path);
                self.history_index = 0;
            }
            NavigationIntent::Push => {
                if self.history.is_empty() {
                    self.history.push(path);
                    self.history_index = 0;
                    return;
                }
                if same_path(&self.history[self.history_index], &path) {
                    return;
                }
                self.history.truncate(self.history_index + 1);
                self.history.push(path);
                self.history_index = self.history.len().saturating_sub(1);
            }
            NavigationIntent::ReplaceIndex(index) => {
                if index < self.history.len() {
                    self.history_index = index;
                }
            }
            NavigationIntent::Refresh => {}
        }
    }

    fn navigate_to(
        &mut self,
        path: PathBuf,
        intent: NavigationIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notice = None;
        self.request_load(path, intent, window, cx);
    }

    fn navigate_to_location(
        &mut self,
        path: PathBuf,
        intent: NavigationIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = AppLaunchTarget::from_existing_path(path.clone()) else {
            self.navigate_to(path, intent, window, cx);
            return;
        };
        self.notice = None;
        self.request_load_with_file_selection(target.directory, intent, target.file, window, cx);
    }

    fn navigate_from_path_bar(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let loaded_column = if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            loaded_column_index(&self.column_panes, &path)
        } else {
            None
        };

        if let Some(column) = loaded_column {
            self.navigate_to_loaded_column(column, window, cx);
        } else {
            self.navigate_to(path, NavigationIntent::Push, window, cx);
        }
    }

    fn navigate_to_loaded_column(
        &mut self,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_column_search(window, cx);
        self.clear_column_preview();
        self.pending_file_selection = None;
        let Some((entries, skipped)) = self
            .column_panes
            .get(column)
            .map(|pane| (pane.directory_entries.clone(), pane.skipped))
        else {
            return;
        };
        let path = self.column_panes[column].path.clone();

        self.cancel_search();
        self.invalidate_vcs();
        self._load_task = Task::ready(());
        self.load_generation = self.load_generation.wrapping_add(1);
        self.loading = false;
        self.error = None;
        self.notice = None;
        self.active_search = None;
        self.search_truncated = false;
        self.address_editing = false;
        self.search_visible = false;
        self.skipped = skipped;
        self.apply_history(path.clone(), NavigationIntent::Push);
        let table_directory = path.clone();
        self.current_dir = path;

        self.address_input.update(cx, |input, cx| {
            input.set_value(path_text(&self.current_dir), window, cx);
        });
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.table.update(cx, |state, cx| {
            state.delegate_mut().set_loading(false);
            state.delegate_mut().set_working_directory(table_directory);
            state.delegate_mut().replace_entries(entries, None);
            state.clear_selection(cx);
            state.refresh(cx);
            cx.notify();
        });

        self.column_panes.truncate(column + 1);
        for index in 0..self.column_panes.len().saturating_sub(1) {
            let child_path = self.column_panes[index + 1].path.clone();
            self.column_panes[index].set_single_selection(child_path);
        }
        if let Some(pane) = self.column_panes.last_mut() {
            pane.clear_selection();
        }
        self.column_selected_path = None;
        self.column_scroll_pending = true;
        self.path_bar_scroll_pending = true;
        cx.notify();
    }

    fn navigate_from_text(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        match resolve_location_input(&value, &self.current_dir) {
            Ok(path) => self.navigate_to_location(path, NavigationIntent::Push, window, cx),
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
            }
        }
    }

    fn go_back(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_back(window, cx);
    }

    fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_index == 0 {
            return;
        }
        let index = self.history_index - 1;
        self.notice = None;
        self.request_load(
            self.history[index].clone(),
            NavigationIntent::ReplaceIndex(index),
            window,
            cx,
        );
    }

    fn go_forward(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_forward(window, cx);
    }

    fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_index + 1 >= self.history.len() {
            return;
        }
        let index = self.history_index + 1;
        self.notice = None;
        self.request_load(
            self.history[index].clone(),
            NavigationIntent::ReplaceIndex(index),
            window,
            cx,
        );
    }

    fn go_up(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_up(window, cx);
    }

    fn navigate_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = navigation_parent(&self.current_dir) else {
            self.error = Some("已经在驱动器根目录".into());
            cx.notify();
            return;
        };
        self.navigate_to(parent.to_path_buf(), NavigationIntent::Push, window, cx);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        if let Some(search) = self.active_search.clone() {
            self.start_search(search.query, window, cx);
        } else {
            self.request_load(
                self.current_dir.clone(),
                NavigationIntent::Refresh,
                window,
                cx,
            );
        }
    }

    fn refresh_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(window, cx);
    }
    fn submit_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.clear_search(window, cx);
        } else {
            self.start_search(query, window, cx);
        }
    }

    fn remember_column(&mut self, path: &Path) {
        self.last_operated_column = Some(path.to_path_buf());
    }

    fn start_last_column_search(
        &mut self,
        initial_query: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .last_operated_column
            .clone()
            .or_else(|| self.column_panes.last().map(|pane| pane.path.clone()));
        let Some(path) = path else { return };
        if !self
            .column_panes
            .iter()
            .any(|pane| same_path(&pane.path, &path))
        {
            return;
        }
        if !self
            .column_search_path
            .as_ref()
            .is_some_and(|active| same_path(active, &path))
        {
            self.toggle_column_search(path, window, cx);
        }
        if let Some(query) = initial_query {
            self.column_search_input.update(cx, |input, cx| {
                input.set_value(query.clone(), window, cx);
            });
            self.schedule_column_search(query, window, cx);
        }
        let search_input = self.column_search_input.clone();
        window.defer(cx, move |window, cx| {
            search_input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn handle_global_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view_mode != ViewMode::Columns
            || self.active_search.is_some()
            || self.column_search_path.is_some()
            || self.address_editing
            || self.modal.is_some()
            || self.available_update.is_some()
            || event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
            || event.keystroke.modifiers.function
        {
            return;
        }
        let character = event.keystroke.key_char.as_deref().or_else(|| {
            let key = event.keystroke.key.as_str();
            (key.chars().count() == 1).then_some(key)
        });
        let Some(character) = character else { return };
        if character.is_empty() || character.chars().any(char::is_control) {
            return;
        }
        window.prevent_default();
        cx.stop_propagation();
        self.start_last_column_search(Some(character.to_string()), window, cx);
    }

    fn toggle_column_search(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .column_search_path
            .as_ref()
            .is_some_and(|active| same_path(active, &path))
        {
            self.close_column_search(window, cx);
            return;
        }

        self.cancel_column_search_task();
        self.column_search_path = Some(path);
        for pane in &mut self.column_panes {
            pane.clear_search_results();
        }
        self.column_search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn submit_column_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.start_column_search(query, Duration::ZERO, window, cx);
    }

    fn schedule_column_search(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_column_search(query, COLUMN_SEARCH_DEBOUNCE, window, cx);
    }

    fn start_column_search(
        &mut self,
        query: String,
        delay: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.clear_column_search_results(window, cx);
            return;
        }
        let Some(root) = self.column_search_path.clone() else {
            return;
        };
        let sort = self
            .column_panes
            .iter()
            .find(|pane| same_path(&pane.path, &root))
            .map(|pane| pane.sort)
            .unwrap_or_default();

        self.cancel_column_search_task();
        let generation = self.column_search_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.column_search_cancel = Some(cancel.clone());
        self.column_search_loading = true;
        self.error = None;
        cx.notify();

        self._column_search_task = cx.spawn_in(window, async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let result = cx
                .background_spawn(
                    async move { search_directory(&root, &query, sort, cancel.as_ref()) },
                )
                .await;

            let _ = this.update_in(cx, |this, window, cx| {
                if this.column_search_generation != generation {
                    return;
                }
                this.column_search_cancel = None;
                this.column_search_loading = false;
                match result {
                    Ok(Some(snapshot)) => {
                        if let Some(pane) = this
                            .column_panes
                            .iter_mut()
                            .find(|pane| same_path(&pane.path, &snapshot.root))
                        {
                            pane.set_search_results(
                                snapshot.query,
                                snapshot.entries,
                                snapshot.truncated,
                            );
                        }
                        this.clear_hidden_column_selection();
                        this.column_scroll_pending = true;
                        this.error = None;
                        this.refresh_vcs_statuses(snapshot.root, cx);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.error = Some(error.into());
                    }
                }
                this.column_search_input.update(cx, |input, cx| {
                    input.focus(window, cx);
                });
                cx.notify();
            });
        });
    }

    fn cancel_column_search_task(&mut self) {
        if let Some(cancel) = self.column_search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.column_search_loading = false;
        self.column_search_generation = self.column_search_generation.wrapping_add(1);
        self._column_search_task = Task::ready(());
    }

    fn clear_column_search_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_column_search_task();
        for pane in &mut self.column_panes {
            pane.clear_search_results();
        }
        self.column_search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.clear_hidden_column_selection();
        cx.notify();
    }

    fn close_column_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_column_search_results(window, cx);
        self.column_search_path = None;
        cx.notify();
    }

    fn clear_hidden_column_selection(&mut self) {
        let selection_is_visible = self.column_selected_path.as_ref().is_some_and(|selected| {
            self.column_panes
                .iter()
                .any(|pane| pane.is_selected(selected))
        });
        if !selection_is_visible {
            self.column_selected_path = None;
            self.clear_column_preview();
        }
    }

    fn on_column_pane_action(
        &mut self,
        action: &ColumnPaneAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout_changed = match action.command {
            ColumnPaneCommand::ToggleDateColumn => {
                self.show_date_column = !self.show_date_column;
                self.show_column_details = true;
                true
            }
            ColumnPaneCommand::ToggleKindColumn => {
                self.show_kind_column = !self.show_kind_column;
                self.show_column_details = true;
                true
            }
            ColumnPaneCommand::ToggleSizeColumn => {
                self.show_size_column = !self.show_size_column;
                self.show_column_details = true;
                true
            }
            ColumnPaneCommand::SetCompactColumnWidth => {
                self.preferred_column_width = 250;
                true
            }
            ColumnPaneCommand::SetStandardColumnWidth => {
                self.preferred_column_width = 320;
                true
            }
            ColumnPaneCommand::SetWideColumnWidth => {
                self.preferred_column_width = 420;
                true
            }
            ColumnPaneCommand::ResetLayout => {
                self.show_column_details = true;
                self.show_date_column = true;
                self.show_kind_column = true;
                self.show_size_column = true;
                self.preferred_column_width = 320;
                true
            }
            _ => false,
        };
        if layout_changed {
            if let Err(error) = self.save_layout_preferences() {
                self.notice = Some(error.into());
            }
            self.column_scroll_pending = true;
            cx.notify();
            return;
        }

        let Some(index) = self
            .column_panes
            .iter()
            .position(|pane| same_path(&pane.path, &action.path))
        else {
            return;
        };
        let (path, rule) = {
            let Some(pane) = self.column_panes.get_mut(index) else {
                return;
            };

            match action.command {
                ColumnPaneCommand::SortNameAscending => {
                    pane.sort = SortSpec {
                        key: crate::fs::SortKey::Name,
                        ascending: true,
                    }
                }
                ColumnPaneCommand::SortNameDescending => {
                    pane.sort = SortSpec {
                        key: crate::fs::SortKey::Name,
                        ascending: false,
                    }
                }
                ColumnPaneCommand::SortModifiedNewest => {
                    pane.sort = SortSpec {
                        key: crate::fs::SortKey::Modified,
                        ascending: false,
                    }
                }
                ColumnPaneCommand::SortModifiedOldest => {
                    pane.sort = SortSpec {
                        key: crate::fs::SortKey::Modified,
                        ascending: true,
                    }
                }
                ColumnPaneCommand::SortKind => {
                    pane.sort = SortSpec {
                        key: crate::fs::SortKey::Kind,
                        ascending: true,
                    }
                }
                ColumnPaneCommand::SortSizeDescending => {
                    pane.sort = SortSpec {
                        key: crate::fs::SortKey::Size,
                        ascending: false,
                    }
                }
                ColumnPaneCommand::FilterAll => pane.filter = ColumnFilter::All,
                ColumnPaneCommand::FilterFolders => pane.filter = ColumnFilter::Folders,
                ColumnPaneCommand::FilterFiles => pane.filter = ColumnFilter::Files,
                ColumnPaneCommand::FilterMedia => pane.filter = ColumnFilter::Media,
                ColumnPaneCommand::FilterDocuments => pane.filter = ColumnFilter::Documents,
                ColumnPaneCommand::GroupNone => {
                    if pane.group != ColumnGroup::None {
                        pane.group = ColumnGroup::None;
                        pane.collapsed_groups.clear();
                    }
                }
                ColumnPaneCommand::GroupKind => {
                    if pane.group != ColumnGroup::Kind {
                        pane.group = ColumnGroup::Kind;
                        pane.collapsed_groups.clear();
                    }
                }
                ColumnPaneCommand::GroupExtension => {
                    if pane.group != ColumnGroup::Extension {
                        pane.group = ColumnGroup::Extension;
                        pane.collapsed_groups.clear();
                    }
                }
                ColumnPaneCommand::GroupModified => {
                    if pane.group != ColumnGroup::Modified {
                        pane.group = ColumnGroup::Modified;
                        pane.collapsed_groups.clear();
                    }
                }
                ColumnPaneCommand::ResetView => {
                    pane.sort = SortSpec::default();
                    pane.filter = ColumnFilter::All;
                    pane.group = ColumnGroup::None;
                    pane.collapsed_groups.clear();
                }
                ColumnPaneCommand::ToggleDateColumn
                | ColumnPaneCommand::ToggleKindColumn
                | ColumnPaneCommand::ToggleSizeColumn
                | ColumnPaneCommand::SetCompactColumnWidth
                | ColumnPaneCommand::SetStandardColumnWidth
                | ColumnPaneCommand::SetWideColumnWidth
                | ColumnPaneCommand::ResetLayout => return,
            }
            pane.refresh_view();
            (pane.path.clone(), pane.view_rule())
        };
        if let Err(error) = ui_preferences::save_column_view_rule(&path, rule) {
            self.notice = Some(error.into());
        }
        self.clear_hidden_column_selection();
        self.column_scroll_pending = true;
        cx.notify();
    }

    fn toggle_column_group(&mut self, column: usize, label: SharedString, cx: &mut Context<Self>) {
        let (path, rule) = {
            let Some(pane) = self.column_panes.get_mut(column) else {
                return;
            };
            pane.toggle_group_collapse(label.as_ref());
            (pane.path.clone(), pane.view_rule())
        };
        if let Err(error) = ui_preferences::save_column_view_rule(&path, rule) {
            self.notice = Some(error.into());
        }
        self.clear_hidden_column_selection();
        self.column_scroll_pending = true;
        cx.notify();
    }

    fn invalidate_vcs(&mut self) {
        self.vcs_generation = self.vcs_generation.wrapping_add(1);
        self._vcs_task = Task::ready(());
        self.git_head = None;
    }

    fn start_git_auto_fetch(
        &mut self,
        enabled: bool,
        interval_minutes: u32,
        cx: &mut Context<Self>,
    ) {
        self._git_auto_fetch_task = Task::ready(());
        if !enabled {
            return;
        }
        let interval = Duration::from_secs(u64::from(interval_minutes.clamp(5, 240)) * 60);
        self._git_auto_fetch_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let Ok(path) = this.update(cx, |this, _| {
                    this.active_git_operation
                        .is_none()
                        .then(|| this.current_dir.clone())
                }) else {
                    return;
                };
                let Some(path) = path else {
                    continue;
                };
                let fetched = cx
                    .background_spawn(async move {
                        let repository = kite_core::git::remote::repository(&path).ok()?;
                        let monitor = kite_core::git::remote::GitOperationMonitor::new();
                        kite_core::git::remote::fetch(
                            &repository.root,
                            &kite_core::git::remote::GitFetchOptions {
                                remote: None,
                                all_remotes: false,
                                prune: true,
                            },
                            None,
                            &monitor,
                        )
                        .ok()
                    })
                    .await
                    .is_some();
                if fetched {
                    let _ = this.update(cx, |this, cx| {
                        this.invalidate_vcs();
                        this.refresh_vcs_statuses(this.current_dir.clone(), cx);
                    });
                }
            }
        });
    }

    fn refresh_vcs_statuses(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.vcs_generation = self.vcs_generation.wrapping_add(1);
        if shell_fs::is_shell_namespace_path(&root) {
            self._vcs_task = Task::ready(());
            self.git_head = None;
            return;
        }
        let generation = self.vcs_generation;
        let status_root = root.clone();
        let head_root = root.clone();
        self._vcs_task = cx.spawn(async move |this, cx| {
            let (snapshot, git_head) = cx
                .background_spawn(async move {
                    let snapshot = load_vcs_snapshot(&status_root);
                    let git_head = crate::git_branches::git_head_summary(&head_root)
                        .ok()
                        .flatten();
                    (snapshot, git_head)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.vcs_generation != generation {
                    return;
                }
                this.git_head = git_head;
                this.table.update(cx, |state, cx| {
                    state.delegate_mut().apply_vcs_statuses(&root, &snapshot);
                    state.refresh(cx);
                    cx.notify();
                });
                if let Some(pane) = this
                    .column_panes
                    .iter_mut()
                    .find(|pane| same_path(&pane.path, &root))
                {
                    apply_statuses(&mut pane.directory_entries, &root, &snapshot);
                    if let Some(search_entries) = pane.search_entries.as_mut() {
                        apply_statuses(search_entries, &root, &snapshot);
                    }
                    pane.git_available = snapshot.git_available();
                    pane.svn_available = snapshot.svn_available();
                    pane.refresh_view();
                }
                cx.notify();
            });
        });
    }

    fn invalidate_columns(&mut self) {
        self.column_generation = self.column_generation.wrapping_add(1);
        self._column_task = Task::ready(());
        self.column_refresh_generation = self.column_refresh_generation.wrapping_add(1);
        self._column_refresh_task = Task::ready(());
    }

    fn load_columns(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.column_generation = self.column_generation.wrapping_add(1);
        self.column_refresh_generation = self.column_refresh_generation.wrapping_add(1);
        self._column_refresh_task = Task::ready(());
        let generation = self.column_generation;
        let chain = column_chain(&root);
        let preferences = ui_preferences::load_ui_preferences();
        let rules = chain
            .iter()
            .map(|path| preferences.column_view_rule(path))
            .collect::<Vec<_>>();
        self.column_panes = chain
            .iter()
            .zip(rules.iter())
            .map(|(path, rule)| {
                ColumnPane::new(
                    path.clone(),
                    Vec::new(),
                    0,
                    true,
                    false,
                    false,
                    rule.clone(),
                )
            })
            .collect();
        self.sync_directory_watchers();
        self.column_selected_path = None;
        self.column_scroll_pending = true;
        self.path_bar_scroll_pending = true;
        cx.notify();

        let load_paths = chain.into_iter().zip(rules).collect::<Vec<_>>();
        self._column_task = cx.spawn(async move |this, cx| {
            let panes = cx
                .background_spawn(async move {
                    let mut panes = Vec::with_capacity(load_paths.len());
                    for (path, rule) in load_paths {
                        panes.push(load_column_pane(path, rule));
                    }
                    panes
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.column_generation != generation {
                    return;
                }
                let mut panes = panes;
                for index in 0..panes.len().saturating_sub(1) {
                    let child_path = panes[index + 1].path.clone();
                    panes[index].set_single_selection(child_path);
                }
                this.column_panes = panes;
                this.sync_directory_watchers();
                this.column_scroll_pending = true;
                this.path_bar_scroll_pending = true;
                cx.notify();
            });
        });
    }

    fn update_columns_for_loaded_path(
        &mut self,
        root: PathBuf,
        entries: Vec<FileEntry>,
        skipped: usize,
        cx: &mut Context<Self>,
    ) {
        self.column_generation = self.column_generation.wrapping_add(1);
        self.column_refresh_generation = self.column_refresh_generation.wrapping_add(1);
        self._column_refresh_task = Task::ready(());
        let generation = self.column_generation;
        let chain = column_chain(&root);
        let common = common_column_prefix(&self.column_panes, &chain);
        let inherited = self.column_panes.get(common.saturating_sub(1));
        let git_available = inherited.is_some_and(|pane| pane.git_available);
        let svn_available = inherited.is_some_and(|pane| pane.svn_available);
        let final_index = chain.len() - 1;
        let mut entries = Some(entries);
        let mut panes = self.column_panes[..common].to_vec();
        let preferences = ui_preferences::load_ui_preferences();

        for (index, path) in chain.iter().enumerate().skip(common) {
            let ready_entries = (index == final_index).then(|| entries.take().unwrap_or_default());
            panes.push(ColumnPane::new(
                path.clone(),
                ready_entries.unwrap_or_default(),
                if index == final_index { skipped } else { 0 },
                index != final_index,
                git_available,
                svn_available,
                preferences.column_view_rule(path),
            ));
        }
        if let Some(entries) = entries
            && let Some(last) = panes.get_mut(final_index)
        {
            last.set_directory_entries(entries, skipped);
        }

        self.column_panes = panes;
        self.sync_directory_watchers();
        for index in 0..self.column_panes.len().saturating_sub(1) {
            let child_path = self.column_panes[index + 1].path.clone();
            self.column_panes[index].set_single_selection(child_path);
        }
        if let Some(last) = self.column_panes.last_mut() {
            last.clear_selection();
        }
        self.column_selected_path = None;
        self.column_scroll_pending = true;
        self.path_bar_scroll_pending = true;

        let load_paths = (common..final_index)
            .map(|index| {
                let path = chain[index].clone();
                let rule = preferences.column_view_rule(&path);
                (index, path, rule)
            })
            .collect::<Vec<_>>();
        if load_paths.is_empty() {
            self._column_task = Task::ready(());
        } else {
            self._column_task = cx.spawn(async move |this, cx| {
                let loaded = cx
                    .background_spawn(async move {
                        load_paths
                            .into_iter()
                            .map(|(index, path, rule)| (index, load_column_pane(path, rule)))
                            .collect::<Vec<_>>()
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.column_generation != generation {
                        return;
                    }
                    for (index, pane) in loaded {
                        if let Some(slot) = this.column_panes.get_mut(index) {
                            *slot = pane;
                        }
                    }
                    for index in 0..this.column_panes.len().saturating_sub(1) {
                        let child_path = this.column_panes[index + 1].path.clone();
                        this.column_panes[index].set_single_selection(child_path);
                    }
                    cx.notify();
                });
            });
        }
        cx.notify();
    }

    fn show_list_view(&mut self, _: &ShowListView, window: &mut Window, cx: &mut Context<Self>) {
        self.close_column_search(window, cx);
        self.clear_column_preview();
        if self.view_mode == ViewMode::List {
            return;
        }
        self.view_mode = ViewMode::List;
        self.column_selected_path = None;
        self.invalidate_columns();
        self.sync_directory_watchers();
        if self.active_search.is_none() {
            self.refresh_vcs_statuses(self.current_dir.clone(), cx);
        }
        cx.notify();
    }

    fn show_columns_view(&mut self, _: &ShowColumnsView, _: &mut Window, cx: &mut Context<Self>) {
        self.view_mode = ViewMode::Columns;
        self.column_selected_path = None;
        if self.active_search.is_none() {
            self.load_columns(self.current_dir.clone(), cx);
        }
        cx.notify();
    }

    fn toggle_path_bar(&mut self, _: &TogglePathBar, _: &mut Window, cx: &mut Context<Self>) {
        self.show_path_bar = !self.show_path_bar;
        self.path_bar_scroll_pending = self.show_path_bar;
        cx.notify();
    }
    fn start_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.close_column_search(window, cx);
        if shell_fs::is_shell_namespace_path(&self.current_dir) {
            self.notice = Some("移动设备内搜索暂不支持".into());
            self.error = None;
            cx.notify();
            return;
        }
        self.cancel_search();
        self.invalidate_vcs();
        self.invalidate_columns();
        self.search_generation = self.search_generation.wrapping_add(1);
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.search_generation;
        let root = self.current_dir.clone();
        let sort = self.table.read(cx).delegate().sort_spec();
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = Some(cancel.clone());
        self.active_search = Some(SearchState {
            query: query.clone(),
            root: root.clone(),
        });
        self.loading = true;
        self.error = None;
        self.notice = None;
        self.search_truncated = false;
        self.table.update(cx, |state, cx| {
            state.delegate_mut().clear_entries();
            state.delegate_mut().set_loading(true);
            state.clear_selection(cx);
            state.refresh(cx);
            cx.notify();
        });
        cx.notify();

        self._search_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { search_directory(&root, &query, sort, cancel.as_ref()) },
                )
                .await;

            let _ = this.update_in(cx, |this, _, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.loading = false;
                this.search_cancel = None;
                this.table.update(cx, |state, cx| {
                    state.delegate_mut().set_loading(false);
                    cx.notify();
                });

                match result {
                    Ok(Some(snapshot)) => {
                        this.active_search = Some(SearchState {
                            query: snapshot.query.clone(),
                            root: snapshot.root,
                        });
                        this.search_truncated = snapshot.truncated;
                        this.error = None;
                        this.table.update(cx, |state, cx| {
                            state
                                .delegate_mut()
                                .replace_entries(snapshot.entries, Some(snapshot.query));
                            state.clear_selection(cx);
                            state.refresh(cx);
                            cx.notify();
                        });
                        this.refresh_vcs_statuses(this.current_dir.clone(), cx);
                    }
                    Ok(None) => {
                        this.active_search = None;
                    }
                    Err(error) => {
                        this.active_search = None;
                        this.error = Some(error.into());
                        this.table.update(cx, |state, cx| {
                            state.delegate_mut().clear_entries();
                            state.refresh(cx);
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            });
        });
    }

    fn cancel_search(&mut self) {
        if let Some(cancel) = self.search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.search_generation = self.search_generation.wrapping_add(1);
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_search.is_none() && !self.loading {
            return;
        }
        self.cancel_search();
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.active_search = None;
        self.search_truncated = false;
        self.search_visible = false;
        self.request_load(
            self.current_dir.clone(),
            NavigationIntent::Refresh,
            window,
            cx,
        );
    }

    fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.address_editing = true;
        self.address_input.update(cx, |input, cx| {
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            self.start_last_column_search(None, window, cx);
            return;
        }
        self.search_visible = true;
        self.search_input.update(cx, |input, cx| {
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn focus_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.table.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn on_table_event(
        &mut self,
        _: &Entity<TableState<FileTableDelegate>>,
        event: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TableEvent::DoubleClickedRow(index) => {
                if let Some(entry) = self.table.read(cx).delegate().entry(*index).cloned() {
                    self.open_entry(entry, window, cx);
                }
            }
            TableEvent::RightClickedRow(index) => {
                let index = *index;
                let table = self.table.clone();
                let app = cx.entity();
                window.defer(cx, move |_, cx| {
                    table.update(cx, |state, cx| match index {
                        Some(index) => state.set_selected_row(index, cx),
                        None => state.clear_selection(cx),
                    });
                    app.update(cx, |_, cx| cx.notify());
                });
            }
            TableEvent::SelectRow(_) | TableEvent::ClearSelection => cx.notify(),
            _ => {}
        }
    }

    fn open_entry(&mut self, entry: FileEntry, window: &mut Window, cx: &mut Context<Self>) {
        if entry.is_dir() {
            self.navigate_to(entry.path, NavigationIntent::Push, window, cx);
            return;
        }
        self.open_path(entry.path, window, cx);
    }

    fn open_preview_window(
        &mut self,
        entry: FileEntry,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_target = is_editor_preview_entry(&entry);
        let title = if editor_target {
            format!("Kite 编辑器 — {}", entry.name)
        } else {
            format!("Kite 预览 — {}", entry.name)
        };
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(size(px(980.), px(760.)), cx)),
            window_min_size: Some(size(px(640.), px(480.))),
            ..Default::default()
        };
        match gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| PreviewWindow::new(entry, window, cx))
        }) {
            Ok(_) => {
                self.error = None;
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(format!("无法打开预览窗口：{error}").into());
            }
        }
        cx.notify();
    }

    fn open_workspace_editor(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let root = self.current_dir.clone();
        let title = format!("Kite 工作区 — {}", root.display());
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
            window_min_size: Some(size(px(860.), px(560.))),
            ..Default::default()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| WorkspaceWindow::new(root, window, cx))
        }) {
            self.error = Some(format!("无法打开工作区编辑器：{error}").into());
        }
        cx.notify();
    }

    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let shell_path = shell_fs::is_shell_namespace_path(&path);
        if path.is_dir() || (shell_path && shell_fs::is_directory(&path)) {
            self.navigate_to(path, NavigationIntent::Push, window, cx);
            return;
        }
        self.error = None;
        self.notice = Some("正在打开文件…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if shell_path {
                        shell_fs::open_item(&path)
                    } else {
                        open_file(&path)
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(()) => this.notice = None,
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn selected_entries(&self, cx: &App) -> Vec<FileEntry> {
        if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            let Some(selected) = self.column_selected_path.as_ref() else {
                return Vec::new();
            };
            let Some(pane) = self
                .column_panes
                .iter()
                .rev()
                .find(|pane| pane.is_selected(selected))
            else {
                return Vec::new();
            };
            return pane
                .entries
                .iter()
                .filter(|entry| pane.is_selected(&entry.path))
                .cloned()
                .collect();
        }

        self.table
            .read(cx)
            .selected_row()
            .and_then(|selected| self.table.read(cx).delegate().entry(selected).cloned())
            .into_iter()
            .collect()
    }

    fn selected_entry(&self, cx: &App) -> Option<FileEntry> {
        if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            let selected = self.column_selected_path.as_ref()?;
            return self
                .column_panes
                .iter()
                .flat_map(|pane| pane.entries.iter())
                .find(|entry| same_path(&entry.path, selected))
                .cloned();
        }

        self.selected_entries(cx).into_iter().next()
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry(cx) {
            self.open_entry(entry, window, cx);
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            let Some(last) = self.column_panes.len().checked_sub(1) else {
                return;
            };
            if let Some(pane) = self.column_panes.get_mut(last) {
                pane.select_all();
                self.column_selected_path = pane.selected_path.clone();
            }
            self.clear_column_preview();
            self.column_scroll_pending = true;
            cx.notify();
            return;
        }

        let has_entries = !self.table.read(cx).delegate().entries().is_empty();
        if has_entries {
            self.table
                .update(cx, |state, cx| state.set_selected_row(0, cx));
        }
    }

    fn on_context_menu_action(
        &mut self,
        action: &ContextMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action.command {
            ContextMenuCommand::Open => self.open_path(action.target.clone(), window, cx),
            ContextMenuCommand::Copy => {
                self.set_clipboard_path(action.target.clone(), ClipboardMode::Copy, cx)
            }
            ContextMenuCommand::CopyTo => self.copy_to_directory(action.target.clone(), window, cx),
            ContextMenuCommand::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(path_text(&action.target)));
                self.error = None;
                self.notice = Some("已复制完整路径".into());
                cx.notify();
            }
            ContextMenuCommand::Cut => {
                self.set_clipboard_path(action.target.clone(), ClipboardMode::Cut, cx)
            }
            ContextMenuCommand::Paste => {
                let destination = paste_destination(&action.target, &action.working_directory);
                self.paste_clipboard_into(destination, window, cx)
            }
            ContextMenuCommand::NewFolder => {
                self.begin_new_folder_in(action.working_directory.clone(), window, cx)
            }
            ContextMenuCommand::NewFile => {
                self.begin_new_file_in(action.working_directory.clone(), window, cx)
            }
            ContextMenuCommand::AddFavorite => self.add_favorite(action.target.clone(), cx),
            ContextMenuCommand::RemoveFavorite => self.remove_favorite(action.target.clone(), cx),
            ContextMenuCommand::Rename => self.begin_rename_path(action.target.clone(), window, cx),
            ContextMenuCommand::Recycle => {
                self.confirm_recycle_path(action.target.clone(), window, cx)
            }
            ContextMenuCommand::CompressZip => {
                self.compress_path(action.target.clone(), ArchiveFormat::Zip, window, cx)
            }
            ContextMenuCommand::CompressTarGz => {
                self.compress_path(action.target.clone(), ArchiveFormat::TarGz, window, cx)
            }
            ContextMenuCommand::ExtractHere => {
                self.extract_archive(action.target.clone(), None, window, cx);
            }
            ContextMenuCommand::ExtractTo => {
                self.extract_to_directory(action.target.clone(), window, cx)
            }
            ContextMenuCommand::ShowProperties => {
                match show_properties(window, &action.target) {
                    Ok(()) => {
                        self.error = None;
                        self.notice = Some("已打开属性".into());
                    }
                    Err(error) => {
                        self.notice = None;
                        self.error = Some(error.into());
                    }
                }
                cx.notify();
            }
            ContextMenuCommand::Vcs(command) => {
                self.run_vcs_command(command, action.target.clone(), window, cx)
            }
            ContextMenuCommand::SystemMenu => {
                self.open_system_menu(action.target.clone(), window, cx)
            }
            ContextMenuCommand::Refresh => self.refresh(window, cx),
        }
    }

    fn open_system_menu(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = match request_system_menu(window, &path) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        self.notice = None;

        cx.spawn_in(window, async move |this, cx| {
            let result = receiver.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(Ok(true)) => this.refresh(window, cx),
                Ok(Ok(false)) => {}
                Ok(Err(error)) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(format!("无法读取 Windows 原生菜单结果：{error}").into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn context_target_missing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = Some("项目已不存在，列表已刷新".into());
        self.refresh(window, cx);
    }

    fn check_for_updates(
        &mut self,
        _: &CheckForUpdates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_in_progress {
            self.notice = Some("正在检查或下载 Kite 更新…".into());
            cx.notify();
            return;
        }
        self.update_in_progress = true;
        self.error = None;
        self.notice = Some("正在检查 Kite 更新…".into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async { crate::updater::check_for_update() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.update_in_progress = false;
                match result {
                    Ok(update) => this.offer_update(update, window, cx),
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn offer_update(
        &mut self,
        update: crate::updater::UpdateInfo,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let notes = crate::update_notes::parse_release_notes(&update.release_notes);
        if update.update_available {
            let readiness_error = crate::updater::windows_update_target().err();
            self.error = readiness_error.map(SharedString::from);
            self.notice = Some(
                format!(
                    "发现 Kite {} 新版本，可在设置中下载安装",
                    update.latest_version
                )
                .into(),
            );
            self.available_update = Some(AvailableUpdate {
                info: update,
                notes,
            });
        } else {
            self.error = None;
            self.notice = Some(format!("Kite {} 已是最新版本", update.current_version).into());
            self.available_update = None;
        }
        cx.notify();
    }

    fn open_update_release_notes(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self
            .available_update
            .as_ref()
            .map(|update| update.info.release_url.clone())
        else {
            return;
        };
        if !url.starts_with("https://github.com/wyz541/kite/releases/") {
            self.notice = None;
            self.error = Some("发布说明地址无效。".into());
            cx.notify();
            return;
        }
        if let Err(error) = open::that(url) {
            self.notice = None;
            self.error = Some(format!("无法打开完整发布说明：{error}").into());
            cx.notify();
        }
    }

    fn start_update_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_in_progress {
            return;
        }
        let Some(update) = self
            .available_update
            .as_ref()
            .filter(|update| update.info.update_available)
            .map(|update| update.info.clone())
        else {
            return;
        };
        if let Err(error) = crate::updater::windows_update_target() {
            self.notice = None;
            self.error = Some(error.into());
            cx.notify();
            return;
        }

        let version = update.latest_version.clone();
        let initial_progress = crate::updater::UpdateDownloadProgress {
            downloaded_bytes: 0,
            total_bytes: update.asset_size,
            active: true,
        };
        self.update_in_progress = true;
        self.error = None;
        self.update_download_progress = Some(initial_progress);
        self.notice = Some(update_download_notice(&version, initial_progress).into());
        cx.notify();

        let download_finished = Arc::new(AtomicBool::new(false));
        let observed_download_finished = download_finished.clone();
        let progress_version = update.latest_version.clone();
        let progress_entity = cx.entity();
        cx.spawn_in(window, async move |this, cx| {
            let progress_task = cx.spawn(async move |cx| {
                loop {
                    let progress = crate::updater::download_progress();
                    if progress.active || progress.downloaded_bytes == 0 {
                        progress_entity.update(cx, |this, cx| {
                            this.update_download_progress = Some(progress);
                            this.notice =
                                Some(update_download_notice(&progress_version, progress).into());
                            cx.notify();
                        });
                    }
                    if observed_download_finished.load(Ordering::Relaxed) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(120))
                        .await;
                }
            });

            let download_finished_for_task = download_finished.clone();
            let result = cx
                .background_spawn(async move {
                    let download_result = update.download();
                    download_finished_for_task.store(true, Ordering::Relaxed);
                    let downloaded = download_result?;
                    crate::updater::install_windows_update(
                        &downloaded,
                        &update.asset_sha256,
                        &update.latest_version,
                    )?;
                    Ok::<String, String>(update.latest_version)
                })
                .await;
            download_finished.store(true, Ordering::Relaxed);
            let _ = progress_task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.update_in_progress = false;
                this.update_download_progress = None;
                match result {
                    Ok(version) => {
                        this.error = None;
                        this.notice =
                            Some(format!("Kite {version} 已准备好，正在重启安装…").into());
                        cx.notify();
                        window.defer(cx, |_, cx| cx.quit());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn set_default_explorer(
        &mut self,
        _: &SetDefaultExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = "将 Kite 设为默认资源管理器？";
        let detail = "Kite 将接管文件夹打开命令。\nKite 将创建开始菜单入口。\nWindows 桌面、任务栏和系统文件对话框仍由 Windows 管理。";
        let answers = [PromptButton::ok("设为默认"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, message, Some(detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let result = cx
                .background_spawn(async {
                    crate::explorer_integration::set_as_default_file_manager()
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(message) => {
                        this.error = None;
                        this.notice = Some(message.into());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_default_explorer(
        &mut self,
        _: &RestoreDefaultExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = "恢复之前的文件夹打开方式？";
        let detail = "Kite 将恢复最近保存的文件夹打开命令。";
        let answers = [PromptButton::ok("恢复"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, message, Some(detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let result = cx
                .background_spawn(async {
                    crate::explorer_integration::restore_previous_file_manager()
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(message) => {
                        this.error = None;
                        this.notice = Some(message.into());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_start_menu_shortcut(
        &mut self,
        _: &CreateStartMenuShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        self.notice = Some("正在创建开始菜单快捷方式…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async {
                    crate::explorer_integration::register_start_menu_shortcut()
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(message) => {
                        this.error = None;
                        this.notice = Some(message.into());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run_vcs_command(
        &mut self,
        command: VcsMenuCommand,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            VcsMenuCommand::GitOpenClient => self.open_git_client_at_path(path, window, cx),
            VcsMenuCommand::GitAdd => {
                self.run_vcs_task(VcsTool::Git, VcsOperation::Add, path, window, cx)
            }
            VcsMenuCommand::GitUnstage => {
                self.run_vcs_task(VcsTool::Git, VcsOperation::Unstage, path, window, cx)
            }
            VcsMenuCommand::GitCommit => self.begin_vcs_commit(VcsTool::Git, path, window, cx),
            VcsMenuCommand::GitPull => self.begin_git_pull(path, window, cx),
            VcsMenuCommand::GitPush => self.open_git_push_dialog(path, window, cx),
            VcsMenuCommand::GitFetch => self.start_git_network(
                GitNetworkRequest::new(GitOperationCommand::Fetch {
                    path,
                    all_remotes: false,
                }),
                window,
                cx,
            ),
            VcsMenuCommand::GitFetchAll => self.start_git_network(
                GitNetworkRequest::new(GitOperationCommand::Fetch {
                    path,
                    all_remotes: true,
                }),
                window,
                cx,
            ),
            VcsMenuCommand::GitManageRemotes => self.open_git_remotes_dialog(path, window, cx),
            VcsMenuCommand::GitClone => {
                self.open_git_clone_dialog(working_directory_for_path(&path), window, cx)
            }
            VcsMenuCommand::GitInit => {
                self.initialize_git_repository(working_directory_for_path(&path), window, cx)
            }
            VcsMenuCommand::GitDiff => {
                self.run_vcs_query_task(VcsTool::Git, VcsQuery::Diff, path, window, cx)
            }
            VcsMenuCommand::GitLog => {
                match git_log_window::open_git_log_window(path, cx) {
                    Ok(()) => {
                        self.error = None;
                        self.notice = Some("已打开提交历史窗口".into());
                    }
                    Err(error) => {
                        self.notice = None;
                        self.error = Some(error.into());
                    }
                }
                cx.notify();
            }
            VcsMenuCommand::GitRebase => {
                match crate::git_rebase_window::open_git_rebase_window(path, cx) {
                    Ok(()) => {
                        self.error = None;
                        self.notice = Some("已打开交互式 Rebase 窗口".into());
                    }
                    Err(error) => {
                        self.notice = None;
                        self.error = Some(error.into());
                    }
                }
                cx.notify();
            }
            VcsMenuCommand::GitRevert => self.confirm_vcs_revert(VcsTool::Git, path, window, cx),
            VcsMenuCommand::GitClean => self.confirm_git_clean(path, window, cx),
            VcsMenuCommand::SvnUpdate => {
                self.run_vcs_task(VcsTool::Svn, VcsOperation::Update, path, window, cx)
            }
            VcsMenuCommand::SvnAdd => {
                self.run_vcs_task(VcsTool::Svn, VcsOperation::Add, path, window, cx)
            }
            VcsMenuCommand::SvnCommit => self.begin_vcs_commit(VcsTool::Svn, path, window, cx),
            VcsMenuCommand::SvnLock => {
                self.run_vcs_task(VcsTool::Svn, VcsOperation::Lock, path, window, cx)
            }
            VcsMenuCommand::SvnUnlock => {
                self.run_vcs_task(VcsTool::Svn, VcsOperation::Unlock, path, window, cx)
            }
            VcsMenuCommand::SvnDiff => {
                self.run_vcs_query_task(VcsTool::Svn, VcsQuery::Diff, path, window, cx)
            }
            VcsMenuCommand::SvnLog => self.open_vcs_log_window_task(VcsTool::Svn, path, window, cx),
            VcsMenuCommand::SvnRevert => self.confirm_vcs_revert(VcsTool::Svn, path, window, cx),
            VcsMenuCommand::SvnCleanup => {
                self.run_vcs_task(VcsTool::Svn, VcsOperation::Cleanup, path, window, cx)
            }
        }
    }

    fn run_vcs_task(
        &mut self,
        tool: VcsTool,
        operation: VcsOperation,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = match &operation {
            VcsOperation::Add => format!("正在{}添加…", tool.label()),
            VcsOperation::Unstage => format!("正在{}取消暂存…", tool.label()),
            VcsOperation::Commit(_) => format!("正在{}提交…", tool.label()),
            VcsOperation::Pull => format!("正在{}拉取…", tool.label()),
            VcsOperation::Push => format!("正在{}推送…", tool.label()),
            VcsOperation::Fetch => format!("正在{}获取…", tool.label()),
            VcsOperation::Stash => format!("正在{}暂存…", tool.label()),
            VcsOperation::Revert => format!("正在{}还原…", tool.label()),
            VcsOperation::Clean => format!("正在{}清理…", tool.label()),
            VcsOperation::Update => format!("正在{}更新…", tool.label()),
            VcsOperation::Lock => format!("正在{}锁定…", tool.label()),
            VcsOperation::Unlock => format!("正在{}解锁…", tool.label()),
            VcsOperation::Cleanup => format!("正在{}清理…", tool.label()),
        };
        self.error = None;
        self.notice = Some(pending.into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { run_vcs_operation_for_ui(tool, operation, &path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(message) => {
                    this.notice = Some(message.into());
                    this.refresh(window, cx);
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn run_vcs_query_task(
        &mut self,
        tool: VcsTool,
        query: VcsQuery,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("{} {}", tool.label(), query.label());
        self.error = None;
        self.notice = Some(format!("正在读取{title}…").into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { open_vcs_query_text(tool, query, &path) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(message) => this.notice = Some(message.into()),
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_vcs_log_window_task(
        &mut self,
        tool: VcsTool,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        self.notice = Some(format!("正在打开{}日志窗口…", tool.label()).into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    open_vcs_log_window(tool, &path).or_else(|window_error| {
                        open_vcs_query_text(tool, VcsQuery::Log, &path).map_err(|text_error| {
                            format!("{window_error}；文本回退失败：{text_error}")
                        })
                    })
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(message) => this.notice = Some(message.into()),
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_vcs_commit(
        &mut self,
        tool: VcsTool,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(tool, VcsTool::Git) {
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Kite — Git 提交".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::centered(size(px(980.), px(720.)), cx)),
                window_min_size: Some(size(px(760.), px(600.))),
                ..Default::default()
            };
            match gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| crate::git_commit_window::GitCommitWindow::new(path, window, cx))
            }) {
                Ok(_) => {
                    self.error = None;
                    self.notice = Some("Git 提交窗口已打开".into());
                }
                Err(error) => {
                    self.notice = None;
                    self.error = Some(format!("无法打开 Git 提交窗口：{error}").into());
                }
            }
            cx.notify();
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("提交说明")
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_modal(window, cx);
                }
            },
        );
        let kind = match tool {
            VcsTool::Git => ModalKind::GitCommit(path),
            VcsTool::Svn => ModalKind::SvnCommit(path),
        };
        self.modal = Some(ModalState {
            kind,
            input: input.clone(),
            _subscription: subscription,
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn confirm_vcs_revert(
        &mut self,
        tool: VcsTool,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path_text(&path));
        let message = format!("还原“{name}”？{} 将丢弃未提交更改。", tool.label());
        let detail = path_text(&path);
        let answers = [PromptButton::ok("还原"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let result = cx
                .background_spawn(async move {
                    run_vcs_operation_for_ui(tool, VcsOperation::Revert, &path)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(message) => {
                    this.notice = Some(message.into());
                    this.refresh(window, cx);
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_git_clean(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let message = "清理所选路径的未跟踪文件和文件夹？";
        let detail = format!("{}\n此操作不可撤销。忽略文件不受影响。", path_text(&path));
        let answers = [PromptButton::ok("清理"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, message, Some(&detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let result = cx
                .background_spawn(async move {
                    run_vcs_operation_for_ui(VcsTool::Git, VcsOperation::Clean, &path)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(message) => {
                    this.notice = Some(message.into());
                    this.refresh(window, cx);
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn sync_clipboard_state(&mut self, cx: &mut Context<Self>) {
        let can_paste = self.clipboard.borrow().is_some() || file_clipboard::has_files();
        self.table.update(cx, |state, cx| {
            state.delegate_mut().set_can_paste(can_paste);
            cx.notify();
        });
    }

    fn select_loaded_file(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            let Some(column) = self.column_panes.len().checked_sub(1) else {
                return false;
            };
            let Some(row) = self.column_panes[column]
                .display_rows()
                .iter()
                .position(|row| {
                    matches!(
                        row,
                        ColumnDisplayRow::Entry(entry)
                            if !entry.is_dir() && same_path(&entry.path, path)
                    )
                })
            else {
                return false;
            };
            self.select_column_entry(column, row, cx);
            return true;
        }

        let Some(row) = self
            .table
            .read(cx)
            .delegate()
            .entries()
            .iter()
            .position(|entry| !entry.is_dir() && same_path(&entry.path, path))
        else {
            return false;
        };
        self.table
            .update(cx, |state, cx| state.set_selected_row(row, cx));
        true
    }

    fn select_column_entry(&mut self, column: usize, row: usize, cx: &mut Context<Self>) {
        self.select_column_entry_with_modifiers(column, row, "", cx);
    }

    fn select_column_entry_with_modifiers(
        &mut self,
        column: usize,
        row: usize,
        mode: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.column_panes.get(column).map(|pane| pane.path.clone()) {
            self.remember_column(&path);
        }
        let Some(entry) = self
            .column_panes
            .get(column)
            .and_then(|pane| pane.entry_at_visible_row(row))
        else {
            return;
        };
        if let Some(pane) = self.column_panes.get_mut(column) {
            match mode {
                "toggle" => pane.toggle_selection(entry.path.clone()),
                "range" => pane.select_range_to(entry.path.clone()),
                _ => pane.set_single_selection(entry.path.clone()),
            }
        }
        let selected_count = self
            .column_panes
            .get(column)
            .map_or(0, |pane| pane.selected_paths.len());
        self.column_selected_path = self
            .column_panes
            .get(column)
            .and_then(|pane| pane.selected_path.clone());
        if selected_count != 1 {
            self.clear_column_preview();
            self.path_bar_scroll_pending = true;
            cx.notify();
            return;
        }
        let Some(entry) = self.column_panes.get(column).and_then(|pane| {
            pane.selected_path.as_ref().and_then(|selected| {
                pane.entries
                    .iter()
                    .find(|entry| same_path(&entry.path, selected))
                    .cloned()
            })
        }) else {
            return;
        };
        if !entry.is_dir() {
            self.column_panes
                .truncate(column_count_after_file_selection(
                    column,
                    self.column_panes.len(),
                ));
            if self.show_preview {
                self.start_column_preview(entry, cx);
            } else {
                self.clear_column_preview();
            }
            self.column_scroll_pending = true;
        } else {
            self.clear_column_preview();
        }
        self.path_bar_scroll_pending = true;
        cx.notify();
    }

    fn clear_column_preview(&mut self) {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self._preview_task = Task::ready(());
        self.column_preview = None;
        self._git_preview_task = Task::ready(());
        self.column_git_preview = None;
    }

    fn start_column_git_preview(&mut self, entry: &FileEntry, cx: &mut Context<Self>) {
        // Paths without a Git status from the directory snapshot are outside a
        // work tree or ignored; skip the extra git processes for them.
        let path = entry.path.clone();
        if entry.git_status.is_none() && !self.git_available_for(&path) {
            self._git_preview_task = Task::ready(());
            self.column_git_preview = None;
            return;
        }
        let generation = self.preview_generation;
        self.column_git_preview = Some(ColumnGitPreview {
            path: path.clone(),
            summary: None,
            loading: true,
        });
        self._git_preview_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_LOAD_DEBOUNCE).await;
            let summary_path = path.clone();
            let summary = cx
                .background_spawn(async move { git_path_summary(&summary_path).ok().flatten() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.preview_generation != generation {
                    return;
                }
                if let Some(preview) = this.column_git_preview.as_mut() {
                    preview.loading = false;
                    preview.summary = summary;
                }
                cx.notify();
            });
        });
    }

    fn git_available_for(&self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        self.column_panes
            .iter()
            .any(|pane| pane.git_available && same_path(&pane.path, parent))
    }

    fn start_column_preview(&mut self, entry: FileEntry, cx: &mut Context<Self>) {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;
        let path = entry.path.clone();
        let load_text = !is_image_preview_entry(&entry) && is_text_preview_entry(&entry);
        let log_preview = is_log_preview_entry(&entry);
        let image_preview = is_raster_image_preview_entry(&entry);
        let cache_key = image_preview.then(|| PreviewImageCacheKey::for_entry(&entry));
        let cached_image = cache_key
            .as_ref()
            .and_then(|key| self.preview_image_cache.get(key));
        let load_image = image_preview && cached_image.is_none();
        let markdown_view = (load_text && is_markdown_preview_entry(&entry)).then(|| {
            cx.new(|cx| {
                TextViewState::markdown("正在加载预览…", cx)
                    .selectable(true)
                    .scrollable(false)
            })
        });
        self._preview_task = Task::ready(());
        self.column_preview = Some(ColumnPreviewState {
            path: path.clone(),
            lines: Arc::new(Vec::new()),
            selection: None,
            markdown_view,
            image: cached_image,
            error: None,
            loading: load_text || load_image,
            scroll: UniformListScrollHandle::new(),
            markdown_scroll: ScrollHandle::new(),
        });
        self.start_column_git_preview(&entry, cx);
        if !load_text && !load_image {
            return;
        }

        self._preview_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_LOAD_DEBOUNCE).await;
            let result = cx
                .background_spawn(async move {
                    if load_image {
                        PreviewLoadResult::Image(load_image_preview(
                            &path,
                            COLUMN_PREVIEW_IMAGE_MAX_WIDTH,
                            COLUMN_PREVIEW_IMAGE_MAX_HEIGHT,
                        ))
                    } else if log_preview {
                        PreviewLoadResult::Text(read_log_preview(&path))
                    } else {
                        PreviewLoadResult::Text(text_editor::read_preview_text(&path))
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.preview_generation != generation {
                    return;
                }
                match result {
                    PreviewLoadResult::Image(Ok(image_data)) => {
                        let image = preview_render_image(image_data);
                        if let Some(key) = cache_key {
                            this.preview_image_cache.insert(key, image.clone());
                        }
                        if let Some(preview) = this.column_preview.as_mut() {
                            preview.loading = false;
                            preview.image = Some(image);
                        }
                    }
                    PreviewLoadResult::Image(Err(error)) => {
                        if let Some(preview) = this.column_preview.as_mut() {
                            preview.loading = false;
                            preview.error = Some(error.into());
                        }
                    }
                    PreviewLoadResult::Text(result) => {
                        let markdown_view = this
                            .column_preview
                            .as_ref()
                            .and_then(|preview| preview.markdown_view.clone());
                        if let Some(preview) = this.column_preview.as_mut() {
                            preview.loading = false;
                            match result {
                                Ok(text) => {
                                    if let Some(markdown_view) = markdown_view {
                                        markdown_view
                                            .update(cx, |view, cx| view.set_text(&text, cx));
                                    } else {
                                        preview.lines = Arc::new(preview_text_lines(&text));
                                        preview.selection =
                                            Some(TextSelectionHandle::new(text, cx));
                                    }
                                }
                                Err(error) => preview.error = Some(error.into()),
                            }
                        }
                    }
                }
                cx.notify();
            });
        });
    }
    fn activate_column_entry(
        &mut self,
        column: usize,
        row: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.column_panes.get(column).map(|pane| pane.path.clone()) {
            self.remember_column(&path);
        }
        let Some(entry) = self
            .column_panes
            .get(column)
            .and_then(|pane| pane.entry_at_visible_row(row))
        else {
            return;
        };
        let modifiers = event.modifiers();
        let selection_mode = if modifiers.shift {
            "range"
        } else if modifiers.secondary() {
            "toggle"
        } else {
            ""
        };
        if !selection_mode.is_empty() {
            self.select_column_entry_with_modifiers(column, row, selection_mode, cx);
            return;
        }
        if entry.is_dir() {
            self.column_selected_path = None;
            if let Some(pane) = self.column_panes.get_mut(column) {
                pane.clear_selection();
            }
            self.navigate_to(entry.path, NavigationIntent::Push, window, cx);
            return;
        }
        self.select_column_entry(column, row, cx);
        if event.click_count() == 2 {
            self.open_entry(entry, window, cx);
        }
    }

    fn set_clipboard(&mut self, mode: ClipboardMode, cx: &mut Context<Self>) {
        let paths = self
            .selected_entries(cx)
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        self.set_clipboard_paths(paths, mode, cx);
    }

    fn set_clipboard_path(&mut self, path: PathBuf, mode: ClipboardMode, cx: &mut Context<Self>) {
        self.set_clipboard_paths(vec![path], mode, cx);
    }

    fn set_clipboard_paths(
        &mut self,
        paths: Vec<PathBuf>,
        mode: ClipboardMode,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        if mode == ClipboardMode::Cut
            && paths.iter().any(|path| {
                shell_fs::is_shell_namespace_path(path) && !shell_fs::can_move_item(path)
            })
        {
            self.error = None;
            self.notice = Some("此设备项目不支持剪切".into());
            cx.notify();
            return;
        }
        let path_count = paths.len();
        if paths
            .iter()
            .any(|path| shell_fs::is_shell_namespace_path(path))
        {
            self.error = None;
            self.notice = Some("移动设备项目只在 Kite 内部剪贴板中保留".into());
            *self.clipboard.borrow_mut() = Some(FileClipboard {
                paths: paths.clone(),
                mode,
            });
            self.sync_clipboard_state(cx);
            cx.notify();
            return;
        }
        let system_error = file_clipboard::write_file_clipboard(&paths, mode).err();
        *self.clipboard.borrow_mut() = Some(FileClipboard { paths, mode });
        self.sync_clipboard_state(cx);
        self.error = system_error
            .map(|error| format!("系统文件剪贴板不可用，复制仅在 Kite 内有效：{error}").into());
        self.notice = Some(if mode == ClipboardMode::Cut {
            format!("已剪切 {path_count} 项").into()
        } else {
            format!("已复制 {path_count} 项").into()
        });
        cx.notify();
    }

    fn copy_clicked(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_clipboard(ClipboardMode::Copy, cx);
    }

    fn cut_clicked(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_clipboard(ClipboardMode::Cut, cx);
    }

    fn paste_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard(window, cx);
    }

    fn paste_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard_into(self.current_dir.clone(), window, cx);
    }

    fn copy_to_directory(&mut self, source: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("拷贝到这里".into()),
        });
        self.error = None;
        self.notice = Some("请选择目标文件夹…".into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.notice = None;
                        this.error = Some(format!("无法打开目标文件夹选择器：{error}").into());
                        cx.notify();
                    });
                    return;
                }
                Err(_) => None,
            };
            let Some(destination_dir) = selected else {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.notice = None;
                    cx.notify();
                });
                return;
            };

            let Ok(owner) = this.update_in(cx, |this, window, cx| {
                this.notice = Some(format!("正在拷贝到 {}…", destination_dir.display()).into());
                cx.notify();
                file_clipboard::window_hwnd(window)
                    .ok()
                    .map(|hwnd| hwnd.0 as isize)
            }) else {
                return;
            };
            let result =
                shell_fs::copy_item_with_progress(source, destination_dir.clone(), owner).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.error = None;
                    this.notice = Some(format!("已拷贝到 {}", destination_dir.display()).into());
                    if same_path(&this.current_dir, &destination_dir) {
                        this.refresh(window, cx);
                    } else {
                        cx.notify();
                    }
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn compress_path(
        &mut self,
        source: PathBuf,
        format: ArchiveFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let format_name = match format {
            ArchiveFormat::Zip => "ZIP",
            ArchiveFormat::TarGz => "TAR.GZ",
        };
        self.error = None;
        self.notice = Some(format!("正在创建 {format_name} 文件…").into());
        cx.notify();

        let progress =
            archive_progress_window::open("压缩", Some(format), &source, source.parent(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let result = archive::compress_with_progress(&source, format, |event| {
                        progress.report(event)
                    });
                    progress.finish(&result);
                    result
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(created) => {
                    this.error = None;
                    this.notice = Some(
                        format!(
                            "已创建 {}",
                            created
                                .file_name()
                                .unwrap_or_else(|| created.as_os_str())
                                .to_string_lossy()
                        )
                        .into(),
                    );
                    let parent = created.parent().map(Path::to_path_buf);
                    if parent
                        .as_ref()
                        .is_some_and(|parent| same_path(&this.current_dir, parent))
                    {
                        this.refresh(window, cx);
                    } else {
                        cx.notify();
                    }
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn extract_to_directory(
        &mut self,
        source: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("解压到这里".into()),
        });
        self.error = None;
        self.notice = Some("请选择解压目标文件夹…".into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.notice = None;
                        this.error = Some(format!("无法打开解压目标选择器：{error}").into());
                        cx.notify();
                    });
                    return;
                }
                Err(_) => None,
            };
            let Some(destination) = selected else {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.notice = None;
                    cx.notify();
                });
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.extract_archive(source, Some(destination), window, cx);
            });
        })
        .detach();
    }

    fn extract_archive(
        &mut self,
        source: PathBuf,
        destination: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        self.notice = Some("正在解压文件…".into());
        cx.notify();

        let progress = archive_progress_window::open(
            "解压",
            archive::format_for_path(&source),
            &source,
            destination.as_deref().or_else(|| source.parent()),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let refresh_dir = destination
                .clone()
                .or_else(|| source.parent().map(Path::to_path_buf));
            let result = cx
                .background_spawn(async move {
                    let result = if let Some(destination) = destination {
                        archive::extract_with_progress(&source, &destination, |event| {
                            progress.report(event)
                        })
                    } else {
                        archive::extract_here_with_progress(&source, |event| progress.report(event))
                    };
                    progress.finish(&result);
                    result
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(extracted_to) => {
                    this.error = None;
                    this.notice = Some(format!("已解压到 {}", extracted_to.display()).into());
                    if refresh_dir
                        .as_deref()
                        .is_some_and(|directory| same_path(&this.current_dir, directory))
                    {
                        this.refresh(window, cx);
                    } else {
                        cx.notify();
                    }
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn drag_payload_for_entry(&self, column: usize, entry: &FileEntry) -> KiteFileDrag {
        let Some(pane) = self.column_panes.get(column) else {
            return KiteFileDrag {
                paths: vec![(entry.path.clone(), entry.is_dir())],
                label: entry.name.clone().into(),
            };
        };
        let use_selection = pane.selected_paths.len() > 1
            && pane
                .selected_paths
                .iter()
                .any(|selected| same_path(selected, &entry.path));
        let entries = if use_selection {
            pane.entries
                .iter()
                .filter(|candidate| pane.is_selected(&candidate.path))
                .collect::<Vec<_>>()
        } else {
            vec![entry]
        };
        let paths = entries
            .iter()
            .map(|candidate| (candidate.path.clone(), candidate.is_dir()))
            .collect::<Vec<_>>();
        let label = if entries.len() == 1 {
            entries[0].name.clone().into()
        } else {
            SharedString::from(format!("{} 个项目", entries.len()))
        };
        KiteFileDrag { paths, label }
    }

    fn transfer_dropped_paths(
        &mut self,
        sources: Vec<PathBuf>,
        destination_dir: PathBuf,
        move_source: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut sources = sources;
        sources.sort();
        sources.dedup();
        if sources.is_empty() {
            return;
        }
        if shell_fs::is_shell_namespace_path(&destination_dir)
            || sources
                .iter()
                .any(|source| shell_fs::is_shell_namespace_path(source))
        {
            self.error = None;
            self.notice = Some("移动设备项目拖放暂不支持".into());
            cx.notify();
            return;
        }

        self.error = None;
        self.notice = Some(if move_source {
            format!("正在移动 {} 项…", sources.len()).into()
        } else {
            format!("正在复制 {} 项…", sources.len()).into()
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut destinations = Vec::with_capacity(sources.len());
                    for source in &sources {
                        destinations.push(transfer_entry(source, &destination_dir, move_source)?);
                    }
                    Ok::<_, String>(destinations)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(destinations) => {
                    this.error = None;
                    this.notice = Some(if move_source {
                        format!("已移动 {} 项", destinations.len()).into()
                    } else {
                        format!("已复制 {} 项", destinations.len()).into()
                    });
                    this.refresh(window, cx);
                }
                Err(error) => {
                    this.notice = None;
                    this.error = Some(error.into());
                    this.refresh(window, cx);
                }
            });
        })
        .detach();
    }

    fn paste_clipboard_into(
        &mut self,
        destination_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let internal_clipboard = self.clipboard.borrow().clone();
        let clipboard = if internal_clipboard.as_ref().is_some_and(|clipboard| {
            clipboard
                .paths
                .iter()
                .any(|path| shell_fs::is_shell_namespace_path(path))
        }) {
            internal_clipboard
        } else {
            file_clipboard::read_file_clipboard().or(internal_clipboard)
        };
        let Some(clipboard) = clipboard else {
            return;
        };
        let sources = clipboard.paths;
        let move_source = clipboard.mode == ClipboardMode::Cut;
        if sources.is_empty() {
            return;
        }
        let uses_shell_operation = sources
            .iter()
            .any(|source| shell_fs::is_shell_namespace_path(source))
            || shell_fs::is_shell_namespace_path(&destination_dir);
        let cleared_sources = sources.clone();
        self.error = None;
        self.notice = Some(if move_source {
            "正在移动项目…".into()
        } else {
            "正在复制项目…".into()
        });
        let operation_destination = destination_dir.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if uses_shell_operation {
                        return match shell_fs::transfer_items(
                            &sources,
                            &operation_destination,
                            move_source,
                        ) {
                            Ok(()) => Ok(sources.len()),
                            Err(error) => Err((0usize, error)),
                        };
                    }
                    for (completed, source) in sources.iter().enumerate() {
                        if let Err(error) =
                            transfer_entry(source, &operation_destination, move_source)
                        {
                            return Err((completed, error));
                        }
                    }
                    Ok(sources.len())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(item_count) => {
                    if move_source {
                        *this.clipboard.borrow_mut() = None;
                        file_clipboard::clear_if_matches(&cleared_sources);
                        this.sync_clipboard_state(cx);
                    }
                    this.notice = Some(if move_source {
                        format!("已移动 {item_count} 项").into()
                    } else {
                        format!("已粘贴 {item_count} 项").into()
                    });
                    this.refresh(window, cx);
                }
                Err((completed, error)) => {
                    if uses_shell_operation {
                        this.error = None;
                        this.notice = Some(format!("设备写入未完成：{error}").into());
                        if same_path(&this.current_dir, &destination_dir) {
                            this.refresh(window, cx);
                        } else {
                            cx.notify();
                        }
                    } else if completed > 0 {
                        if move_source {
                            let remaining = cleared_sources
                                .iter()
                                .skip(completed)
                                .cloned()
                                .collect::<Vec<_>>();
                            if remaining.is_empty() {
                                *this.clipboard.borrow_mut() = None;
                                file_clipboard::clear_if_matches(&cleared_sources);
                            } else {
                                let _ = file_clipboard::write_file_clipboard(
                                    &remaining,
                                    ClipboardMode::Cut,
                                );
                                *this.clipboard.borrow_mut() = Some(FileClipboard {
                                    paths: remaining,
                                    mode: ClipboardMode::Cut,
                                });
                            }
                            this.sync_clipboard_state(cx);
                        }
                        this.error = None;
                        this.notice =
                            Some(format!("已处理 {completed} 项，后续操作失败：{error}").into());
                        if same_path(&this.current_dir, &destination_dir) {
                            this.refresh(window, cx);
                        } else {
                            cx.notify();
                        }
                    } else {
                        this.notice = None;
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn add_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if shell_fs::is_shell_namespace_path(&path) {
            self.error = None;
            self.notice = Some("设备项目不能添加为收藏".into());
            cx.notify();
            return;
        }
        let name = favorite_name(&path);
        let is_directory = path.is_dir();
        match favorites_store::add_favorite(&path, &name, is_directory) {
            Ok(true) => {
                self.favorites = favorites_store::load_favorites();
                self.error = None;
                self.notice = Some("已添加到收藏".into());
            }
            Ok(false) => {
                self.error = None;
                self.notice = Some("已在收藏中".into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    fn toggle_current_favorite(&mut self, cx: &mut Context<Self>) {
        if self.current_dir_is_favorite() {
            self.remove_favorite(self.current_dir.clone(), cx);
        } else {
            self.add_favorite(self.current_dir.clone(), cx);
        }
    }

    fn current_dir_is_favorite(&self) -> bool {
        self.favorites
            .iter()
            .any(|favorite| same_path(Path::new(&favorite.path), &self.current_dir))
    }

    fn remove_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match favorites_store::remove_favorite(&path) {
            Ok(true) => {
                self.favorites = favorites_store::load_favorites();
                self.error = None;
                self.notice = Some("已从收藏移除".into());
            }
            Ok(false) => {
                self.error = None;
                self.notice = Some("项目不在收藏中".into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    fn begin_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_new_folder_in(self.current_dir.clone(), window, cx);
    }

    fn begin_new_folder_in(
        &mut self,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("文件夹名称")
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_modal(window, cx);
                }
            },
        );
        self.modal = Some(ModalState {
            kind: ModalKind::NewFolder(directory),
            input: input.clone(),
            _subscription: subscription,
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn begin_new_file_in(
        &mut self,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("文件名，例如 notes.txt")
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_modal(window, cx);
                }
            },
        );
        self.modal = Some(ModalState {
            kind: ModalKind::NewFile(directory),
            input: input.clone(),
            _subscription: subscription,
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry(cx) {
            self.begin_rename_path(entry.path, window, cx);
        }
    }

    fn begin_rename_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let is_shell_path = shell_fs::is_shell_namespace_path(&path);
        if is_shell_path && !shell_fs::can_rename_item(&path) {
            self.error = None;
            self.notice = Some("此设备项目不支持重命名".into());
            cx.notify();
            return;
        }
        let name = if is_shell_path {
            shell_fs::display_name(&path)
        } else {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        };
        let Some(name) = name else {
            self.error = Some("无法重命名驱动器根目录".into());
            cx.notify();
            return;
        };
        if !is_shell_path && !path.exists() {
            self.context_target_missing(window, cx);
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("新名称")
                .default_value(name)
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_modal(window, cx);
                }
            },
        );
        self.modal = Some(ModalState {
            kind: ModalKind::Rename(path),
            input: input.clone(),
            _subscription: subscription,
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.modal.take() {
            modal
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.focus_table(window, cx);
        cx.notify();
    }

    fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.as_ref() else {
            return;
        };
        let value = modal.input.read(cx).value().trim().to_string();
        let kind = modal.kind.clone();
        let validation = match &kind {
            ModalKind::NewFolder(_) | ModalKind::NewFile(_) | ModalKind::Rename(_) => {
                validate_name(&value)
            }
            ModalKind::GitCommit(_) | ModalKind::SvnCommit(_) if value.is_empty() => {
                Err("请输入提交说明".to_string())
            }
            ModalKind::GitCommit(_) | ModalKind::SvnCommit(_) => Ok(()),
            ModalKind::GitUsername(_) if value.is_empty() => Err("请输入用户名".to_string()),
            ModalKind::GitUsername(_) => Ok(()),
            ModalKind::GitPassword { .. } if value.is_empty() => {
                Err("请输入密码或令牌".to_string())
            }
            ModalKind::GitPassword { .. } => Ok(()),
        };
        if let Err(error) = validation {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        self.error = None;

        match kind {
            ModalKind::NewFolder(parent) => {
                let name = value;
                let uses_shell_operation = shell_fs::is_shell_namespace_path(&parent);
                self.notice = Some("正在新建文件夹…".into());
                cx.spawn_in(window, async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            if uses_shell_operation {
                                shell_fs::create_item_in(&parent, &name, true)
                            } else {
                                create_folder(&parent, &name).map(|_| ())
                            }
                        })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| match result {
                        Ok(_) => {
                            this.close_modal(window, cx);
                            this.notice = Some("文件夹已创建".into());
                            this.refresh(window, cx);
                        }
                        Err(error) => {
                            this.notice = None;
                            this.error = Some(error.into());
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            ModalKind::NewFile(parent) => {
                let name = value;
                let uses_shell_operation = shell_fs::is_shell_namespace_path(&parent);
                self.notice = Some("正在新建文件…".into());
                cx.spawn_in(window, async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            if uses_shell_operation {
                                shell_fs::create_item_in(&parent, &name, false)
                            } else {
                                create_file(&parent, &name).map(|_| ())
                            }
                        })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| match result {
                        Ok(_) => {
                            this.close_modal(window, cx);
                            this.notice = Some("文件已创建".into());
                            this.refresh(window, cx);
                        }
                        Err(error) => {
                            this.notice = None;
                            this.error = Some(error.into());
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            ModalKind::Rename(path) => {
                let name = value;
                let rename_source = path.clone();
                let renamed_path = path
                    .parent()
                    .map(|parent| parent.join(&name))
                    .unwrap_or_else(|| path.clone());
                let uses_shell_operation = shell_fs::is_shell_namespace_path(&path);
                self.notice = Some("正在重命名…".into());
                cx.spawn_in(window, async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            if uses_shell_operation {
                                shell_fs::rename_item(&rename_source, &name)
                            } else {
                                rename_entry(&rename_source, &name).map(|_| ())
                            }
                        })
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| match result {
                        Ok(_) => {
                            this.close_modal(window, cx);
                            let destination =
                                path_after_rename(&this.current_dir, &path, &renamed_path);
                            this.navigate_to(destination, NavigationIntent::Refresh, window, cx);
                            this.notice = Some("名称已更新".into());
                        }
                        Err(error) => {
                            this.notice = None;
                            this.error = Some(error.into());
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            ModalKind::GitCommit(path) => {
                self.close_modal(window, cx);
                self.run_vcs_task(VcsTool::Git, VcsOperation::Commit(value), path, window, cx);
            }
            ModalKind::SvnCommit(path) => {
                self.close_modal(window, cx);
                self.run_vcs_task(VcsTool::Svn, VcsOperation::Commit(value), path, window, cx);
            }
            ModalKind::GitUsername(request) => {
                self.close_modal(window, cx);
                self.begin_git_password(request, value, window, cx);
            }
            ModalKind::GitPassword { request, username } => {
                self.close_modal(window, cx);
                self.start_git_network(request.with_credentials(username, value), window, cx);
            }
        }
    }

    fn confirm_recycle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self
            .selected_entries(cx)
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        self.confirm_recycle_paths(paths, window, cx);
    }

    fn confirm_recycle_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_recycle_paths(vec![path], window, cx);
    }

    fn confirm_recycle_paths(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let has_shell_path = paths
            .iter()
            .any(|path| shell_fs::is_shell_namespace_path(path));
        if paths
            .iter()
            .any(|path| shell_fs::is_shell_namespace_path(path) && !shell_fs::can_delete_item(path))
        {
            self.error = None;
            self.notice = Some("此设备项目不支持删除".into());
            cx.notify();
            return;
        }
        let missing_local_path = paths
            .iter()
            .any(|path| !shell_fs::is_shell_namespace_path(path) && !path.exists());
        if missing_local_path {
            self.context_target_missing(window, cx);
            return;
        }
        let destination = path_after_recycle_paths(&self.current_dir, &paths);
        let message = if paths.len() == 1 {
            let path = &paths[0];
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_text(path));
            if has_shell_path {
                format!("删除“{name}”？设备项目不会进入回收站。")
            } else {
                format!("将“{name}”移到回收站？")
            }
        } else if has_shell_path {
            format!(
                "删除所选的 {} 个项目？设备项目不会进入回收站。",
                paths.len()
            )
        } else {
            format!("将所选的 {} 个项目移到回收站？", paths.len())
        };
        let detail = paths
            .iter()
            .take(5)
            .map(|path| path_text(path))
            .collect::<Vec<_>>()
            .join("\n");
        let action = if has_shell_path {
            "删除"
        } else {
            "移到回收站"
        };
        let answers = [PromptButton::ok(action), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let result = cx
                .background_spawn(async move {
                    for (completed, path) in paths.iter().enumerate() {
                        let result = if shell_fs::is_shell_namespace_path(path) {
                            shell_fs::delete_item(path)
                        } else {
                            recycle_entry(path)
                        };
                        if let Err(error) = result {
                            return Err((completed, error));
                        }
                    }
                    Ok(paths.len())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(item_count) => {
                    this.navigate_to(destination, NavigationIntent::Refresh, window, cx);
                    this.notice = Some(if has_shell_path {
                        format!("已删除 {item_count} 项").into()
                    } else {
                        format!("已将 {item_count} 项移到回收站").into()
                    });
                }
                Err((completed, error)) => {
                    if has_shell_path || completed > 0 {
                        this.error = None;
                        this.navigate_to(destination, NavigationIntent::Refresh, window, cx);
                        this.notice = Some(if completed > 0 {
                            format!("已处理 {completed} 项，后续操作失败：{error}").into()
                        } else {
                            format!("设备删除未完成：{error}").into()
                        });
                    } else {
                        this.notice = None;
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn clear_search_or_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.address_editing {
            self.address_editing = false;
            self.address_input.update(cx, |input, cx| {
                input.set_value(path_text(&self.current_dir), window, cx);
            });
            self.focus_table(window, cx);
            cx.notify();
        } else if self.modal.is_some() {
            self.close_modal(window, cx);
        } else if self.column_search_path.is_some() {
            self.close_column_search(window, cx);
        } else if self.active_search.is_some() {
            self.clear_search(window, cx);
        } else if self.search_visible {
            self.search_visible = false;
            self.search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.focus_table(window, cx);
            cx.notify();
        }
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let can_back = self.history_index > 0;
        let can_forward = self.history_index + 1 < self.history.len();
        let can_up = navigation_parent(&self.current_dir).is_some();
        let selected_count = self.selected_entries(cx).len();
        let has_selection = selected_count > 0;
        let can_modify_current = !self.current_dir.as_os_str().is_empty();
        let has_clipboard = self.clipboard.borrow().is_some() || file_clipboard::has_files();
        let current_favorite = self.current_dir_is_favorite();
        let git_available = self.current_git_available(cx);
        let git_head = self.git_head.clone();
        let loading = self.loading;
        let search_expanded = self.search_visible
            || self.active_search.is_some()
            || !self.search_input.read(cx).value().is_empty();
        let breadcrumbs =
            (!self.address_editing && self.show_path_bar).then(|| self.render_path_bar(cx));

        let navigation_group = h_flex()
            .h(px(TOOLBAR_CONTROL_HEIGHT))
            .gap_0p5()
            .px_1()
            .items_center()
            .rounded_md()
            .bg(cx.theme().background.opacity(0.42))
            .child(
                Button::new("nav-back")
                    .ghost()
                    .compact()
                    .icon(IconName::ChevronLeft)
                    .tooltip("返回 (Alt+←)")
                    .disabled(!can_back)
                    .on_click(cx.listener(Self::go_back)),
            )
            .child(
                Button::new("nav-forward")
                    .ghost()
                    .compact()
                    .icon(IconName::ChevronRight)
                    .tooltip("前进 (Alt+→)")
                    .disabled(!can_forward)
                    .on_click(cx.listener(Self::go_forward)),
            )
            .child(
                Button::new("nav-up")
                    .ghost()
                    .compact()
                    .icon(IconName::ArrowUp)
                    .tooltip("上一级 (Alt+↑)")
                    .disabled(!can_up)
                    .on_click(cx.listener(Self::go_up)),
            )
            .child(
                Button::new("refresh")
                    .ghost()
                    .compact()
                    .icon(IconName::RotateCw)
                    .tooltip("刷新 (Ctrl+R)")
                    .on_click(cx.listener(Self::refresh_clicked)),
            );

        let navigation_slot = h_flex()
            .w(px(SIDEBAR_WIDTH))
            .h(px(TOOLBAR_CONTROL_HEIGHT))
            .flex_shrink_0()
            .items_center()
            .child(navigation_group);

        let actions = h_flex()
            .h(px(TOOLBAR_CONTROL_HEIGHT))
            .gap_0p5()
            .px_1()
            .items_center()
            .rounded_md()
            .bg(cx.theme().background.opacity(0.42))
            .child(
                Button::new("favorite-current-directory")
                    .ghost()
                    .compact()
                    .icon(if current_favorite {
                        IconName::StarMinus
                    } else {
                        IconName::StarPlus
                    })
                    .tooltip(if current_favorite {
                        "取消收藏当前文件夹"
                    } else {
                        "收藏当前文件夹"
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_current_favorite(cx)
                    })),
            )
            .when_some(git_head, |this, head| {
                let branch = head.branch.clone().unwrap_or_else(|| {
                    head.short_hash
                        .map(|hash| format!("HEAD {hash}"))
                        .unwrap_or_else(|| "分离 HEAD".to_string())
                });
                let label = if head.ahead > 0 || head.behind > 0 {
                    format!("{branch}  ↑{} ↓{}", head.ahead, head.behind)
                } else {
                    branch
                };
                this.child(
                    Button::new("git-branches")
                        .ghost()
                        .compact()
                        .icon(IconName::GitBranch)
                        .label(label)
                        .tooltip("分支与标签")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_git_branches_window(window, cx)
                        })),
                )
            })
            .when(git_available, |this| {
                this.child(
                    Button::new("open-git-client")
                        .ghost()
                        .compact()
                        .icon(IconName::GitBranch)
                        .tooltip("打开 Git 客户端 (Ctrl+Shift+G)")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_git_client_for_current(&OpenGitClient, window, cx)
                        })),
                )
            })
            .child(
                Button::new("open-workspace-editor")
                    .ghost()
                    .compact()
                    .icon(IconName::FilePen)
                    .tooltip("打开工作区编辑器（目录树）")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_workspace_editor(window, cx)
                    })),
            )
            .child(
                Button::new("copy")
                    .ghost()
                    .compact()
                    .icon(IconName::Copy)
                    .tooltip(if selected_count > 1 {
                        "复制所选项目 (Ctrl+C)"
                    } else {
                        "复制 (Ctrl+C)"
                    })
                    .disabled(!has_selection)
                    .on_click(cx.listener(Self::copy_clicked)),
            )
            .child(
                Button::new("cut")
                    .ghost()
                    .compact()
                    .icon(IconName::Scissors)
                    .tooltip("剪切 (Ctrl+X)")
                    .disabled(!has_selection)
                    .on_click(cx.listener(Self::cut_clicked)),
            )
            .child(
                Button::new("paste")
                    .ghost()
                    .compact()
                    .icon(IconName::ClipboardPaste)
                    .tooltip("粘贴 (Ctrl+V)")
                    .disabled(!has_clipboard || !can_modify_current)
                    .on_click(cx.listener(Self::paste_clicked)),
            )
            .child(
                Button::new("new-folder")
                    .ghost()
                    .compact()
                    .icon(IconName::FolderPlus)
                    .tooltip("新建文件夹 (Ctrl+Shift+N)")
                    .disabled(loading || !can_modify_current)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_new_folder(window, cx)
                    })),
            )
            .when(!search_expanded, |this| {
                this.child(
                    Button::new("show-search")
                        .ghost()
                        .compact()
                        .icon(IconName::Search)
                        .tooltip("搜索当前文件夹 (Ctrl+F)")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.focus_search(window, cx)
                        })),
                )
            });

        let address_control = if let Some(breadcrumbs) = breadcrumbs {
            h_flex()
                .flex_1()
                .min_w(px(180.))
                .h(px(TOOLBAR_CONTROL_HEIGHT))
                .gap_1()
                .px_1()
                .items_center()
                .rounded_md()
                .bg(cx.theme().background.opacity(0.42))
                .child(
                    Icon::new(IconName::Folder)
                        .size_4()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().flex_1().min_w_0().child(breadcrumbs))
                .child(
                    Button::new("edit-address")
                        .ghost()
                        .compact()
                        .icon(IconName::Pencil)
                        .tooltip("编辑路径 (Ctrl+L)")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.focus_address(window, cx)
                        })),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w(px(180.))
                .child(
                    Input::new(&self.address_input)
                        .aria_label("地址")
                        .prefix(Icon::new(IconName::Folder).size_4()),
                )
                .into_any_element()
        };

        let search_input = search_expanded.then(|| {
            Input::new(&self.search_input)
                .aria_label("搜索")
                .prefix(Icon::new(IconName::Search).size_4())
                .cleanable(true)
                .w(px(220.))
                .into_any_element()
        });

        let toolbar = v_flex()
            .w_full()
            .flex_shrink_0()
            .gap_1()
            .py(px(TOOLBAR_VERTICAL_PADDING))
            .rounded_lg()
            .bg(cx.theme().title_bar)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(TOOLBAR_CONTROL_HEIGHT))
                    .w_full()
                    .gap(px(ISLAND_GAP))
                    .items_center()
                    .child(navigation_slot)
                    .child(address_control)
                    .when_some(search_input, |this, input| this.child(input))
                    .child(actions),
            );
        toolbar.into_any_element()
    }

    fn current_git_available(&self, cx: &App) -> bool {
        if self.view_mode == ViewMode::Columns && self.active_search.is_none() {
            self.column_panes
                .last()
                .is_some_and(|pane| pane.git_available)
        } else {
            self.table.read(cx).delegate().git_available()
        }
    }

    fn open_git_client_for_current(
        &mut self,
        _: &OpenGitClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .selected_entries(cx)
            .first()
            .map(|entry| entry.path.clone())
            .unwrap_or_else(|| self.current_dir.clone());
        self.open_git_client_at_path(path, window, cx);
    }

    fn open_git_client_at_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notice = Some("正在查找 Git 仓库…".into());
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { git_repository_root(&path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Some(root)) => this.open_git_client_root(root, window, cx),
                    Ok(None) => {
                        this.notice = None;
                        this.error = Some("当前项目不在 Git 仓库中".into());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_git_client_root(
        &mut self,
        repository_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match git_client_window::open_git_client_window(repository_root, window, cx) {
            Ok(()) => {
                self.error = None;
                self.notice = Some("Git 客户端已打开".into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    fn open_git_branches_window(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(head) = self.git_head.as_ref() else {
            self.notice = None;
            self.error = Some("当前文件夹不在 Git 工作区中".into());
            cx.notify();
            return;
        };
        git_branches_window::open_git_branches_window(
            cx.entity(),
            head.repository_root.clone(),
            cx,
        );
        self.error = None;
        cx.notify();
    }

    pub(crate) fn request_git_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_git_refresh = true;
        self.git_head = None;
        cx.notify();
    }

    pub(crate) fn open_git_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.pending_git_open = Some(path);
        cx.notify();
    }

    fn open_global_search(
        &mut self,
        _: &OpenGlobalSearch,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let favorite_roots = self
            .favorites
            .iter()
            .filter(|favorite| favorite.is_directory)
            .map(|favorite| PathBuf::from(&favorite.path))
            .collect::<Vec<_>>();
        let volumes = crate::devices::snapshot().volumes;
        let local_roots = volumes
            .iter()
            .filter(|volume| {
                matches!(
                    volume.kind,
                    VolumeKind::Fixed | VolumeKind::Usb | VolumeKind::Removable
                )
            })
            .map(|volume| volume.path.clone())
            .collect::<Vec<_>>();
        let mut network_roots = volumes
            .iter()
            .filter(|volume| matches!(volume.kind, VolumeKind::Network))
            .map(|volume| volume.path.clone())
            .collect::<Vec<_>>();
        network_roots.extend(
            favorite_roots
                .iter()
                .filter(|path| crate::devices::is_network_path(path))
                .cloned(),
        );
        if network_roots.is_empty() && crate::devices::is_network_path(&self.current_dir) {
            network_roots.push(self.current_dir.clone());
        }
        network_roots.sort();
        network_roots.dedup();
        match global_search_window::open_global_search_window(
            self.current_dir.clone(),
            favorite_roots,
            local_roots,
            network_roots,
            cx,
        ) {
            Ok(()) => {
                self.error = None;
                self.notice = Some("已打开全局搜索窗口".into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    fn open_settings_window(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let initial_update_state = self.settings_update_state();
        match settings_window::open_settings_window(cx.entity(), initial_update_state, cx) {
            Ok(()) => {
                self.error = None;
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    pub(crate) fn apply_ui_preferences(
        &mut self,
        preferences: UiPreferences,
        cx: &mut Context<Self>,
    ) {
        let git_auto_fetch = preferences.git_auto_fetch;
        let git_auto_fetch_minutes = preferences.git_auto_fetch_minutes;
        self.show_sidebar = preferences.sidebar_visible;
        self.show_preview = preferences.preview_visible;
        self.show_column_details = preferences.column_details_visible;
        self.show_date_column = preferences.date_column_visible;
        self.show_kind_column = preferences.kind_column_visible;
        self.show_size_column = preferences.size_column_visible;
        self.preferred_column_width = preferences.column_width;
        self.start_git_auto_fetch(git_auto_fetch, git_auto_fetch_minutes, cx);
        cx.notify();
    }

    pub(crate) fn settings_update_state(&self) -> SettingsUpdateState {
        let available = self.available_update.as_ref();
        SettingsUpdateState {
            in_progress: self.update_in_progress,
            status: self.error.clone().or_else(|| self.notice.clone()),
            status_is_error: self.error.is_some(),
            available_version: available.map(|update| update.info.latest_version.clone().into()),
            current_version: available.map(|update| update.info.current_version.clone().into()),
            notes: available.and_then(|update| update_notes_preview(&update.notes)),
            download_progress: self.update_download_progress,
        }
    }

    pub(crate) fn check_for_updates_from_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.check_for_updates(&CheckForUpdates, window, cx);
    }

    pub(crate) fn start_update_download_from_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_update_download(window, cx);
    }

    pub(crate) fn open_update_release_notes_from_settings(&mut self, cx: &mut Context<Self>) {
        self.open_update_release_notes(cx);
    }

    pub(crate) fn set_default_explorer_from_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_default_explorer(&SetDefaultExplorer, window, cx);
    }

    pub(crate) fn restore_default_explorer_from_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_default_explorer(&RestoreDefaultExplorer, window, cx);
    }

    pub(crate) fn create_start_menu_shortcut_from_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_start_menu_shortcut(&CreateStartMenuShortcut, window, cx);
    }

    fn toggle_theme(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        let appearance = if mode.is_dark() {
            AppearancePreference::Dark
        } else {
            AppearancePreference::Light
        };
        let mut preferences = ui_preferences::load_ui_preferences();
        preferences.appearance = appearance;
        if let Err(error) = ui_preferences::save_ui_preferences(preferences) {
            self.notice = None;
            self.error = Some(error.into());
        }
        Theme::change(mode, Some(window), cx);
        cx.notify();
    }
    fn sidebar_item(&self, item: SidebarItem, cx: &mut Context<Self>) -> AnyElement {
        let SidebarItem {
            id,
            label,
            path,
            icon,
            detail,
            storage,
            active,
            is_directory,
            favorite,
        } = item;
        let accent = cx.theme().accent;
        let active_fg = cx.theme().accent_foreground;
        let fg = cx.theme().sidebar_foreground;
        let muted = cx.theme().muted_foreground;
        let row_id = id.clone();
        let navigation_path = path.clone();
        let context = self.focus_handle.clone();
        let open_action = ContextMenuAction {
            target: path.clone(),
            working_directory: path.clone(),
            command: ContextMenuCommand::Open,
        };
        let copy_path_action = ContextMenuAction {
            target: path.clone(),
            working_directory: path.clone(),
            command: ContextMenuCommand::CopyPath,
        };
        let properties_action = ContextMenuAction {
            target: path.clone(),
            working_directory: path.clone(),
            command: ContextMenuCommand::ShowProperties,
        };
        let remove_action = favorite.then(|| ContextMenuAction {
            target: path.clone(),
            working_directory: path.clone(),
            command: ContextMenuCommand::RemoveFavorite,
        });
        let storage_row = storage.is_some();

        let main_content: AnyElement = if let Some(storage) = storage {
            let free_percent = storage.free_percent();
            let pressure = storage.pressure();
            let progress_color = match pressure {
                StoragePressure::Critical => cx.theme().danger,
                StoragePressure::Low => cx.theme().warning,
                StoragePressure::Normal if active => active_fg,
                StoragePressure::Normal => cx.theme().primary,
            };
            let detail_color = match pressure {
                StoragePressure::Normal if active => active_fg.opacity(0.72),
                StoragePressure::Normal => muted,
                StoragePressure::Low | StoragePressure::Critical if active => active_fg,
                StoragePressure::Low | StoragePressure::Critical => progress_color,
            };
            let warning_foreground = if active { active_fg } else { progress_color };
            let badge_background = if active {
                active_fg.opacity(0.18)
            } else {
                muted.opacity(0.12)
            };
            let badge_foreground = if active { active_fg } else { fg };
            let progress_id = format!("{row_id}-capacity");
            let accessibility_label = free_percent
                .map(|percent| format!("{label} 可用空间 {percent:.0}%"))
                .unwrap_or_else(|| format!("{label} 容量未知"));

            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .when_some(storage.drive_letter, |this, drive_letter| {
                            this.child(
                                div()
                                    .h(px(18.))
                                    .flex_shrink_0()
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .bg(badge_background)
                                    .text_xs()
                                    .font_medium()
                                    .text_color(badge_foreground)
                                    .child(drive_letter),
                            )
                        })
                        .child(div().flex_1().min_w_0().truncate().child(label)),
                )
                .when_some(detail, |this, detail| {
                    this.child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_1()
                            .when(pressure != StoragePressure::Normal, |this| {
                                this.child(
                                    Icon::new(IconName::TriangleAlert)
                                        .size_3()
                                        .text_color(warning_foreground),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(detail_color)
                                    .child(detail),
                            ),
                    )
                })
                .when_some(free_percent, |this, percent| {
                    this.child(
                        Progress::new(progress_id)
                            .value(percent)
                            .color(progress_color)
                            .with_size(Size::XSmall)
                            .accessibility_label(accessibility_label),
                    )
                })
                .into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(div().flex_1().min_w_0().truncate().child(label))
                .when_some(detail, |this, detail| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(if active { active_fg } else { muted })
                            .child(detail),
                    )
                })
                .into_any_element()
        };

        let row = div()
            .id(row_id)
            .h(px(if storage_row {
                STORAGE_SIDEBAR_ROW_HEIGHT
            } else {
                30.
            }))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .when(storage_row, |this| this.py_1().gap_2())
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .text_color(if active { active_fg } else { fg })
            .when(active, |this| this.bg(accent))
            .when(!active, |this| {
                this.hover(move |style| style.bg(muted.opacity(0.12)))
            })
            .child(
                Icon::new(icon)
                    .size_4()
                    .text_color(if active { active_fg } else { muted }),
            )
            .child(main_content)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if is_directory {
                    this.navigate_to(navigation_path.clone(), NavigationIntent::Push, window, cx);
                } else {
                    this.open_path(navigation_path.clone(), window, cx);
                }
            }))
            .context_menu(move |mut menu, window, cx| {
                context.focus(window, cx);
                menu = menu.menu_with_icon(
                    "打开",
                    IconName::FolderOpen,
                    Box::new(open_action.clone()),
                );
                menu = menu.menu_with_icon(
                    "复制完整路径",
                    IconName::Link,
                    Box::new(copy_path_action.clone()),
                );
                menu = menu.separator();
                menu = menu.menu_with_icon(
                    "属性…",
                    IconName::Info,
                    Box::new(properties_action.clone()),
                );
                if let Some(remove_action) = remove_action.as_ref() {
                    menu = menu.separator();
                    menu = menu.menu_with_icon(
                        "从收藏移除",
                        IconName::StarMinus,
                        Box::new(remove_action.clone()),
                    );
                }
                menu
            });

        v_flex().w_full().gap_0p5().child(row).into_any_element()
    }

    fn favorite_item(
        &self,
        favorite: FavoriteEntryDto,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = PathBuf::from(&favorite.path);
        self.sidebar_item(
            SidebarItem {
                id: SharedString::from(format!("favorite-{}", favorite.path)),
                label: favorite.name.into(),
                path,
                icon: if favorite.is_directory {
                    IconName::Folder
                } else {
                    IconName::File
                },
                detail: None,
                storage: None,
                active,
                is_directory: favorite.is_directory,
                favorite: true,
            },
            cx,
        )
    }

    fn portable_device_item(&self, device: PortableDevice, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let hover_color = muted.opacity(0.12);
        let clicked = device.clone();
        let path = PathBuf::from(&device.shell_path);
        let open_action = ContextMenuAction {
            target: path.clone(),
            working_directory: path.clone(),
            command: ContextMenuCommand::Open,
        };
        let properties_action = ContextMenuAction {
            target: path,
            working_directory: PathBuf::from(&device.shell_path),
            command: ContextMenuCommand::ShowProperties,
        };
        let context = self.focus_handle.clone();
        let active = same_path(&self.current_dir, Path::new(&device.shell_path));
        div()
            .id(SharedString::from(format!(
                "portable-device-{}",
                device.shell_path
            )))
            .h(px(32.))
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .rounded_md()
            .cursor_pointer()
            .hover(move |style| style.bg(hover_color))
            .text_sm()
            .text_color(if active {
                cx.theme().accent_foreground
            } else {
                cx.theme().sidebar_foreground
            })
            .when(active, |this| this.bg(cx.theme().accent))
            .child(Icon::new(IconName::Usb).size_4().text_color(if active {
                cx.theme().accent_foreground
            } else {
                muted
            }))
            .child(div().flex_1().min_w_0().truncate().child(device.name))
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child("MTP"),
            )
            .child(Icon::new(IconName::ChevronRight).size_3())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_portable_device(clicked.clone(), window, cx);
            }))
            .context_menu(move |mut menu, window, cx| {
                context.focus(window, cx);
                menu = menu.menu_with_icon(
                    "打开",
                    IconName::FolderOpen,
                    Box::new(open_action.clone()),
                );
                menu = menu.separator();
                menu = menu.menu_with_icon(
                    "属性…",
                    IconName::Info,
                    Box::new(properties_action.clone()),
                );
                menu
            })
            .into_any_element()
    }

    fn open_portable_device(
        &mut self,
        device: PortableDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_to(
            PathBuf::from(device.shell_path),
            NavigationIntent::Push,
            window,
            cx,
        );
    }
    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let section_color = cx.theme().muted_foreground;
        let mut content = v_flex().gap_0p5();
        content = content.child(
            div()
                .px(px(COMPACT_ISLAND_PADDING))
                .pt_1()
                .pb_1()
                .text_xs()
                .font_medium()
                .text_color(section_color)
                .child("快速访问"),
        );
        for link in self.quick_links.clone() {
            let active = same_path(&link.path, &self.current_dir);
            content = content.child(self.sidebar_item(
                SidebarItem {
                    id: SharedString::from(format!("quick-{}", link.path.display())),
                    label: link.label,
                    path: link.path,
                    icon: link.icon,
                    detail: link.detail,
                    storage: link.storage,
                    active,
                    is_directory: true,
                    favorite: false,
                },
                cx,
            ));
        }
        content = content.child(
            div()
                .px(px(COMPACT_ISLAND_PADDING))
                .pt_3()
                .pb_1()
                .text_xs()
                .font_medium()
                .text_color(section_color)
                .child("收藏"),
        );
        if self.favorites.is_empty() {
            content = content.child(
                div()
                    .px(px(COMPACT_ISLAND_PADDING))
                    .pb_2()
                    .text_xs()
                    .text_color(section_color)
                    .child("点击工具栏星标或右键菜单添加"),
            );
        } else {
            for favorite in self.favorites.clone() {
                let active = favorite.is_directory
                    && same_path(Path::new(&favorite.path), &self.current_dir);
                content = content.child(self.favorite_item(favorite, active, cx));
            }
        }
        if !self.connected_devices.is_empty() || !self.portable_devices.is_empty() {
            content = content.child(
                div()
                    .px(px(COMPACT_ISLAND_PADDING))
                    .pt_3()
                    .pb_1()
                    .text_xs()
                    .font_medium()
                    .text_color(section_color)
                    .child("设备"),
            );
            for device in self.connected_devices.clone() {
                let active = same_path(&device.path, &self.current_dir);
                content = content.child(self.sidebar_item(
                    SidebarItem {
                        id: SharedString::from(format!("device-{}", device.path.display())),
                        label: device.label,
                        path: device.path,
                        icon: device.icon,
                        detail: device.detail,
                        storage: device.storage,
                        active,
                        is_directory: true,
                        favorite: false,
                    },
                    cx,
                ));
            }
            for device in self.portable_devices.clone() {
                content = content.child(self.portable_device_item(device, cx));
            }
        }
        content = content.child(
            div()
                .px(px(COMPACT_ISLAND_PADDING))
                .pt_3()
                .pb_1()
                .text_xs()
                .font_medium()
                .text_color(section_color)
                .child("此电脑"),
        );
        for drive in self.drives.clone() {
            let active = same_path(&drive.path, &self.current_dir);
            content = content.child(self.sidebar_item(
                SidebarItem {
                    id: SharedString::from(format!("drive-{}", drive.path.display())),
                    label: drive.label,
                    path: drive.path,
                    icon: drive.icon,
                    detail: drive.detail,
                    storage: drive.storage,
                    active,
                    is_directory: true,
                    favorite: false,
                },
                cx,
            ));
        }

        let settings = Button::new("sidebar-settings")
            .ghost()
            .compact()
            .label("设置")
            .tooltip("Kite 设置")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_settings_window(window, cx)
            }));
        let theme_icon = if cx.theme().mode.is_dark() {
            IconName::Sun
        } else {
            IconName::Moon
        };
        let theme_tooltip = if cx.theme().mode.is_dark() {
            "切换到浅色主题"
        } else {
            "切换到深色主题"
        };

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .gap_1()
            .p(px(COMPACT_ISLAND_PADDING))
            .rounded_lg()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(content),
            )
            .child(
                h_flex()
                    .h(px(34.))
                    .w_full()
                    .px_1()
                    .gap_1()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().child(settings))
                    .child(
                        Button::new("sidebar-theme")
                            .ghost()
                            .compact()
                            .icon(theme_icon)
                            .tooltip(theme_tooltip)
                            .on_click(cx.listener(Self::toggle_theme)),
                    ),
            )
            .into_any_element()
    }
    fn render_path_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let segments = path_segments(&self.current_dir, false);
        let scroll_handle = self.path_bar_scroll.clone();
        let mut bar = h_flex()
            .id("path-bar")
            .h(px(30.))
            .w_full()
            .gap_1()
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&scroll_handle);
        let count = segments.len();
        for (index, segment) in segments.into_iter().enumerate() {
            if segment.clickable {
                bar = bar.child(
                    Button::new(format!("path-segment-{index}"))
                        .ghost()
                        .compact()
                        .label(segment.label)
                        .tooltip(path_text(&segment.path))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate_from_path_bar(segment.path.clone(), window, cx);
                        })),
                );
            } else {
                bar = bar.child(div().px_1().text_sm().font_medium().child(segment.label));
            }
            if index + 1 < count {
                bar = bar.child(
                    Icon::new(IconName::ChevronRight)
                        .size_3()
                        .text_color(cx.theme().muted_foreground),
                );
            }
        }
        bar.into_any_element()
    }

    fn render_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let scroll_handle = self.column_scroll.clone();
        let mut strip = h_flex()
            .id("column-strip")
            .size_full()
            .items_stretch()
            .gap(px(COLUMN_STRIP_GAP))
            .p(px(COLUMN_STRIP_PADDING))
            .bg(cx.theme().background)
            .overflow_hidden()
            .track_scroll(&scroll_handle);
        if self.column_panes.is_empty() {
            if let Some(error) = self.error.clone() {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .px_8()
                    .child(
                        div()
                            .max_w(px(560.))
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error),
                    )
                    .into_any_element();
            }
            return strip
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if self.loading {
                    "正在读取分栏…"
                } else {
                    "文件夹不可用"
                })
                .into_any_element();
        }
        self.sync_column_list_scrolls();
        for index in 0..self.column_panes.len() {
            strip = strip.child(self.render_column(index, window, cx));
        }
        if self.show_preview
            && let Some(entry) = self.selected_last_column_file()
        {
            strip = strip.child(self.render_column_preview(&entry, cx));
        }
        let horizontal_wheel_handle = scroll_handle.clone();
        div()
            .relative()
            .size_full()
            .child(strip.into_any_element())
            .child(
                ScrollableMask::new(Axis::Horizontal, &horizontal_wheel_handle)
                    .id("column-horizontal-wheel"),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::horizontal(&scroll_handle)
                        .id("column-horizontal-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            )
            .into_any_element()
    }

    fn selected_last_column_file(&self) -> Option<FileEntry> {
        let pane = self.column_panes.last()?;
        let selected = pane.selected_path.as_ref()?;
        pane.entries
            .iter()
            .find(|entry| same_path(&entry.path, selected))
            .filter(|entry| !entry.is_dir())
            .cloned()
    }

    fn render_column_preview(&mut self, entry: &FileEntry, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let copy_text = preview_info_text(entry);
        let preview_more_items = preview_more_menu_items();
        let preview_more_target = entry.path.clone();
        let preview_more_working_directory = self.current_dir.clone();
        let preview_window_entry = entry.clone();
        let preview_more_supports_local_ops = !shell_fs::is_shell_namespace_path(&entry.path);
        let editor_preview = is_editor_preview_entry(entry);
        let open_label = if editor_preview && text_editor::is_markdown_path(&entry.path) {
            "在窗口中编辑和预览 Markdown"
        } else if editor_preview {
            "在文本编辑器中打开"
        } else {
            "在窗口中预览"
        };
        let preview = self
            .column_preview
            .as_ref()
            .filter(|preview| same_path(&preview.path, &entry.path))
            .cloned();
        let preview_area = if is_image_preview_entry(entry) {
            let raw_image = preview.as_ref().and_then(|preview| preview.image.clone());
            let image_error = preview.as_ref().and_then(|preview| preview.error.clone());
            let image_loading = preview.as_ref().is_some_and(|preview| preview.loading);
            let image_element = if let Some(image) = raw_image {
                img(image)
                    .id(SharedString::from(format!(
                        "column-preview-image-{}",
                        entry.path.display()
                    )))
                    .max_w_full()
                    .max_h(px(520.))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            } else if let Some(error) = image_error {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .px_3()
                    .child(Icon::new(icon_for(entry)).size(px(48.)).text_color(muted))
                    .child(div().text_xs().text_color(cx.theme().danger).child(error))
                    .into_any_element()
            } else if image_loading {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Icon::new(icon_for(entry)).size(px(48.)).text_color(muted))
                    .child(div().text_xs().text_color(muted).child("正在生成预览…"))
                    .into_any_element()
            } else {
                let fallback_entry = entry.clone();
                img(entry.path.clone())
                    .id(SharedString::from(format!(
                        "column-preview-source-image-{}",
                        entry.path.display()
                    )))
                    .max_w_full()
                    .max_h(px(520.))
                    .object_fit(ObjectFit::Contain)
                    .with_fallback(move || {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(
                                Icon::new(icon_for(&fallback_entry))
                                    .size(px(48.))
                                    .text_color(muted),
                            )
                            .child(div().text_xs().text_color(muted).child("无法加载图片预览"))
                            .into_any_element()
                    })
                    .into_any_element()
            };
            h_flex()
                .h(px(440.))
                .w_full()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.25))
                .child(image_element)
                .into_any_element()
        } else if is_text_preview_entry(entry) {
            let foreground = cx.theme().foreground;
            let mut scrollbar: Option<PreviewScrollbar> = None;
            let mut content = v_flex().size_full().p_2();
            content = if let Some(preview) = preview.as_ref() {
                if preview.loading {
                    content.child(div().text_xs().text_color(muted).child("正在加载预览…"))
                } else if let Some(error) = preview.error.clone() {
                    content.child(div().text_xs().text_color(cx.theme().danger).child(
                        SelectableText::new(
                            format!("column-preview-error-{}", entry.path.display()),
                            error,
                        ),
                    ))
                } else if let Some(markdown_view) = preview.markdown_view.clone() {
                    let scroll = preview.markdown_scroll.clone();
                    let scroll_for_wheel = scroll.clone();
                    scrollbar = Some(PreviewScrollbar::Markdown(scroll.clone()));
                    content
                        .text_color(foreground)
                        .on_scroll_wheel(cx.listener(
                            move |_this, event: &ScrollWheelEvent, window, cx| {
                                let delta = event.delta.pixel_delta(window.line_height());
                                let delta_x = delta.x.as_f32();
                                let delta_y = delta.y.as_f32();
                                if delta_y == 0.0 {
                                    return;
                                }
                                if wheel_scrolls_columns(delta_x, delta_y) {
                                    let current = scroll_for_wheel.offset();
                                    let max_y = scroll_for_wheel.max_offset().y.as_f32().max(0.0);
                                    let applied_y =
                                        if delta.y.is_zero() { delta_x } else { delta_y };
                                    let restored_y =
                                        (current.y.as_f32() - applied_y).clamp(-max_y, 0.0);
                                    scroll_for_wheel.set_offset(point(current.x, px(restored_y)));
                                    cx.notify();
                                    return;
                                }
                                cx.stop_propagation();
                            },
                        ))
                        .child(
                            div()
                                .id("column-preview-markdown-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                .child(
                                    TextView::new(&markdown_view)
                                        .w_full()
                                        .text_sm()
                                        .text_color(foreground)
                                        .selectable(true)
                                        .scrollable(false),
                                ),
                        )
                } else if let Some(selection) = preview.selection.clone() {
                    let lines = preview.lines.clone();
                    let line_count = lines.len();
                    let scroll = preview.scroll.clone();
                    let scroll_for_wheel = scroll.clone();
                    scrollbar = Some(PreviewScrollbar::Lines(scroll.clone()));
                    let list_id = SharedString::from(format!(
                        "column-preview-lines-{}",
                        entry.path.display()
                    ));
                    content
                        .on_scroll_wheel(cx.listener(
                            move |_this, event: &ScrollWheelEvent, window, cx| {
                                let delta = event.delta.pixel_delta(window.line_height());
                                let delta_x = delta.x.as_f32();
                                let delta_y = delta.y.as_f32();
                                if delta_y == 0.0 {
                                    return;
                                }
                                if wheel_scrolls_columns(delta_x, delta_y) {
                                    let base = scroll_for_wheel.0.borrow().base_handle.clone();
                                    let current = base.offset();
                                    let max_y = base.max_offset().y.as_f32().max(0.0);
                                    let applied_y =
                                        if delta.y.is_zero() { delta_x } else { delta_y };
                                    let restored_y =
                                        (current.y.as_f32() - applied_y).clamp(-max_y, 0.0);
                                    base.set_offset(point(current.x, px(restored_y)));
                                    cx.notify();
                                    return;
                                }
                                cx.stop_propagation();
                            },
                        ))
                        .child(
                            uniform_list(
                                list_id,
                                line_count,
                                cx.processor(move |_this, range: Range<usize>, _window, _cx| {
                                    range
                                        .map(|row_ix| {
                                            let line =
                                                lines.get(row_ix).cloned().unwrap_or_default();
                                            h_flex()
                                                .id(("column-preview-line", row_ix))
                                                .h(px(20.))
                                                .w_full()
                                                .min_w_0()
                                                .items_center()
                                                .overflow_hidden()
                                                .px_1()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(foreground)
                                                        .whitespace_nowrap()
                                                        .child(
                                                            SelectableText::with_handle(
                                                                (
                                                                    "column-preview-line-text",
                                                                    row_ix,
                                                                ),
                                                                selection.clone(),
                                                                line,
                                                            )
                                                            .document_order(row_ix as u64),
                                                        ),
                                                )
                                                .into_any_element()
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .track_scroll(&scroll),
                        )
                } else {
                    content.child(div().text_xs().text_color(muted).child("无文本预览"))
                }
            } else {
                content.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("选择文件后加载预览"),
                )
            };

            let mut panel = v_flex()
                .id("column-preview-text")
                .h(px(440.))
                .w_full()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.25))
                .relative()
                .overflow_hidden()
                .child(content);
            if let Some(scroll) = scrollbar {
                let scrollbar = match scroll {
                    PreviewScrollbar::Lines(handle) => preview_scrollbar_element(&handle, cx),
                    PreviewScrollbar::Markdown(handle) => preview_scrollbar_element(&handle, cx),
                };
                panel = panel.child(div().absolute().inset_0().child(scrollbar));
            }
            panel.into_any_element()
        } else {
            v_flex()
                .h(px(440.))
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.25))
                .child(
                    Icon::new(icon_for(entry))
                        .size(px(56.))
                        .text_color(icon_color(entry, cx)),
                )
                .child(div().text_xs().text_color(muted).child("无内置预览"))
                .into_any_element()
        };

        let mut properties = v_flex().w_full().gap_2();
        properties = properties
            .child(preview_property("名称", entry.name.clone(), cx))
            .child(preview_property("类型", entry.type_label(), cx))
            .child(preview_property("大小", entry.size_label(), cx))
            .child(preview_property("修改日期", entry.modified_label(), cx))
            .child(preview_property("位置", path_text(&entry.path), cx));
        let mut vcs = Vec::new();
        if let Some(status) = entry.git_status {
            vcs.push(format!("Git {} ({})", status.label(), status.code()));
        }
        if let Some(status) = entry.svn_status {
            vcs.push(format!("SVN {} ({})", status.label(), status.code()));
        }
        if !vcs.is_empty() {
            properties = properties.child(preview_property("版本控制", vcs.join("\n"), cx));
        }
        let git_section = self
            .column_git_preview
            .as_ref()
            .filter(|preview| same_path(&preview.path, &entry.path))
            .cloned()
            .map(|preview| self.render_column_git_section(preview, cx));

        v_flex()
            .w(px(PREVIEW_WIDTH))
            .h_full()
            .flex_shrink_0()
            .rounded_lg()
            .overflow_hidden()
            .bg(cx.theme().title_bar)
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(COLUMN_HEADER_HEIGHT))
                    .w_full()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(muted)
                            .child("预览"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(format!("preview-enlarge-{}", entry.path.display()))
                            .ghost()
                            .compact()
                            .icon(if editor_preview {
                                IconName::FilePen
                            } else {
                                IconName::Maximize2
                            })
                            .tooltip(open_label)
                            .accessibility_label(open_label)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_preview_window(preview_window_entry.clone(), window, cx);
                            })),
                    )
                    .child(
                        Clipboard::new(format!("copy-preview-info-{}", entry.path.display()))
                            .value(copy_text)
                            .tooltip("复制预览信息")
                            .accessibility_label("复制预览信息"),
                    )
                    .child(
                        Button::new(format!("preview-more-{}", entry.path.display()))
                            .ghost()
                            .compact()
                            .icon(IconName::Ellipsis)
                            .tooltip("更多预览选项")
                            .accessibility_label("更多预览选项")
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                                for (index, (label, icon, command)) in
                                    preview_more_items.iter().enumerate()
                                {
                                    if matches!(index, 3 | 4) {
                                        menu = menu.separator();
                                    }
                                    menu = menu.menu_with_icon_and_disabled(
                                        *label,
                                        *icon,
                                        Box::new(ContextMenuAction {
                                            target: preview_more_target.clone(),
                                            working_directory: preview_more_working_directory
                                                .clone(),
                                            command: *command,
                                        }),
                                        !preview_more_supports_local_ops
                                            && *command == ContextMenuCommand::Copy,
                                    );
                                }
                                menu
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("column-preview")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .p(px(COMPACT_ISLAND_PADDING))
                    .gap_2()
                    .child(preview_area)
                    .when_some(git_section, |this, section| this.child(section))
                    .child(properties),
            )
            .into_any_element()
    }

    fn render_column_git_section(
        &self,
        preview: ColumnGitPreview,
        cx: &Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut section = v_flex()
            .id("column-preview-git")
            .w_full()
            .gap_1p5()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(IconName::GitBranch).size_3p5().text_color(muted))
                    .child(div().text_xs().font_medium().text_color(muted).child("Git")),
            );
        let Some(summary) = preview.summary.as_ref() else {
            let message = if preview.loading {
                "正在读取 Git 信息…"
            } else {
                "无法读取 Git 信息"
            };
            return section
                .child(div().text_xs().text_color(muted).child(message))
                .into_any_element();
        };

        let mut branch_line = summary
            .branch
            .clone()
            .or_else(|| {
                summary
                    .head_short_hash
                    .as_ref()
                    .map(|hash| format!("分离 HEAD {hash}"))
            })
            .unwrap_or_else(|| "无分支".to_string());
        if summary.ahead > 0 || summary.behind > 0 {
            branch_line.push_str(&format!("  ↑{} ↓{}", summary.ahead, summary.behind));
        }
        section = section.child(div().text_sm().truncate().child(branch_line));
        if let Some(operation) = summary.operation {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(operation.label()),
            );
        }

        if summary.is_directory {
            let counts = &summary.changes;
            let text = if counts.total() == 0 {
                "没有未提交的变更".to_string()
            } else {
                git_change_counts_text(counts)
            };
            section = section.child(div().text_xs().text_color(muted).child(text));
        } else {
            let mut chips = h_flex().gap_2().flex_wrap();
            for (label, status) in [
                ("已暂存", summary.staged_status),
                ("工作区", summary.unstaged_status),
            ] {
                if let Some(status) = status {
                    chips = chips.child(
                        div()
                            .text_xs()
                            .text_color(vcs_status_color(status))
                            .child(format!("{label} {}", status.label())),
                    );
                }
            }
            if summary.ignored {
                chips = chips.child(div().text_xs().text_color(muted).child("已忽略"));
            } else if summary.staged_status.is_none() && summary.unstaged_status.is_none() {
                chips = chips.child(div().text_xs().text_color(muted).child("无变更"));
            }
            if summary.lines_added > 0 || summary.lines_removed > 0 {
                chips = chips
                    .child(
                        div()
                            .text_xs()
                            .text_color(vcs_status_color(VcsStatus::Added))
                            .child(format!("+{}", summary.lines_added)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(vcs_status_color(VcsStatus::Deleted))
                            .child(format!("-{}", summary.lines_removed)),
                    );
            }
            section = section.child(chips);
        }

        if let Some(commit) = summary.last_commit.as_ref() {
            section = section.child(
                v_flex()
                    .gap_0p5()
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{}  {}  {}",
                        commit.short_hash,
                        commit.author,
                        git_commit_time_label(commit.unix_seconds)
                    )))
                    .child(div().text_xs().truncate().child(commit.subject.clone())),
            );
        }

        let repository_root = summary.repository_root.clone();
        section = section.child(
            Button::new("column-preview-git-client")
                .ghost()
                .compact()
                .label("Git 客户端")
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_git_client_root(repository_root.clone(), window, cx);
                })),
        );
        if !summary.is_directory && !summary.ignored {
            let changed = summary.staged_status.is_some() || summary.unstaged_status.is_some();
            let path = preview.path.clone();
            let mut actions = h_flex().gap_1().flex_wrap();
            if changed {
                let viewer_path = path.clone();
                let viewer_old = DiffEndpoint::Revision("HEAD".to_string());
                let viewer_new =
                    if summary.staged_status.is_some() && summary.unstaged_status.is_none() {
                        DiffEndpoint::Index
                    } else {
                        DiffEndpoint::WorkingTree
                    };
                actions = actions.child(
                    Button::new("column-preview-git-diff")
                        .ghost()
                        .compact()
                        .label("Diff 查看器")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            match diff_viewer_window::open_diff_viewer_window(
                                viewer_path.clone(),
                                viewer_old.clone(),
                                viewer_new.clone(),
                                cx,
                            ) {
                                Ok(()) => this.error = None,
                                Err(error) => this.error = Some(error.into()),
                            }
                            cx.notify();
                        })),
                );
            }
            if summary.unstaged_status.is_some() {
                let path = path.clone();
                actions = actions.child(
                    Button::new("column-preview-git-stage")
                        .ghost()
                        .compact()
                        .label("暂存")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_vcs_task(
                                VcsTool::Git,
                                VcsOperation::Add,
                                path.clone(),
                                window,
                                cx,
                            );
                        })),
                );
            }
            if summary.staged_status.is_some() {
                let path = path.clone();
                actions = actions.child(
                    Button::new("column-preview-git-unstage")
                        .ghost()
                        .compact()
                        .label("取消暂存")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_vcs_task(
                                VcsTool::Git,
                                VcsOperation::Unstage,
                                path.clone(),
                                window,
                                cx,
                            );
                        })),
                );
            }
            let commit_path = path.clone();
            actions = actions.child(
                Button::new("column-preview-git-commit")
                    .ghost()
                    .compact()
                    .label("提交…")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.begin_vcs_commit(VcsTool::Git, commit_path.clone(), window, cx);
                    })),
            );
            actions = actions.child(
                Button::new("column-preview-git-log")
                    .ghost()
                    .compact()
                    .label("历史")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.run_vcs_query_task(
                            VcsTool::Git,
                            VcsQuery::Log,
                            path.clone(),
                            window,
                            cx,
                        );
                    })),
            );
            section = section.child(actions);
        }

        section.into_any_element()
    }

    fn sync_column_list_scrolls(&mut self) {
        self.column_vertical_scrolls
            .truncate(self.column_panes.len());
        for (index, pane) in self.column_panes.iter().enumerate() {
            let path_changed = self
                .column_vertical_scrolls
                .get(index)
                .is_none_or(|state| !same_path(&state.path, &pane.path));
            if path_changed {
                let state = ColumnListScroll {
                    path: pane.path.clone(),
                    handle: UniformListScrollHandle::new(),
                };
                if let Some(slot) = self.column_vertical_scrolls.get_mut(index) {
                    *slot = state;
                } else {
                    self.column_vertical_scrolls.push(state);
                }
            }
        }
    }

    fn render_column(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.column_panes.get(index) else {
            return div().into_any_element();
        };
        let path = pane.path.clone();
        let loading = pane.loading;
        let entry_count = pane.entries.len();
        let row_count = pane.visible_row_count();
        let rules = ColumnViewRules {
            sort: pane.sort,
            filter: pane.filter,
            group: pane.group,
        };
        let search_query = pane.search_query.clone();
        let search_truncated = pane.search_truncated;
        let vertical_scroll = self
            .column_vertical_scrolls
            .get(index)
            .map(|state| state.handle.clone())
            .unwrap_or_default();
        let search_open = self
            .column_search_path
            .as_ref()
            .is_some_and(|search_path| same_path(search_path, &path));
        let search_loading = search_open && self.column_search_loading;
        let active = same_path(&path, &self.current_dir);
        let details_visible =
            active && self.show_column_details && index + 1 == self.column_panes.len();
        let layout = ColumnLayoutState {
            show_date_column: self.show_date_column,
            show_kind_column: self.show_kind_column,
            show_size_column: self.show_size_column,
            preferred_width: self.preferred_column_width,
        };
        let column_width = if details_visible {
            detailed_column_width(
                &pane.entries,
                window,
                layout.show_date_column,
                layout.show_kind_column,
                layout.show_size_column,
                layout.preferred_width,
            )
        } else {
            COLUMN_WIDTH
        };
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let can_paste = self.clipboard.borrow().is_some() || file_clipboard::has_files();
        let column_menu_target = column_context_menu_target(pane, can_paste);
        let action_context = self.focus_handle.clone();
        let menu_path = path.clone();
        let search_path = path.clone();
        let close_search_path = path.clone();
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let light = !cx.theme().mode.is_dark();
        let header_background = if active && !light {
            accent.opacity(0.5)
        } else {
            cx.theme().title_bar
        };
        let surface_background = if light {
            cx.theme().background
        } else {
            cx.theme().secondary.opacity(0.72)
        };
        let empty_label = if search_query.is_some() {
            "无搜索结果"
        } else if rules.filter != ColumnFilter::All {
            "筛选后无项目"
        } else {
            "空文件夹"
        };
        let body = if loading {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(muted)
                .child("正在读取…")
                .into_any_element()
        } else if entry_count == 0 {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(muted)
                .child(empty_label)
                .into_any_element()
        } else {
            uniform_list(
                SharedString::from(format!("column-list-{index}-{}", path.display())),
                row_count,
                cx.processor(move |this, range, window, cx| {
                    this.render_column_rows(index, range, details_visible, layout, window, cx)
                }),
            )
            .flex_1()
            .min_h_0()
            .w_full()
            .track_scroll(&vertical_scroll)
            .into_any_element()
        };
        let mut header = h_flex()
            .h(px(COLUMN_HEADER_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(border)
            .bg(header_background)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Icon::new(FileIconKind::Folder)
                            .size_4()
                            .text_color(folder_icon_color(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_medium()
                            .text_color(if active { cx.theme().foreground } else { muted })
                            .child(title),
                    ),
            )
            .when(search_loading, |this| {
                this.child(div().text_xs().text_color(muted).child("搜索中…"))
            })
            .when(search_truncated, |this| {
                this.child(div().text_xs().text_color(muted).child("1000+"))
            })
            .child(
                div()
                    .px_1()
                    .py_0p5()
                    .rounded_md()
                    .bg(muted.opacity(0.14))
                    .text_xs()
                    .text_color(muted)
                    .child(entry_count.to_string()),
            )
            .child(
                Button::new(("column-sort-order", index))
                    .ghost()
                    .compact()
                    .icon(if rules.sort.ascending {
                        IconName::SortAscending
                    } else {
                        IconName::SortDescending
                    })
                    .tooltip(if rules.sort.ascending {
                        "当前升序，点击改为降序"
                    } else {
                        "当前降序，点击改为升序"
                    })
                    .accessibility_label(if rules.sort.ascending {
                        "切换为降序排序"
                    } else {
                        "切换为升序排序"
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_column_sort_order(index, cx)
                    })),
            );
        if active {
            header = header.child(
                Button::new(("toggle-column-details", index))
                    .ghost()
                    .compact()
                    .icon(IconName::Grid2x2)
                    .tooltip(if details_visible {
                        "隐藏详细信息"
                    } else {
                        "显示详细信息"
                    })
                    .accessibility_label(if details_visible {
                        "隐藏详细信息"
                    } else {
                        "显示详细信息"
                    })
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_column_details(cx)),
                    ),
            );
        }
        header = header
            .child(
                Button::new(("column-search", index))
                    .ghost()
                    .compact()
                    .icon(IconName::Search)
                    .tooltip("搜索此分栏")
                    .accessibility_label("搜索此分栏")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_column_search(search_path.clone(), window, cx)
                    })),
            )
            .child(
                Button::new(("column-more", index))
                    .ghost()
                    .compact()
                    .icon(IconName::Ellipsis)
                    .tooltip("分栏显示规则")
                    .accessibility_label("分栏显示规则")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                        build_column_control_menu(menu, &menu_path, rules, layout, window, cx)
                    }),
            );

        let details_header = details_visible.then(|| {
            let sort_key = rules.sort.key;
            let sort_ascending = rules.sort.ascending;
            let mut header = h_flex()
                .h(px(24.))
                .w_full()
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(border)
                .bg(muted.opacity(0.08))
                .text_xs()
                .font_medium()
                .text_color(muted)
                .child(
                    Button::new(("column-header-name", index))
                        .ghost()
                        .compact()
                        .h(px(22.))
                        .px_0()
                        .flex_1()
                        .min_w_0()
                        .toggled(sort_key == SortKey::Name)
                        .tooltip(column_header_tooltip(
                            "名称",
                            SortKey::Name,
                            sort_key,
                            sort_ascending,
                        ))
                        .accessibility_label(column_header_tooltip(
                            "名称",
                            SortKey::Name,
                            sort_key,
                            sort_ascending,
                        ))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.sort_column_from_header(index, SortKey::Name, cx);
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .pl(px(24.))
                                .gap_1()
                                .child("名称")
                                .when(sort_key == SortKey::Name, |this| {
                                    this.child(column_sort_header_icon(sort_ascending))
                                }),
                        ),
                );
            if layout.show_date_column {
                header = header.child(
                    Button::new(("column-header-modified", index))
                        .ghost()
                        .compact()
                        .h(px(22.))
                        .px_0()
                        .w(px(DETAILED_DATE_WIDTH))
                        .toggled(sort_key == SortKey::Modified)
                        .tooltip(column_header_tooltip(
                            "日期",
                            SortKey::Modified,
                            sort_key,
                            sort_ascending,
                        ))
                        .accessibility_label(column_header_tooltip(
                            "日期",
                            SortKey::Modified,
                            sort_key,
                            sort_ascending,
                        ))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.sort_column_from_header(index, SortKey::Modified, cx);
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1()
                                .child("日期")
                                .when(sort_key == SortKey::Modified, |this| {
                                    this.child(column_sort_header_icon(sort_ascending))
                                }),
                        ),
                );
            }
            if layout.show_kind_column {
                header = header.child(
                    Button::new(("column-header-kind", index))
                        .ghost()
                        .compact()
                        .h(px(22.))
                        .px_0()
                        .w(px(DETAILED_KIND_WIDTH))
                        .toggled(sort_key == SortKey::Kind)
                        .tooltip(column_header_tooltip(
                            "类型",
                            SortKey::Kind,
                            sort_key,
                            sort_ascending,
                        ))
                        .accessibility_label(column_header_tooltip(
                            "类型",
                            SortKey::Kind,
                            sort_key,
                            sort_ascending,
                        ))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.sort_column_from_header(index, SortKey::Kind, cx);
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1()
                                .child("类型")
                                .when(sort_key == SortKey::Kind, |this| {
                                    this.child(column_sort_header_icon(sort_ascending))
                                }),
                        ),
                );
            }
            if layout.show_size_column {
                header = header.child(
                    Button::new(("column-header-size", index))
                        .ghost()
                        .compact()
                        .h(px(22.))
                        .px_0()
                        .w(px(DETAILED_SIZE_WIDTH))
                        .toggled(sort_key == SortKey::Size)
                        .tooltip(column_header_tooltip(
                            "大小",
                            SortKey::Size,
                            sort_key,
                            sort_ascending,
                        ))
                        .accessibility_label(column_header_tooltip(
                            "大小",
                            SortKey::Size,
                            sort_key,
                            sort_ascending,
                        ))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.sort_column_from_header(index, SortKey::Size, cx);
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .justify_end()
                                .gap_1()
                                .child("大小")
                                .when(sort_key == SortKey::Size, |this| {
                                    this.child(column_sort_header_icon(sort_ascending))
                                }),
                        ),
                );
            }
            header
        });

        let search_bar = search_open.then(|| {
            h_flex()
                .h(px(COLUMN_SEARCH_HEIGHT))
                .w_full()
                .px_2()
                .py_1()
                .items_center()
                .bg(cx.theme().status_bar)
                .border_b_1()
                .border_color(border)
                .child(
                    Input::new(&self.column_search_input)
                        .aria_label("搜索当前分栏")
                        .prefix(Icon::new(IconName::Search).size_3())
                        .cleanable(true)
                        .suffix(
                            Button::new(("column-close-search", index))
                                .ghost()
                                .compact()
                                .icon(IconName::X)
                                .tooltip("关闭分栏搜索 (Esc)")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if this.column_search_path.as_ref().is_some_and(|active_path| {
                                        same_path(active_path, &close_search_path)
                                    }) {
                                        this.close_column_search(window, cx);
                                    }
                                })),
                        )
                        .small()
                        .flex_1()
                        .rounded_md()
                        .bg(cx.theme().background)
                        .border_color(cx.theme().border),
                )
                .into_any_element()
        });

        let mut column = v_flex()
            .id(("column", index))
            .on_hover(cx.listener(move |this, hovered: &bool, _window, _cx| {
                if *hovered
                    && let Some(path) = this.column_panes.get(index).map(|pane| pane.path.clone())
                {
                    this.remember_column(&path);
                }
            }))
            .w(px(column_width))
            .h_full()
            .flex_shrink_0()
            .rounded_lg()
            .overflow_hidden()
            .bg(surface_background)
            .border_1()
            .border_color(border)
            .shadow_sm()
            .child(header);
        if let Some(details_header) = details_header {
            column = column.child(details_header);
        }
        if let Some(search_bar) = search_bar {
            column = column.child(search_bar);
        }
        let internal_destination = path.clone();
        let external_destination = path.clone();
        column
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .on_scroll_wheel(cx.listener(
                        move |this, event: &ScrollWheelEvent, window, cx| {
                            this.handle_column_scroll_wheel(index, event, window, cx)
                        },
                    ))
                    .child(body),
            )
            .drag_over::<KiteFileDrag>(move |style, _, _, _| style.bg(accent.opacity(0.14)))
            .on_drop(cx.listener(move |this, drag: &KiteFileDrag, window, cx| {
                let sources = drag
                    .paths
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>();
                this.transfer_dropped_paths(
                    sources,
                    internal_destination.clone(),
                    !window.modifiers().control,
                    window,
                    cx,
                );
            }))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(accent.opacity(0.10)))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                this.transfer_dropped_paths(
                    paths.paths().to_vec(),
                    external_destination.clone(),
                    false,
                    window,
                    cx,
                );
            }))
            .context_menu(move |menu, window, cx| {
                render_context_menu(
                    menu,
                    &column_menu_target,
                    action_context.clone(),
                    window,
                    cx,
                )
            })
            .into_any_element()
    }

    fn handle_column_scroll_wheel(
        &mut self,
        index: usize,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = match self.column_vertical_scrolls.get(index) {
            Some(state) => state.handle.clone(),
            None => return,
        };
        let base = handle.0.borrow().base_handle.clone();
        let delta = event.delta.pixel_delta(window.line_height());
        let delta_x = delta.x.as_f32();
        let delta_y = delta.y.as_f32();
        let current = base.offset();

        if let Some(horizontal_delta) =
            horizontal_wheel_delta(event.modifiers.shift, delta_x, delta_y)
        {
            // The list maps one-axis gestures to its vertical offset. Restore it
            // before the column strip receives this horizontal gesture.
            let applied_y = if delta.y.is_zero() { delta_x } else { delta_y };
            let max_y = base.max_offset().y.as_f32().max(0.0);
            let restored_y = (current.y.as_f32() - applied_y).clamp(-max_y, 0.0);
            base.set_offset(point(current.x, px(restored_y)));
            self.scroll_columns_by(horizontal_delta, cx);
            cx.notify();
            cx.stop_propagation();
            return;
        }

        if delta.y.is_zero() {
            return;
        }

        let max_y = base.max_offset().y.as_f32().max(0.0);
        let clamped_y = directory_scroll_offset(current.y.as_f32(), max_y);
        base.set_offset(point(current.x, px(clamped_y)));
        // The list handles wheel input before this wrapper during bubbling.
        // Keep vertical input inside the directory at both scroll boundaries.
        cx.notify();
        cx.stop_propagation();
    }

    fn scroll_columns_by(&self, delta: f32, cx: &mut Context<Self>) {
        if delta == 0.0 {
            return;
        }
        let handle = self.column_scroll.clone();
        let offset = handle.offset();
        let max_x = handle.max_offset().x.as_f32().max(0.0);
        let target_x = (offset.x.as_f32() + delta).clamp(-max_x, 0.0);
        if (target_x - offset.x.as_f32()).abs() > f32::EPSILON {
            handle.set_offset(point(px(target_x), offset.y));
            cx.notify();
        }
    }

    fn render_column_rows(
        &mut self,
        column: usize,
        range: Range<usize>,
        details_visible: bool,
        layout: ColumnLayoutState,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(pane) = self.column_panes.get(column) else {
            return Vec::new();
        };
        let display_rows = pane.display_rows();
        let column_path = pane.path.clone();
        let loading = pane.loading;
        let can_paste = self.clipboard.borrow().is_some() || file_clipboard::has_files();
        let git_available = pane.git_available;
        let svn_available = pane.svn_available;
        let accent = cx.theme().accent;
        let accent_foreground = cx.theme().accent_foreground;
        let hover = cx.theme().muted;
        let muted = cx.theme().muted_foreground;
        let action_context = self.focus_handle.clone();
        let mut rows = Vec::new();
        for row_ix in range {
            let Some(display_row) = display_rows.get(row_ix) else {
                continue;
            };
            let entry = match display_row {
                ColumnDisplayRow::Group {
                    label,
                    count,
                    collapsed,
                } => {
                    let click_label = label.clone();
                    rows.push(
                        h_flex()
                            .id(("column-group", column * 100_000 + row_ix))
                            .h(px(30.))
                            .w_full()
                            .gap_1()
                            .px_2()
                            .items_center()
                            .text_xs()
                            .font_medium()
                            .text_color(muted)
                            .bg(cx.theme().status_bar)
                            .cursor_pointer()
                            .child(
                                Icon::new(IconName::ChevronRight)
                                    .size_3()
                                    .rotate(percentage(if *collapsed { 0. } else { 0.25 })),
                            )
                            .child(div().flex_1().truncate().child(label.clone()))
                            .child(div().text_xs().child(count.to_string()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle_column_group(column, click_label.clone(), cx)
                            }))
                            .into_any_element(),
                    );
                    continue;
                }
                ColumnDisplayRow::Entry(entry) => entry.clone(),
            };
            let selected = pane.is_selected(&entry.path);
            let metadata_color = if selected { accent_foreground } else { muted };
            let modified_label = column_modified_label(&entry);
            let kind_label = entry.type_label();
            let size_label = column_size_label(&entry);
            let row_column_path = column_path.clone();
            let menu_entry = entry.clone();
            let row_action_context = action_context.clone();
            let row_git_available = git_available || entry.git_status.is_some();
            let row_svn_available = svn_available || entry.svn_status.is_some();
            let icon_status = worse_status(entry.git_status, entry.svn_status);
            let icon_status_is_svn = match (entry.git_status, entry.svn_status) {
                (Some(git), Some(svn)) => svn.rank() > git.rank(),
                (None, Some(_)) => true,
                _ => false,
            };
            let mut row = h_flex()
                .id(("column-row", column * 100_000 + row_ix))
                .h(px(30.))
                .w_full()
                .px_2()
                .gap_1p5()
                .items_center()
                .cursor_pointer()
                .text_sm()
                .when(selected, |this| this.bg(accent))
                .when(!selected, |this| {
                    this.hover(move |style| style.bg(hover.opacity(0.45)))
                })
                .child(
                    div()
                        .relative()
                        .size(px(18.))
                        .child(
                            Icon::new(column_icon_for(&entry))
                                .size(px(18.))
                                .text_color(icon_color(&entry, cx)),
                        )
                        .when_some(icon_status, |this, status| {
                            this.child(vcs_icon_badge(status, icon_status_is_svn, cx))
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .font_medium()
                        .text_color(if selected {
                            accent_foreground
                        } else {
                            cx.theme().foreground
                        })
                        .child(entry.name.clone()),
                );
            if entry.is_dir() {
                row = row.child(
                    Icon::new(IconName::ChevronRight)
                        .size_3()
                        .text_color(metadata_color),
                );
            }
            if details_visible {
                if layout.show_date_column {
                    row = row.child(
                        div()
                            .w(px(DETAILED_DATE_WIDTH))
                            .truncate()
                            .text_xs()
                            .text_color(metadata_color)
                            .child(modified_label),
                    );
                }
                if layout.show_kind_column {
                    row = row.child(
                        div()
                            .w(px(DETAILED_KIND_WIDTH))
                            .truncate()
                            .text_xs()
                            .text_color(metadata_color)
                            .child(kind_label),
                    );
                }
                if layout.show_size_column {
                    row = row.child(
                        div()
                            .w(px(DETAILED_SIZE_WIDTH))
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(metadata_color)
                            .child(size_label),
                    );
                }
            }
            let drag_payload = self.drag_payload_for_entry(column, &entry);
            let mut row = row
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.activate_column_entry(column, row_ix, event, window, cx)
                }))
                .on_drag(drag_payload, |drag: &KiteFileDrag, position, _, cx| {
                    file_clipboard::trace_drag(&format!(
                        "GPUI drag preview created at ({:.1},{:.1}) label={} paths={:?}",
                        position.x.as_f32(),
                        position.y.as_f32(),
                        drag.label,
                        drag.paths
                    ));
                    cx.new(|_| KiteFileDragPreview {
                        label: drag.label.clone(),
                        count: drag.paths.len(),
                        position,
                    })
                })
                .external_drag_payload::<KiteFileDrag>(|drag, _, _| {
                    Some(ExternalDragPayload::Files(FileDragPaths::new(
                        drag.paths.clone(),
                    )))
                });
            if entry.is_dir() {
                let internal_destination = entry.path.clone();
                let external_destination = entry.path.clone();
                row = row
                    .drag_over::<KiteFileDrag>(move |style, _, _, _| style.bg(accent.opacity(0.28)))
                    .on_drop(cx.listener(move |this, drag: &KiteFileDrag, window, cx| {
                        let sources = drag
                            .paths
                            .iter()
                            .map(|(path, _)| path.clone())
                            .collect::<Vec<_>>();
                        this.transfer_dropped_paths(
                            sources,
                            internal_destination.clone(),
                            !window.modifiers().control,
                            window,
                            cx,
                        );
                    }))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.bg(accent.opacity(0.20))
                    })
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                        this.transfer_dropped_paths(
                            paths.paths().to_vec(),
                            external_destination.clone(),
                            false,
                            window,
                            cx,
                        );
                    }));
            }
            let row = row
                // Right-click must not select the row. Selection loads the next
                // column for folders and starts preview loading for files.
                .context_menu(move |menu, window, cx| {
                    let capabilities = ContextMenuCapabilities::for_path(
                        &menu_entry.path,
                        loading,
                        can_paste,
                        row_git_available,
                        row_svn_available,
                    );
                    let target = ContextMenuTarget::entry(
                        menu_entry.path.clone(),
                        menu_entry.name.clone(),
                        menu_entry.is_dir(),
                        row_column_path.clone(),
                        capabilities,
                    );
                    render_context_menu(menu, &target, row_action_context.clone(), window, cx)
                })
                .into_any_element();
            rows.push(row);
        }
        rows
    }

    fn render_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(modal) = self.modal.as_ref() else {
            return div().into_any_element();
        };
        let (title, description, action_label) = match &modal.kind {
            ModalKind::NewFolder(_) => ("新建文件夹", "输入文件夹名称。按 Enter 创建。", "创建"),
            ModalKind::NewFile(_) => ("新建文件", "输入文件名。按 Enter 创建空文件。", "创建"),
            ModalKind::Rename(path) => (
                "重命名",
                "输入新名称。文件内容不会改变。",
                if path.is_dir() {
                    "重命名文件夹"
                } else {
                    "重命名文件"
                },
            ),
            ModalKind::GitCommit(_) => (
                "Git 提交",
                "输入提交说明。Git 将提交所选路径的已暂存更改。",
                "提交",
            ),
            ModalKind::SvnCommit(_) => (
                "SVN 提交",
                "输入提交说明。SVN 将提交所选路径的本地更改。",
                "提交",
            ),
            ModalKind::GitUsername(_) => (
                "Git 认证",
                "输入远程仓库的用户名。凭据只用于本次操作，不会保存。",
                "继续",
            ),
            ModalKind::GitPassword { .. } => (
                "Git 认证",
                "输入密码或个人访问令牌。凭据只用于本次操作，不会保存。",
                "连接",
            ),
        };
        let input = Input::new(&modal.input)
            .aria_label(title)
            .cleanable(true)
            .w_full();
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000a8))
            .child(
                v_flex()
                    .w(px(430.))
                    .max_w(relative(0.9))
                    .gap_4()
                    .p_5()
                    .rounded_lg()
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .shadow_xl()
                    .child(
                        h_flex()
                            .w_full()
                            .items_start()
                            .justify_between()
                            .gap_4()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(div().text_lg().font_semibold().child(title))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(description),
                                    ),
                            )
                            .child(
                                Button::new("modal-close")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::X)
                                    .tooltip("关闭")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.close_modal(window, cx)
                                    })),
                            ),
                    )
                    .child(input)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("modal-cancel")
                                    .label("取消")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.close_modal(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("modal-submit")
                                    .primary()
                                    .label(action_label)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.submit_modal(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn begin_git_pull(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let answers = [
            PromptButton::ok("合并"),
            PromptButton::new("变基"),
            PromptButton::new("仅快进"),
            PromptButton::cancel("取消"),
        ];
        let answer = window.prompt(
            PromptLevel::Info,
            "选择拉取方式",
            Some("工作区有未提交更改时会先自动暂存，并在拉取后恢复。"),
            &answers,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            let strategy = match choice {
                0 => kite_core::git::remote::GitPullStrategy::Merge,
                1 => kite_core::git::remote::GitPullStrategy::Rebase,
                2 => kite_core::git::remote::GitPullStrategy::FastForwardOnly,
                _ => return,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.start_git_network(
                    GitNetworkRequest::new(GitOperationCommand::Pull {
                        path,
                        options: kite_core::git::remote::GitPullOptions {
                            strategy,
                            auto_stash: true,
                            remote: None,
                            branch: None,
                        },
                    }),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn open_git_push_dialog(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let remote_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("自动选择（通常为 origin）")
                .clean_on_escape()
        });
        let branch_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("自动选择当前分支")
                .clean_on_escape()
        });
        self.git_dialog = Some(GitDialogState::Push(GitPushDialogState {
            path,
            remote_input: remote_input.clone(),
            branch_input,
            preview: None,
            selected_commit: None,
            files: Vec::new(),
            include_tags: false,
            force_with_lease: false,
            loading: true,
            error: None,
            _preview_task: Task::ready(()),
            _files_task: Task::ready(()),
        }));
        remote_input.update(cx, |input, cx| input.focus(window, cx));
        self.refresh_git_push_preview(cx);
        cx.notify();
    }

    fn refresh_git_push_preview(&mut self, cx: &mut Context<Self>) {
        let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() else {
            return;
        };
        let path = dialog.path.clone();
        let remote = nonempty_input(dialog.remote_input.read(cx));
        let branch = nonempty_input(dialog.branch_input.read(cx));
        let include_tags = dialog.include_tags;
        let force_with_lease = dialog.force_with_lease;
        dialog.loading = true;
        dialog.error = None;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    kite_core::git::remote::push_preview(
                        &path,
                        &kite_core::git::remote::GitPushOptions {
                            remote,
                            branch,
                            include_tags,
                            force_with_lease,
                            up_to_commit: None,
                        },
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let selected_oid = {
                    let Some(GitDialogState::Push(dialog)) = this.git_dialog.as_mut() else {
                        return;
                    };
                    dialog.loading = false;
                    match result {
                        Ok(preview) => {
                            let selected = preview.commits.len().checked_sub(1);
                            dialog.selected_commit = selected;
                            let oid =
                                selected.and_then(|index| preview.commits.get(index).cloned());
                            dialog.preview = Some(preview);
                            dialog.error = None;
                            oid.map(|commit| commit.oid)
                        }
                        Err(error) => {
                            dialog.preview = None;
                            dialog.files.clear();
                            dialog.error = Some(error.message.into());
                            None
                        }
                    }
                };
                if let Some(oid) = selected_oid {
                    this.load_git_commit_files(oid, cx);
                }
                cx.notify();
            });
        });
        if let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() {
            dialog._preview_task = task;
        }
    }

    fn select_git_push_commit(&mut self, index: usize, cx: &mut Context<Self>) {
        let oid = {
            let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() else {
                return;
            };
            let Some(commit) = dialog
                .preview
                .as_ref()
                .and_then(|preview| preview.commits.get(index))
            else {
                return;
            };
            let oid = commit.oid.clone();
            dialog.selected_commit = Some(index);
            dialog.files.clear();
            oid
        };
        self.load_git_commit_files(oid, cx);
        cx.notify();
    }

    fn load_git_commit_files(&mut self, oid: String, cx: &mut Context<Self>) {
        let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() else {
            return;
        };
        let path = dialog.path.clone();
        let requested_oid = oid.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    kite_core::git::remote::commit_files(&path, &requested_oid)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(GitDialogState::Push(dialog)) = this.git_dialog.as_mut() else {
                    return;
                };
                let still_selected = dialog
                    .preview
                    .as_ref()
                    .and_then(|preview| {
                        dialog
                            .selected_commit
                            .and_then(|index| preview.commits.get(index))
                    })
                    .is_some_and(|commit| commit.oid == oid);
                if still_selected {
                    match result {
                        Ok(files) => dialog.files = files,
                        Err(error) => dialog.error = Some(error.message.into()),
                    }
                }
                cx.notify();
            });
        });
        if let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() {
            dialog._files_task = task;
        }
    }

    fn submit_git_push(
        &mut self,
        up_to_selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_ref() else {
            return;
        };
        let path = dialog.path.clone();
        let remote = nonempty_input(dialog.remote_input.read(cx));
        let branch = nonempty_input(dialog.branch_input.read(cx));
        let include_tags = dialog.include_tags;
        let force_with_lease = dialog.force_with_lease;
        let up_to_commit = up_to_selected
            .then(|| {
                dialog
                    .selected_commit
                    .and_then(|index| dialog.preview.as_ref()?.commits.get(index))
                    .map(|commit| commit.oid.clone())
            })
            .flatten();
        if up_to_selected && up_to_commit.is_none() {
            if let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() {
                dialog.error = Some("请先选择要推送到的提交".into());
            }
            cx.notify();
            return;
        }
        if force_with_lease
            && dialog
                .preview
                .as_ref()
                .is_some_and(|preview| preview.protected_branch)
        {
            if let Some(GitDialogState::Push(dialog)) = self.git_dialog.as_mut() {
                dialog.error = Some("受保护分支不能强制推送".into());
            }
            cx.notify();
            return;
        }
        self.git_dialog = None;
        self.start_git_network(
            GitNetworkRequest::new(GitOperationCommand::Push {
                path,
                options: kite_core::git::remote::GitPushOptions {
                    remote,
                    branch,
                    include_tags,
                    force_with_lease,
                    up_to_commit,
                },
            }),
            window,
            cx,
        );
    }

    fn open_git_clone_dialog(
        &mut self,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://example.com/repository.git 或 SSH URL")
                .clean_on_escape()
        });
        let target_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("目标目录（留空则使用仓库名称）")
                .clean_on_escape()
        });
        let depth_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("1")
                .clean_on_escape()
        });
        self.git_dialog = Some(GitDialogState::Clone(GitCloneDialogState {
            directory,
            url_input: url_input.clone(),
            target_input,
            depth_input,
            shallow: false,
            error: None,
        }));
        url_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn submit_git_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(GitDialogState::Clone(dialog)) = self.git_dialog.as_ref() else {
            return;
        };
        let url = dialog.url_input.read(cx).value().trim().to_string();
        let target_value = dialog.target_input.read(cx).value().trim().to_string();
        let directory = dialog.directory.clone();
        let shallow = dialog.shallow;
        let depth = if shallow {
            dialog
                .depth_input
                .read(cx)
                .value()
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|depth| *depth > 0)
        } else {
            None
        };
        if url.is_empty() {
            if let Some(GitDialogState::Clone(dialog)) = self.git_dialog.as_mut() {
                dialog.error = Some("请输入仓库 URL".into());
            }
            cx.notify();
            return;
        }
        if shallow && depth.is_none() {
            if let Some(GitDialogState::Clone(dialog)) = self.git_dialog.as_mut() {
                dialog.error = Some("浅克隆深度必须是大于 0 的整数".into());
            }
            cx.notify();
            return;
        }
        let target_name = clone_target_name(&url);
        let target = if target_value.is_empty() {
            directory.join(target_name)
        } else {
            let path = PathBuf::from(target_value);
            if path.is_absolute() {
                path
            } else {
                directory.join(path)
            }
        };
        self.git_dialog = None;
        self.start_git_network(
            GitNetworkRequest::new(GitOperationCommand::Clone {
                url,
                target,
                shallow_depth: depth,
            }),
            window,
            cx,
        );
    }

    fn initialize_git_repository(
        &mut self,
        path: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        self.notice = Some("正在初始化 Git 仓库…".into());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { kite_core::git::remote::init_repository(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(path) => {
                        this.notice = Some(format!("Git 仓库已初始化：{}", path.display()).into());
                        this.invalidate_vcs();
                        this.refresh_vcs_statuses(this.current_dir.clone(), cx);
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.message.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_git_remotes_dialog(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("远程名称，例如 origin")
                .clean_on_escape()
        });
        let fetch_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Fetch URL")
                .clean_on_escape()
        });
        let push_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Push URL（可选）")
                .clean_on_escape()
        });
        self.git_dialog = Some(GitDialogState::Remotes(GitRemotesDialogState {
            path,
            remotes: Vec::new(),
            name_input: name_input.clone(),
            fetch_url_input,
            push_url_input,
            selected: None,
            loading: true,
            error: None,
            _task: Task::ready(()),
        }));
        name_input.update(cx, |input, cx| input.focus(window, cx));
        self.reload_git_remotes(cx);
        cx.notify();
    }

    fn reload_git_remotes(&mut self, cx: &mut Context<Self>) {
        let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() else {
            return;
        };
        let path = dialog.path.clone();
        dialog.loading = true;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { kite_core::git::remote::list_remotes(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(GitDialogState::Remotes(dialog)) = this.git_dialog.as_mut() else {
                    return;
                };
                dialog.loading = false;
                match result {
                    Ok(remotes) => {
                        if dialog.selected.as_ref().is_some_and(|selected| {
                            !remotes.iter().any(|remote| &remote.name == selected)
                        }) {
                            dialog.selected = None;
                        }
                        dialog.remotes = remotes;
                        dialog.error = None;
                    }
                    Err(error) => dialog.error = Some(error.message.into()),
                }
                cx.notify();
            });
        });
        if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
            dialog._task = task;
        }
    }

    fn select_git_remote(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() else {
            return;
        };
        let Some(remote) = dialog.remotes.iter().find(|remote| remote.name == name) else {
            return;
        };
        dialog.selected = Some(remote.name.clone());
        dialog.name_input.update(cx, |input, cx| {
            input.set_value(remote.name.clone(), window, cx);
        });
        dialog.fetch_url_input.update(cx, |input, cx| {
            input.set_value(remote.fetch_url.clone(), window, cx);
        });
        dialog.push_url_input.update(cx, |input, cx| {
            input.set_value(remote.push_url.clone().unwrap_or_default(), window, cx);
        });
        cx.notify();
    }

    fn save_git_remote(&mut self, cx: &mut Context<Self>) {
        let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_ref() else {
            return;
        };
        let path = dialog.path.clone();
        let selected = dialog.selected.clone();
        let name = dialog.name_input.read(cx).value().trim().to_string();
        let fetch_url = dialog.fetch_url_input.read(cx).value().trim().to_string();
        let push_url = dialog.push_url_input.read(cx).value().trim().to_string();
        if name.is_empty() || fetch_url.is_empty() {
            if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
                dialog.error = Some("远程名称和 Fetch URL 不能为空".into());
            }
            cx.notify();
            return;
        }
        if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
            dialog.loading = true;
            dialog.error = None;
        }
        let worker_name = name.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if let Some(old_name) = selected {
                        if old_name != worker_name {
                            kite_core::git::remote::rename_remote(&path, &old_name, &worker_name)?;
                        }
                        kite_core::git::remote::set_remote_url(
                            &path,
                            &worker_name,
                            &fetch_url,
                            (!push_url.is_empty()).then_some(push_url.as_str()),
                        )?;
                    } else {
                        kite_core::git::remote::add_remote(&path, &worker_name, &fetch_url)?;
                        if !push_url.is_empty() {
                            kite_core::git::remote::set_remote_url(
                                &path,
                                &worker_name,
                                &fetch_url,
                                Some(&push_url),
                            )?;
                        }
                    }
                    Ok::<(), kite_core::git::remote::GitFailure>(())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(GitDialogState::Remotes(dialog)) = this.git_dialog.as_mut() {
                    dialog.loading = false;
                    let succeeded = result.is_ok();
                    match result {
                        Ok(()) => {
                            dialog.selected = Some(name);
                            dialog.error = None;
                        }
                        Err(error) => dialog.error = Some(error.message.into()),
                    }
                    if succeeded {
                        this.reload_git_remotes(cx);
                    }
                }
                cx.notify();
            });
        });
        if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
            dialog._task = task;
        }
    }

    fn delete_selected_git_remote(&mut self, cx: &mut Context<Self>) {
        let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_ref() else {
            return;
        };
        let Some(name) = dialog.selected.clone() else {
            if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
                dialog.error = Some("请先选择一个远程".into());
            }
            cx.notify();
            return;
        };
        let path = dialog.path.clone();
        if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
            dialog.loading = true;
        }
        let requested_name = name.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    kite_core::git::remote::remove_remote(&path, &requested_name)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(GitDialogState::Remotes(dialog)) = this.git_dialog.as_mut() {
                    dialog.loading = false;
                    let succeeded = result.is_ok();
                    match result {
                        Ok(()) => {
                            dialog.selected = None;
                            dialog.error = None;
                        }
                        Err(error) => dialog.error = Some(error.message.into()),
                    }
                    if succeeded {
                        this.reload_git_remotes(cx);
                    }
                }
                cx.notify();
            });
        });
        if let Some(GitDialogState::Remotes(dialog)) = self.git_dialog.as_mut() {
            dialog._task = task;
        }
    }

    fn start_git_network(
        &mut self,
        request: GitNetworkRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_git_operation.is_some() {
            self.notice = Some("已有 Git 网络操作正在进行".into());
            cx.notify();
            return;
        }
        let label = request.command.label();
        let monitor = kite_core::git::remote::GitOperationMonitor::new();
        self.git_dialog = None;
        self.error = None;
        self.git_recovery_prompt = None;
        self.notice = Some(format!("{label}…").into());
        self.active_git_operation = Some(ActiveGitOperation {
            label: label.into(),
            monitor: monitor.clone(),
            progress: monitor.snapshot(),
            _task: Task::ready(()),
        });
        let finished = Arc::new(AtomicBool::new(false));
        let observed_finished = finished.clone();
        let progress_monitor = monitor.clone();
        let progress_entity = cx.entity();
        let worker_request = request.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let progress_task = cx.spawn(async move |cx| {
                loop {
                    let progress = progress_monitor.snapshot();
                    progress_entity.update(cx, |this, cx| {
                        if let Some(operation) = this.active_git_operation.as_mut() {
                            operation.progress = progress;
                            cx.notify();
                        }
                    });
                    if observed_finished.load(Ordering::Relaxed) {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
            });
            let result = cx
                .background_spawn(async move { worker_request.execute(&monitor) })
                .await;
            finished.store(true, Ordering::Relaxed);
            let _ = progress_task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_git_network(result, request, window, cx);
            });
        });
        if let Some(operation) = self.active_git_operation.as_mut() {
            operation._task = task;
        }
        cx.notify();
    }

    fn finish_git_network(
        &mut self,
        result: Result<GitOperationOutcome, kite_core::git::remote::GitFailure>,
        request: GitNetworkRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_git_operation = None;
        match result {
            Ok(outcome) => {
                self.error = None;
                self.notice = Some(outcome.message.into());
                self.invalidate_vcs();
                if let Some(path) = outcome.open_path {
                    self.navigate_to(path, NavigationIntent::Refresh, window, cx);
                } else {
                    self.refresh_vcs_statuses(self.current_dir.clone(), cx);
                }
            }
            Err(error) => match error.kind {
                kite_core::git::remote::GitFailureKind::AuthenticationRequired => {
                    self.notice = None;
                    self.begin_git_credentials(request, window, cx);
                    return;
                }
                kite_core::git::remote::GitFailureKind::PushRejected => {
                    self.notice = None;
                    if let GitOperationCommand::Push { path, options } = request.command {
                        self.git_recovery_prompt = Some(GitRecoveryPrompt {
                            path,
                            push: options,
                            credentials: request.credentials,
                        });
                        self.error = None;
                    } else {
                        self.error = Some(error.message.into());
                    }
                }
                kite_core::git::remote::GitFailureKind::Cancelled => {
                    self.notice = Some(error.message.clone().into());
                    if let Some(partial_path) = error.partial_clone_path {
                        let target = match &request.command {
                            GitOperationCommand::Clone { target, .. } => Some(target.clone()),
                            _ => None,
                        };
                        if let Some(target) = target {
                            self.clone_cleanup_prompt = Some(CloneCleanupPrompt {
                                partial_path,
                                target,
                            });
                        }
                    }
                }
                _ => {
                    self.notice = None;
                    self.error = Some(error.message.into());
                }
            },
        }
        cx.notify();
    }

    fn cancel_git_network_operation(&mut self, cx: &mut Context<Self>) {
        if let Some(operation) = self.active_git_operation.as_ref() {
            operation.monitor.cancel();
            self.notice = Some("正在取消 Git 操作…".into());
            cx.notify();
        }
    }

    fn resolve_clone_cleanup(&mut self, delete: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.clone_cleanup_prompt.take() else {
            return;
        };
        self.error = None;
        self.notice = Some(if delete {
            "正在删除未完成的克隆目录…".into()
        } else {
            "正在保留未完成的克隆目录…".into()
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if delete {
                        kite_core::git::remote::delete_partial_clone(&prompt.partial_path)
                    } else {
                        kite_core::git::remote::adopt_partial_clone(
                            &prompt.partial_path,
                            &prompt.target,
                        )
                    }
                    .map(|()| prompt.target)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_target) if delete => this.notice = Some("未完成的克隆目录已删除".into()),
                    Ok(target) => {
                        this.notice = Some(format!("已保留克隆目录：{}", target.display()).into());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.message.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn begin_git_credentials(
        &mut self,
        request: GitNetworkRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("用户名")
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_modal(window, cx);
                }
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.modal = Some(ModalState {
            kind: ModalKind::GitUsername(request),
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn begin_git_password(
        &mut self,
        request: GitNetworkRequest,
        username: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("密码或个人访问令牌")
                .masked(true)
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_modal(window, cx);
                }
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.modal = Some(ModalState {
            kind: ModalKind::GitPassword { request, username },
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn handle_git_pull_recovery(
        &mut self,
        strategy: kite_core::git::remote::GitPullStrategy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(recovery) = self.git_recovery_prompt.take() else {
            return;
        };
        self.start_git_network(
            GitNetworkRequest {
                command: GitOperationCommand::PullThenPush {
                    path: recovery.path,
                    pull: kite_core::git::remote::GitPullOptions {
                        strategy,
                        auto_stash: true,
                        remote: Some(recovery.push.remote.clone().unwrap_or_default())
                            .filter(|remote| !remote.is_empty()),
                        branch: Some(recovery.push.branch.clone().unwrap_or_default())
                            .filter(|branch| !branch.is_empty()),
                    },
                    push: recovery.push,
                },
                credentials: recovery.credentials,
            },
            window,
            cx,
        );
    }
    fn render_git_operation_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let operation = self.active_git_operation.as_ref()?;
        let progress = &operation.progress;
        let fraction = progress.fraction.unwrap_or(0.0) as f32;
        Some(
            h_flex()
                .h(px(46.))
                .px_3()
                .gap_3()
                .items_center()
                .bg(cx.theme().primary.opacity(0.10))
                .border_b_1()
                .border_color(cx.theme().border)
                .child(Icon::new(IconName::GitBranch).size_4())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().text_xs().font_medium().child(operation.label.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(progress.message.clone()),
                                ),
                        )
                        .child(
                            div()
                                .h(px(3.))
                                .w_full()
                                .rounded_full()
                                .overflow_hidden()
                                .bg(cx.theme().border)
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(fraction.clamp(0.02, 1.0)))
                                        .bg(cx.theme().primary),
                                ),
                        ),
                )
                .child(
                    Button::new("cancel-git-network-operation")
                        .small()
                        .outline()
                        .label("取消")
                        .disabled(progress.cancelled)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.cancel_git_network_operation(cx)
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_clone_cleanup_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.clone_cleanup_prompt.as_ref()?;
        Some(
            h_flex()
                .min_h(px(46.))
                .px_3()
                .py_2()
                .gap_3()
                .items_center()
                .bg(cx.theme().warning.opacity(0.12))
                .border_b_1()
                .border_color(cx.theme().border)
                .child(Icon::new(IconName::TriangleAlert).size_4())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child("克隆已取消。是否删除未完成的目录？")
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(prompt.partial_path.display().to_string()),
                        ),
                )
                .child(
                    Button::new("clone-cleanup-delete")
                        .small()
                        .danger()
                        .label("删除")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.resolve_clone_cleanup(true, cx)
                        })),
                )
                .child(
                    Button::new("clone-cleanup-keep")
                        .small()
                        .outline()
                        .label("保留")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.resolve_clone_cleanup(false, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_git_recovery_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.git_recovery_prompt.as_ref()?;
        Some(
            h_flex()
                .min_h(px(46.))
                .px_3()
                .py_2()
                .gap_3()
                .items_center()
                .bg(cx.theme().danger.opacity(0.10))
                .border_b_1()
                .border_color(cx.theme().border)
                .child(Icon::new(IconName::TriangleAlert).size_4())
                .child(
                    div()
                        .flex_1()
                        .child("远程包含新的提交，推送被拒绝。先整合远程历史再重试。"),
                )
                .child(
                    Button::new("git-recovery-merge")
                        .small()
                        .outline()
                        .label("合并后重试")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.handle_git_pull_recovery(
                                kite_core::git::remote::GitPullStrategy::Merge,
                                window,
                                cx,
                            )
                        })),
                )
                .child(
                    Button::new("git-recovery-rebase")
                        .small()
                        .outline()
                        .label("变基后重试")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.handle_git_pull_recovery(
                                kite_core::git::remote::GitPullStrategy::Rebase,
                                window,
                                cx,
                            )
                        })),
                )
                .child(
                    Button::new("git-recovery-dismiss")
                        .ghost()
                        .compact()
                        .icon(IconName::X)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.git_recovery_prompt = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_git_dialog(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dialog) = self.git_dialog.take() else {
            return div().into_any_element();
        };
        let (element, restored) = match dialog {
            GitDialogState::Push(dialog) => {
                let element = self.render_git_push_dialog(&dialog, cx);
                (element, GitDialogState::Push(dialog))
            }
            GitDialogState::Clone(dialog) => {
                let element = self.render_git_clone_dialog(&dialog, cx);
                (element, GitDialogState::Clone(dialog))
            }
            GitDialogState::Remotes(dialog) => {
                let element = self.render_git_remotes_dialog(&dialog, cx);
                (element, GitDialogState::Remotes(dialog))
            }
        };
        self.git_dialog = Some(restored);
        element
    }

    fn git_dialog_shell(
        &self,
        title: &'static str,
        description: &'static str,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000a8))
            .child(
                v_flex()
                    .w(px(920.))
                    .max_w(relative(0.94))
                    .max_h(relative(0.92))
                    .overflow_hidden()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_lg()
                    .child(
                        h_flex()
                            .items_start()
                            .justify_between()
                            .gap_4()
                            .px_5()
                            .pt_5()
                            .pb_4()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_lg().font_semibold().child(title))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(description),
                                    ),
                            )
                            .child(
                                Button::new("close-git-dialog")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.git_dialog = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(div().flex_1().min_h_0().px_5().pb_5().child(body)),
            )
            .into_any_element()
    }

    fn render_git_push_dialog(
        &mut self,
        dialog: &GitPushDialogState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let remote = Input::new(&dialog.remote_input)
            .aria_label("目标远程")
            .cleanable(true)
            .w_full();
        let branch = Input::new(&dialog.branch_input)
            .aria_label("目标分支")
            .cleanable(true)
            .w_full();
        let preview = dialog.preview.as_ref();
        let commits = preview.map(|preview| &preview.commits[..]).unwrap_or(&[]);
        let protected = preview.is_some_and(|preview| preview.protected_branch);
        let mut commit_rows = v_flex().gap_1();
        for (index, commit) in commits.iter().enumerate().rev() {
            let selected = dialog.selected_commit == Some(index);
            commit_rows = commit_rows.child(
                Button::new(("git-push-commit", index))
                    .ghost()
                    .selected(selected)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_git_push_commit(index, cx)
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(cx.theme().muted_foreground)
                                    .child(commit.short_oid.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(commit.subject.clone()),
                            ),
                    ),
            );
        }
        let mut file_rows = v_flex().gap_1();
        for change in &dialog.files {
            file_rows = file_rows.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(
                        div()
                            .w(px(18.))
                            .font_family("monospace")
                            .text_color(cx.theme().primary)
                            .child(change.status.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(change.path.clone()),
                    ),
            );
        }
        let body = v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .child(v_flex().flex_1().gap_1().child("远程").child(remote))
                    .child(v_flex().flex_1().gap_1().child("目标分支").child(branch)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("git-push-tags")
                            .small()
                            .outline()
                            .selected(dialog.include_tags)
                            .label("同时推送标签")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(GitDialogState::Push(dialog)) = this.git_dialog.as_mut()
                                {
                                    dialog.include_tags = !dialog.include_tags;
                                }
                                this.refresh_git_push_preview(cx);
                            })),
                    )
                    .child(
                        Button::new("git-push-force")
                            .small()
                            .outline()
                            .selected(dialog.force_with_lease)
                            .disabled(protected)
                            .label(if protected {
                                "受保护分支禁止强制推送"
                            } else {
                                "force-with-lease"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(GitDialogState::Push(dialog)) = this.git_dialog.as_mut()
                                {
                                    dialog.force_with_lease = !dialog.force_with_lease;
                                }
                                this.refresh_git_push_preview(cx);
                            })),
                    )
                    .child(
                        Button::new("git-push-refresh")
                            .small()
                            .ghost()
                            .label("刷新预览")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.refresh_git_push_preview(cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap_2()
                            .child(
                                h_flex().justify_between().child("待推送提交").child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} 个", commits.len())),
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(320.))
                                    .overflow_y_scrollbar()
                                    .child(commit_rows),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap_2()
                            .child("选中提交更改的文件")
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(320.))
                                    .overflow_y_scrollbar()
                                    .child(file_rows),
                            ),
                    ),
            )
            .when(dialog.loading, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("正在读取推送预览…"),
                )
            })
            .when_some(dialog.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("git-push-cancel")
                            .outline()
                            .label("取消")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.git_dialog = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-push-to-selected")
                            .outline()
                            .label("只推送到所选提交")
                            .disabled(
                                dialog.selected_commit.is_none()
                                    || protected && dialog.force_with_lease,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit_git_push(true, window, cx)
                            })),
                    )
                    .child(
                        Button::new("git-push-submit")
                            .primary()
                            .label("推送")
                            .disabled(dialog.loading || protected && dialog.force_with_lease)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit_git_push(false, window, cx)
                            })),
                    ),
            );
        self.git_dialog_shell(
            "推送提交",
            "检查待推送提交和文件，选择目标远程与分支。强制推送始终使用 --force-with-lease。",
            body.into_any_element(),
            cx,
        )
    }

    fn render_git_clone_dialog(
        &mut self,
        dialog: &GitCloneDialogState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let url = Input::new(&dialog.url_input)
            .aria_label("仓库 URL")
            .cleanable(true)
            .w_full();
        let target = Input::new(&dialog.target_input)
            .aria_label("目标目录")
            .cleanable(true)
            .w_full();
        let depth = Input::new(&dialog.depth_input)
            .aria_label("浅克隆深度")
            .w_full();
        let body = v_flex()
            .gap_4()
            .child(v_flex().gap_1().child("仓库 URL").child(url))
            .child(
                v_flex().gap_1().child("目标目录").child(target).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(dialog.directory.display().to_string()),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Button::new("git-clone-shallow")
                            .small()
                            .outline()
                            .selected(dialog.shallow)
                            .label("浅克隆")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(GitDialogState::Clone(dialog)) =
                                    this.git_dialog.as_mut()
                                {
                                    dialog.shallow = !dialog.shallow;
                                }
                                cx.notify();
                            })),
                    )
                    .when(dialog.shallow, |this| {
                        this.child(div().w(px(120.)).child(depth))
                    }),
            )
            .when_some(dialog.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("git-clone-cancel")
                            .outline()
                            .label("取消")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.git_dialog = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-clone-submit")
                            .primary()
                            .label("克隆")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit_git_clone(window, cx)
                            })),
                    ),
            );
        self.git_dialog_shell(
            "克隆 Git 仓库",
            "克隆在后台进行，可随时取消。完成后 Kite 会打开目标目录。",
            body.into_any_element(),
            cx,
        )
    }

    fn render_git_remotes_dialog(
        &mut self,
        dialog: &GitRemotesDialogState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = Input::new(&dialog.name_input)
            .aria_label("远程名称")
            .cleanable(true)
            .w_full();
        let fetch_url = Input::new(&dialog.fetch_url_input)
            .aria_label("Fetch URL")
            .cleanable(true)
            .w_full();
        let push_url = Input::new(&dialog.push_url_input)
            .aria_label("Push URL")
            .cleanable(true)
            .w_full();
        let mut remote_rows = v_flex().gap_1();
        for remote in &dialog.remotes {
            let selected = dialog.selected.as_deref() == Some(remote.name.as_str());
            remote_rows = remote_rows.child(
                Button::new(SharedString::from(format!("git-remote-{}", remote.name)))
                    .ghost()
                    .selected(selected)
                    .on_click(cx.listener({
                        let remote_name = remote.name.clone();
                        move |this, _: &ClickEvent, window, cx| {
                            this.select_git_remote(remote_name.clone(), window, cx)
                        }
                    }))
                    .child(
                        v_flex()
                            .w_full()
                            .items_start()
                            .child(div().font_medium().child(remote.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(remote.fetch_url.clone()),
                            ),
                    ),
            );
        }
        let body = h_flex()
            .gap_4()
            .child(
                v_flex()
                    .w(px(340.))
                    .flex_shrink_0()
                    .gap_2()
                    .child("远程")
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(300.))
                            .overflow_y_scrollbar()
                            .child(remote_rows),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .gap_3()
                    .child(v_flex().gap_1().child("名称").child(name))
                    .child(v_flex().gap_1().child("Fetch URL").child(fetch_url))
                    .child(v_flex().gap_1().child("Push URL（可选）").child(push_url))
                    .when_some(dialog.error.clone(), |this, error| {
                        this.child(div().text_xs().text_color(cx.theme().danger).child(error))
                    })
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("git-remote-delete")
                                    .outline()
                                    .danger()
                                    .label("删除所选远程")
                                    .disabled(dialog.selected.is_none() || dialog.loading)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.delete_selected_git_remote(cx)
                                    })),
                            )
                            .child(
                                Button::new("git-remote-save")
                                    .primary()
                                    .label(if dialog.selected.is_some() {
                                        "保存远程"
                                    } else {
                                        "添加远程"
                                    })
                                    .disabled(dialog.loading)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.save_git_remote(cx)
                                    })),
                            ),
                    ),
            );
        self.git_dialog_shell(
            "管理 Git 远程",
            "添加、改名、删除或修改 Fetch/Push URL。",
            body.into_any_element(),
            cx,
        )
    }
}

fn favorite_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn build_quick_links() -> Vec<QuickLink> {
    let mut links = Vec::new();
    push_quick_link(&mut links, "主文件夹", dirs::home_dir(), IconName::House);
    push_quick_link(&mut links, "桌面", dirs::desktop_dir(), IconName::Monitor);
    push_quick_link(&mut links, "文档", dirs::document_dir(), IconName::FileText);
    push_quick_link(&mut links, "下载", dirs::download_dir(), IconName::Download);
    push_quick_link(&mut links, "图片", dirs::picture_dir(), IconName::Image);
    links
}

fn push_quick_link(links: &mut Vec<QuickLink>, label: &str, path: Option<PathBuf>, icon: IconName) {
    let Some(path) = path else {
        return;
    };
    if !path.is_dir() || links.iter().any(|link| same_path(&link.path, &path)) {
        return;
    }
    links.push(QuickLink {
        label: label.into(),
        path,
        icon,
        detail: None,
        storage: None,
    });
}

fn storage_capacity_label(
    free_bytes: Option<u64>,
    total_bytes: Option<u64>,
) -> Option<SharedString> {
    match (free_bytes, total_bytes) {
        (Some(free), Some(total)) => {
            Some(format!("{} 可用 / {}", format_size(free), format_size(total)).into())
        }
        (Some(free), None) => Some(format!("{} 可用", format_size(free)).into()),
        (None, Some(total)) => Some(format_size(total).into()),
        (None, None) => None,
    }
}

fn drive_letter_label(path: &Path) -> Option<SharedString> {
    let first = path.to_string_lossy().chars().next()?;
    first
        .is_ascii_alphabetic()
        .then(|| SharedString::from(format!("{first}:")))
}

fn volume_quick_links(volumes: &[StorageVolume]) -> (Vec<QuickLink>, Vec<QuickLink>) {
    let mut connected_devices = Vec::new();
    let mut drives = Vec::new();

    for volume in volumes {
        let name = volume
            .volume_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or_else(|| {
                volume
                    .kind
                    .is_removable()
                    .then_some(volume.device_name.as_deref())
                    .flatten()
            })
            .unwrap_or(match volume.kind {
                VolumeKind::Usb => "USB 设备",
                VolumeKind::Removable => "可移动磁盘",
                VolumeKind::Fixed => "本地磁盘",
                VolumeKind::Optical => "光驱",
                VolumeKind::Network => "网络驱动器",
                VolumeKind::Other => "磁盘",
            });
        let icon = match volume.kind {
            VolumeKind::Usb => IconName::Usb,
            VolumeKind::Network => IconName::Network,
            VolumeKind::Optical => IconName::Disc,
            _ => IconName::HardDrive,
        };
        let storage = StorageSummary {
            drive_letter: drive_letter_label(&volume.path),
            free_bytes: volume.free_bytes,
            total_bytes: volume.total_bytes,
        };
        let link = QuickLink {
            label: name.into(),
            path: volume.path.clone(),
            icon,
            detail: storage.capacity_label(),
            storage: Some(storage),
        };

        if volume.kind.is_removable() {
            connected_devices.push(link);
        } else {
            drives.push(link);
        }
    }

    (connected_devices, drives)
}

fn same_quick_links(left: &[QuickLink], right: &[QuickLink]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.label == right.label
                && left.path == right.path
                && left.icon == right.icon
                && left.detail == right.detail
                && left.storage == right.storage
        })
}

fn update_download_notice(
    version: &str,
    progress: crate::updater::UpdateDownloadProgress,
) -> String {
    let downloaded = format_size(progress.downloaded_bytes);
    let Some(fraction) = progress.fraction() else {
        return format!("正在下载 Kite {version}… {downloaded}");
    };
    let percent = (fraction * 100.0).round() as u32;
    let total = format_size(progress.total_bytes);
    format!("正在下载 Kite {version}… {percent}% · {downloaded} / {total}")
}

fn update_notes_preview(notes: &crate::update_notes::UpdateNotesContent) -> Option<SharedString> {
    let mut lines = Vec::new();
    if let Some(summary) = notes.summary.as_deref() {
        lines.push(summary.to_string());
    }
    for section in &notes.sections {
        if !section.title.is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(section.title.clone());
        }
        lines.extend(section.items.iter().map(|item| format!("• {item}")));
    }

    let mut preview = lines.join("\n");
    if preview.trim().is_empty() {
        return None;
    }
    const PREVIEW_CHAR_LIMIT: usize = 1_200;
    if preview.chars().count() > PREVIEW_CHAR_LIMIT {
        preview = preview.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>();
        preview.push_str("\n…");
    }
    Some(preview.into())
}

fn load_directory_snapshot(path: &Path, sort: SortSpec) -> Result<DirectorySnapshot, String> {
    if shell_fs::is_shell_namespace_path(path) {
        shell_fs::load_directory(path, sort)
    } else {
        load_directory(path, sort)
    }
}

fn navigation_parent(path: &Path) -> Option<PathBuf> {
    if shell_fs::is_shell_namespace_path(path) {
        shell_fs::parent_path(path)
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn nonempty_input(input: &InputState) -> Option<String> {
    let value = input.value();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn working_directory_for_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    }
}

fn clone_target_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches(['/', '\\']);
    let name = trimmed
        .rsplit(['/', '\\', ':'])
        .find(|part| !part.is_empty() && *part != ".git")
        .unwrap_or("repository");
    let name = name.strip_suffix(".git").unwrap_or(name);
    if name.is_empty() {
        "repository".to_string()
    } else {
        name.to_string()
    }
}

fn absolute_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|current_dir| current_dir.join(&path))
        .unwrap_or(path)
}

fn same_path(left: &Path, right: &Path) -> bool {
    crate::platform::paths_equal(left, right)
}

fn paste_destination(target: &Path, working_directory: &Path) -> PathBuf {
    let target_is_directory = if shell_fs::is_shell_namespace_path(target) {
        shell_fs::is_directory(target)
    } else {
        target.is_dir()
    };
    if target_is_directory {
        target.to_path_buf()
    } else {
        working_directory.to_path_buf()
    }
}

fn path_after_rename(current_dir: &Path, original: &Path, renamed: &Path) -> PathBuf {
    if current_dir == original {
        return renamed.to_path_buf();
    }
    match current_dir.strip_prefix(original) {
        Ok(relative) => renamed.join(relative),
        Err(_) => current_dir.to_path_buf(),
    }
}

fn path_after_recycle(current_dir: &Path, removed: &Path) -> PathBuf {
    if current_dir == removed || current_dir.starts_with(removed) {
        return removed
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_root_path);
    }
    current_dir.to_path_buf()
}

fn path_after_recycle_paths(current_dir: &Path, removed: &[PathBuf]) -> PathBuf {
    removed
        .iter()
        .fold(current_dir.to_path_buf(), |current, path| {
            path_after_recycle(&current, path)
        })
}

fn run_vcs_operation_for_ui(
    tool: VcsTool,
    operation: VcsOperation,
    path: &Path,
) -> Result<String, String> {
    match tool {
        VcsTool::Git => GitService::shared()
            .run_legacy_operation(path, &operation)
            .map_err(|error| error.to_string()),
        VcsTool::Svn => run_vcs_operation(tool, operation, path),
    }
}

fn run_vcs_query_for_ui(tool: VcsTool, query: VcsQuery, path: &Path) -> Result<String, String> {
    match tool {
        VcsTool::Git => GitService::shared()
            .run_legacy_query(path, query)
            .map_err(|error| error.to_string()),
        VcsTool::Svn => run_vcs_query(tool, query, path),
    }
}

fn open_vcs_query_text(tool: VcsTool, query: VcsQuery, path: &Path) -> Result<String, String> {
    let title = format!("{} {}", tool.label(), query.label());
    let content = run_vcs_query_for_ui(tool, query, path)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let kind = match query {
        VcsQuery::Diff => "diff",
        VcsQuery::Log => "log",
    };
    let file = std::env::temp_dir().join(format!(
        "kite-{}-{kind}-{}-{stamp}.txt",
        tool.label().to_lowercase(),
        std::process::id()
    ));
    let text = format!("{title}\n目标：{}\n\n{content}\n", path.display());
    std::fs::write(&file, text.as_bytes())
        .map_err(|error| format!("无法写入{title}临时文件：{error}"))?;
    open_file(&file)?;
    Ok(format!("{title}已打开"))
}

fn wheel_scrolls_columns(delta_x: f32, delta_y: f32) -> bool {
    delta_x != 0.0 && delta_x.abs() > delta_y.abs()
}

fn horizontal_wheel_delta(shift: bool, delta_x: f32, delta_y: f32) -> Option<f32> {
    if wheel_scrolls_columns(delta_x, delta_y) {
        Some(delta_x)
    } else if shift && delta_y != 0.0 {
        Some(delta_y)
    } else {
        None
    }
}

fn detailed_column_width(
    entries: &[FileEntry],
    window: &Window,
    show_date_column: bool,
    show_kind_column: bool,
    show_size_column: bool,
    preferred_width: u16,
) -> f32 {
    let text_style = window.text_style();
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&text_style.font());
    let font_size = px(14.);
    let max_name_width = entries
        .iter()
        .map(|entry| {
            entry
                .name
                .chars()
                .map(|character| {
                    text_system
                        .layout_width(font_id, font_size, character)
                        .as_f32()
                })
                .sum::<f32>()
        })
        .fold(0.0, f32::max);
    detailed_column_width_for_name_width(
        max_name_width,
        show_date_column,
        show_kind_column,
        show_size_column,
        preferred_width,
    )
}

fn detailed_column_width_for_name_width(
    name_width: f32,
    show_date_column: bool,
    show_kind_column: bool,
    show_size_column: bool,
    preferred_width: u16,
) -> f32 {
    let visible_widths = [
        (show_date_column, DETAILED_DATE_WIDTH),
        (show_kind_column, DETAILED_KIND_WIDTH),
        (show_size_column, DETAILED_SIZE_WIDTH),
    ];
    let visible_count = visible_widths
        .iter()
        .filter(|(visible, _)| *visible)
        .count();
    let details_width = visible_widths
        .iter()
        .filter(|(visible, _)| *visible)
        .map(|(_, width)| *width)
        .sum::<f32>();
    let gaps = visible_count.saturating_sub(1) as f32 * 8.0;
    (DETAILED_COLUMN_NAME_OVERHEAD + details_width + gaps + name_width.ceil())
        .max(f32::from(preferred_width))
        .clamp(DETAILED_COLUMN_MIN_WIDTH, DETAILED_COLUMN_MAX_WIDTH)
}

fn default_column_header_sort_ascending(key: SortKey) -> bool {
    !matches!(key, SortKey::Modified | SortKey::Size)
}

fn column_header_tooltip(
    title: &str,
    key: SortKey,
    active_key: SortKey,
    ascending: bool,
) -> SharedString {
    if active_key == key {
        if ascending {
            format!("当前按{title}升序，点击改为降序")
        } else {
            format!("当前按{title}降序，点击改为升序")
        }
    } else {
        format!("按{title}排序")
    }
    .into()
}

fn column_sort_header_icon(ascending: bool) -> Icon {
    Icon::new(if ascending {
        IconName::SortAscending
    } else {
        IconName::SortDescending
    })
    .size_3()
}

fn column_modified_label(entry: &FileEntry) -> String {
    entry
        .modified_label()
        .split_whitespace()
        .next()
        .unwrap_or("未知")
        .to_string()
}

fn column_size_label(entry: &FileEntry) -> String {
    if entry.is_dir() {
        "—".to_string()
    } else {
        entry.size_label()
    }
}

fn directory_scroll_offset(offset_after_scroll: f32, max_offset: f32) -> f32 {
    let max_offset = max_offset.max(0.0);
    offset_after_scroll.clamp(-max_offset, 0.0)
}

fn resolve_location_input(value: &str, current_dir: &Path) -> Result<PathBuf, String> {
    let raw = value.trim().trim_matches('"');
    if raw.is_empty() {
        return Err("请输入路径".to_string());
    }
    if raw == "~" {
        return dirs::home_dir().ok_or_else(|| "无法确定个人文件夹".to_string());
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = dirs::home_dir().ok_or_else(|| "无法确定个人文件夹".to_string())?;
        return Ok(home.join(rest));
    }

    if raw == "/" {
        return Ok(root_directory(current_dir));
    }

    #[cfg(target_os = "windows")]
    if raw.starts_with('/') && !raw.starts_with("//") {
        return Ok(root_directory(current_dir).join(raw.trim_start_matches('/')));
    }

    let path = PathBuf::from(raw);
    if shell_fs::is_shell_namespace_path(&path) {
        return Ok(path);
    }
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(current_dir.join(path))
    }
}

fn git_change_counts_text(counts: &GitChangeCounts) -> String {
    [
        ("修改", counts.modified),
        ("新增", counts.added),
        ("删除", counts.deleted),
        ("重命名", counts.renamed),
        ("未跟踪", counts.untracked),
        ("冲突", counts.conflicted),
    ]
    .iter()
    .filter(|(_, count)| *count > 0)
    .map(|(label, count)| format!("{label} {count}"))
    .collect::<Vec<_>>()
    .join(" · ")
}

fn git_commit_time_label(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y/%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn preview_property(label: &str, value: String, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(div().text_sm().child(SelectableText::new(
            format!("preview-property-{label}"),
            value,
        )))
        .into_any_element()
}

fn preview_scrollbar_element<H>(handle: &H, cx: &App) -> AnyElement
where
    H: ScrollbarHandle + Clone + 'static,
{
    Scrollbar::vertical(handle)
        .id("column-preview-text-scrollbar")
        .mode(ScrollbarMode::Always)
        .styles(|styles| {
            let track = cx.theme().muted.opacity(0.55);
            let thumb = cx.theme().muted_foreground.opacity(0.75);
            let thumb_hover = cx.theme().muted_foreground.opacity(0.95);
            styles
                .track(|style| style.bg(track).width(px(10.)))
                .thumb(|style| style.bg(thumb).width(px(6.)).inset(px(2.)).radius(px(99.)))
                .thumb_hover(|style| style.bg(thumb_hover))
        })
        .viewport_from_layout()
        .into_any_element()
}

fn preview_more_menu_items() -> [(&'static str, IconName, ContextMenuCommand); 5] {
    [
        ("打开文件", IconName::FolderOpen, ContextMenuCommand::Open),
        ("复制", IconName::Copy, ContextMenuCommand::Copy),
        ("复制完整路径", IconName::Link, ContextMenuCommand::CopyPath),
        (
            "文件属性…",
            IconName::Info,
            ContextMenuCommand::ShowProperties,
        ),
        (
            "显示原生菜单…",
            IconName::Ellipsis,
            ContextMenuCommand::SystemMenu,
        ),
    ]
}

fn preview_info_text(entry: &FileEntry) -> String {
    let mut lines = vec![
        format!("名称：{}", entry.name),
        format!("类型：{}", entry.type_label()),
        format!("大小：{}", entry.size_label()),
        format!("修改日期：{}", entry.modified_label()),
        format!("位置：{}", path_text(&entry.path)),
    ];
    if let Some(status) = entry.git_status {
        lines.push(format!("Git：{} ({})", status.label(), status.code()));
    }
    if let Some(status) = entry.svn_status {
        lines.push(format!("SVN：{} ({})", status.label(), status.code()));
    }
    lines.join("\n")
}

fn preview_text_lines(text: &str) -> Vec<SharedString> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).into())
        .collect()
}

pub(crate) fn is_image_preview_entry(entry: &FileEntry) -> bool {
    ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"]
        .iter()
        .any(|extension| entry.extension.eq_ignore_ascii_case(extension))
}

pub(crate) fn is_raster_image_preview_entry(entry: &FileEntry) -> bool {
    is_image_preview_entry(entry) && !entry.extension.eq_ignore_ascii_case("svg")
}

pub(crate) fn is_log_preview_entry(entry: &FileEntry) -> bool {
    entry.extension.eq_ignore_ascii_case("log")
}

pub(crate) fn is_markdown_preview_entry(entry: &FileEntry) -> bool {
    entry.extension.eq_ignore_ascii_case("txt") || text_editor::is_markdown_path(&entry.path)
}

pub(crate) fn is_editor_preview_entry(entry: &FileEntry) -> bool {
    !shell_fs::is_shell_namespace_path(&entry.path)
        && entry.size <= text_editor::editable_document_limit(&entry.path)
        && text_editor::is_editable_preview_path(&entry.path)
}

pub(crate) fn is_text_preview_entry(entry: &FileEntry) -> bool {
    if text_editor::is_packaged_document_path(&entry.path) {
        return entry.size <= text_editor::editable_document_limit(&entry.path);
    }
    if entry.size > 512 * 1024 && !is_log_preview_entry(entry) {
        return false;
    }
    if text_editor::is_mind_map_path(&entry.path) || text_editor::is_markdown_path(&entry.path) {
        return true;
    }
    if entry.extension.is_empty() {
        return true;
    }
    let extension = entry.extension.to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "txt"
            | "md"
            | "markdown"
            | "log"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "ini"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "html"
            | "css"
            | "c"
            | "cpp"
            | "h"
            | "cs"
            | "java"
            | "go"
            | "kt"
            | "sh"
            | "ps1"
            | "bat"
    )
}

fn column_chain(root: &Path) -> Vec<PathBuf> {
    if shell_fs::is_shell_namespace_path(root) {
        return vec![root.to_path_buf()];
    }
    let mut chain = root.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
    chain.reverse();
    chain
}

fn common_column_prefix(panes: &[ColumnPane], chain: &[PathBuf]) -> usize {
    panes
        .iter()
        .zip(chain)
        .take_while(|(pane, path)| !pane.loading && same_path(&pane.path, path))
        .count()
}

fn load_column_pane(path: PathBuf, rule: ColumnViewRule) -> ColumnPane {
    try_load_column_pane(path.clone(), rule.clone())
        .unwrap_or_else(|_| ColumnPane::new(path, Vec::new(), 0, false, false, false, rule))
}

fn try_load_column_pane(path: PathBuf, rule: ColumnViewRule) -> Result<ColumnPane, String> {
    let (mut entries, skipped) = load_directory_snapshot(&path, rule.sort)
        .map(|snapshot| (snapshot.entries, snapshot.skipped))?;
    if shell_fs::is_shell_namespace_path(&path) {
        return Ok(ColumnPane::new(
            path, entries, skipped, false, false, false, rule,
        ));
    }
    let snapshot = load_vcs_snapshot(&path);
    apply_statuses(&mut entries, &path, &snapshot);
    Ok(ColumnPane::new(
        path,
        entries,
        skipped,
        false,
        snapshot.git_available(),
        snapshot.svn_available(),
        rule,
    ))
}

fn column_pane_action(path: &Path, command: ColumnPaneCommand) -> Box<dyn Action> {
    Box::new(ColumnPaneAction {
        path: path.to_path_buf(),
        command,
    })
}

fn build_column_control_menu(
    menu: PopupMenu,
    path: &Path,
    rules: ColumnViewRules,
    layout: ColumnLayoutState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let sort_path = path.to_path_buf();
    let filter_path = path.to_path_buf();
    let group_path = path.to_path_buf();
    let layout_path = path.to_path_buf();
    let reset_path = path.to_path_buf();
    let is_default = rules.sort == SortSpec::default()
        && rules.filter == ColumnFilter::All
        && rules.group == ColumnGroup::None;

    menu.min_w(px(250.))
        .submenu_with_icon(
            Some(Icon::new(IconName::ArrowUpDown).size_4()),
            "排序",
            window,
            cx,
            move |menu, _, _| {
                let action_path = sort_path.clone();
                menu.item(
                    PopupMenuItem::new("名称：升序")
                        .checked(rules.sort.key == SortKey::Name && rules.sort.ascending)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SortNameAscending,
                        )),
                )
                .item(
                    PopupMenuItem::new("名称：降序")
                        .checked(rules.sort.key == SortKey::Name && !rules.sort.ascending)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SortNameDescending,
                        )),
                )
                .separator()
                .item(
                    PopupMenuItem::new("修改日期：最新优先")
                        .checked(rules.sort.key == SortKey::Modified && !rules.sort.ascending)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SortModifiedNewest,
                        )),
                )
                .item(
                    PopupMenuItem::new("修改日期：最旧优先")
                        .checked(rules.sort.key == SortKey::Modified && rules.sort.ascending)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SortModifiedOldest,
                        )),
                )
                .item(
                    PopupMenuItem::new("类型")
                        .checked(rules.sort.key == SortKey::Kind)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SortKind,
                        )),
                )
                .item(
                    PopupMenuItem::new("大小：从大到小")
                        .checked(rules.sort.key == SortKey::Size && !rules.sort.ascending)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SortSizeDescending,
                        )),
                )
            },
        )
        .submenu_with_icon(
            Some(Icon::new(IconName::ListFilter).size_4()),
            "筛选",
            window,
            cx,
            move |menu, _, _| {
                let action_path = filter_path.clone();
                menu.item(
                    PopupMenuItem::new("全部项目")
                        .checked(rules.filter == ColumnFilter::All)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::FilterAll,
                        )),
                )
                .item(
                    PopupMenuItem::new("仅文件夹")
                        .checked(rules.filter == ColumnFilter::Folders)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::FilterFolders,
                        )),
                )
                .item(
                    PopupMenuItem::new("仅文件")
                        .checked(rules.filter == ColumnFilter::Files)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::FilterFiles,
                        )),
                )
                .item(
                    PopupMenuItem::new("媒体文件")
                        .checked(rules.filter == ColumnFilter::Media)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::FilterMedia,
                        )),
                )
                .item(
                    PopupMenuItem::new("文档文件")
                        .checked(rules.filter == ColumnFilter::Documents)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::FilterDocuments,
                        )),
                )
            },
        )
        .submenu_with_icon(
            Some(Icon::new(IconName::Group).size_4()),
            "分组规则",
            window,
            cx,
            move |menu, _, _| {
                let action_path = group_path.clone();
                menu.item(
                    PopupMenuItem::new("不分组")
                        .checked(rules.group == ColumnGroup::None)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::GroupNone,
                        )),
                )
                .item(
                    PopupMenuItem::new("按类型")
                        .checked(rules.group == ColumnGroup::Kind)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::GroupKind,
                        )),
                )
                .item(
                    PopupMenuItem::new("按扩展名")
                        .checked(rules.group == ColumnGroup::Extension)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::GroupExtension,
                        )),
                )
                .item(
                    PopupMenuItem::new("按修改日期")
                        .checked(rules.group == ColumnGroup::Modified)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::GroupModified,
                        )),
                )
            },
        )
        .submenu_with_icon(
            Some(Icon::new(IconName::Grid2x2).size_4()),
            "列与布局",
            window,
            cx,
            move |menu, _, _| {
                let action_path = layout_path.clone();
                menu.item(
                    PopupMenuItem::new("显示修改日期列")
                        .checked(layout.show_date_column)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::ToggleDateColumn,
                        )),
                )
                .item(
                    PopupMenuItem::new("显示类型列")
                        .checked(layout.show_kind_column)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::ToggleKindColumn,
                        )),
                )
                .item(
                    PopupMenuItem::new("显示大小列")
                        .checked(layout.show_size_column)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::ToggleSizeColumn,
                        )),
                )
                .separator()
                .item(
                    PopupMenuItem::new("紧凑宽度")
                        .checked(layout.preferred_width == 250)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SetCompactColumnWidth,
                        )),
                )
                .item(
                    PopupMenuItem::new("标准宽度")
                        .checked(layout.preferred_width == 320)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SetStandardColumnWidth,
                        )),
                )
                .item(
                    PopupMenuItem::new("宽列")
                        .checked(layout.preferred_width == 420)
                        .action(column_pane_action(
                            &action_path,
                            ColumnPaneCommand::SetWideColumnWidth,
                        )),
                )
                .separator()
                .item(
                    PopupMenuItem::new("重置列与布局").action(column_pane_action(
                        &action_path,
                        ColumnPaneCommand::ResetLayout,
                    )),
                )
            },
        )
        .separator()
        .menu_with_disabled(
            "重置分栏视图",
            column_pane_action(&reset_path, ColumnPaneCommand::ResetView),
            is_default,
        )
}

fn column_context_menu_target(pane: &ColumnPane, can_paste: bool) -> ContextMenuTarget {
    let title = if shell_fs::is_shell_namespace_path(&pane.path) {
        shell_fs::display_name(&pane.path)
    } else {
        None
    }
    .or_else(|| {
        pane.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
    .unwrap_or_else(|| pane.path.display().to_string());
    ContextMenuTarget::directory(
        pane.path.clone(),
        title,
        ContextMenuCapabilities::for_path(
            &pane.path,
            pane.loading,
            can_paste,
            pane.git_available,
            pane.svn_available,
        ),
    )
}

fn path_segments(path: &Path, final_is_file: bool) -> Vec<PathSegment> {
    if shell_fs::is_shell_namespace_path(path) {
        return vec![PathSegment {
            label: shell_fs::display_name(path)
                .unwrap_or_else(|| path.display().to_string())
                .into(),
            path: path.to_path_buf(),
            clickable: false,
        }];
    }
    let mut ancestors = path.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
    ancestors.reverse();
    let count = ancestors.len();
    ancestors
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| path.display().to_string());
            PathSegment {
                label: label.into(),
                path,
                clickable: !final_is_file || index + 1 < count,
            }
        })
        .collect()
}

fn loaded_column_index(panes: &[ColumnPane], path: &Path) -> Option<usize> {
    panes
        .iter()
        .position(|pane| !pane.loading && same_path(&pane.path, path))
}

fn column_count_after_file_selection(column: usize, current_count: usize) -> usize {
    (column + 1).min(current_count)
}

#[cfg(test)]
fn search_selection_step(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(index) if forward => (index + 1).min(len - 1),
        Some(index) => index.saturating_sub(1),
        None if forward => 0,
        None => len - 1,
    })
}

/// Steps over display rows that hold entries, skipping group headers.
#[cfg(test)]
fn entry_row_step(
    entry_rows: &[usize],
    current_row: Option<usize>,
    forward: bool,
) -> Option<usize> {
    let position =
        current_row.and_then(|row| entry_rows.iter().position(|candidate| *candidate == row));
    let next = search_selection_step(position, entry_rows.len(), forward)?;
    entry_rows.get(next).copied()
}

#[cfg(test)]
fn search_query_narrows(previous: &str, next: &str) -> bool {
    let previous = previous.trim().to_lowercase();
    let next = next.trim().to_lowercase();
    !previous.is_empty() && next.contains(&previous)
}

#[cfg(test)]
fn narrow_search_entries(entries: &[FileEntry], query: &str) -> Vec<FileEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|entry| entry.name.to_lowercase().contains(&query))
        .cloned()
        .collect()
}

impl Focusable for KiteApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<TabRequest> for KiteApp {}

impl Render for KiteApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.loading {
            kite_core::perf_trace::mark_once("windows.first_content_render");
        }
        let selected_entries = self.selected_entries(cx);
        let selected_count = selected_entries.len();
        let selected = selected_entries.first().cloned();
        let selected_exists = selected.is_some();
        let column_mode = self.view_mode == ViewMode::Columns && self.active_search.is_none();
        let item_count = if column_mode {
            self.column_panes
                .last()
                .map(|pane| pane.entries.len())
                .unwrap_or(0)
        } else {
            self.table.read(cx).delegate().entries().len()
        };
        let selected_size = if selected_count == 1 {
            selected
                .as_ref()
                .map(FileEntry::size_label)
                .unwrap_or_default()
        } else if selected_entries.iter().any(|entry| !entry.is_dir()) {
            format_size(
                selected_entries
                    .iter()
                    .filter(|entry| !entry.is_dir())
                    .map(|entry| entry.size)
                    .sum(),
            )
        } else {
            String::new()
        };
        let error = self.error.clone();
        let notice = self.notice.clone();
        let update_download_fraction = self
            .update_download_progress
            .and_then(|progress| progress.fraction())
            .map(|fraction| fraction as f32);
        let loading = self.loading;
        let active_search = self.active_search.clone();
        let search_truncated = self.search_truncated;
        let skipped = self.skipped;
        if self.pending_git_refresh {
            self.pending_git_refresh = false;
            let app = cx.entity();
            window.defer(cx, move |window, cx| {
                app.update(cx, |this, cx| this.refresh(window, cx));
            });
        }
        if let Some(path) = self.pending_git_open.take() {
            let app = cx.entity();
            window.defer(cx, move |window, cx| {
                app.update(cx, |this, cx| {
                    this.navigate_to(path.clone(), NavigationIntent::Push, window, cx)
                });
            });
        }
        if self.column_scroll_pending {
            self.column_scroll_pending = false;
            let handle = self.column_scroll.clone();
            window.defer(cx, move |_, _| {
                handle.set_offset(point(-handle.max_offset().x, handle.offset().y));
            });
        }
        if self.show_path_bar && !self.address_editing && self.path_bar_scroll_pending {
            self.path_bar_scroll_pending = false;
            let handle = self.path_bar_scroll.clone();
            window.defer(cx, move |_, _| {
                handle.set_offset(point(-handle.max_offset().x, handle.offset().y));
            });
        }

        let footer_left = if let Some(error) = error.clone() {
            error
        } else if loading {
            if let Some(search) = &active_search {
                SharedString::from(format!("正在搜索“{}”…", search.query))
            } else {
                SharedString::from("正在读取文件夹…")
            }
        } else if let Some(search) = &active_search {
            if search_truncated {
                SharedString::from(format!("{} 个匹配项目 · 仅显示前 1000 个结果", item_count))
            } else {
                SharedString::from(format!(
                    "{} 个匹配项目 · 搜索“{}”",
                    item_count, search.query
                ))
            }
        } else {
            SharedString::from(format!("{} 个项目", item_count))
        };
        let footer_right = if let Some(notice) = notice {
            notice
        } else if selected_exists {
            SharedString::from(if selected_size.is_empty() {
                format!("已选择 {selected_count} 项")
            } else {
                format!("已选择 {selected_count} 项 · {selected_size}")
            })
        } else if skipped > 0 {
            SharedString::from(format!("已跳过 {skipped} 个不可读取项目"))
        } else if column_mode {
            SharedString::from("Ctrl+1 列表 · Ctrl+2 分栏 · Alt+P 路径栏")
        } else {
            SharedString::from("Alt+← 返回 · Alt+↑ 上一级 · Ctrl+L 地址栏")
        };

        let search_banner = active_search.as_ref().map(|search| {
            h_flex()
                .h(px(34.))
                .px_3()
                .gap_2()
                .items_center()
                .bg(cx.theme().accent.opacity(0.35))
                .border_b_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::Search).size_3p5())
                .child(format!(
                    "在 {} 中搜索“{}”",
                    search.root.display(),
                    search.query
                ))
                .child(
                    Button::new("clear-search-banner")
                        .ghost()
                        .compact()
                        .icon(IconName::X)
                        .tooltip("清除搜索 (Esc)")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.clear_search_or_modal(window, cx)
                        })),
                )
                .into_any_element()
        });
        let git_operation_banner = self.render_git_operation_banner(cx);
        let clone_cleanup_banner = self.render_clone_cleanup_banner(cx);
        let git_recovery_banner = self.render_git_recovery_banner(cx);

        let show_sidebar = self.show_sidebar;
        let sidebar = self.render_sidebar(cx);
        let main_view = if column_mode {
            self.render_columns(window, cx)
        } else {
            let menu_target = self
                .table
                .read(cx)
                .delegate()
                .directory_context_menu_target();
            let action_context = self.focus_handle.clone();
            div()
                .size_full()
                .context_menu(move |menu, window, cx| {
                    render_context_menu(menu, &menu_target, action_context.clone(), window, cx)
                })
                .child(
                    DataTable::new(&self.table)
                        .with_size(Size::Small)
                        .bordered(false)
                        .stripe(false)
                        .scroll_bounce(false)
                        .scrollbar_visible(false, true),
                )
                .into_any_element()
        };
        v_flex()
            .id("kite-root")
            .key_context("Kite")
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(Self::handle_global_key))
            .relative()
            .size_full()
            .gap(px(ISLAND_GAP))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(
                cx.listener(|this, _: &NavigateBack, window, cx| this.navigate_back(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
                this.navigate_forward(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NavigateUp, window, cx| this.navigate_up(window, cx)))
            .on_action(cx.listener(|this, _: &RefreshFolder, window, cx| this.refresh(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenSelection, window, cx| this.open_selected(window, cx)),
            )
            .on_action(cx.listener(Self::on_context_menu_action))
            .on_action(cx.listener(Self::on_column_pane_action))
            .on_action(cx.listener(Self::show_list_view))
            .on_action(cx.listener(Self::show_columns_view))
            .on_action(cx.listener(Self::toggle_path_bar))
            .on_action(cx.listener(|this, _: &CopySelection, _window, cx| {
                this.set_clipboard(ClipboardMode::Copy, cx)
            }))
            .on_action(cx.listener(|this, _: &CutSelection, _window, cx| {
                this.set_clipboard(ClipboardMode::Cut, cx)
            }))
            .on_action(
                cx.listener(|this, _: &PasteSelection, window, cx| {
                    this.paste_clipboard(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(
                cx.listener(|this, _: &FocusAddress, window, cx| this.focus_address(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)),
            )
            .on_action(cx.listener(|this, action: &OpenGlobalSearch, window, cx| {
                this.open_global_search(action, window, cx)
            }))
            .on_action(cx.listener(|this, action: &OpenGitClient, window, cx| {
                this.open_git_client_for_current(action, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &CreateFolder, window, cx| this.begin_new_folder(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &RenameSelection, window, cx| this.begin_rename(window, cx)),
            )
            .on_action(cx.listener(|this, _: &RecycleSelection, window, cx| {
                this.confirm_recycle(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ClearSearchOverlay, window, cx| {
                this.clear_search_or_modal(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewTab, _, cx| this.request_tab(TabRequest::New, cx)))
            .on_action(
                cx.listener(|this, _: &CloseTab, _, cx| this.request_tab(TabRequest::Close, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NextTab, _, cx| this.request_tab(TabRequest::Next, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| {
                this.request_tab(TabRequest::Previous, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings_window(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenWorkspaceEditor, window, cx| {
                this.open_workspace_editor(window, cx)
            }))
            .on_action(cx.listener(Self::check_for_updates))
            .on_action(cx.listener(Self::set_default_explorer))
            .on_action(cx.listener(Self::restore_default_explorer))
            .on_action(cx.listener(Self::create_start_menu_shortcut))
            .on_drag_move::<KiteFileDrag>(|event, window, cx| {
                let position = event.event.position;
                let viewport = window.viewport_size();
                let outside = position.x < px(0.)
                    || position.y < px(0.)
                    || position.x > viewport.width
                    || position.y > viewport.height;
                if !outside {
                    return;
                }
                let paths = event
                    .drag(cx)
                    .paths
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>();
                file_clipboard::trace_drag(&format!(
                    "GPUI drag left window at ({:.1},{:.1}) paths={:?}",
                    position.x.as_f32(),
                    position.y.as_f32(),
                    paths
                ));
                // SHDoDragDrop enters a nested Windows message loop. Post a native
                // window message so OLE starts after this GPUI input event returns.
                if let Err(error) = file_clipboard::request_file_drag(window, &paths) {
                    file_clipboard::trace_drag(&format!("failed to queue native drag: {error}"));
                    eprintln!("failed to queue native file drag: {error}");
                    return;
                }
                cx.stop_active_drag(window);
            })
            .child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .gap(px(ISLAND_GAP))
                    .when(show_sidebar, |this| this.child(sidebar))
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .min_h_0()
                            .rounded_lg()
                            .overflow_hidden()
                            .bg(cx.theme().title_bar)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(if loading {
                                div()
                                    .h(px(2.))
                                    .w_full()
                                    .bg(cx.theme().primary)
                                    .into_any_element()
                            } else {
                                div()
                                    .h(px(2.))
                                    .w_full()
                                    .bg(cx.theme().background)
                                    .into_any_element()
                            })
                            .when_some(search_banner, |this, banner| this.child(banner))
                            .when_some(git_operation_banner, |this, banner| this.child(banner))
                            .when_some(clone_cleanup_banner, |this, banner| this.child(banner))
                            .when_some(git_recovery_banner, |this, banner| this.child(banner))
                            .child(div().flex_1().min_h_0().w_full().child(main_view)),
                    ),
            )
            .child(
                h_flex()
                    .h(px(STATUS_BAR_HEIGHT))
                    .w_full()
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .rounded_lg()
                    .bg(cx.theme().status_bar)
                    .border_1()
                    .border_color(cx.theme().status_bar_border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(error.is_some(), |this| this.text_color(cx.theme().danger))
                            .child(footer_left),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when_some(update_download_fraction, |this, fraction| {
                                this.child(
                                    div()
                                        .w(px(64.))
                                        .h(px(4.))
                                        .rounded_md()
                                        .overflow_hidden()
                                        .bg(cx.theme().border)
                                        .child(
                                            div()
                                                .h_full()
                                                .w(px((64. * fraction).max(2.)))
                                                .rounded_md()
                                                .bg(cx.theme().primary),
                                        ),
                                )
                            })
                            .child(div().max_w(px(500.)).truncate().child(footer_right)),
                    ),
            )
            .child(self.render_git_dialog(cx))
            .child(self.render_modal(cx))
    }
}

#[cfg(test)]
mod clipboard_destination_tests {
    use super::paste_destination;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn file_target_pastes_into_the_working_directory() {
        let destination =
            paste_destination(Path::new(r"C:\repo\src\main.rs"), Path::new(r"C:\repo\src"));
        assert_eq!(destination, PathBuf::from(r"C:\repo\src"));
    }

    #[test]
    fn directory_target_pastes_inside_the_directory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kite-paste-destination-{}-{unique}",
            std::process::id()
        ));
        let target = root.join("src");
        std::fs::create_dir_all(&target).expect("create target directory");

        let destination = paste_destination(&target, &root);
        assert_eq!(destination, target);
        std::fs::remove_dir_all(root).expect("remove test directory");
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::{ClipboardMode, FileClipboard, SharedClipboard};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    #[test]
    fn shared_clipboard_changes_are_visible_to_every_handle() {
        let source: SharedClipboard = Rc::new(RefCell::new(None));
        let destination = source.clone();

        *source.borrow_mut() = Some(FileClipboard {
            paths: vec![PathBuf::from(r"C:\example.txt")],
            mode: ClipboardMode::Copy,
        });

        assert!(destination.borrow().is_some());
        *destination.borrow_mut() = None;
        assert!(source.borrow().is_none());
    }
}

#[cfg(test)]
mod update_ui_tests {
    use super::{update_download_notice, update_notes_preview};
    use crate::updater::UpdateDownloadProgress;

    #[test]
    fn download_notice_includes_percent_and_size() {
        let notice = update_download_notice(
            "0.6.73",
            UpdateDownloadProgress {
                downloaded_bytes: 1024 * 1024,
                total_bytes: 2 * 1024 * 1024,
                active: true,
            },
        );

        assert!(notice.contains("50%"));
        assert!(notice.contains("1.0 MB / 2.0 MB"));
    }

    #[test]
    fn download_notice_handles_unknown_total_size() {
        let notice = update_download_notice(
            "0.6.73",
            UpdateDownloadProgress {
                downloaded_bytes: 1024,
                total_bytes: 0,
                active: true,
            },
        );

        assert!(!notice.contains('%'));
        assert!(notice.contains("1.0 KB"));
    }

    #[test]
    fn settings_notes_preview_keeps_user_facing_sections() {
        let notes = crate::update_notes::parse_release_notes(
            "## Highlights\n- Faster directory loading.\n\n## Verification\n- Windows release build.\n",
        );
        let preview = update_notes_preview(&notes).expect("preview");

        assert!(preview.contains("Highlights"));
        assert!(preview.contains("Faster directory loading."));
        assert!(!preview.contains("Verification"));
    }
}

#[cfg(test)]
mod storage_summary_tests {
    use super::{StoragePressure, StorageSummary, drive_letter_label, storage_capacity_label};
    use std::path::Path;

    fn summary(free_bytes: u64, total_bytes: u64) -> StorageSummary {
        StorageSummary {
            drive_letter: None,
            free_bytes: Some(free_bytes),
            total_bytes: Some(total_bytes),
        }
    }

    #[test]
    fn formats_capacity_with_free_space_first() {
        let free = 300 * 1024_u64.pow(3);
        let total = 500 * 1024_u64.pow(3);

        assert_eq!(
            storage_capacity_label(Some(free), Some(total)).as_deref(),
            Some("300.0 GB 可用 / 500.0 GB")
        );
    }

    #[test]
    fn capacity_label_keeps_each_known_value() {
        assert_eq!(
            storage_capacity_label(Some(1024), None).as_deref(),
            Some("1.0 KB 可用")
        );
        assert_eq!(
            storage_capacity_label(None, Some(1024)).as_deref(),
            Some("1.0 KB")
        );
        assert!(storage_capacity_label(None, None).is_none());
    }

    #[test]
    fn storage_pressure_uses_free_space_percentage() {
        assert_eq!(summary(100, 100).pressure(), StoragePressure::Normal);
        assert_eq!(summary(10, 100).pressure(), StoragePressure::Normal);
        assert_eq!(summary(9, 100).pressure(), StoragePressure::Low);
        assert_eq!(summary(5, 100).pressure(), StoragePressure::Low);
        assert_eq!(summary(4, 100).pressure(), StoragePressure::Critical);
    }

    #[test]
    fn extracts_drive_letter_for_windows_roots() {
        assert_eq!(drive_letter_label(Path::new(r"C:\")).as_deref(), Some("C:"));
        assert!(drive_letter_label(Path::new(r"\\server\share")).is_none());
    }
}

#[cfg(test)]
mod launch_target_tests {
    use super::{AppLaunchTarget, resolve_location_input};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("kite-launch-{label}-{unique}"))
    }

    #[test]
    fn file_path_uses_the_containing_directory_and_keeps_the_file() {
        let root = temporary_directory("file");
        fs::create_dir_all(&root).expect("create test directory");
        let file = root.join("app_config 工作架构与运行原理.md");
        fs::write(&file, "notes").expect("write test file");

        let resolved = resolve_location_input(&format!("\"{}\"", file.display()), &root)
            .expect("resolve quoted file path");
        let target = AppLaunchTarget::from_existing_path(resolved).expect("classify file path");

        assert_eq!(target.directory, root);
        assert_eq!(target.file.as_deref(), Some(file.as_path()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn directory_path_stays_a_directory_target() {
        let root = temporary_directory("directory");
        fs::create_dir_all(&root).expect("create test directory");

        let target = AppLaunchTarget::from_existing_path(root.clone()).expect("classify directory");

        assert_eq!(target.directory, root);
        assert!(target.file.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_path_is_not_classified_as_a_file() {
        let root = temporary_directory("missing");
        let missing = root.join("missing.md");

        assert_eq!(AppLaunchTarget::from_existing_path(missing), None);
    }
}

#[cfg(test)]
mod path_bar_tests {
    use super::{
        ColumnDisplayRow, ColumnFilter, ColumnPane, column_chain, column_context_menu_target,
        column_count_after_file_selection, common_column_prefix, loaded_column_index,
        path_after_recycle, path_after_recycle_paths, path_after_rename,
    };
    use crate::fs::{EntryKind, FileEntry, SortKey, SortSpec};
    use crate::ui_preferences::{ColumnGroupPreference as ColumnGroup, ColumnViewRule};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    fn pane(path: &str, loading: bool) -> ColumnPane {
        ColumnPane::new(
            PathBuf::from(path),
            Vec::new(),
            0,
            loading,
            false,
            false,
            ColumnViewRule::default(),
        )
    }

    fn entry(path: &str, kind: EntryKind) -> FileEntry {
        let path = PathBuf::from(path);
        FileEntry {
            extension: path
                .extension()
                .map(|value| value.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
            name: path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
            kind,
            size: 0,
            modified: None,
            git_status: None,
            svn_status: None,
        }
    }

    #[test]
    fn column_filter_and_grouping_change_visible_rows() {
        let root = PathBuf::from(r"C:\repo");
        let mut column = ColumnPane::new(
            root.clone(),
            vec![
                entry(r"C:\repo\src", EntryKind::Directory),
                entry(r"C:\repo\notes.txt", EntryKind::File),
                entry(r"C:\repo\photo.png", EntryKind::File),
            ],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );

        column.filter = ColumnFilter::Documents;
        column.refresh_view();
        assert_eq!(column.entries.len(), 1);
        assert_eq!(column.entries[0].name, "notes.txt");

        column.filter = ColumnFilter::All;
        column.group = ColumnGroup::Extension;
        column.refresh_view();
        let rows = column.display_rows();
        let groups = rows
            .iter()
            .filter_map(|row| match row {
                ColumnDisplayRow::Group { label, .. } => Some(label.to_string()),
                ColumnDisplayRow::Entry(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(groups, vec!["文件夹", "PNG", "TXT"]);

        column.toggle_group_collapse("PNG");
        let collapsed_rows = column.display_rows();
        assert!(collapsed_rows.iter().any(|row| matches!(
            row,
            ColumnDisplayRow::Group { label, count, collapsed }
                if label.as_ref() == "PNG" && *count == 1 && *collapsed
        )));
        assert!(!collapsed_rows.iter().any(|row| matches!(
            row,
            ColumnDisplayRow::Entry(entry) if entry.name == "photo.png"
        )));
    }

    #[test]
    fn column_sort_button_toggles_current_sort_direction() {
        let root = PathBuf::from(r"C:\repo");
        let mut column = ColumnPane::new(
            root,
            vec![
                entry(r"C:\repo\zeta.txt", EntryKind::File),
                entry(r"C:\repo\alpha.txt", EntryKind::File),
            ],
            0,
            false,
            false,
            false,
            ColumnViewRule {
                sort: SortSpec {
                    key: SortKey::Name,
                    ascending: true,
                },
                ..ColumnViewRule::default()
            },
        );
        assert_eq!(column.entries[0].name, "alpha.txt");
        assert_eq!(column.entries[1].name, "zeta.txt");

        column.toggle_sort_order();

        assert_eq!(
            column.sort,
            SortSpec {
                key: SortKey::Name,
                ascending: false,
            }
        );
        assert_eq!(column.entries[0].name, "zeta.txt");
        assert_eq!(column.entries[1].name, "alpha.txt");
    }

    #[test]
    fn column_header_selects_field_and_toggles_direction() {
        let root = PathBuf::from(r"C:\repo");
        let mut older = entry(r"C:\repo\older.txt", EntryKind::File);
        older.modified = Some(UNIX_EPOCH + Duration::from_secs(100));
        let mut newer = entry(r"C:\repo\newer.txt", EntryKind::File);
        newer.modified = Some(UNIX_EPOCH + Duration::from_secs(200));
        let mut column = ColumnPane::new(
            root,
            vec![older, newer],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );

        column.toggle_sort_for_header(SortKey::Modified);
        assert_eq!(
            column.sort,
            SortSpec {
                key: SortKey::Modified,
                ascending: false,
            }
        );
        assert_eq!(column.entries[0].name, "newer.txt");

        column.toggle_sort_for_header(SortKey::Modified);
        assert_eq!(
            column.sort,
            SortSpec {
                key: SortKey::Modified,
                ascending: true,
            }
        );
        assert_eq!(column.entries[0].name, "older.txt");

        column.toggle_sort_for_header(SortKey::Size);
        assert_eq!(column.sort.key, SortKey::Size);
        assert!(!column.sort.ascending);
    }

    #[test]
    fn column_search_results_restore_the_directory_listing() {
        let root = PathBuf::from(r"C:\repo");
        let mut column = ColumnPane::new(
            root.clone(),
            vec![entry(r"C:\repo\notes.txt", EntryKind::File)],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );

        column.set_search_results(
            "target".to_string(),
            vec![entry(r"C:\repo\nested\target.md", EntryKind::File)],
            true,
        );
        assert_eq!(column.search_query.as_deref(), Some("target"));
        assert!(column.search_truncated);
        assert_eq!(column.entries[0].name, "target.md");

        column.clear_search_results();
        assert!(column.search_query.is_none());
        assert!(!column.search_truncated);
        assert_eq!(column.entries[0].name, "notes.txt");
    }

    #[test]
    fn column_selection_supports_toggle_and_visible_range() {
        let root = PathBuf::from(r"C:\repo");
        let mut column = ColumnPane::new(
            root,
            vec![
                entry(r"C:\repo\alpha.txt", EntryKind::File),
                entry(r"C:\repo\bravo.txt", EntryKind::File),
                entry(r"C:\repo\charlie.txt", EntryKind::File),
            ],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );

        column.set_single_selection(PathBuf::from(r"C:\repo\alpha.txt"));
        column.toggle_selection(PathBuf::from(r"C:\repo\charlie.txt"));
        assert_eq!(column.selected_paths.len(), 2);
        assert!(column.is_selected(Path::new(r"C:\repo\charlie.txt")));
        assert_eq!(
            column.selected_path,
            Some(PathBuf::from(r"C:\repo\charlie.txt"))
        );

        column.toggle_selection(PathBuf::from(r"C:\repo\charlie.txt"));
        assert_eq!(
            column.selected_path,
            Some(PathBuf::from(r"C:\repo\alpha.txt"))
        );

        column.selection_anchor_path = Some(PathBuf::from(r"C:\repo\alpha.txt"));
        column.select_range_to(PathBuf::from(r"C:\repo\bravo.txt"));
        assert_eq!(column.selected_paths.len(), 2);
        assert!(column.is_selected(Path::new(r"C:\repo\alpha.txt")));
        assert!(column.is_selected(Path::new(r"C:\repo\bravo.txt")));
        assert!(!column.is_selected(Path::new(r"C:\repo\charlie.txt")));
    }

    #[test]
    fn batch_recycle_uses_the_nearest_removed_ancestor() {
        assert_eq!(
            path_after_recycle_paths(
                Path::new(r"C:\repo\src\ui"),
                &[PathBuf::from(r"C:\repo\src")],
            ),
            PathBuf::from(r"C:\repo")
        );
    }

    #[test]
    fn shell_namespace_path_uses_one_column() {
        let path = PathBuf::from(r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\phone\photo.jpg");
        assert_eq!(column_chain(&path), vec![path]);
    }

    #[test]
    fn path_bar_reuses_only_loaded_columns() {
        let panes = [
            pane(r"C:\", false),
            pane(r"C:\Users", false),
            pane(r"C:\Users\Example", true),
        ];

        assert_eq!(loaded_column_index(&panes, Path::new(r"C:\Users")), Some(1));
        assert_eq!(
            loaded_column_index(&panes, Path::new(r"C:\Users\Example")),
            None
        );
        assert_eq!(loaded_column_index(&panes, Path::new(r"C:\Windows")), None);
    }

    #[test]
    fn column_update_keeps_only_loaded_common_prefix() {
        let panes = [
            pane(r"C:\", false),
            pane(r"C:\Users", false),
            pane(r"C:\Users\Example", true),
        ];
        let chain = [
            PathBuf::from(r"C:\"),
            PathBuf::from(r"C:\Users"),
            PathBuf::from(r"C:\Users\Example"),
            PathBuf::from(r"C:\Users\Example\Documents"),
        ];

        assert_eq!(common_column_prefix(&panes, &chain), 2);
    }

    #[test]
    fn file_selection_removes_deeper_columns() {
        assert_eq!(column_count_after_file_selection(0, 3), 1);
        assert_eq!(column_count_after_file_selection(1, 3), 2);
        assert_eq!(column_count_after_file_selection(2, 3), 3);
    }

    #[test]
    fn column_context_menu_targets_that_directory() {
        let mut column = pane(r"C:\Users\Example\Documents", false);
        column.git_available = true;

        let target = column_context_menu_target(&column, true);

        assert_eq!(target.path, column.path);
        assert_eq!(target.working_directory, column.path);
        assert_eq!(target.title, "Documents");
        assert!(target.is_dir);
        assert!(target.git_available);
    }

    #[test]
    fn renaming_an_ancestor_rewrites_the_current_path() {
        assert_eq!(
            path_after_rename(
                Path::new(r"C:\repo\src\ui"),
                Path::new(r"C:\repo\src"),
                Path::new(r"C:\repo\source"),
            ),
            PathBuf::from(r"C:\repo\source\ui")
        );
        assert_eq!(
            path_after_rename(
                Path::new(r"C:\repo\docs"),
                Path::new(r"C:\repo\src"),
                Path::new(r"C:\repo\source"),
            ),
            PathBuf::from(r"C:\repo\docs")
        );
    }

    #[test]
    fn recycling_an_ancestor_moves_the_current_path_to_its_parent() {
        assert_eq!(
            path_after_recycle(Path::new(r"C:\repo\src\ui"), Path::new(r"C:\repo\src"),),
            PathBuf::from(r"C:\repo")
        );
        assert_eq!(
            path_after_recycle(Path::new(r"C:\repo\src"), Path::new(r"C:\repo\src\ui"),),
            PathBuf::from(r"C:\repo\src")
        );
    }
}

#[cfg(test)]
mod preview_tests {
    use super::{
        is_markdown_preview_entry, is_text_preview_entry, preview_info_text,
        preview_more_menu_items, preview_text_lines, read_log_preview,
    };
    use crate::actions::ContextMenuCommand;
    use crate::fs::{EntryKind, FileEntry, VcsStatus};
    use gpui_kit::SharedString;
    use kite_core::preview::read_text_preview;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn preview_more_menu_contains_file_actions() {
        let commands = preview_more_menu_items()
            .iter()
            .map(|(_, _, command)| *command)
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec![
                ContextMenuCommand::Open,
                ContextMenuCommand::Copy,
                ContextMenuCommand::CopyPath,
                ContextMenuCommand::ShowProperties,
                ContextMenuCommand::SystemMenu,
            ]
        );
    }

    #[test]
    fn txt_and_markdown_entries_use_markdown_preview() {
        let make_entry = |extension: &str| FileEntry {
            path: PathBuf::from(format!(
                r"C:\demo
otes.{extension}"
            )),
            name: format!("notes.{extension}"),
            kind: EntryKind::File,
            size: 128,
            modified: None,
            extension: extension.to_string(),
            git_status: None,
            svn_status: None,
        };

        assert!(is_markdown_preview_entry(&make_entry("txt")));
        assert!(is_markdown_preview_entry(&make_entry("TXT")));
        assert!(is_markdown_preview_entry(&make_entry("md")));
        assert!(is_markdown_preview_entry(&make_entry("MARKDOWN")));
        assert!(is_markdown_preview_entry(&make_entry("MK")));
        assert!(!is_markdown_preview_entry(&make_entry("log")));
        assert!(is_text_preview_entry(&make_entry("markdown")));
        assert!(is_text_preview_entry(&make_entry("TXT")));
    }

    #[test]
    fn splits_preview_text_into_stable_lines() {
        assert!(preview_text_lines("").is_empty());
        let expected: Vec<SharedString> = vec!["first".into(), "second".into(), "".into()];
        assert_eq!(preview_text_lines("first\r\nsecond\n"), expected);
    }

    #[test]
    fn reads_text_preview_and_rejects_binary_content() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kite-preview-{unique}"));
        fs::create_dir_all(&root).expect("create preview directory");
        let text_path = root.join("notes.txt");
        let binary_path = root.join("binary.txt");
        fs::write(&text_path, "first line\nsecond line\n").expect("write text preview");
        fs::write(&binary_path, b"text\0binary").expect("write binary preview");

        let preview = read_text_preview(&text_path).expect("read text preview");
        assert!(preview.contains("first line"));
        assert!(read_text_preview(&binary_path).is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preview_copy_text_includes_properties_and_vcs() {
        let entry = FileEntry {
            path: PathBuf::from(r"C:\demo\notes.txt"),
            name: "notes.txt".to_string(),
            kind: EntryKind::File,
            size: 1_536,
            modified: None,
            extension: "txt".to_string(),
            git_status: Some(VcsStatus::Modified),
            svn_status: None,
        };

        let text = preview_info_text(&entry);

        assert!(text.contains("名称：notes.txt"));
        assert!(text.contains("位置：C:\\demo\\notes.txt"));
        assert!(text.contains("Git：已修改 (M)"));
    }

    #[test]
    fn large_log_preview_reads_only_the_tail() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kite-log-preview-{unique}"));
        fs::create_dir_all(&root).expect("create log preview directory");
        let path = root.join("application.log");
        let mut content = String::from("oldest-entry\n");
        content.push_str(&"a".repeat(128 * 1024));
        content.push_str("\nnewest-entry\n");
        fs::write(&path, content).expect("write log preview");

        let preview = read_log_preview(&path).expect("read log preview");

        assert!(preview.starts_with("日志末尾预览："));
        assert!(preview.contains("newest-entry"));
        assert!(!preview.contains("oldest-entry"));
        assert!(preview.chars().count() < 7_000);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_files_ignore_the_regular_text_size_limit() {
        let make_entry = |extension: &str| FileEntry {
            path: PathBuf::from(format!(r"C:\demo\application.{extension}")),
            name: format!("application.{extension}"),
            kind: EntryKind::File,
            size: 2 * 1024 * 1024,
            modified: None,
            extension: extension.to_string(),
            git_status: None,
            svn_status: None,
        };

        assert!(is_text_preview_entry(&make_entry("log")));
        assert!(is_text_preview_entry(&make_entry("LOG")));
        assert!(!is_text_preview_entry(&make_entry("txt")));
    }
}
#[cfg(test)]
mod column_details_tests {
    use super::{
        DETAILED_COLUMN_MAX_WIDTH, DETAILED_COLUMN_MIN_WIDTH, column_modified_label,
        column_size_label, detailed_column_width_for_name_width,
    };
    use crate::fs::{EntryKind, FileEntry};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    fn entry(is_dir: bool, size: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(r"C:\demo\example.mp4"),
            name: "example.mp4".to_string(),
            kind: if is_dir {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size,
            modified: Some(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
            extension: "mp4".to_string(),
            git_status: None,
            svn_status: None,
        }
    }

    #[test]
    fn detailed_column_width_is_clamped() {
        assert_eq!(
            detailed_column_width_for_name_width(0.0, false, false, false, 250),
            DETAILED_COLUMN_MIN_WIDTH
        );
        assert_eq!(
            detailed_column_width_for_name_width(10_000.0, true, true, true, 320),
            DETAILED_COLUMN_MAX_WIDTH
        );
    }

    #[test]
    fn hidden_detail_columns_reduce_the_required_width() {
        let all_columns = detailed_column_width_for_name_width(80.0, true, true, true, 250);
        let date_only = detailed_column_width_for_name_width(80.0, true, false, false, 250);

        assert!(date_only < all_columns);
    }

    #[test]
    fn detailed_size_uses_adaptive_units() {
        assert_eq!(column_size_label(&entry(false, 211)), "211 B");
        assert_eq!(column_size_label(&entry(false, 1536)), "1.5 KB");
        assert_eq!(column_size_label(&entry(false, 157_286_400)), "150.0 MB");
        assert_eq!(column_size_label(&entry(true, 157_286_400)), "—");
    }

    #[test]
    fn detailed_date_omits_time() {
        let label = column_modified_label(&entry(false, 0));
        assert!(!label.contains(':'));
    }
}

#[cfg(test)]
mod scroll_routing_tests {
    use super::{
        clone_target_name, directory_scroll_offset, horizontal_wheel_delta, wheel_scrolls_columns,
    };

    #[test]
    fn horizontal_gesture_takes_priority_over_vertical_noise() {
        assert!(wheel_scrolls_columns(18.0, 3.0));
        assert!(wheel_scrolls_columns(8.0, 7.0));
        assert!(!wheel_scrolls_columns(3.0, 18.0));
        assert!(!wheel_scrolls_columns(0.0, 18.0));
    }

    #[test]
    fn shift_vertical_wheel_scrolls_columns() {
        assert_eq!(horizontal_wheel_delta(true, 0.0, -90.0), Some(-90.0));
        assert_eq!(horizontal_wheel_delta(true, 0.0, 90.0), Some(90.0));
        assert_eq!(horizontal_wheel_delta(false, 0.0, -90.0), None);
    }

    #[test]
    fn horizontal_wheel_does_not_require_shift() {
        assert_eq!(horizontal_wheel_delta(false, 18.0, 3.0), Some(18.0));
    }

    #[test]
    fn directory_interior_preserves_vertical_offset() {
        assert_eq!(directory_scroll_offset(-420.0, 800.0), -420.0);
    }

    #[test]
    fn bottom_boundary_stops_vertical_offset() {
        assert_eq!(directory_scroll_offset(-830.0, 800.0), -800.0);
    }

    #[test]
    fn top_boundary_stops_vertical_offset() {
        assert_eq!(directory_scroll_offset(25.0, 800.0), 0.0);
    }

    #[test]
    fn non_scrollable_directory_stays_at_start() {
        assert_eq!(directory_scroll_offset(-90.0, 0.0), 0.0);
    }
    #[test]
    fn derives_clone_directory_from_common_git_urls() {
        assert_eq!(
            clone_target_name("https://example.com/team/kite.git"),
            "kite"
        );
        assert_eq!(clone_target_name("git@example.com:team/kite.git"), "kite");
        assert_eq!(clone_target_name("ssh://example.com/team/kite"), "kite");
        assert_eq!(clone_target_name("https://example.com/"), "example.com");
    }
}

#[cfg(test)]
mod column_search_tests {
    use super::{
        ColumnDisplayRow, ColumnPane, entry_row_step, narrow_search_entries, search_query_narrows,
        search_selection_step,
    };
    use crate::fs::{EntryKind, FileEntry};
    use crate::ui_preferences::ColumnViewRule;
    use std::path::PathBuf;

    fn entry(path: &str) -> FileEntry {
        let path = PathBuf::from(path);
        FileEntry {
            extension: path
                .extension()
                .map(|value| value.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
            name: path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
            kind: EntryKind::File,
            size: 0,
            modified: None,
            git_status: None,
            svn_status: None,
        }
    }

    #[test]
    fn steps_through_search_results_without_leaving_the_list() {
        assert_eq!(search_selection_step(None, 0, true), None);
        assert_eq!(search_selection_step(None, 3, true), Some(0));
        assert_eq!(search_selection_step(None, 3, false), Some(2));
        assert_eq!(search_selection_step(Some(0), 3, true), Some(1));
        assert_eq!(search_selection_step(Some(1), 3, false), Some(0));
        assert_eq!(search_selection_step(Some(2), 3, true), Some(2));
        assert_eq!(search_selection_step(Some(0), 3, false), Some(0));
    }

    #[test]
    fn selection_skips_group_headers() {
        let entry_rows = [1, 2, 4];
        assert_eq!(entry_row_step(&entry_rows, None, true), Some(1));
        assert_eq!(entry_row_step(&entry_rows, None, false), Some(4));
        assert_eq!(entry_row_step(&entry_rows, Some(1), true), Some(2));
        assert_eq!(entry_row_step(&entry_rows, Some(2), true), Some(4));
        assert_eq!(entry_row_step(&entry_rows, Some(4), true), Some(4));
        assert_eq!(entry_row_step(&entry_rows, Some(2), false), Some(1));
        assert_eq!(entry_row_step(&entry_rows, Some(0), true), Some(1));
        assert_eq!(entry_row_step(&[], None, true), None);
    }

    #[test]
    fn search_results_keep_group_headers_and_rank_order() {
        let root = PathBuf::from(r"C:\repo");
        let mut column = ColumnPane::new(
            root.clone(),
            vec![entry(r"C:\repo\notes.txt")],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );
        column.set_search_results(
            "target".to_string(),
            vec![
                entry(r"C:\repo\target.rs"),
                entry(r"C:\repo\docs\targeting.md"),
                entry(r"C:\repo\my-target.md"),
                entry(r"C:\repo\target-notes.txt"),
            ],
            false,
        );
        column.group = crate::ui_preferences::ColumnGroupPreference::Extension;
        column.refresh_view();

        let rows = column.display_rows();
        assert!(matches!(
            rows.first(),
            Some(ColumnDisplayRow::Group { label, count, .. })
                if label.as_ref() == "MD" && *count == 2
        ));
        assert!(matches!(
            &rows[1],
            ColumnDisplayRow::Entry(entry) if entry.name == "targeting.md"
        ));
        assert!(matches!(
            &rows[2],
            ColumnDisplayRow::Entry(entry) if entry.name == "my-target.md"
        ));
        assert_eq!(column.first_search_row_index(), Some(4));

        column.clear_search_results();
        column.refresh_view();
        assert!(matches!(
            column.display_rows().first(),
            Some(ColumnDisplayRow::Group { .. })
        ));
    }

    #[test]
    fn keeps_ranked_search_order_and_sorts_previews() {
        let root = PathBuf::from(r"C:\repo");
        let a = entry(r"C:\repo\a.txt");
        let b = entry(r"C:\repo\b.txt");
        let mut column = ColumnPane::new(
            root,
            vec![a.clone(), b.clone()],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );

        column.set_search_results("txt".to_string(), vec![b.clone(), a.clone()], false);
        assert_eq!(
            column
                .entries
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
            vec!["b.txt", "a.txt"]
        );

        column.set_search_preview("txt".to_string(), vec![b, a]);
        assert_eq!(
            column
                .entries
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
            vec!["a.txt", "b.txt"]
        );
    }

    #[test]
    fn detects_narrowing_queries_case_insensitively() {
        assert!(search_query_narrows("Target", "target.rs"));
        assert!(search_query_narrows("tar", "target"));
        assert!(!search_query_narrows("target", "tar"));
        assert!(!search_query_narrows("", "target"));
    }

    #[test]
    fn narrows_previous_results_without_reordering() {
        let entries = vec![
            entry(r"C:\repo\target.rs"),
            entry(r"C:\repo\notes.txt"),
            entry(r"C:\repo\sub\targeting.md"),
        ];
        let narrowed = narrow_search_entries(&entries, "TARGET");
        assert_eq!(
            narrowed
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
            vec!["target.rs", "targeting.md"]
        );
    }

    #[test]
    fn distinguishes_preview_from_final_results() {
        let root = PathBuf::from(r"C:\repo");
        let mut column = ColumnPane::new(
            root,
            vec![entry(r"C:\repo\a.txt")],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );
        column.set_search_preview("a".to_string(), vec![entry(r"C:\repo\a.txt")]);
        assert!(column.search_preview);
        column.set_search_results("a".to_string(), vec![entry(r"C:\repo\a.txt")], false);
        assert!(!column.search_preview);
        column.clear_search_results();
        assert!(!column.search_preview);
    }

    #[test]
    fn closing_search_restores_a_selection_the_query_hid() {
        let root = PathBuf::from(r"C:\repo");
        let notes = entry(r"C:\repo\notes.txt");
        let target = entry(r"C:\repo\target.txt");
        let mut column = ColumnPane::new(
            root,
            vec![notes.clone(), target.clone()],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );
        column.set_single_selection(notes.path.clone());

        column.set_search_preview("tar".to_string(), vec![target.clone()]);
        assert_eq!(column.selected_path, None);
        column.set_search_results("targ".to_string(), vec![target], false);
        assert_eq!(column.selected_path, None);

        column.clear_search_results();
        assert_eq!(column.selected_path.as_deref(), Some(notes.path.as_path()));
        assert!(column.is_selected(&notes.path));
    }

    #[test]
    fn a_selection_made_in_search_survives_closing_it() {
        let root = PathBuf::from(r"C:\repo");
        let notes = entry(r"C:\repo\notes.txt");
        let deep = entry(r"C:\repo\docs\target.md");
        let mut column = ColumnPane::new(
            root,
            vec![notes.clone()],
            0,
            false,
            false,
            false,
            ColumnViewRule::default(),
        );
        column.set_single_selection(notes.path.clone());
        column.set_search_results("target".to_string(), vec![deep.clone()], false);
        column.set_single_selection(deep.path.clone());

        column.clear_search_results();
        // The search pick is not in the listing, so the earlier selection
        // returns instead of leaving the column with nothing selected.
        assert_eq!(column.selected_path.as_deref(), Some(notes.path.as_path()));
    }
}
