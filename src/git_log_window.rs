use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::resizable::{ResizableState, resizable_panel, v_resizable};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::diff::DiffEndpoint;

use crate::diff_viewer_window::{DiffLogScope, DiffViewer};
use kite_core::git::GitService;
use kite_core::git::log::{
    self, GitCommitDetails, GitGraphContext, GitGraphEdge, GitGraphRow, GitGraphScope,
    GitLogCommit, GitLogFilter, GitLogPage, GitRef, GitRefKind, GitTreeEntry,
};

const PAGE_SIZE: u32 = 200;
const NAVIGATION_WIDTH: f32 = 220.0;
const ROW_HEIGHT: f32 = 30.0;
const DETAIL_HEIGHT: f32 = 300.0;
const GRAPH_LEFT_PADDING: f32 = 4.0;
const GRAPH_LANE_MIN_WIDTH: f32 = 4.0;
const GRAPH_LANE_MAX_WIDTH: f32 = 13.0;
const GRAPH_EDGE_WIDTH: f32 = 2.0;
const GRAPH_NODE_SIZE: f32 = 8.0;
const GRAPH_COLUMN_WIDTH: f32 = 112.0;
const REFS_COLUMN_WIDTH: f32 = 210.0;
const AUTHOR_COLUMN_WIDTH: f32 = 120.0;
const DATE_COLUMN_WIDTH: f32 = 125.0;
const HASH_COLUMN_WIDTH: f32 = 74.0;
const EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const GRAPH_COLORS: [u32; 10] = [
    0x5e9cff, 0x55b978, 0xf0a45c, 0xc678dd, 0x56b6c2, 0xe06c75, 0xe5c07b, 0x7f9cf5, 0x64d1a7,
    0xd884e0,
];

static OPEN_WINDOWS: OnceLock<Mutex<HashMap<String, WindowHandle<GitLogWindow>>>> = OnceLock::new();

fn open_windows() -> &'static Mutex<HashMap<String, WindowHandle<GitLogWindow>>> {
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

fn forget_open_window(key: &str) {
    if let Ok(mut windows) = open_windows().lock() {
        windows.remove(key);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchMode {
    Text,
    Author,
    Path,
    Hash,
}

impl SearchMode {
    fn label(self) -> &'static str {
        match self {
            Self::Text => "文本",
            Self::Author => "作者",
            Self::Path => "路径",
            Self::Hash => "哈希",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Text => "搜索提交标题或说明",
            Self::Author => "按作者过滤",
            Self::Path => "按仓库相对路径过滤",
            Self::Hash => "输入提交哈希后按 Enter",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Text => Self::Author,
            Self::Author => Self::Path,
            Self::Path => Self::Hash,
            Self::Hash => Self::Text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateRange {
    Any,
    Day,
    Week,
    Month,
}

impl DateRange {
    fn label(self) -> &'static str {
        match self {
            Self::Any => "全部时间",
            Self::Day => "最近 24 小时",
            Self::Week => "最近 7 天",
            Self::Month => "最近 30 天",
        }
    }

    fn since(self) -> Option<String> {
        match self {
            Self::Any => None,
            Self::Day => Some("24 hours ago".to_string()),
            Self::Week => Some("7 days ago".to_string()),
            Self::Month => Some("30 days ago".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailTab {
    Commit,
    Changes,
    FileTree,
}

impl DetailTab {
    fn label(self) -> &'static str {
        match self {
            Self::Commit => "Commit",
            Self::Changes => "Changes",
            Self::FileTree => "File Tree",
        }
    }
}

struct GitLogWindow {
    repository_root: PathBuf,
    scope: GitGraphScope,
    context: Option<GitGraphContext>,
    commits: Vec<GitLogCommit>,
    selected_hash: Option<String>,
    details: Option<GitCommitDetails>,
    selected_change_path: Option<String>,
    tree_entries: Vec<GitTreeEntry>,
    detail_tab: DetailTab,
    search_mode: SearchMode,
    search_regex: bool,
    branch: Option<String>,
    date_range: DateRange,
    cursor: Option<String>,
    has_more: bool,
    loading: bool,
    loading_more: bool,
    details_loading: bool,
    details_loading_hash: Option<String>,
    tree_loading: bool,
    tree_loading_hash: Option<String>,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    generation: u64,
    query_input: Entity<InputState>,
    row_scroll: UniformListScrollHandle,
    detail_scroll: ScrollHandle,
    detail_resize_state: Entity<ResizableState>,
    diff_viewer: Option<Entity<DiffViewer>>,
    _subscription: Subscription,
}

impl GitLogWindow {
    fn new(
        repository_root: PathBuf,
        scope: GitGraphScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(SearchMode::Text.placeholder())
                .clean_on_escape()
        });
        let subscription = cx.subscribe_in(
            &query_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.refresh(window, cx);
                }
            },
        );
        let mut this = Self {
            repository_root,
            scope,
            context: None,
            commits: Vec::new(),
            selected_hash: None,
            details: None,
            selected_change_path: None,
            tree_entries: Vec::new(),
            detail_tab: DetailTab::Commit,
            search_mode: SearchMode::Text,
            search_regex: false,
            branch: None,
            date_range: DateRange::Any,
            cursor: None,
            has_more: false,
            loading: true,
            loading_more: false,
            details_loading: false,
            details_loading_hash: None,
            tree_loading: false,
            tree_loading_hash: None,
            error: None,
            notice: None,
            generation: 0,
            query_input,
            row_scroll: UniformListScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            detail_resize_state: cx.new(|_| ResizableState::default()),
            diff_viewer: None,
            _subscription: subscription,
        };
        this.register_close_cleanup(window, cx);
        window.set_window_title(&this.window_title());
        this.refresh(window, cx);
        this.query_input
            .update(cx, |input, cx| input.focus(window, cx));
        this
    }

    fn register_close_cleanup(&self, window: &mut Window, cx: &mut Context<Self>) {
        let key = repository_key(&self.repository_root);
        window.on_window_should_close(cx, move |_window, _app| {
            forget_open_window(&key);
            true
        });
    }

    fn window_title(&self) -> String {
        window_title_for(&self.repository_root, &self.scope)
    }

    fn scope_label(&self) -> String {
        scope_label(&self.scope)
    }

    fn scope_path(&self) -> Option<&Path> {
        self.scope.path()
    }

    fn current_filter(&self, cx: &App) -> GitLogFilter {
        let query = self.query_input.read(cx).value().trim().to_string();
        let mut filter = GitLogFilter {
            branch: self.branch.clone(),
            since: self.date_range.since(),
            regex: self.search_regex,
            ..GitLogFilter::default()
        };
        if !query.is_empty() {
            match self.search_mode {
                SearchMode::Text => filter.text = Some(query),
                SearchMode::Author => filter.author = Some(query),
                SearchMode::Path => filter.path = Some(query),
                SearchMode::Hash => filter.hash = Some(query),
            }
        }
        filter
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let repository_root = self.repository_root.clone();
        let scope = self.scope.clone();
        let filter = self.current_filter(cx);
        self.loading = true;
        self.loading_more = false;
        self.details_loading_hash = None;
        self.tree_loading_hash = None;
        self.commits.clear();
        self.selected_hash = None;
        self.details = None;
        self.selected_change_path = None;
        self.diff_viewer = None;
        self.tree_entries.clear();
        self.cursor = None;
        self.has_more = false;
        self.error = None;
        self.notice = Some("正在加载提交历史…".into());
        window.set_window_title(&self.window_title());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let context = log::git_graph_context(&repository_root, &scope)
                        .map_err(|error| error.to_string())?;
                    let page =
                        log::git_graph_page(&repository_root, &scope, filter, None, PAGE_SIZE)
                            .map_err(|error| error.to_string());
                    Ok::<_, String>((context, page))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((context, page)) => {
                        this.scope = context.scope.clone();
                        this.context = Some(context);
                        match page {
                            Ok(page) => {
                                this.notice =
                                    Some(format!("已加载 {} 个提交", page.commits.len()).into());
                                this.apply_page(page);
                                if let Some(hash) = this.selected_hash.clone() {
                                    this.select_commit(hash, false, window, cx);
                                }
                                if this.has_more {
                                    this.load_more(window, cx);
                                }
                            }
                            Err(error) if error.contains("没有可显示的提交历史") => {
                                this.error = None;
                                this.notice = Some(error.into());
                            }
                            Err(error) => {
                                this.error = Some(error.into());
                                this.notice = None;
                            }
                        }
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                        this.notice = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cursor) = self.cursor.clone() else {
            return;
        };
        if self.loading_more || !self.has_more {
            return;
        }
        self.loading_more = true;
        self.error = None;
        let generation = self.generation;
        let repository_root = self.repository_root.clone();
        let scope = self.scope.clone();
        let filter = self.current_filter(cx);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    log::git_graph_page(&repository_root, &scope, filter, Some(&cursor), PAGE_SIZE)
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading_more = false;
                match result {
                    Ok(page) => {
                        let count = page.commits.len();
                        this.apply_page(page);
                        this.notice = Some(format!("又加载了 {count} 个提交").into());
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_page(&mut self, page: GitLogPage) {
        let first_new = self.commits.is_empty();
        self.has_more = page.has_more;
        self.cursor = page.next_cursor;
        self.commits.extend(page.commits);
        if first_new {
            self.selected_hash = self.commits.first().map(|commit| commit.hash.clone());
        }
    }

    fn select_commit(
        &mut self,
        hash: String,
        open_changes: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_hash.as_deref() != Some(hash.as_str()) {
            self.selected_hash = Some(hash.clone());
            self.details = None;
            self.selected_change_path = None;
            self.diff_viewer = None;
            self.tree_entries.clear();
        }
        self.detail_tab = if open_changes {
            DetailTab::Changes
        } else {
            DetailTab::Commit
        };
        self.load_details(hash, window, cx);
        cx.notify();
    }

    fn load_details(&mut self, hash: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_loading && self.details_loading_hash.as_deref() == Some(hash.as_str()) {
            return;
        }
        self.details_loading = true;
        self.details_loading_hash = Some(hash.clone());
        let generation = self.generation;
        let repository_root = self.repository_root.clone();
        let scope = self.scope.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let hash = hash.clone();
                    async move {
                        log::git_graph_commit_details(&repository_root, &scope, &hash)
                            .map_err(|error| error.to_string())
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation
                    || this.selected_hash.as_deref() != Some(hash.as_str())
                {
                    return;
                }
                if this.details_loading_hash.as_deref() == Some(hash.as_str()) {
                    this.details_loading = false;
                    this.details_loading_hash = None;
                }
                match result {
                    Ok(details) => {
                        this.selected_change_path =
                            details.changed_files.first().map(|file| file.path.clone());
                        this.details = Some(details);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_tree(&mut self, hash: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree_loading && self.tree_loading_hash.as_deref() == Some(hash.as_str()) {
            return;
        }
        if self.selected_hash.as_deref() != Some(hash.as_str()) {
            self.selected_hash = Some(hash.clone());
            self.details = None;
            self.selected_change_path = None;
            self.diff_viewer = None;
            self.tree_entries.clear();
        }
        self.tree_loading = true;
        self.tree_loading_hash = Some(hash.clone());
        self.detail_tab = DetailTab::FileTree;
        let generation = self.generation;
        let repository_root = self.repository_root.clone();
        let scope = self.scope.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let hash = hash.clone();
                    async move {
                        log::git_graph_commit_tree(&repository_root, &scope, &hash)
                            .map_err(|error| error.to_string())
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation
                    || this.selected_hash.as_deref() != Some(hash.as_str())
                {
                    return;
                }
                if this.tree_loading_hash.as_deref() == Some(hash.as_str()) {
                    this.tree_loading = false;
                    this.tree_loading_hash = None;
                }
                match result {
                    Ok(entries) => {
                        this.tree_entries = entries;
                        if let Some(index) = this.scope_tree_index() {
                            this.detail_scroll.scroll_to_item(index);
                        }
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_scope(&mut self, scope: GitGraphScope, window: &mut Window, cx: &mut Context<Self>) {
        if self.scope == scope {
            self.refresh(window, cx);
            return;
        }
        self.scope = scope;
        self.selected_hash = None;
        self.details = None;
        self.selected_change_path = None;
        self.tree_entries.clear();
        self.refresh(window, cx);
    }

    fn clear_scope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_scope(GitGraphScope::Repository, window, cx);
    }

    fn set_branch_filter(
        &mut self,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.branch == branch {
            return;
        }
        self.branch = branch;
        self.refresh(window, cx);
    }

    fn show_file_history(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_scope(GitGraphScope::file(PathBuf::from(path)), window, cx);
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn selected_commit(&self) -> Option<&GitLogCommit> {
        let hash = self.selected_hash.as_deref()?;
        self.commits.iter().find(|commit| commit.hash == hash)
    }

    fn scope_tree_index(&self) -> Option<usize> {
        let scope_path = self.scope_path()?.to_string_lossy().replace('\\', "/");
        self.tree_entries.iter().position(|entry| {
            entry.path == scope_path || entry.path.starts_with(&format!("{scope_path}/"))
        })
    }

    fn scope_contains_path(&self, path: &str) -> bool {
        let Some(scope_path) = self.scope_path() else {
            return false;
        };
        let scope_path = scope_path.to_string_lossy().replace('\\', "/");
        path == scope_path || path.starts_with(&format!("{scope_path}/"))
    }

    fn show_embedded_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (old, new, relative_path) = {
            let Some(details) = self.details.as_ref() else {
                return;
            };
            let Some(relative_path) = self.selected_change_path.clone() else {
                return;
            };
            let old = details
                .commit
                .parents
                .first()
                .cloned()
                .map(DiffEndpoint::Revision)
                .unwrap_or_else(|| DiffEndpoint::Revision(EMPTY_TREE_HASH.to_string()));
            let new = DiffEndpoint::Revision(details.commit.hash.clone());
            (old, new, relative_path)
        };
        let path = self.repository_root.join(relative_path);
        let log_scope = DiffLogScope::from_path(&path);

        if let Some(viewer) = self.diff_viewer.as_mut() {
            viewer.update(cx, |viewer, cx| {
                viewer.show(path, old, new, Some(log_scope), window, cx);
            });
        } else {
            let workspace = cx.entity().downgrade();
            let callback: crate::diff_viewer_window::DiffLogNavigation =
                Box::new(move |scope, window, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        this.scope = match scope {
                            DiffLogScope::File(path) => GitGraphScope::File(path),
                            DiffLogScope::Directory(path) => GitGraphScope::Directory(path),
                        };
                        this.refresh(window, cx);
                    });
                });
            let viewer = cx.new(|cx| {
                DiffViewer::new(path, old, new, Some(log_scope), Some(callback), window, cx)
            });
            self.diff_viewer = Some(viewer);
        }

        self.detail_tab = DetailTab::Changes;
        self.error = None;
        self.notice = Some("已加载内嵌并排 Diff".into());
        cx.notify();
    }

    fn open_selected_file_diff_window(&mut self, cx: &mut Context<Self>) {
        let Some(details) = self.details.as_ref() else {
            return;
        };
        let Some(relative_path) = self.selected_change_path.clone() else {
            return;
        };
        let old = details
            .commit
            .parents
            .first()
            .cloned()
            .map(DiffEndpoint::Revision)
            .unwrap_or_else(|| DiffEndpoint::Revision(EMPTY_TREE_HASH.to_string()));
        let new = DiffEndpoint::Revision(details.commit.hash.clone());
        let path = self.repository_root.join(relative_path);
        match crate::diff_viewer_window::open_diff_viewer_window(path, old, new, cx) {
            Ok(()) => {
                self.error = None;
                self.notice = Some("已打开独立 Diff 查看器".into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }
}

impl Render for GitLogWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let foreground = cx.theme().foreground;
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(foreground)
            .child(self.render_toolbar(cx))
            .child(div().h(px(1.)).w_full().bg(border))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_navigation(cx))
                    .child(div().w(px(1.)).h_full().bg(border))
                    .child(self.render_workspace(window, cx)),
            )
            .child(div().h(px(1.)).w_full().bg(border))
            .child(self.render_status_bar(cx))
    }
}

impl GitLogWindow {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let branch_label = self
            .branch
            .as_deref()
            .and_then(|full_name| {
                self.context
                    .as_ref()?
                    .log
                    .refs
                    .iter()
                    .find(|reference| reference.full_name == full_name)
                    .map(|reference| reference.name.clone())
            })
            .unwrap_or_else(|| "全部分支".to_string());
        let current_branch = self
            .context
            .as_ref()
            .and_then(|context| context.log.current_branch.clone())
            .unwrap_or_else(|| "分离 HEAD".to_string());
        let repository_root = self.repository_root.display().to_string();
        let scope_label = self.scope_label();
        let is_repository = self.scope.is_repository();

        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .bg(cx.theme().title_bar)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(30.))
                            .rounded_md()
                            .bg(cx.theme().primary.opacity(0.14))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .size_4()
                                    .text_color(cx.theme().primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Git Workspace"),
                                    )
                                    .child(self.branch_badge(&current_branch, cx)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(repository_root),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(cx.theme().muted.opacity(0.2))
                            .child(
                                Icon::new(if is_repository {
                                    IconName::GitBranch
                                } else {
                                    IconName::Folder
                                })
                                .size_3p5()
                                .text_color(cx.theme().primary),
                            )
                            .child(
                                div()
                                    .max_w(px(360.))
                                    .truncate()
                                    .text_xs()
                                    .child(scope_label),
                            )
                            .when(!is_repository, |this| {
                                this.child(
                                    Button::new("git-workspace-clear-scope")
                                        .ghost()
                                        .compact()
                                        .label("清除范围")
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.clear_scope(window, cx)
                                            },
                                        )),
                                )
                            }),
                    )
                    .child(
                        Button::new("git-workspace-refresh")
                            .ghost()
                            .compact()
                            .icon(IconName::RefreshCw)
                            .label("刷新")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.refresh(window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("git-workspace-search-mode")
                            .ghost()
                            .compact()
                            .label(format!("筛选 · {}", self.search_mode.label()))
                            .tooltip("切换文本、作者、路径或哈希筛选")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.search_mode = this.search_mode.next();
                                this.query_input.update(cx, |input, cx| {
                                    input.set_placeholder(
                                        this.search_mode.placeholder(),
                                        window,
                                        cx,
                                    )
                                });
                                this.focus_filter(window, cx);
                            })),
                    )
                    .child(
                        div().flex_1().min_w(px(240.)).child(
                            Input::new(&self.query_input)
                                .aria_label("筛选提交历史")
                                .prefix(Icon::new(IconName::Search).size_4())
                                .cleanable(true),
                        ),
                    )
                    .child(
                        Button::new("git-workspace-regex")
                            .ghost()
                            .compact()
                            .label("正则")
                            .selected(self.search_regex)
                            .tooltip("将文本筛选按扩展正则表达式处理")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.search_regex = !this.search_regex;
                                this.refresh(window, cx);
                            })),
                    )
                    .child(self.render_branch_menu(branch_label, cx))
                    .child(self.render_date_menu(cx)),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .h(px(28.))
            .w_full()
            .items_center()
            .justify_between()
            .px_3()
            .text_xs()
            .text_color(muted)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(self.notice.clone().unwrap_or_else(|| "就绪".into())),
            )
            .child(format!(
                "{} 个提交  ·  {}",
                self.commits.len(),
                self.scope_label()
            ))
            .into_any_element()
    }

    fn render_workspace(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                v_resizable("git-workspace-vertical")
                    .with_state(&self.detail_resize_state)
                    .child(resizable_panel().child(self.render_log_pane(cx)))
                    .child(
                        resizable_panel()
                            .size(px(DETAIL_HEIGHT))
                            .size_range(px(180.)..px(520.))
                            .fixed()
                            .child(self.render_detail_pane(cx)),
                    ),
            )
            .into_any_element()
    }

    fn branch_badge(&self, branch: &str, cx: &App) -> AnyElement {
        div()
            .px_1p5()
            .py_0p5()
            .rounded_full()
            .bg(cx.theme().primary.opacity(0.12))
            .text_xs()
            .text_color(cx.theme().primary)
            .child(branch.to_string())
            .into_any_element()
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let refs = self
            .context
            .as_ref()
            .map(|context| context.log.refs.clone())
            .unwrap_or_default();
        let local_branches = refs
            .iter()
            .filter(|reference| reference.kind == GitRefKind::LocalBranch)
            .cloned()
            .collect::<Vec<_>>();
        let remote_branches = refs
            .iter()
            .filter(|reference| reference.kind == GitRefKind::RemoteBranch)
            .cloned()
            .collect::<Vec<_>>();
        let tags = refs
            .iter()
            .filter(|reference| reference.kind == GitRefKind::Tag)
            .cloned()
            .collect::<Vec<_>>();
        let worktree_name = self
            .repository_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.repository_root.display().to_string());

        div()
            .id("git-workspace-navigation")
            .w(px(NAVIGATION_WIDTH))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(border)
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .child(nav_section_label("Worktrees", cx))
                    .child(
                        h_flex()
                            .h(px(28.))
                            .w_full()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .rounded_md()
                            .bg(cx.theme().primary.opacity(0.08))
                            .child(
                                Icon::new(IconName::Folder)
                                    .size_3p5()
                                    .text_color(cx.theme().primary),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(worktree_name),
                            ),
                    )
                    .child(nav_section_label("Local Changes", cx))
                    .child(
                        Button::new("git-workspace-local-changes")
                            .ghost()
                            .compact()
                            .icon(IconName::GitCommitHorizontal)
                            .label("Local Changes")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                match crate::git_client_window::open_git_client_window(
                                    this.repository_root.clone(),
                                    window,
                                    cx,
                                ) {
                                    Ok(()) => {
                                        this.error = None;
                                        this.notice = Some("Git 客户端已打开".into());
                                    }
                                    Err(error) => {
                                        this.notice = None;
                                        this.error = Some(error.into());
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(nav_section_label("All Commits", cx))
                    .child(
                        Button::new("git-workspace-all-commits")
                            .ghost()
                            .compact()
                            .icon(IconName::GitBranch)
                            .label("All Commits")
                            .selected(self.scope.is_repository() && self.branch.is_none())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.branch = None;
                                this.clear_scope(window, cx);
                            })),
                    )
                    .child(nav_section_label("Branches", cx))
                    .children(
                        local_branches.into_iter().map(|reference| {
                            self.render_nav_ref(reference, IconName::GitBranch, cx)
                        }),
                    )
                    .children(
                        remote_branches
                            .into_iter()
                            .map(|reference| self.render_nav_ref(reference, IconName::Cloud, cx)),
                    )
                    .child(nav_section_label("Tags", cx))
                    .children(
                        tags.into_iter()
                            .map(|reference| self.render_nav_ref(reference, IconName::Tag, cx)),
                    )
                    .child(nav_section_label("Filter", cx))
                    .child(
                        Button::new("git-workspace-focus-filter")
                            .ghost()
                            .compact()
                            .icon(IconName::Search)
                            .label("聚焦筛选")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.focus_filter(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .pt_2()
                            .text_xs()
                            .text_color(muted)
                            .child("Graph / Log / Diff 共用同一仓库工作台"),
                    ),
            )
            .into_any_element()
    }

    fn render_nav_ref(
        &self,
        reference: GitRef,
        icon: IconName,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.branch.as_deref() == Some(reference.full_name.as_str());
        let full_name = reference.full_name.clone();
        Button::new(SharedString::from(format!(
            "git-workspace-ref-{}",
            reference.full_name
        )))
        .ghost()
        .compact()
        .icon(icon)
        .label(reference.name)
        .selected(selected)
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.set_branch_filter(Some(full_name.clone()), window, cx)
        }))
        .into_any_element()
    }

    fn render_branch_menu(&self, label: String, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.branch.clone();
        let refs = self
            .context
            .as_ref()
            .map(|context| {
                context
                    .log
                    .refs
                    .iter()
                    .filter(|reference| {
                        matches!(
                            reference.kind,
                            GitRefKind::LocalBranch | GitRefKind::RemoteBranch
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let entity = cx.entity().downgrade();
        Button::new("git-workspace-branch")
            .ghost()
            .compact()
            .icon(IconName::GitBranch)
            .label(format!("分支 · {label}"))
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(260.)).max_w(px(380.)).label("分支");
                let all_entity = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new("全部分支")
                        .checked(selected.is_none())
                        .on_click(move |_, window, cx| {
                            let _ = all_entity.update(cx, |this, cx| {
                                this.set_branch_filter(None, window, cx);
                            });
                        }),
                );
                menu = menu.separator();
                for reference in &refs {
                    let full_name = reference.full_name.clone();
                    let checked = selected.as_deref() == Some(full_name.as_str());
                    let item_entity = entity.clone();
                    let icon = if reference.kind == GitRefKind::RemoteBranch {
                        IconName::Cloud
                    } else {
                        IconName::GitBranch
                    };
                    menu = menu.item(
                        PopupMenuItem::new(reference.name.clone())
                            .icon(icon)
                            .checked(checked)
                            .on_click(move |_, window, cx| {
                                let full_name = full_name.clone();
                                let _ = item_entity.update(cx, |this, cx| {
                                    this.set_branch_filter(Some(full_name), window, cx);
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_date_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.date_range;
        let entity = cx.entity().downgrade();
        Button::new("git-workspace-date")
            .ghost()
            .compact()
            .icon(IconName::Calendar)
            .label(selected.label())
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(200.)).label("提交时间");
                for value in [
                    DateRange::Any,
                    DateRange::Day,
                    DateRange::Week,
                    DateRange::Month,
                ] {
                    let item_entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(value.label())
                            .checked(selected == value)
                            .on_click(move |_, window, cx| {
                                let _ = item_entity.update(cx, |this, cx| {
                                    this.date_range = value;
                                    this.refresh(window, cx);
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_log_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let list_id = SharedString::from("git-workspace-commit-list");
        let count = self.commits.len();
        let selected = self.selected_hash.clone();
        let scroll = self.row_scroll.clone();

        v_flex()
            .flex_1()
            .min_w(px(520.))
            .min_h_0()
            .child(
                h_flex()
                    .h(px(32.))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(
                        div()
                            .w(px(GRAPH_COLUMN_WIDTH))
                            .flex_shrink_0()
                            .child("Graph"),
                    )
                    .child(div().w(px(REFS_COLUMN_WIDTH)).flex_shrink_0().child("Refs"))
                    .child(div().flex_1().min_w_0().child("Subject"))
                    .child(
                        div()
                            .w(px(AUTHOR_COLUMN_WIDTH))
                            .flex_shrink_0()
                            .child("Author"),
                    )
                    .child(div().w(px(DATE_COLUMN_WIDTH)).flex_shrink_0().child("Date"))
                    .child(div().w(px(HASH_COLUMN_WIDTH)).flex_shrink_0().child("Hash")),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .relative()
                    .when(self.loading, |this| {
                        this.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .text_xs()
                                .text_color(muted)
                                .child(Icon::new(IconName::Loader).size_6())
                                .child("正在读取提交历史…"),
                        )
                    })
                    .when_some(self.error.clone(), |this, error| {
                        this.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .px_8()
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .size_6()
                                        .text_color(cx.theme().danger),
                                )
                                .child(
                                    div()
                                        .max_w(px(520.))
                                        .text_sm()
                                        .text_color(cx.theme().danger)
                                        .text_center()
                                        .child(error),
                                ),
                        )
                    })
                    .when(
                        !self.loading && self.error.is_none() && count == 0,
                        |this| {
                            this.child(
                                v_flex()
                                    .size_full()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(Icon::new(IconName::GitBranch).size_8())
                                    .child(if self.scope.is_repository() {
                                        "没有匹配的提交"
                                    } else {
                                        "该目录没有可显示的提交历史"
                                    })
                                    .when(!self.scope.is_repository(), |this| {
                                        this.child(
                                            Button::new("git-workspace-empty-clear-scope")
                                                .ghost()
                                                .compact()
                                                .label("清除目录范围")
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, window, cx| {
                                                        this.clear_scope(window, cx)
                                                    },
                                                )),
                                        )
                                    }),
                            )
                        },
                    )
                    .when(!self.loading && self.error.is_none() && count > 0, |this| {
                        this.child(
                            uniform_list(
                                list_id,
                                count,
                                cx.processor(
                                    move |this, range: std::ops::Range<usize>, window, cx| {
                                        range
                                            .map(|index| {
                                                let Some(commit) = this.commits.get(index).cloned()
                                                else {
                                                    return div().into_any_element();
                                                };
                                                let is_selected = selected.as_deref()
                                                    == Some(commit.hash.as_str());
                                                this.render_row(
                                                    commit,
                                                    index,
                                                    is_selected,
                                                    window,
                                                    cx,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .track_scroll(&scroll),
                        )
                    }),
            )
            .child(
                h_flex()
                    .h(px(36.))
                    .w_full()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(if self.loading_more {
                        "正在加载下一页…"
                    } else if self.has_more {
                        "滚动列表按需加载，也可手动加载下一页"
                    } else if count > 0 {
                        "已到达历史末尾"
                    } else {
                        " "
                    })
                    .child(
                        Button::new("git-workspace-load-more")
                            .ghost()
                            .compact()
                            .label("加载更多")
                            .disabled(!self.has_more || self.loading_more)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.load_more(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        commit: GitLogCommit,
        index: usize,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = cx.theme().primary;
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().muted.opacity(0.12);
        let hash = commit.hash.clone();
        let tree_entity = cx.entity().downgrade();
        let tree_hash = hash.clone();
        let copy_hash = hash.clone();
        let changes_hash = hash.clone();
        let scope_file = match &self.scope {
            GitGraphScope::File(path) => Some(path.to_string_lossy().replace('\\', "/")),
            _ => None,
        };
        h_flex()
            .id(("git-workspace-row", index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.55))
            .when(selected, |this| this.bg(accent.opacity(0.13)))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.select_commit(hash.clone(), event.click_count() >= 2, window, cx)
            }))
            .context_menu(move |mut menu, _, _| {
                menu = menu.item(
                    PopupMenuItem::new("查看变更")
                        .icon(IconName::GitCommitHorizontal)
                        .on_click({
                            let changes_entity = tree_entity.clone();
                            let changes_hash = changes_hash.clone();
                            move |_, window, cx| {
                                let _ = changes_entity.update(cx, |this, cx| {
                                    this.select_commit(changes_hash.clone(), true, window, cx);
                                });
                            }
                        }),
                );
                if let Some(scope_file) = scope_file.clone() {
                    let history_entity = tree_entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new("查看文件历史")
                            .icon(IconName::GitBranch)
                            .on_click(move |_, window, cx| {
                                let path = scope_file.clone();
                                let _ = history_entity.update(cx, |this, cx| {
                                    this.show_file_history(path.clone(), window, cx);
                                });
                            }),
                    );
                }
                menu = menu.item(
                    PopupMenuItem::new("复制完整哈希")
                        .icon(IconName::Copy)
                        .on_click({
                            let copy_hash = copy_hash.clone();
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_hash.clone()));
                            }
                        }),
                );
                menu = menu.item(
                    PopupMenuItem::new("在提交时浏览文件")
                        .icon(IconName::FolderOpen)
                        .on_click({
                            let tree_entity = tree_entity.clone();
                            let tree_hash = tree_hash.clone();
                            move |_, window, cx| {
                                let _ = tree_entity.update(cx, |this, cx| {
                                    this.load_tree(tree_hash.clone(), window, cx)
                                });
                            }
                        }),
                );
                menu
            })
            .child(
                div()
                    .w(px(GRAPH_COLUMN_WIDTH))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(self.render_graph(&commit.graph, cx)),
            )
            .child(
                h_flex()
                    .w(px(REFS_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .min_w_0()
                    .gap_1()
                    .overflow_hidden()
                    .children(
                        commit
                            .refs
                            .iter()
                            .take(3)
                            .map(|reference| self.render_ref_badge(reference, cx)),
                    )
                    .when(commit.refs.len() > 3, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("+{}", commit.refs.len() - 3)),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(commit.subject.clone()),
            )
            .child(
                div()
                    .w(px(AUTHOR_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(commit.author_name.clone()),
            )
            .child(
                div()
                    .w(px(DATE_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(format_git_time(commit.committed_at)),
            )
            .child(
                div()
                    .w(px(HASH_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(muted)
                    .child(commit.short_hash),
            )
            .into_any_element()
    }

    fn render_ref_badge(&self, reference: &GitRef, cx: &App) -> AnyElement {
        let (background, color, icon) = match reference.kind {
            GitRefKind::LocalBranch if reference.is_head => (
                cx.theme().primary.opacity(0.18),
                cx.theme().primary,
                IconName::GitBranch,
            ),
            GitRefKind::LocalBranch => (
                rgb(0x55b978).opacity(0.15).into(),
                rgb(0x55b978).into(),
                IconName::GitBranch,
            ),
            GitRefKind::RemoteBranch => (
                rgb(0xc678dd).opacity(0.16).into(),
                rgb(0xc678dd).into(),
                IconName::Cloud,
            ),
            GitRefKind::Tag => (
                rgb(0xe5c07b).opacity(0.18).into(),
                rgb(0xe5c07b).into(),
                IconName::Tag,
            ),
            GitRefKind::Other => (
                cx.theme().muted.opacity(0.2),
                cx.theme().muted_foreground,
                IconName::GitBranch,
            ),
        };
        h_flex()
            .max_w(px(110.))
            .gap_1()
            .px_1p5()
            .py_0p5()
            .rounded_md()
            .bg(background)
            .text_xs()
            .text_color(color)
            .child(Icon::new(icon).size_3())
            .child(div().min_w_0().truncate().child(reference.name.clone()))
            .into_any_element()
    }

    fn render_graph(&self, row: &GitGraphRow, cx: &App) -> AnyElement {
        let lane_width = graph_lane_width(row);
        let mut graph = div()
            .relative()
            .w(px(GRAPH_COLUMN_WIDTH))
            .h_full()
            .flex_shrink_0()
            .overflow_hidden();
        for edge in &row.edges {
            graph = graph.child(self.render_graph_edge(edge, row));
        }
        let dot_left = GRAPH_LEFT_PADDING
            + row.lane as f32 * lane_width
            + (lane_width - GRAPH_NODE_SIZE) / 2.0;
        graph = graph.child(
            div()
                .absolute()
                .left(px(dot_left))
                .top(px((ROW_HEIGHT - GRAPH_NODE_SIZE) / 2.0))
                .size(px(GRAPH_NODE_SIZE))
                .rounded_full()
                .bg(graph_color(row.color_index))
                .border_2()
                .border_color(cx.theme().background),
        );
        graph.into_any_element()
    }

    fn render_graph_edge(&self, edge: &GitGraphEdge, row: &GitGraphRow) -> AnyElement {
        let lane_color = graph_color(edge.color_index);
        let lane_width = graph_lane_width(row);
        let from_x = GRAPH_LEFT_PADDING + edge.from_lane as f32 * lane_width + lane_width / 2.0;
        let to_x = GRAPH_LEFT_PADDING + edge.to_lane as f32 * lane_width + lane_width / 2.0;
        let edge_offset = GRAPH_EDGE_WIDTH / 2.0;
        let center_y = ROW_HEIGHT / 2.0;
        if edge.from_lane == edge.to_lane {
            return div()
                .absolute()
                .left(px(from_x - edge_offset))
                .top(px(0.))
                .w(px(GRAPH_EDGE_WIDTH))
                .h(px(ROW_HEIGHT))
                .bg(lane_color)
                .into_any_element();
        }

        // Keep this elbow geometry in sync with KiteGitGraphCell on macOS.
        div()
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .w_full()
            .h_full()
            .child(
                div()
                    .absolute()
                    .left(px(from_x - edge_offset))
                    .top(px(0.))
                    .w(px(GRAPH_EDGE_WIDTH))
                    .h(px(center_y))
                    .bg(lane_color),
            )
            .child(
                div()
                    .absolute()
                    .left(px(from_x.min(to_x)))
                    .top(px(center_y - edge_offset))
                    .w(px((from_x - to_x).abs().max(GRAPH_EDGE_WIDTH)))
                    .h(px(GRAPH_EDGE_WIDTH))
                    .bg(lane_color),
            )
            .child(
                div()
                    .absolute()
                    .left(px(to_x - edge_offset))
                    .top(px(center_y))
                    .w(px(GRAPH_EDGE_WIDTH))
                    .h(px(center_y))
                    .bg(lane_color),
            )
            .into_any_element()
    }
}

impl GitLogWindow {
    fn render_detail_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let selected = self.selected_commit().cloned();
        let selected_hash = self.selected_hash.clone();
        let title = selected
            .as_ref()
            .map(|commit| commit.short_hash.clone())
            .unwrap_or_else(|| "提交详情".to_string());

        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .bg(cx.theme().background.opacity(0.88))
            .child(
                h_flex()
                    .h(px(42.))
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when_some(selected_hash.clone(), |this, hash| {
                        let hash_for_copy = hash.clone();
                        this.child(
                            Button::new("git-workspace-copy-hash")
                                .ghost()
                                .compact()
                                .icon(IconName::Copy)
                                .tooltip("复制完整提交哈希")
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        hash_for_copy.clone(),
                                    ));
                                })),
                        )
                    })
                    .when_some(selected_hash, |this, hash| {
                        this.child(
                            Button::new("git-workspace-browse-tree")
                                .ghost()
                                .compact()
                                .icon(IconName::FolderOpen)
                                .tooltip("浏览这个提交的文件树")
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.load_tree(hash.clone(), window, cx)
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .h(px(36.))
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(border)
                    .child(self.detail_tab_button(DetailTab::Commit, cx))
                    .child(self.detail_tab_button(DetailTab::Changes, cx))
                    .child(self.detail_tab_button(DetailTab::FileTree, cx)),
            )
            .child(
                div()
                    .id("git-workspace-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .child(match self.detail_tab {
                        DetailTab::Commit => self.render_commit_details(cx),
                        DetailTab::Changes => self.render_changed_files(cx),
                        DetailTab::FileTree => self.render_commit_tree(cx),
                    }),
            )
            .child(
                h_flex()
                    .h(px(28.))
                    .w_full()
                    .items_center()
                    .px_3()
                    .text_xs()
                    .text_color(muted)
                    .child(if self.details_loading {
                        "正在读取提交详情…"
                    } else if self.tree_loading {
                        "正在读取提交文件树…"
                    } else {
                        "选择提交查看详情；双击提交打开 Changes"
                    }),
            )
            .into_any_element()
    }

    fn detail_tab_button(&self, tab: DetailTab, cx: &mut Context<Self>) -> AnyElement {
        Button::new(SharedString::from(format!(
            "git-workspace-detail-tab-{}",
            tab.label()
        )))
        .ghost()
        .compact()
        .label(tab.label())
        .selected(self.detail_tab == tab)
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.detail_tab = tab;
            if tab == DetailTab::Commit
                && this.details.is_none()
                && let Some(hash) = this.selected_hash.clone()
            {
                this.load_details(hash, window, cx);
            }
            if tab == DetailTab::FileTree
                && this.tree_entries.is_empty()
                && let Some(hash) = this.selected_hash.clone()
            {
                this.load_tree(hash, window, cx);
            }
            cx.notify();
        }))
        .into_any_element()
    }

    fn render_commit_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(details) = self.details.as_ref() else {
            return empty_detail_state(
                if self.details_loading {
                    "正在读取提交详情…"
                } else {
                    "选择一个提交"
                },
                cx,
            );
        };
        let commit = &details.commit;
        let message = if details.body.trim().is_empty() {
            commit.subject.clone()
        } else {
            details.body.clone()
        };
        let mut content = v_flex().w_full().gap_3().p_4();
        content = content.child(
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(commit.subject.clone()),
                        )
                        .children(
                            commit
                                .refs
                                .iter()
                                .map(|reference| self.render_ref_badge(reference, cx)),
                        ),
                )
                .child(
                    div()
                        .rounded_md()
                        .bg(cx.theme().muted.opacity(0.22))
                        .p_3()
                        .text_sm()
                        .line_height(relative(1.5))
                        .child(message),
                ),
        );
        content = content
            .child(detail_rows(
                vec![
                    ("提交", commit.hash.clone()),
                    (
                        "作者",
                        format!("{} <{}>", commit.author_name, commit.author_email),
                    ),
                    ("提交时间", format_git_time(commit.committed_at)),
                    (
                        "提交者",
                        format!("{} <{}>", commit.committer_name, commit.committer_email),
                    ),
                    (
                        "父提交",
                        if commit.parents.is_empty() {
                            "根提交".to_string()
                        } else {
                            commit
                                .parents
                                .iter()
                                .map(|parent| parent.chars().take(8).collect::<String>())
                                .collect::<Vec<_>>()
                                .join("  ")
                        },
                    ),
                ],
                cx,
            ))
            .child(detail_rows(
                vec![
                    ("签名", details.signature.status_label().to_string()),
                    (
                        "签名者",
                        details
                            .signature
                            .signer
                            .clone()
                            .unwrap_or_else(|| "—".to_string()),
                    ),
                    (
                        "拣选状态",
                        match details.cherry_picked {
                            Some(true) => "上游已有等价补丁",
                            Some(false) => "上游尚未包含",
                            None => "无法确定",
                        }
                        .to_string(),
                    ),
                ],
                cx,
            ));
        if !details.branches_containing.is_empty() {
            content = content.child(
                v_flex()
                    .gap_1()
                    .child(section_label("包含此提交的分支", cx))
                    .child(h_flex().flex_wrap().gap_1().children(
                        details.branches_containing.iter().map(|branch| {
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_md()
                                .bg(cx.theme().primary.opacity(0.1))
                                .text_xs()
                                .text_color(cx.theme().primary)
                                .child(branch.clone())
                                .into_any_element()
                        }),
                    )),
            );
        }
        content = content.child(
            h_flex()
                .gap_2()
                .pt_1()
                .child(
                    Button::new("git-workspace-files-tab-from-detail")
                        .ghost()
                        .compact()
                        .label(format!("{} 个变更文件", details.changed_files.len()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.detail_tab = DetailTab::Changes;
                            cx.notify();
                        })),
                )
                .when(details.signature.status_code == "B", |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child("签名无效"),
                    )
                })
                .when(details.signature.status_code == "E", |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child("缺少签名公钥"),
                    )
                }),
        );
        content.into_any_element()
    }

    fn render_changed_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(details) = self.details.as_ref() else {
            return empty_detail_state("选择一个提交查看变更文件", cx);
        };
        if details.changed_files.is_empty() {
            return empty_detail_state("此提交没有变更文件", cx);
        }

        let selected_path = self.selected_change_path.clone();
        let entity = cx.entity().downgrade();
        let mut rows = v_flex().w_full().p_2();
        for file in &details.changed_files {
            let path = file.path.clone();
            let label = file
                .old_path
                .as_ref()
                .map(|old| format!("{old} → {}", file.path))
                .unwrap_or_else(|| file.path.clone());
            let status = file.status.chars().next().unwrap_or('M');
            let color: Hsla = rgb(change_status_color(status)).into();
            let selected = selected_path.as_deref() == Some(file.path.as_str());
            let history_path = file.path.clone();
            let embedded_path = file.path.clone();
            let window_path = file.path.clone();
            let diff_entity = entity.clone();
            let history_entity = entity.clone();
            rows = rows.child(
                h_flex()
                    .id(SharedString::from(format!("git-workspace-change-{path}")))
                    .h(px(32.))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .rounded_md()
                    .when(selected, |this| this.bg(cx.theme().primary.opacity(0.12)))
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.15)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.selected_change_path = Some(path.clone());
                        this.diff_viewer = None;
                        if event.click_count() >= 2 {
                            this.show_embedded_diff(window, cx);
                        }
                        cx.notify();
                    }))
                    .context_menu(move |mut menu, _, _| {
                        menu = menu.item(
                            PopupMenuItem::new("显示内嵌并排 Diff")
                                .icon(IconName::FileDiff)
                                .on_click({
                                    let entity = diff_entity.clone();
                                    let embedded_path = embedded_path.clone();
                                    move |_, window, cx| {
                                        let _ = entity.update(cx, |this, cx| {
                                            this.selected_change_path = Some(embedded_path.clone());
                                            this.show_embedded_diff(window, cx);
                                        });
                                    }
                                }),
                        );
                        menu = menu.item(
                            PopupMenuItem::new("在独立窗口打开 Diff")
                                .icon(IconName::FileDiff)
                                .on_click({
                                    let entity = diff_entity.clone();
                                    let window_path = window_path.clone();
                                    move |_, _, cx| {
                                        let _ = entity.update(cx, |this, cx| {
                                            this.selected_change_path = Some(window_path.clone());
                                            this.open_selected_file_diff_window(cx);
                                        });
                                    }
                                }),
                        );
                        menu = menu.item(
                            PopupMenuItem::new("查看文件历史")
                                .icon(IconName::GitBranch)
                                .on_click({
                                    let entity = history_entity.clone();
                                    let history_path = history_path.clone();
                                    move |_, window, cx| {
                                        let _ = entity.update(cx, |this, cx| {
                                            this.show_file_history(
                                                history_path.clone(),
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }),
                        );
                        menu
                    })
                    .child(
                        div()
                            .w(px(20.))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(status.to_string()),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(label))
                    .child(
                        Icon::new(IconName::File)
                            .size_3p5()
                            .text_color(cx.theme().muted_foreground),
                    ),
            );
        }

        let detail = if let Some(viewer) = self.diff_viewer.clone() {
            v_flex()
                .flex_1()
                .h_full()
                .min_h_0()
                .child(viewer)
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .h_full()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("双击变更文件或点击“显示内嵌 Diff”"),
                )
                .child(
                    Button::new("git-workspace-open-file-diff")
                        .outline()
                        .compact()
                        .icon(IconName::FileDiff)
                        .label("显示内嵌 Diff")
                        .disabled(selected_path.is_none())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.show_embedded_diff(window, cx)
                        })),
                )
                .into_any_element()
        };

        h_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(
                v_flex()
                    .id("git-workspace-changed-tree")
                    .w(px(330.))
                    .h_full()
                    .min_h_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .overflow_y_scroll()
                    .child(rows),
            )
            .child(detail)
            .into_any_element()
    }

    fn render_commit_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.tree_loading {
            return empty_detail_state("正在读取提交文件树…", cx);
        }
        if self.tree_entries.is_empty() {
            return empty_detail_state("选择“File Tree”加载此提交的内容", cx);
        }
        let mut list = v_flex().w_full().p_2();
        for entry in &self.tree_entries {
            let path = entry.path.clone();
            let highlighted = self.scope_contains_path(&entry.path);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("git-workspace-tree-{path}")))
                    .h(px(28.))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .rounded_md()
                    .when(highlighted, |this| this.bg(cx.theme().primary.opacity(0.1)))
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.12)))
                    .child(
                        Icon::new(IconName::File)
                            .size_3p5()
                            .text_color(if highlighted {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .child(entry.path.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format_size(entry.size)),
                    ),
            );
        }
        list.into_any_element()
    }
}

fn nav_section_label(label: &'static str, cx: &App) -> AnyElement {
    div()
        .mt_2()
        .px_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn empty_detail_state(message: &'static str, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(IconName::GitBranch).size_8())
        .child(message)
        .into_any_element()
}

fn detail_rows(rows: Vec<(&'static str, String)>, cx: &App) -> AnyElement {
    let mut content = v_flex().w_full().gap_2();
    for (label, value) in rows {
        content = content.child(
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(66.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_family("monospace")
                        .child(value),
                ),
        );
    }
    content.into_any_element()
}

fn section_label(label: &'static str, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn graph_lane_width(row: &GitGraphRow) -> f32 {
    (80.0_f32 / row.lane_count.max(1) as f32).clamp(GRAPH_LANE_MIN_WIDTH, GRAPH_LANE_MAX_WIDTH)
}

fn graph_color(color_index: u32) -> Hsla {
    rgb(GRAPH_COLORS[color_index as usize % GRAPH_COLORS.len()]).into()
}

fn format_git_time(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".to_string())
}

fn format_size(size: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let value = size as f64;
    if value >= MIB {
        format!("{:.1} MB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KB", value / KIB)
    } else {
        format!("{size} B")
    }
}

fn change_status_color(status: char) -> u32 {
    match status {
        'A' => 0x55b978,
        'D' => 0xef6b73,
        'R' | 'C' => 0x5e9cff,
        _ => 0xe5a84b,
    }
}

fn scope_label(scope: &GitGraphScope) -> String {
    match scope {
        GitGraphScope::Repository => "Repository".to_string(),
        GitGraphScope::Directory(path) => format!("Directory: {}", path.display()),
        GitGraphScope::File(path) => format!("File: {}", path.display()),
    }
}

fn window_title_for(repository_root: &Path, scope: &GitGraphScope) -> String {
    let repository_name = repository_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository_root.display().to_string());
    match scope {
        GitGraphScope::Repository => format!("Git Graph — {repository_name}"),
        GitGraphScope::Directory(path) => {
            format!("Git Log — {} — {repository_name}", path.display())
        }
        GitGraphScope::File(path) => format!("Git Log — {} — {repository_name}", path.display()),
    }
}

fn scope_for_path(path: &Path, repository_root: &Path) -> GitGraphScope {
    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let relative = canonical
        .strip_prefix(repository_root)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if relative.as_os_str().is_empty() {
        GitGraphScope::Repository
    } else if canonical.is_dir() {
        GitGraphScope::Directory(relative)
    } else {
        GitGraphScope::File(relative)
    }
}

pub(crate) fn open_git_log_window(path: PathBuf, cx: &mut App) -> Result<(), String> {
    let repository = GitService::shared()
        .discover_repository(&path)
        .map_err(|error| error.to_string())?;
    let repository_root = repository.work_tree;
    let scope = scope_for_path(&path, &repository_root);
    let key = repository_key(&repository_root);
    let existing = {
        let windows = open_windows()
            .lock()
            .map_err(|_| "无法读取 Git Workspace 窗口状态".to_string())?;
        windows.get(&key).copied()
    };
    if let Some(handle) = existing {
        let scope_for_update = scope.clone();
        if handle
            .update(cx, |this, window, cx| {
                this.set_scope(scope_for_update, window, cx);
                window.activate_window();
            })
            .is_ok()
        {
            return Ok(());
        }
        forget_open_window(&key);
    }

    let title = window_title_for(&repository_root, &scope);
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
        window_min_size: Some(size(px(1180.), px(600.))),
        ..Default::default()
    };
    let root_for_view = repository_root.clone();
    let scope_for_view = scope.clone();
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitLogWindow::new(root_for_view, scope_for_view, window, cx))
    })
    .map_err(|error| format!("无法打开 Git Workspace 窗口：{error}"))?;
    let handle = handle
        .downcast::<GitLogWindow>()
        .ok_or_else(|| "Git Workspace 窗口类型不匹配".to_string())?;
    open_windows()
        .lock()
        .map_err(|_| "无法保存 Git Workspace 窗口状态".to_string())?
        .insert(key, handle);
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GitGraphScope, scope_for_path, scope_label, window_title_for};
    use std::path::{Path, PathBuf};

    #[test]
    fn scope_labels_distinguish_repository_directory_and_file() {
        assert_eq!(scope_label(&GitGraphScope::Repository), "Repository");
        assert_eq!(
            scope_label(&GitGraphScope::Directory(PathBuf::from("src/git"))),
            "Directory: src/git"
        );
        assert_eq!(
            scope_label(&GitGraphScope::File(PathBuf::from("src/git/log.rs"))),
            "File: src/git/log.rs"
        );
    }

    #[test]
    fn repository_scope_has_graph_title_and_scoped_scope_has_log_title() {
        let root = Path::new(r"D:\repo\kite");
        assert_eq!(
            window_title_for(root, &GitGraphScope::Repository),
            "Git Graph — kite"
        );
        assert_eq!(
            window_title_for(root, &GitGraphScope::Directory(PathBuf::from("src/git"))),
            "Git Log — src/git — kite"
        );
    }

    #[test]
    fn scope_for_root_is_repository() {
        let root = PathBuf::from(r"D:\repo\kite");
        assert_eq!(scope_for_path(&root, &root), GitGraphScope::Repository);
    }
}
