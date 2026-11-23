use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::log::{
    self, GitCommitDetails, GitGraphEdge, GitGraphRow, GitLogCommit, GitLogContext, GitLogFilter,
    GitLogPage, GitRef, GitRefKind, GitTreeEntry,
};
use std::path::PathBuf;

const PAGE_SIZE: u32 = 200;
const REFERENCE_TREE_WIDTH: f32 = 218.0;
const ROW_HEIGHT: f32 = 30.0;
const GRAPH_LEFT_PADDING: f32 = 4.0;
const GRAPH_LANE_MIN_WIDTH: f32 = 4.0;
const GRAPH_LANE_MAX_WIDTH: f32 = 13.0;
const GRAPH_EDGE_WIDTH: f32 = 2.0;
const GRAPH_NODE_SIZE: f32 = 8.0;
const GRAPH_COLORS: [u32; 10] = [
    0x5e9cff, 0x55b978, 0xf0a45c, 0xc678dd, 0x56b6c2, 0xe06c75, 0xe5c07b, 0x7f9cf5, 0x64d1a7,
    0xd884e0,
];

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
    Files,
    Tree,
}

struct GitLogWindow {
    path: PathBuf,
    context: Option<GitLogContext>,
    commits: Vec<GitLogCommit>,
    selected_hash: Option<String>,
    details: Option<GitCommitDetails>,
    tree_entries: Vec<GitTreeEntry>,
    detail_tab: DetailTab,
    search_mode: SearchMode,
    search_regex: bool,
    branch: Option<String>,
    date_range: DateRange,
    history_path: Option<String>,
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
    _subscription: Subscription,
}

impl GitLogWindow {
    fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            path,
            context: None,
            commits: Vec::new(),
            selected_hash: None,
            details: None,
            tree_entries: Vec::new(),
            detail_tab: DetailTab::Commit,
            search_mode: SearchMode::Text,
            search_regex: false,
            branch: None,
            date_range: DateRange::Any,
            history_path: None,
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
            _subscription: subscription,
        };
        this.refresh(window, cx);
        this.query_input
            .update(cx, |input, cx| input.focus(window, cx));
        this
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
        if let Some(path) = self.history_path.as_ref() {
            filter.path = Some(path.clone());
        }
        filter
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let path = self.path.clone();
        let filter = self.current_filter(cx);
        self.loading = true;
        self.loading_more = false;
        self.details_loading_hash = None;
        self.tree_loading_hash = None;
        self.commits.clear();
        self.selected_hash = None;
        self.details = None;
        self.tree_entries.clear();
        self.cursor = None;
        self.has_more = false;
        self.error = None;
        self.notice = Some("正在加载提交历史…".into());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let context = log::git_log_context(&path)?;
                    let page = log::git_log_page(&path, filter, None, PAGE_SIZE)?;
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
                        this.context = Some(context);
                        this.notice = Some(format!("已加载 {} 个提交", page.commits.len()).into());
                        this.apply_page(page);
                        if let Some(hash) = this.selected_hash.clone() {
                            this.select_commit(hash, window, cx);
                        }
                        if this.has_more {
                            this.load_more(window, cx);
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
        let path = self.path.clone();
        let filter = self.current_filter(cx);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    log::git_log_page(&path, filter, Some(&cursor), PAGE_SIZE)
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

    fn select_commit(&mut self, hash: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_hash.as_deref() != Some(hash.as_str()) {
            self.selected_hash = Some(hash.clone());
            self.details = None;
            self.tree_entries.clear();
            self.detail_tab = DetailTab::Commit;
        }
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
        let path = self.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let hash = hash.clone();
                    async move { log::git_commit_details(&path, &hash) }
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
                    Ok(details) => this.details = Some(details),
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
            self.tree_entries.clear();
        }
        self.tree_loading = true;
        self.tree_loading_hash = Some(hash.clone());
        self.detail_tab = DetailTab::Tree;
        let generation = self.generation;
        let path = self.path.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let hash = hash.clone();
                    async move { log::git_commit_tree(&path, &hash) }
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
                    Ok(entries) => this.tree_entries = entries,
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_file_history(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.history_path = Some(path);
        self.search_mode = SearchMode::Path;
        self.refresh(window, cx);
    }

    fn clear_history_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history_path = None;
        self.refresh(window, cx);
    }

    fn selected_commit(&self) -> Option<&GitLogCommit> {
        let hash = self.selected_hash.as_deref()?;
        self.commits.iter().find(|commit| commit.hash == hash)
    }
}

impl Render for GitLogWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
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
                    .child(self.render_reference_tree(cx))
                    .child(div().w(px(1.)).h_full().bg(border))
                    .child(self.render_log_pane(window, cx))
                    .child(div().w(px(1.)).h_full().bg(border))
                    .child(self.render_detail_pane(cx)),
            )
            .child(div().h(px(1.)).w_full().bg(border))
            .child(
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
                        "{} 个提交  ·  双栏提交图由 Rust Core 计算",
                        self.commits.len()
                    )),
            )
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
                    .refs
                    .iter()
                    .find(|reference| reference.full_name == full_name)
                    .map(|reference| reference.name.clone())
            })
            .unwrap_or_else(|| "全部分支".to_string());
        let repository_root = self
            .context
            .as_ref()
            .map(|context| context.repository_root.display().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        let current_branch = self
            .context
            .as_ref()
            .and_then(|context| context.current_branch.clone())
            .unwrap_or_else(|| "分离 HEAD".to_string());

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
                                            .child("提交历史"),
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
                        Button::new("git-log-refresh")
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
                        Button::new("git-log-search-mode")
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
                                this.query_input
                                    .update(cx, |input, cx| input.focus(window, cx));
                                cx.notify();
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
                        Button::new("git-log-regex")
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
            .when_some(self.history_path.clone(), |this, path| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(cx.theme().muted.opacity(0.28))
                        .child(Icon::new(IconName::File).size_3p5().text_color(muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .child(format!("文件历史 · {path}")),
                        )
                        .child(
                            Button::new("git-log-clear-history")
                                .ghost()
                                .compact()
                                .label("清除")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.clear_history_path(window, cx)
                                })),
                        ),
                )
            })
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

    fn render_reference_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let refs = self
            .context
            .as_ref()
            .map(|context| context.refs.clone())
            .unwrap_or_default();
        let mut rows = vec![self.render_reference_tree_row(
            "全部分支".to_string(),
            None,
            IconName::GitBranch,
            self.branch.is_none(),
            false,
            cx,
        )];
        for (title, kind, icon) in [
            ("本地分支", GitRefKind::LocalBranch, IconName::GitBranch),
            ("远程分支", GitRefKind::RemoteBranch, IconName::Cloud),
            ("标签", GitRefKind::Tag, IconName::Tag),
        ] {
            let group = refs
                .iter()
                .filter(|reference| reference.kind == kind)
                .collect::<Vec<_>>();
            rows.push(Self::render_reference_tree_section(title, group.len(), cx));
            for reference in group {
                let selected = self.branch.as_deref() == Some(reference.full_name.as_str());
                rows.push(self.render_reference_tree_row(
                    reference.name.clone(),
                    Some(reference.full_name.clone()),
                    icon,
                    selected,
                    reference.is_head,
                    cx,
                ));
            }
        }

        div()
            .id("git-log-reference-tree")
            .w(px(REFERENCE_TREE_WIDTH))
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
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .pb_1()
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .size_3p5()
                                    .text_color(cx.theme().primary),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("分支树"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(refs.len().to_string()),
                            ),
                    )
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_reference_tree_section(title: &'static str, count: usize, cx: &App) -> AnyElement {
        h_flex()
            .mt_2()
            .h(px(20.))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().child(title))
            .child(count.to_string())
            .into_any_element()
    }

    fn render_reference_tree_row(
        &self,
        label: String,
        full_name: Option<String>,
        icon: IconName,
        selected: bool,
        is_head: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = cx.theme().primary;
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().muted.opacity(0.12);
        let row_key = full_name.clone().unwrap_or_else(|| "all".to_string());
        h_flex()
            .id(SharedString::from(format!("git-log-ref-{row_key}")))
            .h(px(26.))
            .w_full()
            .min_w_0()
            .items_center()
            .gap_1p5()
            .pl_2()
            .pr_2()
            .rounded_md()
            .when(selected, |this| this.bg(accent.opacity(0.15)))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.set_branch_filter(full_name.clone(), window, cx);
            }))
            .child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(if selected { accent } else { muted }),
            )
            .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
            .when(is_head, |this| {
                this.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child("HEAD"),
                )
            })
            .into_any_element()
    }

    fn render_branch_menu(&self, label: String, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.branch.clone();
        let refs = self
            .context
            .as_ref()
            .map(|context| {
                context
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
        Button::new("git-log-branch")
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
        Button::new("git-log-date")
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
}

impl GitLogWindow {
    fn render_log_pane(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let list_id = SharedString::from("git-log-commit-list");
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
                    .child(div().w(px(88.)).flex_shrink_0().child("提交图"))
                    .child(div().w(px(210.)).flex_shrink_0().child("引用"))
                    .child(div().flex_1().min_w_0().child("提交"))
                    .child(div().w(px(120.)).flex_shrink_0().child("作者"))
                    .child(div().w(px(125.)).flex_shrink_0().child("日期"))
                    .child(div().w(px(74.)).flex_shrink_0().child("哈希")),
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
                                    .child("没有匹配的提交"),
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
                        Button::new("git-log-load-more")
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
        h_flex()
            .id(("git-log-row", index))
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
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_commit(hash.clone(), window, cx)
            }))
            .context_menu(move |mut menu, _, _| {
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
                    .w(px(88.))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(self.render_graph(&commit.graph, cx)),
            )
            .child(
                h_flex()
                    .w(px(210.))
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
                    .w(px(120.))
                    .flex_shrink_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(commit.author_name.clone()),
            )
            .child(
                div()
                    .w(px(125.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(format_git_time(commit.committed_at)),
            )
            .child(
                div()
                    .w(px(74.))
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
            .w(px(88.))
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
            .w(px(390.))
            .flex_shrink_0()
            .h_full()
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
                            Button::new("git-log-copy-hash")
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
                            Button::new("git-log-browse-tree")
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
                    .child(self.detail_tab_button("提交", DetailTab::Commit, cx))
                    .child(self.detail_tab_button("变更文件", DetailTab::Files, cx))
                    .child(self.detail_tab_button("文件树", DetailTab::Tree, cx)),
            )
            .child(
                div()
                    .id("git-log-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .child(match self.detail_tab {
                        DetailTab::Commit => self.render_commit_details(cx),
                        DetailTab::Files => self.render_changed_files(cx),
                        DetailTab::Tree => self.render_commit_tree(cx),
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
                        "选择左侧提交查看详情"
                    }),
            )
            .into_any_element()
    }

    fn detail_tab_button(
        &self,
        label: &'static str,
        tab: DetailTab,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Button::new(SharedString::from(format!("git-log-detail-tab-{label}")))
            .ghost()
            .compact()
            .label(label)
            .selected(self.detail_tab == tab)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.detail_tab = tab;
                if tab == DetailTab::Commit
                    && this.details.is_none()
                    && let Some(hash) = this.selected_hash.clone()
                {
                    this.load_details(hash, window, cx);
                }
                if tab == DetailTab::Tree
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
        let muted = cx.theme().muted_foreground;
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
                    Button::new("git-log-files-tab-from-detail")
                        .ghost()
                        .compact()
                        .label(format!("{} 个变更文件", details.changed_files.len()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.detail_tab = DetailTab::Files;
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
        let _ = muted;
        content.into_any_element()
    }

    fn render_changed_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(details) = self.details.as_ref() else {
            return empty_detail_state("选择一个提交查看变更文件", cx);
        };
        if details.changed_files.is_empty() {
            return empty_detail_state("此提交没有变更文件", cx);
        }
        let mut list = v_flex().w_full().p_2();
        for file in &details.changed_files {
            let path = file.path.clone();
            let label = file
                .old_path
                .as_ref()
                .map(|old| format!("{old} → {}", file.path))
                .unwrap_or_else(|| file.path.clone());
            let status = file.status.chars().next().unwrap_or('M');
            let color: Hsla = rgb(change_status_color(status)).into();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("git-log-file-{path}")))
                    .h(px(32.))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.15)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.show_file_history(path.clone(), window, cx)
                    }))
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
                        Icon::new(IconName::Search)
                            .size_3p5()
                            .text_color(cx.theme().muted_foreground),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_commit_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.tree_loading {
            return empty_detail_state("正在读取提交文件树…", cx);
        }
        if self.tree_entries.is_empty() {
            return empty_detail_state("选择“文件树”加载此提交的内容", cx);
        }
        let mut list = v_flex().w_full().p_2();
        for entry in &self.tree_entries {
            let path = entry.path.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("git-log-tree-{path}")))
                    .h(px(28.))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .rounded_md()
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.12)))
                    .child(
                        Icon::new(IconName::File)
                            .size_3p5()
                            .text_color(cx.theme().muted_foreground),
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

pub(crate) fn open_git_log_window(path: PathBuf, cx: &mut App) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Git".to_string());
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(format!("Kite 提交历史 — {name}").into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
        window_min_size: Some(size(px(1180.), px(600.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitLogWindow::new(path, window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开提交历史窗口：{error}"))
}
