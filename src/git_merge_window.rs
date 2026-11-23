use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{
    self, GitConflictChoice, GitConflictChoiceSide, GitConflictEntry, GitConflictKind,
    GitConflictSide, GitConflictSides, GitConflictState, GitMergeSpec, GitMergeStrategy,
    GitThreeWayMerge,
};
use kite_core::git_branches::{self, GitBranch};

const CONFLICT_SIDEBAR_WIDTH: f32 = 238.0;

pub(crate) struct GitMergeWindow {
    path: PathBuf,
    branches: Vec<GitBranch>,
    target_input: Entity<InputState>,
    message_input: Entity<InputState>,
    strategy: GitMergeStrategy,
    state: Option<GitConflictState>,
    selected: Option<usize>,
    sides: Option<GitConflictSides>,
    merge_document: Option<GitThreeWayMerge>,
    choices: BTreeMap<u32, GitConflictChoiceSide>,
    current_hunk: usize,
    result_text: String,
    manual_editor: Option<Entity<EditorState>>,
    manual_editing: bool,
    busy: bool,
    error: Option<SharedString>,
    status: Option<SharedString>,
    _load_task: Task<()>,
    _selection_task: Task<()>,
    _operation_task: Task<()>,
}

impl GitMergeWindow {
    pub(crate) fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let target_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("分支、标签或提交")
                .clean_on_escape()
        });
        let message_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("合并提交说明（可选）")
                .clean_on_escape()
        });
        let mut this = Self {
            path,
            branches: Vec::new(),
            target_input,
            message_input,
            strategy: GitMergeStrategy::Merge,
            state: None,
            selected: None,
            sides: None,
            merge_document: None,
            choices: BTreeMap::new(),
            current_hunk: 0,
            result_text: String::new(),
            manual_editor: None,
            manual_editing: false,
            busy: true,
            error: None,
            status: None,
            _load_task: Task::ready(()),
            _selection_task: Task::ready(()),
            _operation_task: Task::ready(()),
        };
        this.reload(window, cx);
        this
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        let path = self.path.clone();
        let selected_hint = self.selected;
        self._load_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let branches = git_branches::git_branch_snapshot(&path).map(|snapshot| {
                        let mut branches = snapshot.local_branches;
                        branches.extend(snapshot.remote_branches);
                        branches
                    });
                    let conflicts = git::GitService::shared()
                        .conflicts(&path)
                        .map_err(git_error_message);
                    (branches, conflicts)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    (Ok(branches), Ok(state)) => {
                        this.branches = branches;
                        this.state = state;
                        if let Some(entries) = this.state.as_ref().map(|state| &state.entries)
                            && !entries.is_empty()
                        {
                            let index = selected_hint
                                .unwrap_or(0)
                                .min(entries.len().saturating_sub(1));
                            this.select_conflict(index, window, cx);
                        } else {
                            this.selected = None;
                            this.sides = None;
                            this.merge_document = None;
                            this.choices.clear();
                        }
                        cx.notify();
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        });
    }

    fn select_conflict(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self
            .state
            .as_ref()
            .and_then(|state| state.entries.get(index))
            .cloned()
        else {
            return;
        };
        self.selected = Some(index);
        self.sides = None;
        self.merge_document = None;
        self.choices.clear();
        self.current_hunk = 0;
        self.manual_editor = None;
        self.manual_editing = false;
        self.result_text.clear();
        self.error = None;
        self.busy = true;
        let path = self.path.clone();
        self._selection_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let service = git::GitService::shared();
                    let sides = service
                        .conflict_sides(&path, &entry)
                        .map_err(git_error_message)?;
                    let document = if !entry.binary && entry.kind != GitConflictKind::Rename {
                        Some(
                            service
                                .three_way_merge(&path, &entry)
                                .map_err(git_error_message)?,
                        )
                    } else {
                        None
                    };
                    Ok::<_, String>((sides, document))
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                match result {
                    Ok((sides, document)) => {
                        this.sides = Some(sides);
                        if let Some(document) = document {
                            this.result_text = document.merged_text.clone();
                            this.merge_document = Some(document);
                        }
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn start_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.target_input.read(cx).value().trim().to_string();
        if target.is_empty() {
            self.error = Some("请选择要合并的分支或提交".into());
            cx.notify();
            return;
        }
        let message = self.message_input.read(cx).value().trim().to_string();
        let request = GitMergeSpec {
            target,
            strategy: self.strategy,
            message: (!message.is_empty()).then_some(message),
        };
        let path = self.path.clone();
        self.busy = true;
        self.error = None;
        self.status = Some("正在合并…".into());
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git::GitService::shared()
                        .merge(&path, &request)
                        .map_err(git_error_message)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(outcome) => {
                        this.status = Some(outcome.message.into());
                        this.state = outcome.conflicts;
                        if let Some(entries) = this.state.as_ref().map(|state| &state.entries)
                            && !entries.is_empty()
                        {
                            this.select_conflict(0, window, cx);
                        } else {
                            this.selected = None;
                            this.sides = None;
                            this.merge_document = None;
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.status = None;
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        });
    }

    fn choose_whole_side(
        &mut self,
        side: GitConflictSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let path = self.path.clone();
        self.busy = true;
        self.error = None;
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git::GitService::shared()
                        .choose_conflict_side(&path, &entry, side)
                        .map_err(git_error_message)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.status = Some(match side {
                            GitConflictSide::Ours => "已采用自己的版本".into(),
                            GitConflictSide::Theirs => "已采用对方的版本".into(),
                            GitConflictSide::Both => "已保留双方重命名".into(),
                        });
                        this.reload(window, cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn open_merge_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let path = self.path.clone();
        self.busy = true;
        self.error = None;
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git::GitService::shared()
                        .open_merge_tool(&path, &entry)
                        .map_err(git_error_message)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(message) => {
                        this.status = Some(message.into());
                        this.reload(window, cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn set_choice(&mut self, side: GitConflictChoiceSide, cx: &mut Context<Self>) {
        let Some(document) = self.merge_document.as_ref() else {
            return;
        };
        let Some(conflict) = document.conflicts.get(self.current_hunk) else {
            return;
        };
        self.choices.insert(conflict.index, side);
        let choices = self
            .choices
            .iter()
            .map(|(index, side)| GitConflictChoice {
                index: *index,
                side: *side,
            })
            .collect::<Vec<_>>();
        self.result_text = git::apply_conflict_choices(&document.merged_text, &choices);
        cx.notify();
    }

    fn toggle_manual_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.manual_editing {
            if let Some(editor) = self.manual_editor.as_ref() {
                self.result_text = editor.read(cx).value().to_string();
            }
            self.manual_editor = None;
        } else {
            let initial = self.result_text.clone();
            self.manual_editor = Some(cx.new(|cx| {
                EditorState::new(window, cx)
                    .default_value(&initial)
                    .language("text")
            }));
        }
        self.manual_editing = !self.manual_editing;
        cx.notify();
    }

    fn resolve_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let Some(document) = self.merge_document.as_ref() else {
            return;
        };
        if !self.manual_editing && self.choices.len() != document.conflicts.len() {
            self.error = Some("请先为每个冲突块选择自己、对方或两边".into());
            cx.notify();
            return;
        }
        let content = if self.manual_editing {
            self.manual_editor
                .as_ref()
                .map(|editor| editor.read(cx).value().to_string())
                .unwrap_or_else(|| self.result_text.clone())
        } else {
            self.result_text.clone()
        };
        let relative_path = entry.ours_path.clone().unwrap_or(entry.path.clone());
        let path = self.path.clone();
        self.busy = true;
        self.error = None;
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git::GitService::shared()
                        .resolve_conflict(&path, &relative_path, Some(&content))
                        .map_err(git_error_message)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.status = Some("合并结果已写回并暂存".into());
                        this.reload(window, cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn continue_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.busy = true;
        self.error = None;
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git::GitService::shared()
                        .continue_operation(&path)
                        .map_err(git_error_message)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(message) => {
                        this.status = Some(message.into());
                        this.reload(window, cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn abort_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = "中止当前 Git 操作？";
        let detail = "未提交的合并状态会被放弃。";
        let answers = [PromptButton::ok("中止"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, message, Some(detail), &answers, cx);
        let path = self.path.clone();
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = true;
                this.error = None;
                cx.notify();
            });
            let result = cx
                .background_spawn(async move {
                    git::GitService::shared()
                        .abort_operation(&path)
                        .map_err(git_error_message)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(message) => {
                        this.status = Some(message.into());
                        this.reload(window, cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn selected_entry(&self) -> Option<&GitConflictEntry> {
        let index = self.selected?;
        self.state.as_ref()?.entries.get(index)
    }

    fn render_merge_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut strategies = h_flex().gap_1();
        for (strategy, label) in [
            (GitMergeStrategy::Merge, "普通"),
            (GitMergeStrategy::NoFastForward, "保留合并提交"),
            (GitMergeStrategy::FastForwardOnly, "仅快进"),
            (GitMergeStrategy::Squash, "压缩"),
        ] {
            strategies = strategies.child(
                Button::new(format!("merge-strategy-{label}"))
                    .ghost()
                    .compact()
                    .label(label)
                    .selected(self.strategy == strategy)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.strategy = strategy;
                        cx.notify();
                    })),
            );
        }
        let mut branch_buttons = h_flex().gap_1().overflow_x_scrollbar();
        let selected_target = self.target_input.read(cx).value().to_string();
        for branch in self.branches.iter().take(16) {
            let name = branch.name.clone();
            branch_buttons = branch_buttons.child(
                Button::new(format!("merge-branch-{}", branch.name))
                    .ghost()
                    .compact()
                    .label(branch.name.clone())
                    .selected(selected_target == branch.name)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.target_input
                            .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                    })),
            );
        }
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.18))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("合并目标"),
                            )
                            .child(Input::new(&self.target_input).w_full()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("合并提交说明（可选）"),
                            )
                            .child(Input::new(&self.message_input).w_full()),
                    )
                    .child(
                        Button::new("merge-start")
                            .primary()
                            .label("开始合并")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_merge(window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("策略"),
                    )
                    .child(strategies)
                    .child(div().w(px(1.)).h(px(18.)).bg(cx.theme().border))
                    .child(branch_buttons),
            )
            .into_any_element()
    }

    fn render_conflict_sidebar(
        &self,
        entries: &[GitConflictEntry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = v_flex().gap_1().p_2();
        for (index, entry) in entries.iter().enumerate() {
            let selected = self.selected == Some(index);
            list = list.child(
                Button::new(format!("merge-conflict-{index}"))
                    .ghost()
                    .selected(selected)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_conflict(index, window, cx)
                    }))
                    .child(
                        v_flex()
                            .w_full()
                            .items_start()
                            .gap_0p5()
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .child(entry.path.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(conflict_kind_label(entry)),
                            ),
                    ),
            );
        }
        v_flex()
            .w(px(CONFLICT_SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(38.))
                    .px_3()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().text_sm().font_medium().child("冲突文件"))
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(entries.len().to_string()),
                    ),
            )
            .child(
                div()
                    .id("git-conflict-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .into_any_element()
    }

    fn render_conflict_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.selected_entry() else {
            return centered("选择一个冲突文件", cx);
        };
        let mut toolbar = h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(entry.path.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                conflict_kind_label(entry),
                                if entry.binary {
                                    "二进制文件"
                                } else {
                                    "文本文件"
                                }
                            )),
                    ),
            );
        toolbar = toolbar
            .child(
                Button::new("merge-use-ours")
                    .ghost()
                    .compact()
                    .label(side_action_title("采用自己", entry.ours_present))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.choose_whole_side(GitConflictSide::Ours, window, cx)
                    })),
            )
            .child(
                Button::new("merge-use-theirs")
                    .ghost()
                    .compact()
                    .label(side_action_title("采用对方", entry.theirs_present))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.choose_whole_side(GitConflictSide::Theirs, window, cx)
                    })),
            );
        if entry.kind == GitConflictKind::Rename {
            toolbar = toolbar.child(
                Button::new("merge-use-both")
                    .ghost()
                    .compact()
                    .label("两边都保留")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.choose_whole_side(GitConflictSide::Both, window, cx)
                    })),
            );
        }
        toolbar =
            toolbar.child(
                Button::new("merge-external-tool")
                    .ghost()
                    .compact()
                    .label("外部合并工具")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_merge_tool(window, cx)
                    })),
            );

        let body = if entry.binary {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    Icon::new(IconName::File)
                        .size_8()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_base()
                        .font_medium()
                        .child("二进制文件无法逐行合并"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("选择完整版本，或使用配置的 merge.tool。"),
                )
                .into_any_element()
        } else if entry.kind == GitConflictKind::Rename {
            self.render_rename_body(entry, cx)
        } else if let Some(document) = self.merge_document.as_ref() {
            self.render_text_body(entry, document, cx)
        } else {
            centered("正在载入三方合并内容…", cx)
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    fn render_rename_body(&self, entry: &GitConflictEntry, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().w_full().p_4().gap_3();
        for (label, path) in [
            ("基线", entry.base_path.as_deref()),
            ("自己", entry.ours_path.as_deref()),
            ("对方", entry.theirs_path.as_deref()),
        ] {
            body = body.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.2))
                    .child(
                        div()
                            .w(px(42.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .child(path.unwrap_or("—").to_string()),
                    ),
            );
        }
        if let Some(sides) = self.sides.as_ref() {
            body = body.child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(render_text_pane(
                        "自己的内容",
                        sides.ours_text.as_deref(),
                        sides.ours_present,
                        cx,
                    ))
                    .child(render_text_pane(
                        "对方的内容",
                        sides.theirs_text.as_deref(),
                        sides.theirs_present,
                        cx,
                    )),
            );
        }
        body.into_any_element()
    }

    fn render_text_body(
        &self,
        entry: &GitConflictEntry,
        document: &GitThreeWayMerge,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut navigation = h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border);
        if !document.conflicts.is_empty() {
            let current = &document.conflicts[self.current_hunk.min(document.conflicts.len() - 1)];
            let selected = self.choices.get(&current.index).copied();
            navigation = navigation
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "冲突 {} / {}",
                            self.current_hunk + 1,
                            document.conflicts.len()
                        )),
                )
                .child(
                    Button::new("merge-conflict-prev")
                        .ghost()
                        .compact()
                        .label("上一个")
                        .disabled(self.current_hunk == 0)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.current_hunk = this.current_hunk.saturating_sub(1);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("merge-conflict-next")
                        .ghost()
                        .compact()
                        .label("下一个")
                        .disabled(self.current_hunk + 1 >= document.conflicts.len())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.current_hunk = (this.current_hunk + 1).min(
                                this.merge_document
                                    .as_ref()
                                    .map(|document| document.conflicts.len().saturating_sub(1))
                                    .unwrap_or(0),
                            );
                            cx.notify();
                        })),
                )
                .child(div().w(px(1.)).h(px(18.)).bg(cx.theme().border))
                .child(
                    Button::new("merge-conflict-ours")
                        .ghost()
                        .compact()
                        .label("采用自己")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_choice(GitConflictChoiceSide::Ours, cx)
                        })),
                )
                .child(
                    Button::new("merge-conflict-theirs")
                        .ghost()
                        .compact()
                        .label("采用对方")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_choice(GitConflictChoiceSide::Theirs, cx)
                        })),
                )
                .child(
                    Button::new("merge-conflict-both")
                        .ghost()
                        .compact()
                        .label("两边都要")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_choice(GitConflictChoiceSide::Both, cx)
                        })),
                );
            if let Some(selected) = selected {
                navigation = navigation.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().accent)
                        .child(choice_label(selected)),
                );
            }
        } else {
            navigation = navigation.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("没有冲突块"),
            );
        }
        navigation = navigation
            .child(div().ml_auto())
            .child(
                Button::new("merge-manual-toggle")
                    .ghost()
                    .compact()
                    .label(if self.manual_editing {
                        "返回分栏"
                    } else {
                        "手动编辑"
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_manual_edit(window, cx)
                    })),
            )
            .child(
                Button::new("merge-resolve-text")
                    .primary()
                    .compact()
                    .label("标记已解决")
                    .disabled(
                        self.busy
                            || (!self.manual_editing
                                && self.choices.len() != document.conflicts.len())
                            || document.conflicts.is_empty(),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.resolve_selected(window, cx)
                    })),
            );

        let mut content = h_flex().flex_1().min_h_0().w_full();
        if self.manual_editing {
            if let Some(editor) = self.manual_editor.as_ref() {
                content = content.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .p_2()
                        .bg(cx.theme().background)
                        .child(
                            Editor::new(editor)
                                .appearance(false)
                                .bordered(false)
                                .h_full()
                                .w_full()
                                .aria_label("Git 合并结果编辑器"),
                        ),
                );
            }
        } else {
            content = content
                .child(render_text_pane(
                    "自己的版本",
                    self.sides
                        .as_ref()
                        .and_then(|sides| sides.ours_text.as_deref()),
                    self.sides.as_ref().is_some_and(|sides| sides.ours_present),
                    cx,
                ))
                .child(render_text_pane(
                    "对方的版本",
                    self.sides
                        .as_ref()
                        .and_then(|sides| sides.theirs_text.as_deref()),
                    self.sides
                        .as_ref()
                        .is_some_and(|sides| sides.theirs_present),
                    cx,
                ))
                .child(self.render_merge_result(document, cx));
        }
        let _ = entry;
        v_flex()
            .flex_1()
            .min_h_0()
            .h_full()
            .child(navigation)
            .child(content)
            .into_any_element()
    }

    fn render_merge_result(
        &self,
        document: &GitThreeWayMerge,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current_conflict = document
            .conflicts
            .get(self.current_hunk)
            .map(|hunk| hunk.index);
        let mut lines = v_flex().w_full();
        for line in &document.lines {
            let highlighted =
                line.conflict_index.is_some() && line.conflict_index == current_conflict;
            let background = if highlighted {
                rgba(0xef6b731f)
            } else {
                match line.kind {
                    git::GitMergeLineKind::Ours => rgba(0x4c8bf51a),
                    git::GitMergeLineKind::Theirs => rgba(0xb56cf01a),
                    git::GitMergeLineKind::Base => rgba(0x80808010),
                    git::GitMergeLineKind::Marker => rgba(0xe5a84b16),
                    git::GitMergeLineKind::Context => cx.theme().background.into(),
                }
            };
            let color = match line.kind {
                git::GitMergeLineKind::Ours => rgb(0x4c8bf5).into(),
                git::GitMergeLineKind::Theirs => rgb(0xb56cf0).into(),
                git::GitMergeLineKind::Marker => cx.theme().warning,
                git::GitMergeLineKind::Base => cx.theme().muted_foreground,
                git::GitMergeLineKind::Context => cx.theme().foreground,
            };
            lines = lines.child(
                div()
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .bg(background)
                    .text_xs()
                    .text_color(color)
                    .whitespace_nowrap()
                    .child(if line.text.is_empty() {
                        " ".to_string()
                    } else {
                        line.text.clone()
                    }),
            );
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(34.))
                    .px_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().text_xs().font_medium().child("合并结果"))
                    .when(self.choices.len() == document.conflicts.len(), |bar| {
                        bar.child(
                            div()
                                .ml_auto()
                                .text_xs()
                                .text_color(cx.theme().accent)
                                .child("所有冲突块已选择"),
                        )
                    }),
            )
            .child(
                div()
                    .id("merge-result-lines")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(lines),
            )
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let can_continue = self
            .state
            .as_ref()
            .is_some_and(|state| state.entries.is_empty() && state.can_continue);
        let can_abort = self.state.as_ref().is_some_and(|state| state.can_abort);
        h_flex()
            .w_full()
            .h(px(46.))
            .px_3()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .when_some(self.error.as_ref(), |bar, error| {
                bar.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .truncate()
                        .child(error.clone()),
                )
            })
            .when(self.error.is_none(), |bar| {
                bar.when_some(self.status.as_ref(), |bar, status| {
                    bar.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(status.clone()),
                    )
                })
            })
            .child(div().ml_auto())
            .child(
                Button::new("merge-abort")
                    .ghost()
                    .label("中止操作")
                    .disabled(self.busy || !can_abort)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.abort_operation(window, cx)
                    })),
            )
            .child(
                Button::new("merge-continue")
                    .primary()
                    .label("继续操作")
                    .disabled(self.busy || !can_continue)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.continue_operation(window, cx)
                    })),
            )
            .into_any_element()
    }
}

impl Render for GitMergeWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = self.render_merge_form(cx);
        let body = if self.busy && self.state.is_none() {
            centered("正在读取 Git 状态…", cx)
        } else if let Some(state) = self.state.as_ref()
            && !state.entries.is_empty()
        {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(self.render_conflict_sidebar(&state.entries, cx))
                .child(self.render_conflict_detail(cx))
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    Icon::new(IconName::GitBranch)
                        .size_8()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().text_base().font_medium().child(
                    if self.state.as_ref().is_some_and(|state| state.can_continue) {
                        "所有冲突已解决"
                    } else {
                        "没有待解决的冲突"
                    },
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            if self.state.as_ref().is_some_and(|state| state.can_continue) {
                                "可以继续完成当前操作，或中止并回到操作前状态。"
                            } else {
                                "选择上方目标分支后开始合并。"
                            },
                        ),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h(px(52.))
                    .w_full()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size_5()
                            .text_color(cx.theme().warning),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_medium().child("Git 合并与冲突解决"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(self.path.display().to_string()),
                            ),
                    )
                    .when_some(
                        self.state.as_ref().and_then(|state| state.operation),
                        |bar, operation| {
                            bar.child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child(operation.label()),
                            )
                        },
                    ),
            )
            .child(form)
            .child(body)
            .child(self.render_footer(cx))
    }
}

pub(crate) fn open_git_merge_window(path: PathBuf, cx: &mut App) -> Result<(), String> {
    let title = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string());
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(format!("Git 合并 · {title}").into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1180.), px(760.)), cx)),
        window_min_size: Some(size(px(980.), px(640.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitMergeWindow::new(path, window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Git 合并窗口：{error}"))
}

fn git_error_message(error: git::GitError) -> String {
    error.message
}

fn render_text_pane(
    title: &'static str,
    text: Option<&str>,
    present: bool,
    cx: &App,
) -> AnyElement {
    let value = if present {
        text.unwrap_or_default().to_string()
    } else {
        "（此版本已删除）".to_string()
    };
    v_flex()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .border_l_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .h(px(34.))
                .px_2()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_xs()
                .font_medium()
                .child(title),
        )
        .child(
            div()
                .id(format!("merge-pane-{title}"))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .text_xs()
                .whitespace_nowrap()
                .text_color(if present {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .child(value),
        )
        .into_any_element()
}

fn centered(message: &'static str, cx: &App) -> AnyElement {
    div()
        .flex_1()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message)
        .into_any_element()
}

fn conflict_kind_label(entry: &GitConflictEntry) -> &'static str {
    match entry.kind {
        GitConflictKind::BothModified => "双方修改",
        GitConflictKind::ModifyDeleteOurs => "自己删除，对方修改",
        GitConflictKind::ModifyDeleteTheirs => "自己修改，对方删除",
        GitConflictKind::AddAdd => "双方新增同名文件",
        GitConflictKind::Rename => "重命名冲突",
        GitConflictKind::Other => "其他冲突",
    }
}

fn choice_label(side: GitConflictChoiceSide) -> &'static str {
    match side {
        GitConflictChoiceSide::Ours => "已选自己",
        GitConflictChoiceSide::Theirs => "已选对方",
        GitConflictChoiceSide::Both => "已选两边",
    }
}

fn side_action_title(title: &str, present: bool) -> String {
    if present {
        title.to_string()
    } else {
        format!("{title}（删除）")
    }
}
