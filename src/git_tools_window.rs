use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::base::{Selectable as _, SelectableText};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{GitDiffLine, GitDiffLineKind, GitOperationState};
use kite_core::git_ops::{
    GitCommandOutcome, GitRepositoryStatus, GitResetMode, GitStashEntry, GitStashSaveOptions,
    git_cherry_pick, git_cherry_pick_abort, git_cherry_pick_continue, git_cherry_pick_skip,
    git_operation_status, git_reset, git_revert, git_revert_abort, git_revert_continue,
    git_stash_apply, git_stash_branch, git_stash_clear, git_stash_diff, git_stash_drop,
    git_stash_list, git_stash_pop, git_stash_save,
};
use kite_core::vcs::{VcsQuery, VcsTool, run_vcs_query};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GitToolsTab {
    #[default]
    Stash,
    Log,
    Actions,
}

impl GitToolsTab {
    fn label(self, stash_count: usize) -> String {
        match self {
            Self::Stash => format!("Stash ({stash_count})"),
            Self::Log => "历史".to_string(),
            Self::Actions => "操作".to_string(),
        }
    }
}

pub(crate) struct GitToolsWindow {
    path: PathBuf,
    selected_paths: Vec<PathBuf>,
    tab: GitToolsTab,
    status: Option<GitRepositoryStatus>,
    stashes: Vec<GitStashEntry>,
    selected_stash: Option<u32>,
    stash_diff: Option<Vec<GitDiffLine>>,
    log_text: Option<String>,
    loading: bool,
    busy: bool,
    notice: Option<SharedString>,
    error: Option<SharedString>,
    include_untracked: bool,
    keep_index: bool,
    reset_mode: GitResetMode,
    stash_message_input: Entity<InputState>,
    reset_target_input: Entity<InputState>,
    commits_input: Entity<InputState>,
    branch_input: Entity<InputState>,
    _task: Task<()>,
}

impl GitToolsWindow {
    fn new(
        path: PathBuf,
        selected_paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let stash_message_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Stash 说明（可选）")
                .clean_on_escape()
        });
        let reset_target_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("HEAD~1、分支或提交")
                .clean_on_escape();
            input.set_value("HEAD~1", window, cx);
            input
        });
        let commits_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("提交哈希，用逗号分隔")
                .clean_on_escape()
        });
        let branch_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("新分支名称")
                .clean_on_escape()
        });
        let mut this = Self {
            path,
            selected_paths,
            tab: GitToolsTab::Stash,
            status: None,
            stashes: Vec::new(),
            selected_stash: None,
            stash_diff: None,
            log_text: None,
            loading: true,
            busy: false,
            notice: None,
            error: None,
            include_untracked: false,
            keep_index: false,
            reset_mode: GitResetMode::Mixed,
            stash_message_input,
            reset_target_input,
            commits_input,
            branch_input,
            _task: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.loading = true;
        self._task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let status = git_operation_status(&path);
                    let stashes = git_stash_list(&path);
                    let log = run_vcs_query(VcsTool::Git, VcsQuery::Log, &path);
                    (status, stashes, log)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result.0 {
                    Ok(status) => this.status = Some(status),
                    Err(error) => this.error = Some(error.into()),
                }
                match result.1 {
                    Ok(stashes) => {
                        this.stashes = stashes;
                        if this
                            .selected_stash
                            .is_some_and(|index| index as usize >= this.stashes.len())
                        {
                            this.selected_stash = this.stashes.first().map(|_| 0);
                            this.stash_diff = None;
                        }
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                this.log_text = result.2.ok();
                cx.notify();
            });
        });
    }

    fn run_operation<F>(
        &mut self,
        pending: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        operation: F,
    ) where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        self.error = None;
        self.notice = Some(pending.into());
        self.busy = true;
        cx.notify();
        self._task = cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { operation() }).await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
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
                this.reload(cx);
            });
        });
    }

    fn selected_stash_index(&self) -> Option<u32> {
        self.selected_stash
            .filter(|index| (*index as usize) < self.stashes.len())
    }

    fn save_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let message = self.stash_message_input.read(cx).value().trim().to_string();
        let message = (!message.is_empty()).then_some(message);
        let include_untracked = self.include_untracked;
        let keep_index = self.keep_index;
        let paths = self.selected_paths.clone();
        self.run_operation("正在创建 Stash…", window, cx, move || {
            git_stash_save(
                &path,
                GitStashSaveOptions {
                    message,
                    include_untracked,
                    keep_index,
                    paths,
                },
            )
        });
    }

    fn apply_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_stash_index() else {
            return;
        };
        let path = self.path.clone();
        self.run_operation("正在应用 Stash…", window, cx, move || {
            git_stash_apply(&path, index).map(outcome_message)
        });
    }

    fn pop_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_stash_index() else {
            return;
        };
        let path = self.path.clone();
        self.run_operation("正在弹出 Stash…", window, cx, move || {
            git_stash_pop(&path, index).map(outcome_message)
        });
    }

    fn show_stash_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_stash_index() else {
            return;
        };
        let path = self.path.clone();
        self.error = None;
        self.notice = Some("正在读取 Stash 差异…".into());
        cx.notify();
        self._task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { git_stash_diff(&path, index) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(diff) => {
                        this.error = None;
                        this.notice = None;
                        this.stash_diff = Some(diff);
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

    fn create_branch_from_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_stash_index() else {
            return;
        };
        let branch = self.branch_input.read(cx).value().trim().to_string();
        let path = self.path.clone();
        self.run_operation("正在从 Stash 创建分支…", window, cx, move || {
            git_stash_branch(&path, index, &branch).map(outcome_message)
        });
    }

    fn drop_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_stash_index() else {
            return;
        };
        let selector = self.stashes[index as usize].selector.clone();
        let path = self.path.clone();
        let message = format!("永久删除 {selector}？");
        let detail = "Stash 中的改动将被丢弃。此操作不可撤销。";
        let answers = [PromptButton::ok("删除 Stash"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, &message, Some(detail), &answers, cx);
        self._task = cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_operation("正在删除 Stash…", window, cx, move || {
                    git_stash_drop(&path, index, true)
                });
            });
        });
    }

    fn clear_stashes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let answers = [
            PromptButton::ok("清空全部 Stash"),
            PromptButton::cancel("取消"),
        ];
        let answer = window.prompt(
            PromptLevel::Warning,
            "清空全部 Git Stash？",
            Some("所有 Stash 都将被永久删除。此操作不可撤销。"),
            &answers,
            cx,
        );
        self._task = cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_operation("正在清空 Stash…", window, cx, move || {
                    git_stash_clear(&path, true)
                });
            });
        });
    }

    fn reset_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let target = self.reset_target_input.read(cx).value().trim().to_string();
        let mode = self.reset_mode;
        self.error = None;
        if mode.is_destructive() {
            let answers = [
                PromptButton::ok("执行 reset --hard"),
                PromptButton::cancel("取消"),
            ];
            let detail =
                format!("目标：{target}\n\n未提交的已跟踪改动将被永久丢弃。未跟踪文件会保留。");
            let answer = window.prompt(
                PromptLevel::Warning,
                "执行 Git reset --hard？",
                Some(&detail),
                &answers,
                cx,
            );
            self._task = cx.spawn_in(window, async move |this, cx| {
                let Ok(choice) = answer.await else {
                    return;
                };
                if choice != 0 {
                    return;
                }
                let _ = this.update_in(cx, |this, window, cx| {
                    this.run_operation("正在执行 Git reset…", window, cx, move || {
                        git_reset(&path, mode, &target, true)
                    });
                });
            });
        } else {
            self.run_operation("正在执行 Git reset…", window, cx, move || {
                git_reset(&path, mode, &target, false)
            });
        }
    }

    fn revert_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commits = parse_commits(&self.commits_input.read(cx).value());
        let path = self.path.clone();
        self.run_operation("正在还原提交…", window, cx, move || {
            git_revert(&path, &commits).map(outcome_message)
        });
    }

    fn cherry_pick_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commits = parse_commits(&self.commits_input.read(cx).value());
        let path = self.path.clone();
        self.run_operation("正在拣选提交…", window, cx, move || {
            git_cherry_pick(&path, &commits).map(outcome_message)
        });
    }

    fn continue_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let cherry_picking = self
            .status
            .as_ref()
            .is_some_and(|status| status.operation == Some(GitOperationState::CherryPicking));
        self.run_operation("正在继续 Git 操作…", window, cx, move || {
            if cherry_picking {
                git_cherry_pick_continue(&path).map(outcome_message)
            } else {
                git_revert_continue(&path).map(outcome_message)
            }
        });
    }

    fn skip_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.run_operation("正在跳过当前提交…", window, cx, move || {
            git_cherry_pick_skip(&path).map(outcome_message)
        });
    }

    fn abort_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let cherry_picking = self
            .status
            .as_ref()
            .is_some_and(|status| status.operation == Some(GitOperationState::CherryPicking));
        self.run_operation("正在中止 Git 操作…", window, cx, move || {
            if cherry_picking {
                git_cherry_pick_abort(&path)
            } else {
                git_revert_abort(&path)
            }
        });
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let branch = self
            .status
            .as_ref()
            .and_then(|status| status.branch.clone())
            .unwrap_or_else(|| "分离 HEAD".to_string());
        let root = self
            .status
            .as_ref()
            .map(|status| status.repository_root.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned());
        v_flex()
            .gap_2()
            .px(px(24.))
            .pt(px(22.))
            .pb(px(16.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .size(px(38.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(10.))
                            .bg(cx.theme().primary.opacity(0.14))
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .size_5()
                                    .text_color(cx.theme().primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_xl().font_semibold().child(branch))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(root),
                            ),
                    )
                    .child(div().flex_1())
                    .when(self.loading || self.busy, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("正在同步…"),
                        )
                    }),
            )
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.status.as_ref(), |this, status| {
                if status.operation.is_some() || status.has_conflicts {
                    this.child(self.render_operation_banner(status, cx))
                } else {
                    this
                }
            })
            .into_any_element()
    }

    fn render_operation_banner(
        &self,
        status: &GitRepositoryStatus,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let operation = status
            .operation
            .map(GitOperationState::label)
            .unwrap_or("存在 Git 冲突");
        let cherry_picking = status.operation == Some(GitOperationState::CherryPicking);
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().warning.opacity(0.45))
            .bg(cx.theme().warning.opacity(0.10))
            .px_3()
            .py_2()
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(operation.to_string()),
            )
            .child(
                Button::new("git-op-continue")
                    .compact()
                    .primary()
                    .label("继续")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.continue_operation(window, cx)
                    })),
            )
            .when(cherry_picking, |this| {
                this.child(Button::new("git-op-skip").compact().label("跳过").on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.skip_operation(window, cx)),
                ))
            })
            .child(
                Button::new("git-op-abort")
                    .compact()
                    .label("中止")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.abort_operation(window, cx)
                    })),
            )
            .into_any_element()
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .gap_1()
            .px(px(24.))
            .py(px(10.))
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                [GitToolsTab::Stash, GitToolsTab::Log, GitToolsTab::Actions].map(|tab| {
                    Button::new((
                        "git-tools-tab",
                        match tab {
                            GitToolsTab::Stash => 0usize,
                            GitToolsTab::Log => 1usize,
                            GitToolsTab::Actions => 2usize,
                        },
                    ))
                    .compact()
                    .ghost()
                    .selected(self.tab == tab)
                    .label(tab.label(self.stashes.len()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
                    .into_any_element()
                }),
            )
            .into_any_element()
    }

    fn render_stash_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_stash_index();
        let mut list = v_flex().w_full().gap_1();
        if self.stashes.is_empty() {
            list = list.child(
                div()
                    .py_5()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("还没有 Stash。"),
            );
        } else {
            for (index, stash) in self.stashes.iter().enumerate() {
                let index = index as u32;
                list = list.child(
                    Button::new(("git-stash-row", index))
                        .ghost()
                        .selected(selected == Some(index))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.selected_stash = Some(index);
                            this.stash_diff = None;
                            cx.notify();
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .gap_3()
                                .items_center()
                                .rounded_md()
                                .px_2()
                                .py_2()
                                .when(selected == Some(index), |this| {
                                    this.bg(cx.theme().primary.opacity(0.10))
                                })
                                .child(
                                    div()
                                        .w(px(78.))
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(cx.theme().muted_foreground)
                                        .child(stash.selector.clone()),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_0p5()
                                        .child(
                                            div().truncate().text_sm().child(stash.message.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "{} · {}",
                                                    stash.short_hash, stash.date
                                                )),
                                        ),
                                ),
                        ),
                );
            }
        }

        let selected_text = selected
            .map(|index| self.stashes[index as usize].selector.clone())
            .unwrap_or_else(|| "请选择一个 Stash".to_string());
        let diff = if let Some(diff) = self.stash_diff.as_ref() {
            if diff.is_empty() {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("此 Stash 没有可显示的文本差异。")
                    .into_any_element()
            } else {
                v_flex()
                    .id("git-stash-diff")
                    .w_full()
                    .max_h(px(260.))
                    .overflow_y_scrollbar()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.20))
                    .p_2()
                    .children(diff.iter().map(|line| render_diff_line(line, cx)))
                    .into_any_element()
            }
        } else {
            div().into_any_element()
        };

        v_flex()
            .w_full()
            .gap_5()
            .pb_8()
            .child(
                self.card(
                    "保存 Stash",
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(
                            Input::new(&self.stash_message_input)
                                .aria_label("Stash 说明")
                                .w_full(),
                        )
                        .child(
                            h_flex()
                                .gap_5()
                                .child(switch_row(
                                    "包含未跟踪文件",
                                    Switch::new("git-stash-include-untracked")
                                        .checked(self.include_untracked)
                                        .small()
                                        .on_click(cx.listener(|this, checked, _, cx| {
                                            this.include_untracked = *checked;
                                            cx.notify();
                                        })),
                                    cx,
                                ))
                                .child(switch_row(
                                    "保留暂存区",
                                    Switch::new("git-stash-keep-index")
                                        .checked(self.keep_index)
                                        .small()
                                        .on_click(cx.listener(|this, checked, _, cx| {
                                            this.keep_index = *checked;
                                            cx.notify();
                                        })),
                                    cx,
                                )),
                        )
                        .when(!self.selected_paths.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "只保存 {} 个选中项目",
                                        self.selected_paths.len()
                                    )),
                            )
                        })
                        .child(
                            Button::new("git-stash-save")
                                .primary()
                                .label("保存 Stash")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.save_stash(window, cx)
                                })),
                        )
                        .into_any_element(),
                    cx,
                ),
            )
            .child(
                self.card(
                    "Stash 列表",
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(list)
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("git-stash-apply")
                                        .compact()
                                        .label("应用")
                                        .disabled(selected.is_none() || self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.apply_stash(window, cx)
                                            },
                                        )),
                                )
                                .child(
                                    Button::new("git-stash-pop")
                                        .compact()
                                        .label("弹出")
                                        .disabled(selected.is_none() || self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.pop_stash(window, cx)
                                            },
                                        )),
                                )
                                .child(
                                    Button::new("git-stash-diff")
                                        .compact()
                                        .label("查看差异")
                                        .disabled(selected.is_none())
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.show_stash_diff(window, cx)
                                            },
                                        )),
                                )
                                .child(
                                    Button::new("git-stash-drop")
                                        .compact()
                                        .danger()
                                        .label("删除")
                                        .disabled(selected.is_none() || self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.drop_stash(window, cx)
                                            },
                                        )),
                                )
                                .child(
                                    Button::new("git-stash-clear")
                                        .compact()
                                        .danger()
                                        .label("全部清空")
                                        .disabled(self.stashes.is_empty() || self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.clear_stashes(window, cx)
                                            },
                                        )),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .items_center()
                                .child(
                                    Input::new(&self.branch_input)
                                        .aria_label("从 Stash 创建分支")
                                        .w_full(),
                                )
                                .child(
                                    Button::new("git-stash-branch")
                                        .compact()
                                        .label("新建分支")
                                        .disabled(selected.is_none() || self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.create_branch_from_stash(window, cx)
                                            },
                                        )),
                                ),
                        )
                        .into_any_element(),
                    cx,
                ),
            )
            .child(self.card(selected_text, diff, cx))
            .into_any_element()
    }

    fn render_log_tab(&self, cx: &App) -> AnyElement {
        self.card(
            "Git 历史",
            v_flex()
                .id("git-tools-log")
                .w_full()
                .max_h(px(560.))
                .overflow_y_scrollbar()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.20))
                .p_3()
                .text_xs()
                .font_family("monospace")
                .child(SelectableText::new(
                    "git-tools-log-text",
                    self.log_text
                        .clone()
                        .unwrap_or_else(|| "正在读取 Git 历史…".to_string()),
                ))
                .into_any_element(),
            cx,
        )
    }

    fn render_actions_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let modes = [
            (GitResetMode::Soft, "Soft", "保留改动并放入暂存区"),
            (GitResetMode::Mixed, "Mixed", "保留工作区改动"),
            (GitResetMode::Hard, "Hard", "丢弃已跟踪改动"),
            (GitResetMode::Keep, "Keep", "保留未冲突的改动"),
        ];
        let mode_buttons = h_flex().flex_wrap().gap_1().children(
            modes
                .iter()
                .copied()
                .map(|(mode, label, _)| {
                    Button::new(SharedString::from(format!(
                        "git-reset-mode-{}",
                        mode.label()
                    )))
                    .compact()
                    .ghost()
                    .selected(self.reset_mode == mode)
                    .label(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.reset_mode = mode;
                        cx.notify();
                    }))
                    .into_any_element()
                })
                .collect::<Vec<_>>(),
        );
        let reset_detail = modes
            .iter()
            .find(|(mode, _, _)| *mode == self.reset_mode)
            .map(|(_, _, detail)| *detail)
            .unwrap_or("");

        self.card(
            "Reset",
            v_flex()
                .w_full()
                .gap_3()
                .child(mode_buttons)
                .child(
                    div()
                        .text_xs()
                        .text_color(if self.reset_mode.is_destructive() {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(reset_detail),
                )
                .child(
                    Input::new(&self.reset_target_input)
                        .aria_label("Reset 目标")
                        .w_full(),
                )
                .child(
                    Button::new("git-reset-run")
                        .primary()
                        .label(format!("执行 reset --{}", self.reset_mode.label()))
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.reset_repository(window, cx)
                        })),
                )
                .into_any_element(),
            cx,
        )
        .into_any_element()
    }

    fn render_history_actions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_5()
            .pb_8()
            .child(self.render_actions_tab(cx))
            .child(
                self.card(
                    "Revert / Cherry-pick",
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("输入一个或多个提交，用逗号或换行分隔。"),
                        )
                        .child(
                            Input::new(&self.commits_input)
                                .aria_label("提交列表")
                                .w_full(),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("git-revert-run")
                                        .label("Revert")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.revert_commits(window, cx)
                                            },
                                        )),
                                )
                                .child(
                                    Button::new("git-cherry-pick-run")
                                        .primary()
                                        .label("Cherry-pick")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.cherry_pick_commits(window, cx)
                                            },
                                        )),
                                ),
                        )
                        .into_any_element(),
                    cx,
                ),
            )
            .into_any_element()
    }

    fn card(&self, title: impl Into<SharedString>, content: AnyElement, cx: &App) -> AnyElement {
        v_flex()
            .w_full()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar.opacity(0.80))
            .p_4()
            .child(div().text_sm().font_semibold().child(title.into()))
            .child(content)
            .into_any_element()
    }
}

impl Render for GitToolsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            GitToolsTab::Stash => self.render_stash_tab(cx),
            GitToolsTab::Log => self.render_log_tab(cx),
            GitToolsTab::Actions => self.render_history_actions(cx),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_header(cx))
            .child(self.render_tabs(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(div().px(px(24.)).py(px(18.)).child(body)),
            )
    }
}

pub(crate) fn open_git_tools_window(
    path: PathBuf,
    selected_paths: Vec<PathBuf>,
    cx: &mut App,
) -> Result<(), String> {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Kite Git".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(960.), px(720.)), cx)),
        window_min_size: Some(size(px(780.), px(560.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitToolsWindow::new(path.clone(), selected_paths.clone(), window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Git 工具窗口：{error}"))
}

fn outcome_message(outcome: GitCommandOutcome) -> String {
    let mut message = outcome.message;
    if outcome.has_conflicts {
        message.push_str(" 请在修复冲突后继续或中止。");
    }
    message
}

fn parse_commits(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn switch_row(label: &'static str, switch: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(switch)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .into_any_element()
}

fn render_diff_line(line: &GitDiffLine, cx: &App) -> AnyElement {
    let (prefix, color, background) = match line.kind {
        GitDiffLineKind::Added => (
            "+",
            cx.theme().success,
            Some(cx.theme().success.opacity(0.10)),
        ),
        GitDiffLineKind::Removed => (
            "-",
            cx.theme().danger,
            Some(cx.theme().danger.opacity(0.10)),
        ),
        GitDiffLineKind::Hunk => ("", cx.theme().muted_foreground, None),
        GitDiffLineKind::Context => (" ", cx.theme().foreground, None),
    };
    div()
        .w_full()
        .px_1()
        .whitespace_nowrap()
        .overflow_hidden()
        .text_xs()
        .font_family("monospace")
        .text_color(color)
        .when_some(background, |this, background| this.bg(background))
        .child(format!("{prefix}{}", line.text))
        .into_any_element()
}
