use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::GitOperationState;
use kite_core::git_changes::{
    self, GitChangeGroup, GitDiffScope, GitFileChangesDiff, GitHunkLineKind, GitLineSelection,
    GitRepositoryChanges,
};

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
    show_ignored: bool,
    tree_mode: bool,
    loading: bool,
    loading_diff: bool,
    operating: bool,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    _filter_subscription: Subscription,
    _load_task: Task<()>,
    _diff_task: Task<()>,
    _operation_task: Task<()>,
}

impl GitClientWindow {
    pub(crate) fn new(
        repository_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let mut this = Self {
            repository_root,
            changes: None,
            rows: Vec::new(),
            selected: None,
            diff: None,
            checked_lines: HashSet::new(),
            filter_input,
            show_ignored: false,
            tree_mode: true,
            loading: true,
            loading_diff: false,
            operating: false,
            error: None,
            notice: None,
            _filter_subscription: subscription,
            _load_task: Task::ready(()),
            _diff_task: Task::ready(()),
            _operation_task: Task::ready(()),
        };
        this.start_load(cx);
        this
    }

    fn start_load(&mut self, cx: &mut Context<Self>) {
        let root = self.repository_root.clone();
        let include_ignored = self.show_ignored;
        self.loading = true;
        self.error = None;
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
                        this.repository_root = changes.repository_root.clone();
                        this.changes = Some(changes);
                        this.rebuild_rows(cx);
                        this.restore_or_select_initial(cx);
                    }
                    Err(error) => {
                        this.changes = None;
                        this.rows.clear();
                        this.diff = None;
                        this.error = Some(error.into());
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

pub(crate) fn open_git_client_window(repository_root: PathBuf, cx: &mut App) -> Result<(), String> {
    let title = repository_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository_root.display().to_string());
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(format!("Git · {title}").into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1180.), px(760.)), cx)),
        window_min_size: Some(size(px(900.), px(600.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitClientWindow::new(repository_root.clone(), window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Git 客户端窗口：{error}"))
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
        let operation = changes
            .and_then(|changes| changes.operation)
            .map(GitOperationState::label);
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
                        Button::new("git-client-merge")
                            .ghost()
                            .compact()
                            .label(
                                if changes.is_some_and(|changes| {
                                    changes.count(GitChangeGroup::Conflicted) > 0
                                }) {
                                    "解决冲突…"
                                } else {
                                    "合并…"
                                },
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Err(error) = crate::git_merge_window::open_git_merge_window(
                                    this.repository_root.clone(),
                                    cx,
                                ) {
                                    this.error = Some(error.into());
                                    cx.notify();
                                }
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

impl Render for GitClientWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_header(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_changes_panel(cx))
                    .child(self.render_diff(cx)),
            )
    }
}
