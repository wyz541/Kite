use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::actions::{
    GitClientAbort, GitClientCancelTask, GitClientCommit, GitClientContinue, GitClientDismiss,
    GitClientFetch, GitClientPull, GitClientPush, GitClientRefresh, GitClientShowShortcuts,
    GitClientSkip, GitClientStash, GitClientToggleConsole,
};

use crate::git_tools_window::open_git_tools_window;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{GitOperationAction, GitOperationState, run_git_operation_control};
use kite_core::git_changes::{
    self, GitChangeGroup, GitDiffScope, GitFileChangesDiff, GitHunkLineKind, GitLineSelection,
    GitRepositoryChanges,
};
use kite_core::session::{
    GitClientWindowSessionDto, load_git_client_window_state, save_git_client_window_state,
};
use kite_core::vcs::{VcsCancellation, VcsOperation, VcsTool, run_vcs_operation_cancellable};

const DEFAULT_WINDOW_WIDTH: u32 = 1_180;
const DEFAULT_WINDOW_HEIGHT: u32 = 760;
const DEFAULT_CHANGES_RATIO_BP: u16 = 3_300;
const DEFAULT_DETAILS_RATIO_BP: u16 = 2_900;
const DEFAULT_CONSOLE_RATIO_BP: u16 = 1_800;
const MIN_WINDOW_WIDTH: u32 = 900;
const MIN_WINDOW_HEIGHT: u32 = 600;
const MAX_CONSOLE_LINES: usize = 200;

static OPEN_WINDOWS: OnceLock<Mutex<HashMap<String, AnyWindowHandle>>> = OnceLock::new();

fn open_windows() -> &'static Mutex<HashMap<String, AnyWindowHandle>> {
    OPEN_WINDOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn repository_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

fn forget_open_window(path: &Path) {
    if let Ok(mut windows) = open_windows().lock() {
        windows.remove(&repository_key(path));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitClientView {
    Changes,
    Diff,
}

impl GitClientView {
    fn as_str(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Diff => "diff",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "diff" => Self::Diff,
            _ => Self::Changes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitNoticeKind {
    Success,
    Info,
    Warning,
    Error,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitNoticeAction {
    Refresh,
    ShowChanges,
    Pull,
    OpenConsole,
}

#[derive(Clone, Debug)]
struct GitNotice {
    kind: GitNoticeKind,
    title: SharedString,
    message: SharedString,
    action: Option<GitNoticeAction>,
    action_label: Option<&'static str>,
}

struct GitClientTask {
    label: SharedString,
    cancellation: Option<Arc<VcsCancellation>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitClientSelection {
    path: String,
    group: GitChangeGroup,
    is_directory: bool,
}

#[derive(Clone, Debug)]
enum GitVisibleRow {
    Group {
        group: GitChangeGroup,
        count: usize,
    },
    Entry {
        path: String,
        label: String,
        depth: usize,
        group: GitChangeGroup,
        status: kite_core::fs::VcsStatus,
        old_path: Option<String>,
        is_directory: bool,
    },
}

impl GitVisibleRow {
    fn selection(&self) -> Option<GitClientSelection> {
        match self {
            Self::Group { .. } => None,
            Self::Entry {
                path,
                group,
                is_directory,
                ..
            } => Some(GitClientSelection {
                path: path.clone(),
                group: *group,
                is_directory: *is_directory,
            }),
        }
    }
}

#[derive(Clone, Debug)]
enum GitWindowOperation {
    StageLines(String, Vec<GitLineSelection>),
    UnstageLines(String, Vec<GitLineSelection>),
    DiscardLines(String, Vec<GitLineSelection>),
    StagePaths(Vec<String>),
    UnstagePaths(Vec<String>),
    DiscardPaths(Vec<String>),
}

pub(crate) struct GitClientWindow {
    repository_root: PathBuf,
    changes: Option<GitRepositoryChanges>,
    rows: Vec<GitVisibleRow>,
    selected: Option<GitClientSelection>,
    diff: Option<GitFileChangesDiff>,
    checked_lines: HashSet<GitLineSelection>,
    filter_input: Entity<InputState>,
    commit_input: Entity<TextareaState>,
    show_ignored: bool,
    tree_mode: bool,
    loading: bool,
    loading_diff: bool,
    operating: bool,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    task: Option<GitClientTask>,
    console_expanded: bool,
    console_lines: Vec<SharedString>,
    active_view: GitClientView,
    notification: Option<GitNotice>,
    show_shortcuts: bool,
    horizontal_state: Entity<ResizableState>,
    vertical_state: Entity<ResizableState>,
    initial_changes_width: f32,
    initial_console_height: f32,
    task_generation: u64,
    focus_handle: FocusHandle,
    _filter_subscription: Subscription,
    _load_task: Task<()>,
    _diff_task: Task<()>,
    _operation_task: Task<()>,
}

impl GitClientWindow {
    pub(crate) fn new(
        repository_root: PathBuf,
        saved: Option<GitClientWindowSessionDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let width = saved
            .as_ref()
            .map_or(DEFAULT_WINDOW_WIDTH, |state| state.width)
            .max(MIN_WINDOW_WIDTH) as f32;
        let height = saved
            .as_ref()
            .map_or(DEFAULT_WINDOW_HEIGHT, |state| state.height)
            .max(MIN_WINDOW_HEIGHT) as f32;
        let changes_ratio_bp = saved
            .as_ref()
            .map_or(DEFAULT_CHANGES_RATIO_BP, |state| state.changes_ratio_bp);
        let console_ratio_bp = saved
            .as_ref()
            .map_or(DEFAULT_CONSOLE_RATIO_BP, |state| state.console_ratio_bp);
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("按路径筛选")
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &filter_input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.rebuild_rows(cx);
                    cx.notify();
                }
            },
        );
        let commit_input = cx.new(|cx| TextareaState::new(window, cx));
        let mut this = Self {
            repository_root,
            changes: None,
            rows: Vec::new(),
            selected: None,
            diff: None,
            checked_lines: HashSet::new(),
            filter_input,
            commit_input,
            show_ignored: false,
            tree_mode: true,
            loading: true,
            loading_diff: false,
            operating: false,
            error: None,
            notice: None,
            task: None,
            console_expanded: saved.as_ref().is_none_or(|state| state.console_expanded),
            console_lines: vec!["Git 客户端已就绪。".into()],
            active_view: saved.as_ref().map_or(GitClientView::Changes, |state| {
                GitClientView::from_str(&state.last_view)
            }),
            notification: None,
            show_shortcuts: false,
            horizontal_state: cx.new(|_| ResizableState::default()),
            vertical_state: cx.new(|_| ResizableState::default()),
            initial_changes_width: width * f32::from(changes_ratio_bp) / 10_000.0,
            initial_console_height: height * f32::from(console_ratio_bp) / 10_000.0,
            task_generation: 0,
            focus_handle: cx.focus_handle(),
            _filter_subscription: subscription,
            _load_task: Task::ready(()),
            _diff_task: Task::ready(()),
            _operation_task: Task::ready(()),
        };
        this.register_close_persistence(window, cx);
        cx.defer_in(window, |this, window, cx| {
            window.focus(&this.focus_handle, cx);
            cx.notify();
        });
        this.start_load(cx);
        this
    }

    fn register_close_persistence(&self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.weak_entity();
        let repository_root = self.repository_root.clone();
        window.on_window_should_close(cx, move |window, app| {
            if let Some(entity) = entity.upgrade() {
                entity.update(app, |this, cx| this.persist_session(window, cx));
            }
            forget_open_window(&repository_root);
            true
        });
    }

    fn persist_session(&self, window: &Window, cx: &App) {
        let bounds = window.bounds();
        let width = bounds.size.width.as_f32().round().max(1.0) as u32;
        let height = bounds.size.height.as_f32().round().max(1.0) as u32;
        let horizontal = self.horizontal_state.read(cx).sizes();
        let total_width = horizontal.iter().map(|value| value.as_f32()).sum::<f32>();
        let changes_ratio_bp = if horizontal.len() >= 2 && total_width > 0.0 {
            ((horizontal[0].as_f32() / total_width) * 10_000.0)
                .round()
                .clamp(1.0, u16::MAX as f32) as u16
        } else {
            DEFAULT_CHANGES_RATIO_BP
        };
        let vertical = self.vertical_state.read(cx).sizes();
        let total_height = vertical.iter().map(|value| value.as_f32()).sum::<f32>();
        let console_ratio_bp = if self.console_expanded && vertical.len() >= 2 && total_height > 0.0
        {
            ((vertical[1].as_f32() / total_height) * 10_000.0)
                .round()
                .clamp(1.0, u16::MAX as f32) as u16
        } else {
            DEFAULT_CONSOLE_RATIO_BP
        };
        let state = GitClientWindowSessionDto {
            repository_root: self.repository_root.to_string_lossy().into_owned(),
            width,
            height,
            changes_ratio_bp,
            details_ratio_bp: DEFAULT_DETAILS_RATIO_BP,
            console_ratio_bp,
            console_expanded: self.console_expanded,
            last_view: self.active_view.as_str().to_string(),
        };
        if let Err(error) = save_git_client_window_state(&state) {
            eprintln!("无法保存 Git 客户端窗口状态：{error}");
        }
    }

    fn push_console(&mut self, message: impl Into<SharedString>) {
        self.console_lines
            .push(format!("› {}", message.into()).into());
        if self.console_lines.len() > MAX_CONSOLE_LINES {
            let overflow = self.console_lines.len() - MAX_CONSOLE_LINES;
            self.console_lines.drain(0..overflow);
        }
    }

    fn busy(&self) -> bool {
        self.task.is_some() || self.operating || self.loading || self.loading_diff
    }

    fn set_view(&mut self, view: GitClientView, cx: &mut Context<Self>) {
        self.active_view = view;
        cx.notify();
    }

    fn start_vcs_operation(
        &mut self,
        operation: VcsOperation,
        label: &'static str,
        clear_commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            self.notification = Some(GitNotice {
                kind: GitNoticeKind::Warning,
                title: "已有任务正在运行".into(),
                message: "请等待当前任务结束，或在 Git 控制台取消。".into(),
                action: Some(GitNoticeAction::OpenConsole),
                action_label: Some("查看任务"),
            });
            cx.notify();
            return;
        }

        let cancellation = VcsCancellation::new();
        self.task_generation = self.task_generation.wrapping_add(1);
        let generation = self.task_generation;
        self.task = Some(GitClientTask {
            label: label.into(),
            cancellation: Some(cancellation.clone()),
        });
        self.push_console(format!("{label}…"));
        let repository_root = self.repository_root.clone();
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    run_vcs_operation_cancellable(
                        VcsTool::Git,
                        operation,
                        &repository_root,
                        &cancellation,
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.task_generation != generation {
                    return;
                }
                this.task = None;
                match result {
                    Ok(message) => {
                        if clear_commit {
                            this.commit_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                        }
                        this.push_console(message.clone());
                        this.notice = Some(message.clone().into());
                        this.notification = Some(GitNotice {
                            kind: GitNoticeKind::Success,
                            title: label.into(),
                            message: message.into(),
                            action: Some(GitNoticeAction::Refresh),
                            action_label: Some("刷新"),
                        });
                        this.refresh(cx);
                    }
                    Err(error) => {
                        this.push_console(format!("{label}失败：{error}"));
                        this.error = Some(error.clone().into());
                        this.notification = Some(notice_for_error(label, error));
                    }
                }
                cx.notify();
            });
        });
    }

    fn start_git_control(
        &mut self,
        action: GitOperationAction,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        self.task_generation = self.task_generation.wrapping_add(1);
        let generation = self.task_generation;
        self.task = Some(GitClientTask {
            label: format!("{verb} Git 操作").into(),
            cancellation: None,
        });
        self.push_console(format!("{verb}进行中的 Git 操作…"));
        let repository_root = self.repository_root.clone();
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { run_git_operation_control(&repository_root, action) },
                )
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.task_generation != generation {
                    return;
                }
                this.task = None;
                match result {
                    Ok(message) => {
                        this.push_console(message.clone());
                        this.notification = Some(GitNotice {
                            kind: GitNoticeKind::Success,
                            title: "Git 操作已继续".into(),
                            message: message.into(),
                            action: Some(GitNoticeAction::Refresh),
                            action_label: Some("刷新"),
                        });
                        this.refresh(cx);
                    }
                    Err(error) => {
                        this.push_console(format!("{verb}失败：{error}"));
                        this.notification = Some(notice_for_error(verb, error));
                    }
                }
                cx.notify();
            });
        });
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.notification = Some(GitNotice {
                kind: GitNoticeKind::Warning,
                title: "提交信息为空".into(),
                message: "输入提交信息后才能提交。".into(),
                action: None,
                action_label: None,
            });
            cx.notify();
            return;
        }
        self.start_vcs_operation(VcsOperation::Commit(message), "Git 提交", true, window, cx);
    }

    fn cancel_task(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.task.take() else {
            return;
        };
        if let Some(cancellation) = task.cancellation {
            cancellation.cancel();
        }
        self.task_generation = self.task_generation.wrapping_add(1);
        self.push_console(format!("已取消：{}", task.label));
        self.notification = Some(GitNotice {
            kind: GitNoticeKind::Info,
            title: "任务已取消".into(),
            message: task.label,
            action: None,
            action_label: None,
        });
        cx.notify();
    }

    fn apply_notice_action(
        &mut self,
        action: GitNoticeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notification = None;
        match action {
            GitNoticeAction::Refresh => self.refresh(cx),
            GitNoticeAction::ShowChanges => self.set_view(GitClientView::Changes, cx),
            GitNoticeAction::Pull => {
                self.start_vcs_operation(VcsOperation::Pull, "Git 拉取", false, window, cx)
            }
            GitNoticeAction::OpenConsole => {
                self.console_expanded = true;
                cx.notify();
            }
        }
    }

    fn on_refresh(&mut self, _: &GitClientRefresh, _: &mut Window, cx: &mut Context<Self>) {
        if !self.busy() {
            self.refresh(cx);
        }
    }

    fn on_fetch(&mut self, _: &GitClientFetch, window: &mut Window, cx: &mut Context<Self>) {
        self.start_vcs_operation(VcsOperation::Fetch, "Git 获取", false, window, cx);
    }

    fn on_pull(&mut self, _: &GitClientPull, window: &mut Window, cx: &mut Context<Self>) {
        self.start_vcs_operation(VcsOperation::Pull, "Git 拉取", false, window, cx);
    }

    fn on_push(&mut self, _: &GitClientPush, window: &mut Window, cx: &mut Context<Self>) {
        self.start_vcs_operation(VcsOperation::Push, "Git 推送", false, window, cx);
    }

    fn on_commit(&mut self, _: &GitClientCommit, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
    }

    fn on_stash(&mut self, _: &GitClientStash, window: &mut Window, cx: &mut Context<Self>) {
        self.start_vcs_operation(VcsOperation::Stash, "Git 贮藏", false, window, cx);
    }

    fn on_toggle_console(
        &mut self,
        _: &GitClientToggleConsole,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.console_expanded = !self.console_expanded;
        cx.notify();
    }

    fn on_show_shortcuts(
        &mut self,
        _: &GitClientShowShortcuts,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_shortcuts = true;
        cx.notify();
    }

    fn on_continue(&mut self, _: &GitClientContinue, window: &mut Window, cx: &mut Context<Self>) {
        self.start_git_control(GitOperationAction::Continue, "继续", window, cx);
    }

    fn on_skip(&mut self, _: &GitClientSkip, window: &mut Window, cx: &mut Context<Self>) {
        self.start_git_control(GitOperationAction::Skip, "跳过", window, cx);
    }

    fn on_abort(&mut self, _: &GitClientAbort, window: &mut Window, cx: &mut Context<Self>) {
        self.start_git_control(GitOperationAction::Abort, "中止", window, cx);
    }

    fn on_cancel_task(&mut self, _: &GitClientCancelTask, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_task(cx);
    }

    fn on_dismiss(&mut self, _: &GitClientDismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.show_shortcuts {
            self.show_shortcuts = false;
        } else {
            self.notification = None;
        }
        cx.notify();
    }

    fn start_load(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() || self.operating {
            return;
        }
        let root = self.repository_root.clone();
        let include_ignored = self.show_ignored;
        self.loading = true;
        self.error = None;
        self.push_console("刷新 Git 变更…");
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { git_changes::repository_changes(&root, include_ignored) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(changes) => {
                        this.push_console(format!("已读取 {} 个变更文件。", changes.files.len()));
                        this.repository_root = changes.repository_root.clone();
                        this.changes = Some(changes);
                        this.error = None;
                        this.rebuild_rows(cx);
                        this.restore_or_select_initial(cx);
                    }
                    Err(error) => {
                        this.changes = None;
                        this.rows.clear();
                        this.diff = None;
                        this.push_console(format!("读取失败：{error}"));
                        this.error = Some(error.clone().into());
                        this.notification = Some(GitNotice {
                            kind: GitNoticeKind::Error,
                            title: "无法读取仓库".into(),
                            message: error.into(),
                            action: Some(GitNoticeAction::OpenConsole),
                            action_label: Some("查看控制台"),
                        });
                    }
                }
                cx.notify();
            });
        });
    }

    fn restore_or_select_initial(&mut self, cx: &mut Context<Self>) {
        let previous = self.selected.clone();
        if let Some(previous) = previous
            && self
                .rows
                .iter()
                .filter_map(GitVisibleRow::selection)
                .any(|selection| selection == previous)
        {
            self.load_selected_diff(cx);
            return;
        }
        self.selected = self
            .rows
            .iter()
            .filter_map(GitVisibleRow::selection)
            .find(|selection| !selection.is_directory);
        if self.selected.is_some() {
            self.load_selected_diff(cx);
        } else {
            self.diff = None;
            self.checked_lines.clear();
        }
    }

    fn rebuild_rows(&mut self, cx: &App) {
        let Some(changes) = self.changes.as_ref() else {
            self.rows.clear();
            return;
        };
        let query = self.filter_input.read(cx).value().to_string();
        let rows = build_visible_rows(changes, &query, self.tree_mode);
        self.rows = rows;
    }

    fn select_row(&mut self, selection: GitClientSelection, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&selection) {
            return;
        }
        self.selected = Some(selection);
        self.checked_lines.clear();
        if self
            .selected
            .as_ref()
            .is_some_and(|selection| !selection.is_directory)
        {
            self.load_selected_diff(cx);
        } else {
            self.diff = None;
            self.loading_diff = false;
            cx.notify();
        }
    }

    fn load_selected_diff(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.selected.clone() else {
            return;
        };
        if selection.is_directory {
            self.diff = None;
            cx.notify();
            return;
        }
        let root = self.repository_root.clone();
        let path = selection.path.clone();
        let scope = if selection.group == GitChangeGroup::Staged {
            GitDiffScope::Staged
        } else {
            GitDiffScope::Unstaged
        };
        self.diff = None;
        self.loading_diff = true;
        self.checked_lines.clear();
        self._diff_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { git_changes::file_changes_diff(&root, &path, scope) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading_diff = false;
                match result {
                    Ok(diff) => {
                        this.diff = Some(diff);
                        this.error = None;
                    }
                    Err(error) => {
                        this.diff = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.start_load(cx);
        cx.notify();
    }

    fn open_stash_and_history_tools(&mut self, cx: &mut Context<Self>) {
        let selected_paths = self
            .selected
            .as_ref()
            .map(|selection| vec![self.repository_root.join(&selection.path)])
            .unwrap_or_default();
        self.error = None;
        match open_git_tools_window(self.repository_root.clone(), selected_paths, cx) {
            Ok(()) => self.notice = Some("已打开 Stash 与历史操作窗口".into()),
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        self.show_ignored = !self.show_ignored;
        self.start_load(cx);
    }

    fn toggle_tree_mode(&mut self, cx: &mut Context<Self>) {
        self.tree_mode = !self.tree_mode;
        self.rebuild_rows(cx);
        cx.notify();
    }

    fn toggle_line(&mut self, selection: GitLineSelection, cx: &mut Context<Self>) {
        if !self.checked_lines.remove(&selection) {
            self.checked_lines.insert(selection);
        }
        cx.notify();
    }

    fn set_hunk_checked(&mut self, hunk_index: u32, checked: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let units = diff.selection_units_for_hunk(hunk_index);
        if checked {
            self.checked_lines.extend(units);
        } else {
            for unit in units {
                self.checked_lines.remove(&unit);
            }
        }
        cx.notify();
    }

    fn selected_line_units(&self) -> Vec<GitLineSelection> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        diff.all_selection_units()
            .into_iter()
            .filter(|selection| self.checked_lines.contains(selection))
            .collect()
    }

    fn run_operation(
        &mut self,
        operation: GitWindowOperation,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.operating {
            return;
        }
        let root = self.repository_root.clone();
        self.operating = true;
        self.error = None;
        self.notice = Some("正在更新 Git 暂存区…".into());
        self._operation_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match operation {
                        GitWindowOperation::StageLines(path, selections) => {
                            git_changes::stage_lines(&root, &path, &selections)
                        }
                        GitWindowOperation::UnstageLines(path, selections) => {
                            git_changes::unstage_lines(&root, &path, &selections)
                        }
                        GitWindowOperation::DiscardLines(path, selections) => {
                            git_changes::discard_lines(&root, &path, &selections)
                        }
                        GitWindowOperation::StagePaths(paths) => {
                            git_changes::stage_paths(&root, &paths)
                        }
                        GitWindowOperation::UnstagePaths(paths) => {
                            git_changes::unstage_paths(&root, &paths)
                        }
                        GitWindowOperation::DiscardPaths(paths) => {
                            git_changes::discard_paths(&root, &paths)
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.operating = false;
                match result {
                    Ok(result) => {
                        this.changes = Some(result.changes);
                        this.notice = Some(result.message.into());
                        if this.show_ignored {
                            this.start_load(cx);
                        } else {
                            this.rebuild_rows(cx);
                            this.restore_or_select_initial(cx);
                        }
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn confirm_discard_lines(
        &mut self,
        path: String,
        selections: Vec<GitLineSelection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if selections.is_empty() {
            return;
        }
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let path = path.clone();
            let selections = selections.clone();
            alert
                .title("丢弃所选改动？")
                .description("这些行会从工作区移除，此操作无法撤销。")
                .confirm()
                .ok_text("丢弃")
                .cancel_text("取消")
                .on_ok(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.run_operation(
                                GitWindowOperation::DiscardLines(path.clone(), selections.clone()),
                                window,
                                cx,
                            );
                        });
                    }
                    true
                })
        });
    }

    fn confirm_discard_path(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let untracked = self.changes.as_ref().is_some_and(|changes| {
            paths.iter().any(|path| {
                changes.files.iter().any(|file| {
                    file.group == GitChangeGroup::Untracked
                        && (file.path == *path
                            || file
                                .path
                                .strip_prefix(path)
                                .is_some_and(|rest| rest.starts_with('/')))
                })
            })
        });
        let description = if untracked {
            "未跟踪项目会移到系统回收站；已跟踪文件的工作区改动会被还原。此操作无法撤销。"
        } else {
            "所选项目的工作区改动会被还原到索引内容。此操作无法撤销。"
        };
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let paths = paths.clone();
            alert
                .title("丢弃所选改动？")
                .description(description)
                .confirm()
                .ok_text("丢弃")
                .cancel_text("取消")
                .on_ok(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.run_operation(
                                GitWindowOperation::DiscardPaths(paths.clone()),
                                window,
                                cx,
                            );
                        });
                    }
                    true
                })
        });
    }

    fn add_ignore_rule(&mut self, local_exclude: bool, cx: &mut Context<Self>) {
        let Some(selection) = self.selected.clone() else {
            return;
        };
        let root = self.repository_root.clone();
        let path = selection.path.clone();
        self.error = None;
        self.notice = Some("正在更新忽略规则…".into());
        self._operation_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git_changes::add_to_gitignore(&root, &path, None, local_exclude)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(message) => {
                        this.notice = Some(message.into());
                        this.start_load(cx);
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }
}

fn build_visible_rows(
    changes: &GitRepositoryChanges,
    query: &str,
    tree_mode: bool,
) -> Vec<GitVisibleRow> {
    let query = query.trim().to_lowercase();
    let mut rows = Vec::new();
    for group in [
        GitChangeGroup::Staged,
        GitChangeGroup::Unstaged,
        GitChangeGroup::Conflicted,
        GitChangeGroup::Untracked,
        GitChangeGroup::Ignored,
    ] {
        let files = changes
            .files
            .iter()
            .filter(|file| {
                file.group == group
                    && (query.is_empty() || file.path.to_lowercase().contains(&query))
            })
            .collect::<Vec<_>>();
        if files.is_empty() {
            continue;
        }
        rows.push(GitVisibleRow::Group {
            group,
            count: files.len(),
        });
        if tree_mode {
            let mut directories = BTreeSet::new();
            for file in &files {
                let mut path = String::new();
                let components = file.path.split('/').collect::<Vec<_>>();
                for component in components.iter().take(components.len().saturating_sub(1)) {
                    if !path.is_empty() {
                        path.push('/');
                    }
                    path.push_str(component);
                    directories.insert(path.clone());
                }
            }
            for directory in directories {
                let depth = directory.matches('/').count();
                let label = directory
                    .rsplit('/')
                    .next()
                    .unwrap_or(&directory)
                    .to_string();
                rows.push(GitVisibleRow::Entry {
                    path: directory.clone(),
                    label,
                    depth,
                    group,
                    status: files
                        .iter()
                        .find(|file| file.path.starts_with(&format!("{directory}/")))
                        .map(|file| file.status)
                        .unwrap_or(kite_core::fs::VcsStatus::Modified),
                    old_path: None,
                    is_directory: true,
                });
            }
        }
        for file in files {
            let depth = if tree_mode {
                file.path.matches('/').count()
            } else {
                0
            };
            let label = if tree_mode {
                file.path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file.path)
                    .to_string()
            } else {
                file.path.clone()
            };
            rows.push(GitVisibleRow::Entry {
                path: file.path.clone(),
                label,
                depth,
                group,
                status: file.status,
                old_path: file.old_path.clone(),
                is_directory: false,
            });
        }
    }
    rows
}

pub(crate) fn open_git_client_window(
    repository_root: PathBuf,
    _window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let repository_root = dunce::canonicalize(&repository_root).unwrap_or(repository_root);
    let key = repository_key(&repository_root);
    let existing = {
        let windows = open_windows()
            .lock()
            .map_err(|_| "无法读取 Git 窗口状态".to_string())?;
        windows.get(&key).copied()
    };
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return Ok(());
        }
        if let Ok(mut windows) = open_windows().lock() {
            windows.remove(&key);
        }
    }
    let saved = load_git_client_window_state(&repository_root);
    let width = saved
        .as_ref()
        .map_or(DEFAULT_WINDOW_WIDTH, |state| state.width)
        .clamp(MIN_WINDOW_WIDTH, 3_840);
    let height = saved
        .as_ref()
        .map_or(DEFAULT_WINDOW_HEIGHT, |state| state.height)
        .clamp(MIN_WINDOW_HEIGHT, 2_160);
    let title = repository_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository_root.display().to_string());
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(format!("Git — {title}").into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(
            size(px(width as f32), px(height as f32)),
            cx,
        )),
        window_min_size: Some(size(
            px(MIN_WINDOW_WIDTH as f32),
            px(MIN_WINDOW_HEIGHT as f32),
        )),
        ..Default::default()
    };
    let root_for_view = repository_root.clone();
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitClientWindow::new(root_for_view, saved, window, cx))
    })
    .map_err(|error| format!("无法打开 Git 客户端窗口：{error}"))?;
    open_windows()
        .lock()
        .map_err(|_| "无法保存 Git 窗口状态".to_string())?
        .insert(key, handle);
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    Ok(())
}

impl GitClientWindow {
    fn render_header(&self, cx: &Context<Self>) -> AnyElement {
        let changes = self.changes.as_ref();
        let branch = changes
            .and_then(|changes| changes.branch.clone())
            .or_else(|| changes.and_then(|changes| changes.head_short_hash.clone()))
            .unwrap_or_else(|| "未命名分支".to_string());
        let (ahead, behind) = changes
            .map(|changes| (changes.ahead, changes.behind))
            .unwrap_or_default();
        let operation_state = changes.and_then(|changes| changes.operation);
        let operation = operation_state.map(GitOperationState::label);
        let busy = self.busy();
        let commit_ready = !self.commit_input.read(cx).value().trim().is_empty();
        let message = self
            .error
            .clone()
            .or_else(|| self.notice.clone())
            .unwrap_or_else(|| "选择文件查看差异，勾选行后暂存或丢弃".into());
        let message_is_error = self.error.is_some();

        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::GitBranch).size_4())
                    .child(div().text_sm().font_semibold().child(branch))
                    .when(ahead > 0, |this| this.child(format!("↑{ahead}")))
                    .when(behind > 0, |this| this.child(format!("↓{behind}")))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(self.repository_root.display().to_string()),
                    )
                    .when_some(operation, |this, operation| {
                        this.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(cx.theme().warning.opacity(0.14))
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(operation),
                        )
                    })
                    .child(
                        Button::new("git-client-stash-tools")
                            .ghost()
                            .compact()
                            .label("Stash / 操作")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.open_stash_and_history_tools(cx)
                            })),
                    )
                    .child(
                        Button::new("git-client-refresh")
                            .ghost()
                            .compact()
                            .label("刷新")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
                    )
                    .child(
                        Button::new("git-client-ignored")
                            .ghost()
                            .compact()
                            .label(if self.show_ignored {
                                "隐藏忽略"
                            } else {
                                "显示忽略"
                            })
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_ignored(cx)),
                            ),
                    )
                    .child(
                        Button::new("git-client-tree")
                            .ghost()
                            .compact()
                            .label(if self.tree_mode {
                                "平铺"
                            } else {
                                "目录树"
                            })
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.toggle_tree_mode(cx)
                                }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new("git-client-fetch")
                            .ghost()
                            .compact()
                            .label("获取")
                            .tooltip("获取 (Ctrl+Shift+F)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_vcs_operation(
                                    VcsOperation::Fetch,
                                    "Git 获取",
                                    false,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("git-client-pull")
                            .ghost()
                            .compact()
                            .label("拉取")
                            .tooltip("拉取 (Ctrl+Shift+P)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_vcs_operation(
                                    VcsOperation::Pull,
                                    "Git 拉取",
                                    false,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("git-client-push")
                            .ghost()
                            .compact()
                            .label("推送")
                            .tooltip("推送 (Ctrl+Shift+K)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_vcs_operation(
                                    VcsOperation::Push,
                                    "Git 推送",
                                    false,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("git-client-commit")
                            .primary()
                            .compact()
                            .label("提交")
                            .tooltip("提交 (Ctrl+K)")
                            .disabled(busy || !commit_ready)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.commit(window, cx)
                            })),
                    )
                    .child(
                        Button::new("git-client-stash")
                            .ghost()
                            .compact()
                            .label("贮藏")
                            .tooltip("贮藏工作区 (Ctrl+Alt+S)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_vcs_operation(
                                    VcsOperation::Stash,
                                    "Git 贮藏",
                                    false,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("git-client-console")
                            .ghost()
                            .compact()
                            .icon(if self.console_expanded {
                                IconName::PanelBottomOpen
                            } else {
                                IconName::PanelBottomClose
                            })
                            .tooltip("显示或隐藏控制台 (Ctrl+J)")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.console_expanded = !this.console_expanded;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-client-shortcuts")
                            .ghost()
                            .compact()
                            .icon(IconName::Info)
                            .tooltip("快捷键 (F1)")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.show_shortcuts = true;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(operation_state, |this, operation| {
                this.child(self.render_operation_banner(operation, cx))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(if message_is_error {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(message),
            )
            .when_some(
                changes.and_then(|changes| changes.crlf_warning.clone()),
                |this, warning| {
                    this.child(
                        div()
                            .rounded_md()
                            .px_2()
                            .py_1()
                            .bg(cx.theme().warning.opacity(0.12))
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(warning),
                    )
                },
            )
            .into_any_element()
    }

    fn render_changes_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let total = self
            .changes
            .as_ref()
            .map_or(0, |changes| changes.files.len());
        let counts = self
            .changes
            .as_ref()
            .map(|changes| {
                format!(
                    "{} 暂存 · {} 未暂存 · {} 冲突",
                    changes.count(GitChangeGroup::Staged),
                    changes.count(GitChangeGroup::Unstaged),
                    changes.count(GitChangeGroup::Conflicted)
                )
            })
            .unwrap_or_default();
        let mut list = v_flex().w_full();
        for row in self.rows.clone() {
            list = list.child(self.render_row(row, cx));
        }

        v_flex()
            .w(px(390.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(div().text_sm().font_semibold().child("变更"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{total} 项")),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(counts),
                    )
                    .child(
                        Input::new(&self.filter_input)
                            .aria_label("按路径筛选 Git 变更")
                            .prefix(Icon::new(IconName::Search).size_3())
                            .cleanable(true),
                    )
                    .child(self.render_path_actions(cx)),
            )
            .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(
                if self.loading && self.rows.is_empty() {
                    empty_state(cx, "正在读取 Git 变更…").into_any_element()
                } else if self.rows.is_empty() {
                    empty_state(
                        cx,
                        if self
                            .changes
                            .as_ref()
                            .is_some_and(GitRepositoryChanges::is_clean)
                        {
                            "工作区干净"
                        } else {
                            "没有匹配的变更"
                        },
                    )
                    .into_any_element()
                } else {
                    list.into_any_element()
                },
            ))
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child("提交信息"),
                    )
                    .child(
                        Textarea::new(&self.commit_input)
                            .h(px(78.))
                            .w_full()
                            .aria_label("提交信息"),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Ctrl+K 提交"),
                            )
                            .child(
                                Button::new("git-client-commit-panel")
                                    .primary()
                                    .compact()
                                    .label("提交")
                                    .disabled(
                                        self.busy()
                                            || self.commit_input.read(cx).value().trim().is_empty(),
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.commit(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_path_actions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(selection) = self.selected.clone() else {
            return div().into_any_element();
        };
        let paths = vec![selection.path.clone()];
        let mut actions = h_flex().w_full().gap_1().flex_wrap();
        match selection.group {
            GitChangeGroup::Staged => {
                actions = actions.child(
                    Button::new("git-client-unstage-path")
                        .ghost()
                        .compact()
                        .label("取消暂存路径")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_operation(
                                GitWindowOperation::UnstagePaths(paths.clone()),
                                window,
                                cx,
                            );
                        })),
                );
            }
            GitChangeGroup::Unstaged | GitChangeGroup::Untracked | GitChangeGroup::Conflicted => {
                actions = actions.child(
                    Button::new("git-client-stage-path")
                        .ghost()
                        .compact()
                        .label("暂存路径")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_operation(
                                GitWindowOperation::StagePaths(paths.clone()),
                                window,
                                cx,
                            );
                        })),
                );
                if selection.group != GitChangeGroup::Conflicted {
                    let discard_paths = vec![selection.path.clone()];
                    actions = actions.child(
                        Button::new("git-client-discard-path")
                            .danger()
                            .ghost()
                            .compact()
                            .label("丢弃路径")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.confirm_discard_path(discard_paths.clone(), window, cx);
                            })),
                    );
                }
            }
            GitChangeGroup::Ignored => {}
        }
        if matches!(
            selection.group,
            GitChangeGroup::Untracked | GitChangeGroup::Unstaged
        ) && !selection.is_directory
        {
            actions = actions
                .child(
                    Button::new("git-client-ignore-project")
                        .ghost()
                        .compact()
                        .label("忽略到项目")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.add_ignore_rule(false, cx)
                        })),
                )
                .child(
                    Button::new("git-client-ignore-local")
                        .ghost()
                        .compact()
                        .label("忽略到本地")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.add_ignore_rule(true, cx)
                        })),
                );
        }
        actions.into_any_element()
    }

    fn render_row(&self, row: GitVisibleRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            GitVisibleRow::Group { group, count } => h_flex()
                .w_full()
                .px_3()
                .py_2()
                .bg(cx.theme().title_bar)
                .border_b_1()
                .border_color(cx.theme().border)
                .child(div().text_xs().font_semibold().child(group.label()))
                .child(
                    div()
                        .ml_auto()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string()),
                )
                .into_any_element(),
            GitVisibleRow::Entry {
                path,
                label,
                depth,
                group,
                status,
                old_path,
                is_directory,
            } => {
                let selection = GitClientSelection {
                    path: path.clone(),
                    group,
                    is_directory,
                };
                let selected = self.selected.as_ref() == Some(&selection);
                let status_color = status_color(cx, status);
                let icon = if is_directory {
                    IconName::Folder
                } else {
                    git_file_icon(Path::new(&path))
                };
                h_flex()
                    .id(SharedString::from(format!(
                        "git-client-row:{group:?}:{path}"
                    )))
                    .w_full()
                    .min_h(px(28.))
                    .gap_2()
                    .items_center()
                    .pl(px(10. + depth as f32 * 14.))
                    .pr_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(cx.theme().primary.opacity(0.12)))
                    .hover(|this| this.bg(cx.theme().primary.opacity(0.07)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_row(selection.clone(), cx)
                    }))
                    .child(Icon::new(icon).size_3().text_color(if selected {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_xs().font_medium().truncate().child(label))
                            .when_some(old_path, |this, old_path| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(format!("← {old_path}")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .rounded_sm()
                            .px_1()
                            .text_xs()
                            .font_semibold()
                            .text_color(status_color)
                            .child(status.code()),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_diff(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(selection) = self.selected.clone() else {
            return empty_state(cx, "选择一个文件查看差异").into_any_element();
        };
        if selection.is_directory {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::Folder).size_8())
                .child(div().text_sm().child(selection.path.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("目录操作会应用到其下所有变更"),
                )
                .into_any_element();
        }
        if self.loading_diff {
            return empty_state(cx, "正在加载差异…").into_any_element();
        }
        let Some(diff) = self.diff.clone() else {
            return empty_state(cx, "无法加载差异").into_any_element();
        };
        let mut body = v_flex().w_full().child(self.render_diff_toolbar(&diff, cx));
        if let Some(warning) = diff.line_ending_warning.as_ref() {
            body = body.child(
                div()
                    .px_3()
                    .py_2()
                    .bg(cx.theme().warning.opacity(0.12))
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(warning.clone()),
            );
        }
        if diff.binary {
            body = body.child(empty_state(cx, "二进制文件已变化，仅支持整文件操作"));
        } else if diff.truncated {
            body = body.child(empty_state(
                cx,
                "差异过大，仅显示部分内容；请使用整文件操作",
            ));
        } else if diff.hunks.is_empty() {
            body = body.child(empty_state(
                cx,
                if selection.group == GitChangeGroup::Conflicted {
                    "冲突文件请先解决冲突，然后暂存整个文件"
                } else {
                    "没有可显示的文本差异"
                },
            ));
        } else {
            for hunk in &diff.hunks {
                body = body.child(self.render_hunk(&diff, hunk, cx));
            }
        }
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scrollbar()
            .child(body)
            .into_any_element()
    }

    fn render_diff_toolbar(&self, diff: &GitFileChangesDiff, cx: &mut Context<Self>) -> AnyElement {
        let checked = self.selected_line_units();
        let count = checked.len();
        let path = self
            .selected
            .as_ref()
            .map(|selection| selection.path.clone());
        let group = self.selected.as_ref().map(|selection| selection.group);
        let mut actions = h_flex().gap_1();
        if let Some(path) = path {
            match group {
                Some(GitChangeGroup::Staged) if count > 0 => {
                    actions = actions.child(
                        Button::new("git-client-unstage-lines")
                            .ghost()
                            .compact()
                            .label(format!("取消暂存所选 ({count})"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.run_operation(
                                    GitWindowOperation::UnstageLines(path.clone(), checked.clone()),
                                    window,
                                    cx,
                                );
                            })),
                    );
                }
                Some(GitChangeGroup::Unstaged | GitChangeGroup::Untracked) if count > 0 => {
                    let stage_path = path.clone();
                    let stage_checked = checked.clone();
                    actions = actions.child(
                        Button::new("git-client-stage-lines")
                            .ghost()
                            .compact()
                            .label(format!("暂存所选 ({count})"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.run_operation(
                                    GitWindowOperation::StageLines(
                                        stage_path.clone(),
                                        stage_checked.clone(),
                                    ),
                                    window,
                                    cx,
                                );
                            })),
                    );
                    let discard_path = path.clone();
                    let discard_checked = checked.clone();
                    actions = actions.child(
                        Button::new("git-client-discard-lines")
                            .danger()
                            .ghost()
                            .compact()
                            .label(format!("丢弃所选 ({count})"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.confirm_discard_lines(
                                    discard_path.clone(),
                                    discard_checked.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    );
                }
                _ => {}
            }
        }
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .truncate()
                            .child(diff.path.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if diff.scope == GitDiffScope::Staged {
                                "暂存区与 HEAD"
                            } else {
                                "工作区与暂存区"
                            }),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    fn render_hunk(
        &self,
        diff: &GitFileChangesDiff,
        hunk: &kite_core::git_changes::GitDiffHunk,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let units = diff.selection_units_for_hunk(hunk.index);
        let all_checked =
            !units.is_empty() && units.iter().all(|unit| self.checked_lines.contains(unit));
        let path = self
            .selected
            .as_ref()
            .map(|selection| selection.path.clone());
        let group = self.selected.as_ref().map(|selection| selection.group);
        let mut lines = v_flex().w_full();
        for line in &hunk.lines {
            lines = lines.child(self.render_diff_line(diff, hunk, line, cx));
        }
        let hunk_index = hunk.index;
        let all_units = units.clone();
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .bg(cx.theme().primary.opacity(0.06))
                    .child(
                        Button::new(SharedString::from(format!("git-hunk-check:{hunk_index}")))
                            .ghost()
                            .compact()
                            .label(if all_checked { "☑" } else { "☐" })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_hunk_checked(hunk_index, !all_checked, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(hunk.header.clone()),
                    )
                    .when_some(path, |this, path| {
                        let units = all_units.clone();
                        this.when(group == Some(GitChangeGroup::Staged), |this| {
                            let path = path.clone();
                            let units = units.clone();
                            this.child(
                                Button::new(SharedString::from(format!(
                                    "git-hunk-unstage:{hunk_index}"
                                )))
                                .ghost()
                                .compact()
                                .label("取消暂存 Hunk")
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, window, cx| {
                                        this.run_operation(
                                            GitWindowOperation::UnstageLines(
                                                path.clone(),
                                                units.clone(),
                                            ),
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                        })
                        .when(
                            matches!(
                                group,
                                Some(GitChangeGroup::Unstaged | GitChangeGroup::Untracked)
                            ),
                            |this| {
                                let stage_path = path.clone();
                                let stage_units = units.clone();
                                let discard_path = path.clone();
                                let discard_units = units.clone();
                                this.child(
                                    Button::new(SharedString::from(format!(
                                        "git-hunk-stage:{hunk_index}"
                                    )))
                                    .ghost()
                                    .compact()
                                    .label("暂存 Hunk")
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.run_operation(
                                                GitWindowOperation::StageLines(
                                                    stage_path.clone(),
                                                    stage_units.clone(),
                                                ),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "git-hunk-discard:{hunk_index}"
                                    )))
                                    .danger()
                                    .ghost()
                                    .compact()
                                    .label("丢弃 Hunk")
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.confirm_discard_lines(
                                                discard_path.clone(),
                                                discard_units.clone(),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                            },
                        )
                    }),
            )
            .child(lines)
            .into_any_element()
    }

    fn render_diff_line(
        &self,
        diff: &GitFileChangesDiff,
        hunk: &kite_core::git_changes::GitDiffHunk,
        line: &kite_core::git_changes::GitHunkLine,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let canonical = canonical_selection_for_line(diff, hunk, line);
        let selected = canonical.is_some_and(|selection| self.checked_lines.contains(&selection));
        let (symbol, background, foreground) = match line.kind {
            GitHunkLineKind::Added => (
                "+",
                Some(cx.theme().success.opacity(0.09)),
                cx.theme().success,
            ),
            GitHunkLineKind::Removed => (
                "-",
                Some(cx.theme().danger.opacity(0.09)),
                cx.theme().danger,
            ),
            GitHunkLineKind::Context => (" ", None, cx.theme().muted_foreground),
        };
        let mut row = h_flex()
            .id(SharedString::from(format!(
                "git-diff-line:{}:{}:{}",
                hunk.index,
                line.old_line.unwrap_or(0),
                line.new_line.unwrap_or(0)
            )))
            .w_full()
            .min_h(px(20.))
            .items_center()
            .text_xs();
        if let Some(background) = background {
            row = row.bg(background);
        }
        if let Some(canonical) = canonical {
            row = row.child(
                Button::new(SharedString::from(format!(
                    "git-line-check:{}:{}:{}",
                    canonical.hunk_index,
                    canonical.old_line.unwrap_or(0),
                    canonical.new_line.unwrap_or(0)
                )))
                .ghost()
                .compact()
                .label(if selected { "☑" } else { "☐" })
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_line(canonical, cx)),
                ),
            );
        } else {
            row = row.child(div().w(px(22.)));
        }
        row.child(
            div()
                .w(px(38.))
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child(
                    line.old_line
                        .map(|number| number.to_string())
                        .unwrap_or_default(),
                ),
        )
        .child(
            div()
                .w(px(38.))
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child(
                    line.new_line
                        .map(|number| number.to_string())
                        .unwrap_or_default(),
                ),
        )
        .child(
            div()
                .w(px(18.))
                .text_center()
                .text_color(foreground)
                .child(symbol),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .text_color(if line.kind == GitHunkLineKind::Context {
                    cx.theme().foreground
                } else {
                    foreground
                })
                .child(if line.text.is_empty() {
                    " ".to_string()
                } else {
                    line.text.clone()
                }),
        )
        .into_any_element()
    }
}

fn canonical_selection_for_line(
    diff: &GitFileChangesDiff,
    hunk: &kite_core::git_changes::GitDiffHunk,
    line: &kite_core::git_changes::GitHunkLine,
) -> Option<GitLineSelection> {
    if line.kind == GitHunkLineKind::Context {
        return None;
    }
    diff.selection_units_for_hunk(hunk.index)
        .into_iter()
        .find(|selection| {
            (line.old_line.is_some() && selection.old_line == line.old_line)
                || (line.new_line.is_some() && selection.new_line == line.new_line)
        })
}

fn status_color(cx: &Context<GitClientWindow>, status: kite_core::fs::VcsStatus) -> Hsla {
    use kite_core::fs::VcsStatus;
    match status {
        VcsStatus::Conflicted | VcsStatus::Deleted | VcsStatus::Missing => cx.theme().danger,
        VcsStatus::Added | VcsStatus::Renamed => cx.theme().success,
        VcsStatus::Modified | VcsStatus::PropertyModified => cx.theme().warning,
        VcsStatus::Untracked | VcsStatus::Ignored => cx.theme().info,
        VcsStatus::External => cx.theme().muted_foreground,
    }
}

fn git_file_icon(path: &Path) -> IconName {
    match path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico") => {
            IconName::FileImage
        }
        Some("mp3" | "wav" | "flac" | "aac" | "ogg") => IconName::FileMusic,
        Some("mp4" | "mkv" | "mov" | "avi" | "webm") => IconName::FileVideoCamera,
        Some("zip" | "7z" | "rar" | "tar" | "gz") => IconName::FileArchive,
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "html" | "css" | "c" | "cpp" | "h" | "cs"
            | "java" | "go" | "kt",
        ) => IconName::FileCode,
        Some(
            "txt" | "md" | "log" | "ini" | "toml" | "yaml" | "yml" | "json" | "pdf" | "doc"
            | "docx" | "xls" | "xlsx" | "ppt" | "pptx",
        ) => IconName::FileText,
        _ => IconName::File,
    }
}

fn empty_state(cx: &Context<GitClientWindow>, message: impl Into<SharedString>) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message.into())
        .into_any_element()
}

impl GitClientWindow {
    fn render_operation_banner(
        &self,
        operation: GitOperationState,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (title, detail) = match operation {
            GitOperationState::Rebasing => {
                ("正在变基", "解决冲突后可继续、跳过当前提交或中止变基。")
            }
            GitOperationState::Merging => {
                ("正在合并", "解决冲突后可继续合并，或中止并回到合并前。")
            }
            GitOperationState::CherryPicking => {
                ("正在拣选提交", "解决冲突后可继续、跳过当前提交或中止拣选。")
            }
            GitOperationState::Reverting => {
                ("正在还原提交", "解决冲突后可继续、跳过当前提交或中止还原。")
            }
            GitOperationState::Bisecting => ("正在二分查找", "可以结束并重置当前二分查找。"),
        };
        h_flex()
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.warning.opacity(0.45))
            .bg(theme.warning.opacity(0.10))
            .child(Icon::new(IconName::Info).size_4().text_color(theme.warning))
            .child(
                v_flex()
                    .flex_1()
                    .gap_0p5()
                    .child(div().text_sm().font_semibold().child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    ),
            )
            .when(!matches!(operation, GitOperationState::Bisecting), |this| {
                this.child(
                    Button::new("git-operation-continue")
                        .primary()
                        .compact()
                        .label("继续")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.start_git_control(GitOperationAction::Continue, "继续", window, cx)
                        })),
                )
            })
            .when(
                !matches!(
                    operation,
                    GitOperationState::Merging | GitOperationState::Bisecting
                ),
                |this| {
                    this.child(
                        Button::new("git-operation-skip")
                            .outline()
                            .compact()
                            .label("跳过")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_git_control(GitOperationAction::Skip, "跳过", window, cx)
                            })),
                    )
                },
            )
            .child(
                Button::new("git-operation-abort")
                    .outline()
                    .compact()
                    .label(if matches!(operation, GitOperationState::Bisecting) {
                        "结束二分"
                    } else {
                        "中止"
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_git_control(GitOperationAction::Abort, "中止", window, cx)
                    })),
            )
            .into_any_element()
    }

    fn render_main_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let main = h_resizable("git-client-horizontal")
            .with_state(&self.horizontal_state)
            .child(
                resizable_panel()
                    .size(px(self.initial_changes_width))
                    .size_range(px(260.)..px(560.))
                    .fixed()
                    .child(self.render_changes_panel(cx)),
            )
            .child(resizable_panel().child(self.render_diff(cx)))
            .into_any_element();
        if self.console_expanded {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    v_resizable("git-client-vertical")
                        .with_state(&self.vertical_state)
                        .child(resizable_panel().child(main))
                        .child(
                            resizable_panel()
                                .size(px(self.initial_console_height))
                                .size_range(px(110.)..px(380.))
                                .fixed()
                                .child(self.render_console(cx)),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap_1()
                .child(main)
                .child(
                    div()
                        .h(px(32.))
                        .flex_shrink_0()
                        .child(self.render_console(cx)),
                )
                .into_any_element()
        }
    }

    fn render_console(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut console = v_flex()
            .id("git-console")
            .size_full()
            .min_h_0()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background.opacity(0.84))
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_shrink_0()
                    .gap_2()
                    .px_3()
                    .items_center()
                    .border_b_1()
                    .border_color(if self.console_expanded {
                        theme.border
                    } else {
                        theme.border.opacity(0.0)
                    })
                    .child(
                        Icon::new(IconName::PanelBottomOpen)
                            .size_3p5()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().text_xs().font_semibold().child("Git 控制台"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if self.task.is_some() {
                            "后台任务运行中"
                        } else {
                            "就绪"
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("git-console-collapse")
                            .ghost()
                            .compact()
                            .label(if self.console_expanded {
                                "收起"
                            } else {
                                "展开"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.console_expanded = !this.console_expanded;
                                cx.notify();
                            })),
                    ),
            );
        if !self.console_expanded {
            return console.into_any_element();
        }
        if let Some(task) = self.task.as_ref() {
            console = console.child(
                h_flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.primary.opacity(0.06))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_xs().font_medium().child(task.label.clone()))
                            .child(
                                Progress::new("git-client-task-progress")
                                    .loading(task.cancellation.is_some())
                                    .value(if task.cancellation.is_some() {
                                        0.35
                                    } else {
                                        0.75
                                    }),
                            ),
                    )
                    .when(task.cancellation.is_some(), |this| {
                        this.child(
                            Button::new("git-client-cancel-task")
                                .outline()
                                .compact()
                                .label("取消")
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_task(cx)),
                                ),
                        )
                    }),
            );
        }
        let mut lines = v_flex().id("git-console-lines").gap_1().p_3();
        for line in &self.console_lines {
            lines = lines.child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(theme.muted_foreground)
                    .child(line.clone()),
            );
        }
        console
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(lines),
            )
            .into_any_element()
    }

    fn render_notice(&self, notice: &GitNotice, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let color = notice_color(notice.kind);
        div()
            .absolute()
            .top(px(112.))
            .right(px(18.))
            .w(px(360.))
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(color.opacity(0.50))
            .bg(theme.background.opacity(0.96))
            .shadow_lg()
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(IconName::Info).size_4().text_color(color))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(color)
                                    .child(notice.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(notice.message.clone()),
                            )
                            .when_some(
                                notice.action.zip(notice.action_label),
                                |this, (action, label)| {
                                    this.child(
                                        Button::new("git-notice-action")
                                            .outline()
                                            .compact()
                                            .label(label)
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, window, cx| {
                                                    this.apply_notice_action(action, window, cx);
                                                },
                                            )),
                                    )
                                },
                            ),
                    )
                    .child(
                        Button::new("git-notice-close")
                            .ghost()
                            .compact()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.notification = None;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_shortcuts(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut rows = v_flex().id("git-shortcut-list").gap_2();
        for (label, shortcut) in GIT_CLIENT_SHORTCUTS {
            rows = rows.child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.background.opacity(0.45))
                    .child(div().text_xs().child(*label))
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(theme.muted_foreground)
                            .child(*shortcut),
                    ),
            );
        }
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background.opacity(0.82))
            .child(
                v_flex()
                    .w(px(520.))
                    .max_h(px(640.))
                    .gap_3()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_xl()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_lg().font_semibold().child("Git 客户端快捷键"))
                            .child(
                                Button::new("git-shortcuts-close")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.show_shortcuts = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(rows))
                    .child(
                        h_flex().justify_end().child(
                            Button::new("git-shortcuts-done")
                                .primary()
                                .compact()
                                .label("完成")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.show_shortcuts = false;
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }
}

const GIT_CLIENT_SHORTCUTS: &[(&str, &str)] = &[
    ("打开 Git 客户端", "Ctrl+Shift+G"),
    ("刷新仓库", "Ctrl+Shift+R"),
    ("获取", "Ctrl+Shift+F"),
    ("拉取", "Ctrl+Shift+P"),
    ("推送", "Ctrl+Shift+K"),
    ("提交", "Ctrl+K"),
    ("贮藏", "Ctrl+Alt+S"),
    ("显示或隐藏控制台", "Ctrl+J"),
    ("快捷键", "F1"),
    ("关闭提示或浮层", "Esc"),
];

fn notice_for_error(title: &'static str, error: String) -> GitNotice {
    let lower = error.to_ascii_lowercase();
    let rejected = lower.contains("non-fast-forward")
        || lower.contains("rejected")
        || lower.contains("fetch first");
    let conflict =
        lower.contains("conflict") || lower.contains("unmerged") || error.contains("冲突");
    let kind = if rejected || conflict {
        GitNoticeKind::Conflict
    } else {
        GitNoticeKind::Error
    };
    GitNotice {
        kind,
        title: if rejected {
            "推送被拒绝"
        } else if conflict {
            "需要解决冲突"
        } else {
            title
        }
        .into(),
        message: error.into(),
        action: Some(if rejected {
            GitNoticeAction::Pull
        } else if conflict {
            GitNoticeAction::ShowChanges
        } else {
            GitNoticeAction::OpenConsole
        }),
        action_label: Some(if rejected {
            "拉取"
        } else if conflict {
            "查看变更"
        } else {
            "查看控制台"
        }),
    }
}

fn notice_color(kind: GitNoticeKind) -> Hsla {
    match kind {
        GitNoticeKind::Success => rgb(0x55b978).into(),
        GitNoticeKind::Info => rgb(0x5e9cff).into(),
        GitNoticeKind::Warning => rgb(0xf47a32).into(),
        GitNoticeKind::Error | GitNoticeKind::Conflict => rgb(0xe5484d).into(),
    }
}

impl Render for GitClientWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notification = self
            .notification
            .clone()
            .map(|notice| self.render_notice(&notice, cx));
        let shortcuts = self.show_shortcuts.then(|| self.render_shortcuts(cx));
        v_flex()
            .id("git-client-root")
            .relative()
            .size_full()
            .gap_1()
            .overflow_hidden()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .key_context("GitClient")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_fetch))
            .on_action(cx.listener(Self::on_pull))
            .on_action(cx.listener(Self::on_push))
            .on_action(cx.listener(Self::on_commit))
            .on_action(cx.listener(Self::on_stash))
            .on_action(cx.listener(Self::on_toggle_console))
            .on_action(cx.listener(Self::on_show_shortcuts))
            .on_action(cx.listener(Self::on_continue))
            .on_action(cx.listener(Self::on_skip))
            .on_action(cx.listener(Self::on_abort))
            .on_action(cx.listener(Self::on_cancel_task))
            .on_action(cx.listener(Self::on_dismiss))
            .child(self.render_header(cx))
            .child(self.render_main_content(cx))
            .when_some(notification, |this, notification| this.child(notification))
            .when_some(shortcuts, |this, shortcuts| this.child(shortcuts))
    }
}
