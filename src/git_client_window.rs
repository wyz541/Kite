use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::actions::{
    GitClientAbort, GitClientCancelTask, GitClientCommit, GitClientContinue, GitClientDismiss,
    GitClientFetch, GitClientNextCommit, GitClientPreviousCommit, GitClientPull, GitClientPush,
    GitClientRefresh, GitClientShowChanges, GitClientShowDetails, GitClientShowLog,
    GitClientShowShortcuts, GitClientSkip, GitClientStash, GitClientToggleConsole,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{
    GitChangedFile, GitOperationAction, GitOperationState, GitRepositorySnapshot,
    git_repository_snapshot, run_git_operation_control,
};
use kite_core::session::{
    GitClientWindowSessionDto, load_git_client_window_state, save_git_client_window_state,
};
use kite_core::vcs::{VcsCancellation, VcsOperation, VcsTool, run_vcs_operation_cancellable};

const DEFAULT_WINDOW_WIDTH: u32 = 1_180;
const DEFAULT_WINDOW_HEIGHT: u32 = 780;
const DEFAULT_CHANGES_RATIO_BP: u16 = 2_800;
const DEFAULT_DETAILS_RATIO_BP: u16 = 3_300;
const DEFAULT_CONSOLE_RATIO_BP: u16 = 1_800;
const MIN_WINDOW_WIDTH: u32 = 760;
const MIN_WINDOW_HEIGHT: u32 = 520;
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
    let title = format!(
        "Git — {}",
        repository_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| repository_root.display().to_string())
    );
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
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
    .map_err(|error| format!("无法打开 Git 客户端：{error}"))?;
    open_windows()
        .lock()
        .map_err(|_| "无法保存 Git 窗口状态".to_string())?
        .insert(key, handle);
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitClientView {
    Changes,
    Log,
    Details,
}

impl GitClientView {
    fn as_str(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Log => "log",
            Self::Details => "details",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "changes" => Self::Changes,
            "details" => Self::Details,
            _ => Self::Log,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Changes => "变更",
            Self::Log => "日志",
            Self::Details => "提交详情",
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

pub(crate) struct GitClientWindow {
    repository_root: PathBuf,
    snapshot: Option<GitRepositorySnapshot>,
    selected_commit: usize,
    active_view: GitClientView,
    task: Option<GitClientTask>,
    loading: bool,
    console_expanded: bool,
    console_lines: Vec<SharedString>,
    notice: Option<GitNotice>,
    show_shortcuts: bool,
    commit_input: Entity<TextareaState>,
    horizontal_state: Entity<ResizableState>,
    vertical_state: Entity<ResizableState>,
    initial_changes_width: f32,
    initial_details_width: f32,
    initial_console_height: f32,
    task_generation: u64,
    focus_handle: FocusHandle,
    _load_task: Task<()>,
    _operation_task: Task<()>,
}

impl GitClientWindow {
    fn new(
        repository_root: PathBuf,
        saved: Option<GitClientWindowSessionDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let width = saved
            .as_ref()
            .map_or(DEFAULT_WINDOW_WIDTH, |state| state.width)
            .max(MIN_WINDOW_WIDTH) as f32;
        let changes_ratio_bp = saved
            .as_ref()
            .map_or(DEFAULT_CHANGES_RATIO_BP, |state| state.changes_ratio_bp);
        let details_ratio_bp = saved
            .as_ref()
            .map_or(DEFAULT_DETAILS_RATIO_BP, |state| state.details_ratio_bp);
        let console_ratio_bp = saved
            .as_ref()
            .map_or(DEFAULT_CONSOLE_RATIO_BP, |state| state.console_ratio_bp);
        let height = saved
            .as_ref()
            .map_or(DEFAULT_WINDOW_HEIGHT, |state| state.height)
            .max(MIN_WINDOW_HEIGHT) as f32;
        let commit_input = cx.new(|cx| TextareaState::new(window, cx));
        let mut this = Self {
            repository_root,
            snapshot: None,
            selected_commit: 0,
            active_view: saved.as_ref().map_or(GitClientView::Log, |state| {
                GitClientView::from_str(&state.last_view)
            }),
            task: None,
            loading: true,
            console_expanded: saved.as_ref().is_none_or(|state| state.console_expanded),
            console_lines: vec!["Git 客户端已就绪。".into()],
            notice: None,
            show_shortcuts: false,
            commit_input,
            horizontal_state: cx.new(|_| ResizableState::default()),
            vertical_state: cx.new(|_| ResizableState::default()),
            initial_changes_width: width * f32::from(changes_ratio_bp) / 10_000.0,
            initial_details_width: width * f32::from(details_ratio_bp) / 10_000.0,
            initial_console_height: height * f32::from(console_ratio_bp) / 10_000.0,
            task_generation: 0,
            focus_handle: cx.focus_handle(),
            _load_task: Task::ready(()),
            _operation_task: Task::ready(()),
        };
        this.register_close_persistence(window, cx);
        cx.defer_in(window, |this, window, cx| {
            window.focus(&this.focus_handle, cx);
            cx.notify();
        });
        this.start_refresh(cx);
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
        let (changes_ratio_bp, details_ratio_bp) = if horizontal.len() >= 3 && total_width > 0.0 {
            (
                ((horizontal[0].as_f32() / total_width) * 10_000.0)
                    .round()
                    .clamp(1.0, u16::MAX as f32) as u16,
                ((horizontal[2].as_f32() / total_width) * 10_000.0)
                    .round()
                    .clamp(1.0, u16::MAX as f32) as u16,
            )
        } else {
            (DEFAULT_CHANGES_RATIO_BP, DEFAULT_DETAILS_RATIO_BP)
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
            details_ratio_bp,
            console_ratio_bp,
            console_expanded: self.console_expanded,
            last_view: self.active_view.as_str().to_string(),
        };
        if let Err(error) = save_git_client_window_state(&state) {
            eprintln!("无法保存 Git 客户端窗口状态：{error}");
        }
    }

    fn start_refresh(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            return;
        }
        self.task_generation = self.task_generation.wrapping_add(1);
        let generation = self.task_generation;
        self.loading = true;
        self.task = Some(GitClientTask {
            label: "读取仓库状态".into(),
            cancellation: None,
        });
        self.push_console("刷新仓库状态…");
        let repository_root = self.repository_root.clone();
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { git_repository_snapshot(&repository_root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.task_generation != generation {
                    return;
                }
                this.loading = false;
                this.task = None;
                match result {
                    Ok(Some(snapshot)) => {
                        this.push_console(format!(
                            "已读取 {} 个变更文件和 {} 个提交。",
                            snapshot.changed_files.len(),
                            snapshot.recent_commits.len()
                        ));
                        this.snapshot = Some(snapshot);
                        this.selected_commit = this.selected_commit.min(
                            this.snapshot.as_ref().map_or(0, |snapshot| {
                                snapshot.recent_commits.len().saturating_sub(1)
                            }),
                        );
                    }
                    Ok(None) => {
                        this.snapshot = None;
                        this.notice = Some(GitNotice {
                            kind: GitNoticeKind::Error,
                            title: "无法打开仓库".into(),
                            message: "当前路径不在 Git 工作区中。".into(),
                            action: Some(GitNoticeAction::OpenConsole),
                            action_label: Some("查看控制台"),
                        });
                    }
                    Err(error) => {
                        this.push_console(format!("读取失败：{error}"));
                        this.notice = Some(GitNotice {
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
    fn push_console(&mut self, message: impl Into<SharedString>) {
        self.console_lines
            .push(format!("› {}", message.into()).into());
        if self.console_lines.len() > MAX_CONSOLE_LINES {
            let overflow = self.console_lines.len() - MAX_CONSOLE_LINES;
            self.console_lines.drain(0..overflow);
        }
    }

    fn set_view(&mut self, view: GitClientView, cx: &mut Context<Self>) {
        self.active_view = view;
        cx.notify();
    }

    fn select_previous_commit(&mut self, cx: &mut Context<Self>) {
        if self.selected_commit > 0 {
            self.selected_commit -= 1;
            self.active_view = GitClientView::Details;
            cx.notify();
        }
    }

    fn select_next_commit(&mut self, cx: &mut Context<Self>) {
        let count = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.recent_commits.len());
        if self.selected_commit + 1 < count {
            self.selected_commit += 1;
            self.active_view = GitClientView::Details;
            cx.notify();
        }
    }

    fn start_operation(
        &mut self,
        operation: VcsOperation,
        label: &'static str,
        clear_commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task.is_some() {
            self.notice = Some(GitNotice {
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
                        this.notice = Some(GitNotice {
                            kind: GitNoticeKind::Success,
                            title: label.into(),
                            message: message.into(),
                            action: Some(GitNoticeAction::Refresh),
                            action_label: Some("刷新"),
                        });
                        this.start_refresh(cx);
                    }
                    Err(error) => {
                        this.push_console(format!("{label}失败：{error}"));
                        this.notice = Some(notice_for_error(label, error));
                    }
                }
                cx.notify();
            });
        });
    }

    fn start_control(
        &mut self,
        action: GitOperationAction,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task.is_some() {
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
                        this.notice = Some(GitNotice {
                            kind: GitNoticeKind::Success,
                            title: "Git 操作已继续".into(),
                            message: message.into(),
                            action: Some(GitNoticeAction::Refresh),
                            action_label: Some("刷新"),
                        });
                        this.start_refresh(cx);
                    }
                    Err(error) => {
                        this.push_console(format!("{verb}失败：{error}"));
                        this.notice = Some(notice_for_error(verb, error));
                    }
                }
                cx.notify();
            });
        });
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.notice = Some(GitNotice {
                kind: GitNoticeKind::Warning,
                title: "提交信息为空".into(),
                message: "输入提交信息后才能提交。".into(),
                action: None,
                action_label: None,
            });
            cx.notify();
            return;
        }
        self.start_operation(VcsOperation::Commit(message), "Git 提交", true, window, cx);
    }

    fn cancel_task(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.task.take() else {
            return;
        };
        if let Some(cancellation) = task.cancellation {
            cancellation.cancel();
        }
        self.task_generation = self.task_generation.wrapping_add(1);
        self.loading = false;
        self.push_console(format!("已取消：{}", task.label));
        self.notice = Some(GitNotice {
            kind: GitNoticeKind::Info,
            title: "任务已取消".into(),
            message: task.label.into(),
            action: None,
            action_label: None,
        });
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.show_shortcuts {
            self.show_shortcuts = false;
        } else {
            self.notice = None;
        }
        cx.notify();
    }

    fn apply_notice_action(
        &mut self,
        action: GitNoticeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notice = None;
        match action {
            GitNoticeAction::Refresh => self.start_refresh(cx),
            GitNoticeAction::OpenConsole => self.console_expanded = true,
        }
        cx.notify();
        let _ = window;
    }

    fn on_refresh(&mut self, _: &GitClientRefresh, _: &mut Window, cx: &mut Context<Self>) {
        self.start_refresh(cx);
    }

    fn on_fetch(&mut self, _: &GitClientFetch, window: &mut Window, cx: &mut Context<Self>) {
        self.start_operation(VcsOperation::Fetch, "Git 获取", false, window, cx);
    }

    fn on_pull(&mut self, _: &GitClientPull, window: &mut Window, cx: &mut Context<Self>) {
        self.start_operation(VcsOperation::Pull, "Git 拉取", false, window, cx);
    }

    fn on_push(&mut self, _: &GitClientPush, window: &mut Window, cx: &mut Context<Self>) {
        self.start_operation(VcsOperation::Push, "Git 推送", false, window, cx);
    }

    fn on_commit(&mut self, _: &GitClientCommit, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
    }

    fn on_stash(&mut self, _: &GitClientStash, window: &mut Window, cx: &mut Context<Self>) {
        self.start_operation(VcsOperation::Stash, "Git 贮藏", false, window, cx);
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

    fn on_show_changes(
        &mut self,
        _: &GitClientShowChanges,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_view(GitClientView::Changes, cx);
    }

    fn on_show_log(&mut self, _: &GitClientShowLog, _: &mut Window, cx: &mut Context<Self>) {
        self.set_view(GitClientView::Log, cx);
    }

    fn on_show_details(
        &mut self,
        _: &GitClientShowDetails,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_view(GitClientView::Details, cx);
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
        self.start_control(GitOperationAction::Continue, "继续", window, cx);
    }

    fn on_skip(&mut self, _: &GitClientSkip, window: &mut Window, cx: &mut Context<Self>) {
        self.start_control(GitOperationAction::Skip, "跳过", window, cx);
    }

    fn on_abort(&mut self, _: &GitClientAbort, window: &mut Window, cx: &mut Context<Self>) {
        self.start_control(GitOperationAction::Abort, "中止", window, cx);
    }

    fn on_cancel_task(&mut self, _: &GitClientCancelTask, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_task(cx);
    }

    fn on_dismiss(&mut self, _: &GitClientDismiss, _: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(cx);
    }

    fn on_previous_commit(
        &mut self,
        _: &GitClientPreviousCommit,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_previous_commit(cx);
    }

    fn on_next_commit(&mut self, _: &GitClientNextCommit, _: &mut Window, cx: &mut Context<Self>) {
        self.select_next_commit(cx);
    }
    fn render_toolbar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let snapshot = self.snapshot.as_ref();
        let branch = snapshot
            .and_then(|snapshot| snapshot.branch.clone())
            .or_else(|| {
                snapshot
                    .and_then(|snapshot| snapshot.head_short_hash.clone())
                    .map(|hash| format!("分离 HEAD {hash}"))
            })
            .unwrap_or_else(|| "无分支".to_string());
        let upstream = snapshot.and_then(|snapshot| snapshot.upstream.clone());
        let ahead = snapshot.map_or(0, |snapshot| snapshot.ahead);
        let behind = snapshot.map_or(0, |snapshot| snapshot.behind);
        let busy = self.task.is_some();

        h_flex()
            .id("git-client-toolbar")
            .h(px(48.))
            .w_full()
            .flex_shrink_0()
            .gap_2()
            .px_3()
            .items_center()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(theme.primary.opacity(0.14))
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .size_4()
                                    .text_color(theme.primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.muted_foreground)
                                    .child("GIT 客户端"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .max_w(px(280.))
                                    .truncate()
                                    .child(repository_name(&self.repository_root)),
                            ),
                    ),
            )
            .child(
                Button::new("git-branch-pill")
                    .ghost()
                    .compact()
                    .icon(IconName::GitBranch)
                    .label(branch)
                    .tooltip(upstream.unwrap_or_else(|| "当前分支".to_string()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.set_view(GitClientView::Log, cx)
                    })),
            )
            .when(ahead > 0 || behind > 0, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("↑{ahead}"))
                        .child(format!("↓{behind}")),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("git-refresh")
                    .ghost()
                    .compact()
                    .icon(IconName::RotateCw)
                    .label("刷新")
                    .tooltip("刷新仓库 (Ctrl+Shift+R)")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _window, cx| this.start_refresh(cx)),
                    ),
            )
            .child(
                Button::new("git-fetch")
                    .ghost()
                    .compact()
                    .icon(IconName::RotateCw)
                    .label("获取")
                    .tooltip("获取 (Ctrl+Shift+F)")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_operation(VcsOperation::Fetch, "Git 获取", false, window, cx)
                    })),
            )
            .child(
                Button::new("git-pull")
                    .ghost()
                    .compact()
                    .icon(IconName::ArrowDown)
                    .label("拉取")
                    .tooltip("拉取 (Ctrl+Shift+P)")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_operation(VcsOperation::Pull, "Git 拉取", false, window, cx)
                    })),
            )
            .child(
                Button::new("git-push")
                    .ghost()
                    .compact()
                    .icon(IconName::ArrowUp)
                    .label("推送")
                    .tooltip("推送 (Ctrl+Shift+K)")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_operation(VcsOperation::Push, "Git 推送", false, window, cx)
                    })),
            )
            .child(
                Button::new("git-commit-toolbar")
                    .primary()
                    .compact()
                    .icon(IconName::Check)
                    .label("提交")
                    .tooltip("提交 (Ctrl+K)")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.commit(window, cx)),
                    ),
            )
            .child(
                Button::new("git-stash")
                    .ghost()
                    .compact()
                    .label("贮藏")
                    .tooltip("暂存工作区 (Ctrl+Alt+S)")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_operation(VcsOperation::Stash, "Git 贮藏", false, window, cx)
                    })),
            )
            .child(
                Button::new("git-console-toggle")
                    .ghost()
                    .compact()
                    .icon(if self.console_expanded {
                        IconName::PanelBottomOpen
                    } else {
                        IconName::PanelBottomOpen
                    })
                    .tooltip("显示或隐藏 Git 控制台 (Ctrl+J)")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.console_expanded = !this.console_expanded;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("git-shortcuts")
                    .ghost()
                    .compact()
                    .icon(IconName::Info)
                    .tooltip("快捷键 (F1)")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.show_shortcuts = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_operation_banner(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let operation = self.snapshot.as_ref()?.operation?;
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
            GitOperationState::Bisecting => {
                ("正在二分查找", "标记 good / bad 后继续，或中止并重置。")
            }
        };
        Some(
            h_flex()
                .w_full()
                .flex_shrink_0()
                .gap_3()
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
                                this.start_control(GitOperationAction::Continue, "继续", window, cx)
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
                                    this.start_control(GitOperationAction::Skip, "跳过", window, cx)
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
                            this.start_control(GitOperationAction::Abort, "中止", window, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_changes_panel(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let snapshot = self.snapshot.as_ref();
        let files = snapshot.map_or(&[][..], |snapshot| snapshot.changed_files.as_slice());
        let staged = files
            .iter()
            .filter_map(|file| file.staged_status.map(|status| (file, status)))
            .collect::<Vec<_>>();
        let unstaged = files
            .iter()
            .filter_map(|file| {
                file.unstaged_status
                    .filter(|status| *status != kite_core::fs::VcsStatus::Untracked)
                    .map(|status| (file, status))
            })
            .collect::<Vec<_>>();
        let untracked = files
            .iter()
            .filter_map(|file| {
                (file.unstaged_status == Some(kite_core::fs::VcsStatus::Untracked))
                    .then_some((file, kite_core::fs::VcsStatus::Untracked))
            })
            .collect::<Vec<_>>();
        let conflicts = files
            .iter()
            .filter(|file| {
                file.staged_status == Some(kite_core::fs::VcsStatus::Conflicted)
                    || file.unstaged_status == Some(kite_core::fs::VcsStatus::Conflicted)
            })
            .count();
        let empty = files.is_empty();
        let busy = self.task.is_some();
        let commit_text = self.commit_input.read(cx).value();
        let active_border = self.panel_border(GitClientView::Changes, theme);

        v_flex()
            .id("git-changes-panel")
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(active_border)
            .rounded_lg()
            .bg(theme.background.opacity(0.72))
            .child(panel_header(
                "变更",
                snapshot.map_or(0, |snapshot| snapshot.changed_files.len()),
                theme,
            ))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("git-stage-all")
                            .outline()
                            .compact()
                            .label("全部暂存")
                            .disabled(busy || files.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_operation(
                                    VcsOperation::Add,
                                    "暂存全部变更",
                                    false,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("git-unstage-all")
                            .ghost()
                            .compact()
                            .label("全部取消暂存")
                            .disabled(busy || staged.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_operation(
                                    VcsOperation::Unstage,
                                    "取消暂存全部变更",
                                    false,
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("git-change-list")
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .overflow_y_scroll()
                    .when(empty, |this| {
                        this.child(empty_state(
                            "工作区干净",
                            "当前没有已暂存、未暂存或未跟踪的变更。",
                            theme,
                        ))
                    })
                    .when(!staged.is_empty(), |this| {
                        this.child(change_group("已暂存", theme.success, &staged, theme))
                    })
                    .when(!unstaged.is_empty(), |this| {
                        this.child(change_group("未暂存", theme.warning, &unstaged, theme))
                    })
                    .when(!untracked.is_empty(), |this| {
                        this.child(change_group(
                            "未跟踪",
                            rgb(0x45b8d8).into(),
                            &untracked,
                            theme,
                        ))
                    })
                    .when(conflicts > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.danger)
                                .child(format!("冲突  {conflicts}")),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child("提交信息"),
                    )
                    .child(
                        div()
                            .relative()
                            .h(px(92.))
                            .w_full()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .overflow_hidden()
                            .child(
                                Textarea::new(&self.commit_input)
                                    .h(px(92.))
                                    .w_full()
                                    .appearance(false)
                                    .aria_label("提交信息"),
                            )
                            .when(commit_text.is_empty(), |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top(px(10.))
                                        .left(px(10.))
                                        .text_xs()
                                        .text_color(theme.muted_foreground.opacity(0.72))
                                        .child("说明这次修改的原因…"),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Ctrl+K 提交"),
                            )
                            .child(
                                Button::new("git-commit-panel")
                                    .primary()
                                    .compact()
                                    .label("提交")
                                    .icon(IconName::Check)
                                    .disabled(busy || commit_text.trim().is_empty())
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.commit(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_log_panel(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let commits = self
            .snapshot
            .as_ref()
            .map_or(&[][..], |snapshot| snapshot.recent_commits.as_slice());
        let active_border = self.panel_border(GitClientView::Log, theme);
        let mut list = v_flex()
            .id("git-log-list")
            .flex_1()
            .min_h_0()
            .gap_1()
            .overflow_y_scroll();
        if commits.is_empty() {
            list = list.child(empty_state(
                "暂无提交",
                "仓库还没有可显示的提交记录。",
                theme,
            ));
        } else {
            for (index, commit) in commits.iter().enumerate() {
                let selected = index == self.selected_commit;
                list = list.child(
                    h_flex()
                        .id(format!("git-log-{index}"))
                        .w_full()
                        .gap_2()
                        .px_2()
                        .py_2()
                        .rounded_md()
                        .bg(if selected {
                            theme.primary.opacity(0.10)
                        } else {
                            theme.background.opacity(0.0)
                        })
                        .border_1()
                        .border_color(if selected {
                            theme.primary.opacity(0.28)
                        } else {
                            theme.border.opacity(0.0)
                        })
                        .child(
                            v_flex()
                                .w(px(20.))
                                .flex_shrink_0()
                                .items_center()
                                .gap_1()
                                .pt_0p5()
                                .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(if selected {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                }))
                                .child(div().w(px(1.)).flex_1().bg(theme.border)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .truncate()
                                        .child(commit.subject.clone()),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(commit.short_hash.clone())
                                        .child(commit.author.clone())
                                        .child(commit_time_label(commit.unix_seconds)),
                                ),
                        )
                        .on_click(cx.listener({
                            move |this, _: &ClickEvent, _window, cx| {
                                this.selected_commit = index;
                                this.active_view = GitClientView::Details;
                                cx.notify();
                            }
                        })),
                );
            }
        }

        v_flex()
            .id("git-log-panel")
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(active_border)
            .rounded_lg()
            .bg(theme.background.opacity(0.72))
            .child(panel_header("日志", commits.len(), theme))
            .child(list)
            .into_any_element()
    }

    fn render_details_panel(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let commit = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.recent_commits.get(self.selected_commit));
        let active_border = self.panel_border(GitClientView::Details, theme);
        let mut details = v_flex()
            .id("git-details-scroll")
            .flex_1()
            .min_h_0()
            .gap_3()
            .overflow_y_scroll();
        if let Some(commit) = commit {
            details = details
                .child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .bg(theme.background.opacity(0.55))
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(theme.primary)
                                        .child(commit.short_hash.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(commit_time_label(commit.unix_seconds)),
                                ),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .line_height(px(22.))
                                .child(commit.subject.clone()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(commit.author.clone()),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.muted_foreground)
                                .child("完整提交哈希"),
                        )
                        .child(
                            div()
                                .p_2()
                                .rounded_md()
                                .bg(theme.background.opacity(0.60))
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .child(commit.hash.clone()),
                        ),
                );
        } else {
            details = details.child(empty_state(
                "选择一个提交",
                "在日志中选择提交以查看作者、时间和提交信息。",
                theme,
            ));
        }
        details = details.child(
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.muted_foreground)
                                .child("变更文件"),
                        )
                        .child(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme.background.opacity(0.65))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("待接入"),
                        ),
                )
                .child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background.opacity(0.45))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("当前版本先显示提交元数据；提交文件清单将在后续日志视图中提供。"),
                ),
        );

        v_flex()
            .id("git-details-panel")
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(active_border)
            .rounded_lg()
            .bg(theme.background.opacity(0.72))
            .child(panel_header("提交详情", 0, theme))
            .child(details)
            .into_any_element()
    }

    fn panel_border(&self, view: GitClientView, theme: &gpui_kit::component::Theme) -> Hsla {
        if self.active_view == view {
            theme.primary.opacity(0.38)
        } else {
            theme.border
        }
    }
    fn render_main_content(&self, cx: &Context<Self>) -> AnyElement {
        h_resizable("git-client-horizontal")
            .with_state(&self.horizontal_state)
            .child(
                resizable_panel()
                    .size(px(self.initial_changes_width))
                    .size_range(px(220.)..px(480.))
                    .fixed()
                    .child(self.render_changes_panel(cx)),
            )
            .child(resizable_panel().child(self.render_log_panel(cx)))
            .child(
                resizable_panel()
                    .size(px(self.initial_details_width))
                    .size_range(px(250.)..px(520.))
                    .fixed()
                    .child(self.render_details_panel(cx)),
            )
            .into_any_element()
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
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
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
                                        0.0
                                    } else {
                                        65.0
                                    }),
                            ),
                    )
                    .child(
                        Button::new("git-client-cancel-task")
                            .outline()
                            .compact()
                            .label("取消")
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.cancel_task(cx)
                            })),
                    ),
            );
        }

        let mut lines = v_flex()
            .id("git-console-lines")
            .flex_1()
            .min_h_0()
            .gap_1()
            .p_3()
            .overflow_y_scroll();
        for line in &self.console_lines {
            lines = lines.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(line.clone()),
            );
        }
        console.child(lines).into_any_element()
    }

    fn render_notice(&self, notice: &GitNotice, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let color = notice_color(notice.kind);
        let action = notice.action;
        let action_label = notice.action_label;
        h_flex()
            .id("git-client-notice")
            .absolute()
            .right(px(18.))
            .bottom(px(42.))
            .w(px(380.))
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(color.opacity(0.45))
            .bg(theme.background)
            .child(
                div()
                    .w(px(3.))
                    .h_full()
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(color),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
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
                    .when_some(action.zip(action_label), |this, (action, label)| {
                        this.child(
                            Button::new("git-notice-action")
                                .outline()
                                .compact()
                                .label(label)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.apply_notice_action(action, window, cx);
                                })),
                        )
                    }),
            )
            .child(
                Button::new("git-notice-close")
                    .ghost()
                    .compact()
                    .icon(IconName::X)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.notice = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_shortcuts(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut rows = v_flex()
            .id("git-shortcut-list")
            .flex_1()
            .min_h_0()
            .gap_1()
            .overflow_y_scroll();
        for (label, shortcut) in GIT_CLIENT_SHORTCUTS {
            rows = rows.child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .child(div().text_sm().child(*label))
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background.opacity(0.65))
                            .text_xs()
                            .font_medium()
                            .child(*shortcut),
                    ),
            );
        }
        div()
            .id("git-shortcuts-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000026))
            .child(
                v_flex()
                    .w(px(580.))
                    .h(px(520.))
                    .gap_3()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                v_flex()
                                    .gap_0p5()
                                    .child(div().text_lg().font_semibold().child("Git 客户端快捷键"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Windows 使用 Ctrl / Alt；macOS 对应 Command / Option。"),
                                    ),
                            )
                            .child(
                                Button::new("git-shortcuts-close")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.show_shortcuts = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(rows)
                    .child(
                        h_flex()
                            .justify_end()
                            .child(
                                Button::new("git-shortcuts-done")
                                    .primary()
                                    .compact()
                                    .label("完成")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.show_shortcuts = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let branch = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.branch.clone())
            .unwrap_or_else(|| "无分支".to_string());
        h_flex()
            .h(px(28.))
            .w_full()
            .flex_shrink_0()
            .gap_3()
            .px_2()
            .items_center()
            .rounded_lg()
            .bg(theme.status_bar)
            .border_1()
            .border_color(theme.status_bar_border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.repository_root.display().to_string()),
            )
            .child(branch)
            .child(self.active_view.title())
            .child("F1 快捷键")
            .into_any_element()
    }
    fn render_window_body(&self, cx: &Context<Self>) -> AnyElement {
        let banner = self.render_operation_banner(cx);
        let main = self.render_main_content(cx);
        let console = self.render_console(cx);
        let content = if self.console_expanded {
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
                                .child(console),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap_2()
                .child(main)
                .child(div().h(px(32.)).flex_shrink_0().child(console))
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .child(self.render_toolbar(cx))
            .when_some(banner, |this, banner| this.child(banner))
            .child(content)
            .child(self.render_status_bar(cx))
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
    ("聚焦变更加载区", "Alt+1"),
    ("聚焦日志", "Alt+2"),
    ("聚焦提交详情", "Alt+3"),
    ("上一个提交", "Alt+↑"),
    ("下一个提交", "Alt+↓"),
    ("快捷键", "F1"),
    ("关闭提示或浮层", "Esc"),
];

impl Render for GitClientWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = self.render_window_body(cx);
        let notice = self
            .notice
            .clone()
            .map(|notice| self.render_notice(&notice, cx));
        let shortcuts = self.show_shortcuts.then(|| self.render_shortcuts(cx));

        div()
            .id("git-client-root")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("GitClient")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_fetch))
            .on_action(cx.listener(Self::on_pull))
            .on_action(cx.listener(Self::on_push))
            .on_action(cx.listener(Self::on_commit))
            .on_action(cx.listener(Self::on_stash))
            .on_action(cx.listener(Self::on_toggle_console))
            .on_action(cx.listener(Self::on_show_changes))
            .on_action(cx.listener(Self::on_show_log))
            .on_action(cx.listener(Self::on_show_details))
            .on_action(cx.listener(Self::on_show_shortcuts))
            .on_action(cx.listener(Self::on_continue))
            .on_action(cx.listener(Self::on_skip))
            .on_action(cx.listener(Self::on_abort))
            .on_action(cx.listener(Self::on_cancel_task))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_previous_commit))
            .on_action(cx.listener(Self::on_next_commit))
            .child(div().size_full().p_2().child(body))
            .when_some(notice, |this, notice| this.child(notice))
            .when_some(shortcuts, |this, shortcuts| this.child(shortcuts))
    }
}

fn repository_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn panel_header(title: &str, count: usize, theme: &gpui_kit::component::Theme) -> AnyElement {
    h_flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(title.to_string()),
        )
        .child(
            div()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .bg(theme.background.opacity(0.65))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(count.to_string()),
        )
        .into_any_element()
}

fn empty_state(title: &str, detail: &str, theme: &gpui_kit::component::Theme) -> AnyElement {
    v_flex()
        .gap_1()
        .items_center()
        .justify_center()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background.opacity(0.42))
        .child(div().text_sm().font_semibold().child(title.to_string()))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .text_center()
                .child(detail.to_string()),
        )
        .into_any_element()
}

fn change_group(
    title: &str,
    color: Hsla,
    files: &[(&GitChangedFile, kite_core::fs::VcsStatus)],
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let mut rows = v_flex().gap_1();
    for (file, status) in files {
        rows = rows.child(
            h_flex()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.background.opacity(0.45))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .child(file.path.clone()),
                )
                .child(
                    div()
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .bg(color.opacity(0.12))
                        .text_xs()
                        .font_medium()
                        .text_color(color)
                        .child(status.code().to_string()),
                ),
        );
    }
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(color))
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(format!("{title}  {}", files.len())),
                ),
        )
        .child(rows)
        .into_any_element()
}

fn notice_for_error(title: &'static str, error: String) -> GitNotice {
    let lower = error.to_ascii_lowercase();
    let rejected = lower.contains("non-fast-forward")
        || lower.contains("rejected")
        || lower.contains("fetch first");
    let conflict =
        lower.contains("conflict") || lower.contains("unmerged") || error.contains("冲突");
    let kind = if rejected {
        GitNoticeKind::Conflict
    } else if conflict {
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
        action: Some(GitNoticeAction::OpenConsole),
        action_label: Some("查看控制台"),
    }
}

fn notice_color(kind: GitNoticeKind) -> Hsla {
    match kind {
        GitNoticeKind::Success => rgb(0x55b978).into(),
        GitNoticeKind::Info => rgb(0x5e9cff).into(),
        GitNoticeKind::Warning => rgb(0xf47a32).into(),
        GitNoticeKind::Error => rgb(0xe5484d).into(),
        GitNoticeKind::Conflict => rgb(0xe5484d).into(),
    }
}

fn commit_time_label(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| unix_seconds.to_string())
}

#[cfg(test)]
mod tests {
    use super::{GIT_CLIENT_SHORTCUTS, GitNoticeAction, GitNoticeKind, notice_for_error};
    use std::collections::HashMap;

    #[test]
    fn shortcut_table_has_consistent_refresh_and_git_entry() {
        let entries = GIT_CLIENT_SHORTCUTS
            .iter()
            .map(|(label, keys)| (*label, *keys))
            .collect::<HashMap<_, _>>();
        assert_eq!(entries.get("打开 Git 客户端"), Some(&"Ctrl+Shift+G"));
        assert_eq!(entries.get("刷新仓库"), Some(&"Ctrl+Shift+R"));
        assert_eq!(entries.get("提交"), Some(&"Ctrl+K"));
        assert_eq!(entries.get("贮藏"), Some(&"Ctrl+Alt+S"));
    }

    #[test]
    fn rejected_push_is_reported_as_a_conflict_notice() {
        let notice = notice_for_error(
            "Git 推送",
            "! [rejected] main -> main (non-fast-forward)".to_string(),
        );
        assert_eq!(notice.kind, GitNoticeKind::Conflict);
        assert_eq!(notice.title.as_ref(), "推送被拒绝");
        assert_eq!(notice.action, Some(GitNoticeAction::OpenConsole));
    }
}
