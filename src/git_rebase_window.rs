use std::path::PathBuf;

use crate::actions::GitRebaseSetAction;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git_rebase::{
    self, GitRebaseAction, GitRebaseCommit, GitRebaseOptions, GitRebasePlan, GitRebaseSpec,
    GitRebaseState, GitRebaseStatus, GitRebaseTodo,
};

struct RebaseRow {
    commit: GitRebaseCommit,
    action: GitRebaseAction,
    message: Entity<TextareaState>,
}

pub(crate) struct GitRebaseWindow {
    repository_path: PathBuf,
    plan: Option<GitRebasePlan>,
    rows: Vec<RebaseRow>,
    upstream_input: Entity<InputState>,
    onto_input: Entity<InputState>,
    update_refs: bool,
    autosquash: bool,
    preserve_merges: bool,
    allow_rewrite_pushed: bool,
    state: Option<GitRebaseState>,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    loading: bool,
    busy: bool,
    _load_task: Task<()>,
    _operation_task: Task<()>,
}

impl GitRebaseWindow {
    pub(crate) fn new(
        repository_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let upstream_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("@ {upstream} 或 HEAD~N")
                .clean_on_escape()
        });
        let onto_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("选择 onto 分支或提交")
                .clean_on_escape()
        });
        let mut view = Self {
            repository_path,
            plan: None,
            rows: Vec::new(),
            upstream_input,
            onto_input,
            update_refs: false,
            autosquash: false,
            preserve_merges: false,
            allow_rewrite_pushed: false,
            state: None,
            error: None,
            notice: None,
            loading: true,
            busy: false,
            _load_task: Task::ready(()),
            _operation_task: Task::ready(()),
        };
        view.load_plan(window, cx);
        view
    }

    fn load_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.repository_path.clone();
        self.loading = true;
        self.error = None;
        self._load_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let plan = git_rebase::git_rebase_plan(&path);
                    let status = git_rebase::git_rebase_status(&path);
                    (plan, status)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                let (plan, status) = result;
                if let Ok(Some(state)) = status {
                    this.state = Some(state);
                }
                match plan {
                    Ok(plan) => {
                        this.upstream_input.update(cx, |input, cx| {
                            input.set_value(plan.upstream.clone(), window, cx)
                        });
                        this.onto_input.update(cx, |input, cx| {
                            input.set_value(plan.onto.clone(), window, cx)
                        });
                        this.rows = plan
                            .commits
                            .iter()
                            .map(|commit| RebaseRow {
                                commit: commit.clone(),
                                action: GitRebaseAction::Pick,
                                message: cx.new(|cx| {
                                    TextareaState::new(window, cx)
                                        .default_value(commit.message.clone())
                                }),
                            })
                            .collect();
                        this.plan = Some(plan);
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn open_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let upstream = self.upstream_input.read(cx).value().to_string();
        let onto = self.onto_input.read(cx).value().to_string();
        let todo = self
            .rows
            .iter()
            .map(|row| {
                let message = match row.action {
                    GitRebaseAction::Reword => Some(row.message.read(cx).value().to_string()),
                    GitRebaseAction::Squash => {
                        let value = row.message.read(cx).value().to_string();
                        (value != row.commit.message).then_some(value)
                    }
                    _ => None,
                };
                GitRebaseTodo {
                    commit: row.commit.hash.clone(),
                    action: row.action,
                    message,
                }
            })
            .collect();
        let spec = GitRebaseSpec {
            upstream,
            onto,
            options: GitRebaseOptions {
                update_refs: self.update_refs,
                autosquash: self.autosquash,
                preserve_merges: self.preserve_merges,
                allow_rewrite_pushed: self.allow_rewrite_pushed,
            },
            todo,
        };
        let path = self.repository_path.clone();
        self.busy = true;
        self.error = None;
        self.notice = Some("正在改写历史…".into());
        cx.notify();
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { git_rebase::git_rebase(&path, &spec) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(state) => {
                        this.notice = Some(state.message.clone().into());
                        this.state = Some(state);
                        this.load_plan(window, cx);
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

    fn fixup_worktree(&mut self, commit: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.repository_path.clone();
        let allow_rewrite_pushed = self.allow_rewrite_pushed;
        self.busy = true;
        self.error = None;
        self.notice = Some("正在把当前改动并入历史…".into());
        cx.notify();
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git_rebase::git_rebase_fixup_worktree(&path, &commit, allow_rewrite_pushed)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(state) => {
                        this.notice = Some(state.message.clone().into());
                        this.state = Some(state);
                        this.load_plan(window, cx);
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

    fn continue_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.repository_path.clone();
        self.run_control(path, window, cx, Control::Continue);
    }

    fn skip_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.repository_path.clone();
        self.run_control(path, window, cx, Control::Skip);
    }

    fn abort_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.repository_path.clone();
        self.run_control(path, window, cx, Control::Abort);
    }

    fn run_control(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
        control: Control,
    ) {
        self.busy = true;
        self.error = None;
        self.notice = Some(control.pending().into());
        cx.notify();
        self._operation_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match control {
                        Control::Continue => git_rebase::git_rebase_continue(&path),
                        Control::Skip => git_rebase::git_rebase_skip(&path),
                        Control::Abort => git_rebase::git_rebase_abort(&path),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(state) => {
                        this.notice = Some(state.message.clone().into());
                        this.state = Some(state);
                        this.load_plan(window, cx);
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

    fn action_from_code(code: u8) -> GitRebaseAction {
        match code {
            1 => GitRebaseAction::Reword,
            2 => GitRebaseAction::Edit,
            3 => GitRebaseAction::Squash,
            4 => GitRebaseAction::Fixup,
            5 => GitRebaseAction::Drop,
            _ => GitRebaseAction::Pick,
        }
    }

    fn action_code(action: GitRebaseAction) -> u8 {
        match action {
            GitRebaseAction::Pick => 0,
            GitRebaseAction::Reword => 1,
            GitRebaseAction::Edit => 2,
            GitRebaseAction::Squash => 3,
            GitRebaseAction::Fixup => 4,
            GitRebaseAction::Drop => 5,
        }
    }

    fn move_row(&mut self, index: usize, offset: isize, cx: &mut Context<Self>) {
        let target = index as isize + offset;
        if target < 0 || target as usize >= self.rows.len() {
            return;
        }
        self.rows.swap(index, target as usize);
        cx.notify();
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[index];
        let selected = row.action;
        let fixup_commit = row.commit.hash.clone();
        let action_button = Button::new(format!("rebase-action-{index}"))
            .ghost()
            .compact()
            .label(selected.display_label())
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                menu.label("提交动作")
                    .separator()
                    .item(action_item(index, selected, GitRebaseAction::Pick))
                    .item(action_item(index, selected, GitRebaseAction::Reword))
                    .item(action_item(index, selected, GitRebaseAction::Edit))
                    .item(action_item(index, selected, GitRebaseAction::Squash))
                    .item(action_item(index, selected, GitRebaseAction::Fixup))
                    .item(action_item(index, selected, GitRebaseAction::Drop))
            })
            .into_any_element();

        let up = Button::new(format!("rebase-up-{index}"))
            .ghost()
            .compact()
            .icon(IconName::ArrowUp)
            .disabled(index == 0)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_row(index, -1, cx)))
            .into_any_element();
        let down = Button::new(format!("rebase-down-{index}"))
            .ghost()
            .compact()
            .icon(IconName::ArrowDown)
            .disabled(index + 1 >= self.rows.len())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_row(index, 1, cx)))
            .into_any_element();

        let message = matches!(
            row.action,
            GitRebaseAction::Reword | GitRebaseAction::Squash
        )
        .then(|| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if row.action == GitRebaseAction::Squash {
                            "合并后的提交信息（保持原文则自动合并）"
                        } else {
                            "新的提交信息"
                        }),
                )
                .child(Textarea::new(&row.message).h(px(72.)))
                .into_any_element()
        });

        v_flex()
            .gap_2()
            .p_2()
            .rounded_lg()
            .bg(cx.theme().muted.opacity(0.12))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(action_button)
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(cx.theme().muted_foreground)
                            .child(row.commit.short_hash.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(row.commit.subject.clone()),
                    )
                    .when(row.commit.pushed, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child("已推送"),
                        )
                    })
                    .child(up)
                    .child(down)
                    .child(
                        Button::new(format!("rebase-fixup-current-{index}"))
                            .ghost()
                            .compact()
                            .label("并入当前改动")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.fixup_worktree(fixup_commit.clone(), window, cx)
                            })),
                    ),
            )
            .when_some(message, |this, message| this.child(message))
            .into_any_element()
    }

    fn render_warning(&self, cx: &App) -> Option<AnyElement> {
        let plan = self.plan.as_ref()?;
        let mut warnings = Vec::new();
        if let Some(operation) = plan.operation.as_deref()
            && operation != "Rebase"
        {
            warnings.push(format!("{operation} 正在进行，请先完成或中止该操作。"));
        }
        if plan.protected {
            warnings.push(format!(
                "{} 是受保护分支，Kite 不允许在此分支改写历史。",
                plan.branch.as_deref().unwrap_or("当前分支")
            ));
        }
        if !plan.pushed_commits.is_empty() {
            warnings.push(format!(
                "有 {} 个提交已推送到远程。改写会重写远端历史，需要确认。",
                plan.pushed_commits.len()
            ));
        }
        if plan.has_merges {
            warnings.push("历史中包含合并提交，只有保留合并提交时才能继续。".to_string());
        }
        if warnings.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .gap_1()
                .p_2()
                .rounded_lg()
                .bg(cx.theme().warning.opacity(0.12))
                .border_1()
                .border_color(cx.theme().warning.opacity(0.35))
                .children(warnings.into_iter().map(|warning| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(warning)
                }))
                .into_any_element(),
        )
    }

    fn render_status(&self, cx: &App) -> Option<AnyElement> {
        let state = self.state.as_ref()?;
        let color = match state.status {
            GitRebaseStatus::Completed => cx.theme().success,
            GitRebaseStatus::Aborted => cx.theme().muted_foreground,
            GitRebaseStatus::Stopped | GitRebaseStatus::InProgress => cx.theme().warning,
        };
        Some(
            h_flex()
                .gap_2()
                .items_center()
                .p_2()
                .rounded_lg()
                .bg(color.opacity(0.12))
                .border_1()
                .border_color(color.opacity(0.35))
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(color)
                        .child(format!("{} · {}", state.status.label(), state.message)),
                )
                .when(state.remaining > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("剩余 {} 个提交", state.remaining)),
                    )
                })
                .into_any_element(),
        )
    }
}

#[derive(Clone, Copy)]
enum Control {
    Continue,
    Skip,
    Abort,
}

impl Control {
    fn pending(self) -> &'static str {
        match self {
            Self::Continue => "正在继续 Rebase…",
            Self::Skip => "正在跳过当前提交…",
            Self::Abort => "正在中止 Rebase…",
        }
    }
}

fn action_item(index: usize, selected: GitRebaseAction, action: GitRebaseAction) -> PopupMenuItem {
    PopupMenuItem::new(action.display_label())
        .checked(selected == action)
        .action(Box::new(GitRebaseSetAction {
            index,
            action: GitRebaseWindow::action_code(action),
        }))
}

impl Render for GitRebaseWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let warning = self.render_warning(cx);
        let status = self.render_status(cx);
        let busy = self.busy;
        let has_state = self.state.is_some();
        let can_abort = self.state.as_ref().is_some_and(|state| {
            matches!(
                state.status,
                GitRebaseStatus::Stopped | GitRebaseStatus::InProgress
            )
        });
        let rows = if self.rows.is_empty() {
            div()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if self.loading {
                    "正在读取提交…"
                } else {
                    "当前范围没有可编辑的提交"
                })
                .into_any_element()
        } else {
            v_flex()
                .gap_2()
                .children((0..self.rows.len()).map(|index| self.render_row(index, cx)))
                .into_any_element()
        };

        v_flex()
            .id("git-rebase-window")
            .size_full()
            .gap_3()
            .p_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, action: &GitRebaseSetAction, _, cx| {
                if let Some(row) = this.rows.get_mut(action.index) {
                    row.action = Self::action_from_code(action.action);
                }
                cx.notify();
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::GitBranch).size_5())
                    .child(div().text_lg().font_semibold().child("交互式 Rebase"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.repository_path.to_string_lossy().to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("rebase-close")
                            .ghost()
                            .compact()
                            .label("关闭")
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
            .when_some(warning, |this, warning| this.child(warning))
            .when_some(status, |this, status| this.child(status))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("上游"),
                            )
                            .child(Input::new(&self.upstream_input)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("onto"),
                            )
                            .child(Input::new(&self.onto_input)),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(rebase_toggle(
                        "更新指向的引用",
                        Switch::new("rebase-update-refs")
                            .checked(self.update_refs)
                            .small()
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.update_refs = *checked;
                                cx.notify();
                            })),
                        cx,
                    ))
                    .child(rebase_toggle(
                        "自动 squash/fixup",
                        Switch::new("rebase-autosquash")
                            .checked(self.autosquash)
                            .small()
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.autosquash = *checked;
                                cx.notify();
                            })),
                        cx,
                    ))
                    .child(rebase_toggle(
                        "保留合并提交",
                        Switch::new("rebase-preserve-merges")
                            .checked(self.preserve_merges)
                            .small()
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.preserve_merges = *checked;
                                cx.notify();
                            })),
                        cx,
                    ))
                    .child(rebase_toggle(
                        "允许改写已推送提交",
                        Switch::new("rebase-allow-pushed")
                            .checked(self.allow_rewrite_pushed)
                            .small()
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.allow_rewrite_pushed = *checked;
                                cx.notify();
                            })),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(rows),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("rebase-start")
                            .primary()
                            .label("开始 Rebase")
                            .loading(busy)
                            .disabled(busy || self.rows.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_rebase(window, cx)
                            })),
                    )
                    .child(
                        Button::new("rebase-continue")
                            .outline()
                            .label("继续")
                            .disabled(
                                busy || !has_state
                                    || !matches!(
                                        self.state.as_ref().map(|state| state.status),
                                        Some(
                                            GitRebaseStatus::Stopped | GitRebaseStatus::InProgress
                                        )
                                    ),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                GitRebaseWindow::continue_rebase(this, window, cx)
                            })),
                    )
                    .child(
                        Button::new("rebase-skip")
                            .outline()
                            .label("跳过")
                            .disabled(
                                busy || !has_state
                                    || !matches!(
                                        self.state.as_ref().map(|state| state.status),
                                        Some(
                                            GitRebaseStatus::Stopped | GitRebaseStatus::InProgress
                                        )
                                    ),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                GitRebaseWindow::skip_rebase(this, window, cx)
                            })),
                    )
                    .child(
                        Button::new("rebase-abort")
                            .danger()
                            .label("中止")
                            .disabled(busy || !can_abort)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                GitRebaseWindow::abort_rebase(this, window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("拖动顺序请使用上下按钮；提交按从旧到新排列。"),
                    ),
            )
    }
}

fn rebase_toggle(label: &'static str, switch: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .gap_1p5()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(switch)
        .into_any_element()
}

pub(crate) fn open_git_rebase_window(repository_path: PathBuf, cx: &mut App) -> Result<(), String> {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("交互式 Rebase".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(980.), px(760.)), cx)),
        window_min_size: Some(size(px(760.), px(580.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitRebaseWindow::new(repository_path.clone(), window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开交互式 Rebase 窗口：{error}"))
}
