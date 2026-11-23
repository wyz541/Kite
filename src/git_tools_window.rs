use std::path::PathBuf;

use crate::app::KiteApp;
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{
    GitSubmodule, GitSubmoduleState, GitWorktree, GitWorktreeRemovalCheck, create_git_worktree,
    git_init_submodules, git_submodules, git_update_submodules, git_worktrees, remove_git_worktree,
    worktree_removal_check,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GitToolsTab {
    #[default]
    Worktrees,
    Submodules,
}

impl GitToolsTab {
    fn label(self) -> &'static str {
        match self {
            Self::Worktrees => "Worktree",
            Self::Submodules => "Submodule",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Worktrees => IconName::GitBranch,
            Self::Submodules => IconName::Boxes,
        }
    }
}

pub(crate) struct GitToolsWindow {
    main_app: Entity<KiteApp>,
    repository_path: PathBuf,
    tab: GitToolsTab,
    worktrees: Vec<GitWorktree>,
    submodules: Vec<GitSubmodule>,
    loading: bool,
    busy: bool,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    showing_create: bool,
    destination_input: Entity<InputState>,
    branch_input: Entity<InputState>,
    start_point_input: Entity<InputState>,
    create_branch: bool,
    pending_removal: Option<GitWorktreeRemovalCheck>,
    _load_task: Task<()>,
    _action_task: Task<()>,
}

impl GitToolsWindow {
    pub(crate) fn new(
        main_app: Entity<KiteApp>,
        repository_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let destination_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Worktree 路径，例如 D:\\Projects\\repo-feature")
                .clean_on_escape()
        });
        let branch_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("分支名")
                .clean_on_escape()
        });
        let start_point_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("HEAD")
                .placeholder("HEAD、分支或提交哈希")
                .clean_on_escape()
        });
        let mut this = Self {
            main_app,
            repository_path,
            tab: GitToolsTab::Worktrees,
            worktrees: Vec::new(),
            submodules: Vec::new(),
            loading: true,
            busy: false,
            error: None,
            notice: None,
            showing_create: false,
            destination_input,
            branch_input,
            start_point_input,
            create_branch: true,
            pending_removal: None,
            _load_task: Task::ready(()),
            _action_task: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.notice = None;
        let repository_path = self.repository_path.clone();
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let worktrees = git_worktrees(&repository_path);
                    let submodules = git_submodules(&repository_path);
                    (worktrees, submodules)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result.0 {
                    Ok(Some(list)) => this.worktrees = list.worktrees,
                    Ok(None) => this.error = Some("所选目录不在 Git 工作区中".into()),
                    Err(error) => this.error = Some(error.into()),
                }
                match result.1 {
                    Ok(Some(list)) => this.submodules = list.submodules,
                    Ok(None) => {}
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn create_worktree(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let destination = self.destination_input.read(cx).value().trim().to_string();
        let branch = self.branch_input.read(cx).value().trim().to_string();
        if destination.is_empty() || branch.is_empty() {
            self.error = Some("请填写 Worktree 路径和分支".into());
            cx.notify();
            return;
        }
        let start_point = self.start_point_input.read(cx).value().trim().to_string();
        let repository_path = self.repository_path.clone();
        let create_branch = self.create_branch;
        self.busy = true;
        self.error = None;
        self.notice = None;
        cx.notify();
        self._action_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    create_git_worktree(
                        &repository_path,
                        &PathBuf::from(destination),
                        if create_branch {
                            kite_core::git::GitWorktreeCreateMode::NewBranch {
                                branch,
                                start_point: Some(start_point),
                            }
                        } else {
                            kite_core::git::GitWorktreeCreateMode::ExistingBranch { branch }
                        },
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(worktree) => {
                        this.notice =
                            Some(format!("已创建 Worktree：{}", worktree.path.display()).into());
                        this.showing_create = false;
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn request_remove(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        self.notice = None;
        cx.notify();
        self._action_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worktree_removal_check(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(check) if check.is_primary => {
                        this.error = Some("不能删除主 Worktree".into());
                    }
                    Ok(check) => this.pending_removal = Some(check),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn confirm_remove(&mut self, cx: &mut Context<Self>) {
        let Some(check) = self.pending_removal.take() else {
            return;
        };
        let path = check.path.clone();
        let force = check.has_uncommitted_changes;
        self.busy = true;
        self.error = None;
        self.notice = None;
        cx.notify();
        self._action_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { remove_git_worktree(&path, force) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.notice = Some("Worktree 已删除".into());
                        let main_app = this.main_app.clone();
                        main_app
                            .update(cx, |app, cx| app.refresh_git_metadata_after_tool_change(cx));
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn run_submodule_action(&mut self, initialize: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let repository_path = self.repository_path.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        cx.notify();
        self._action_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if initialize {
                        git_init_submodules(&repository_path)
                    } else {
                        git_update_submodules(&repository_path)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(message) => {
                        this.notice = Some(message.into());
                        let main_app = this.main_app.clone();
                        main_app
                            .update(cx, |app, cx| app.refresh_git_metadata_after_tool_change(cx));
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn open_directory(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.main_app.update(cx, |app, cx| {
            app.open_directory_from_git_tools(path, window, cx);
        });
        window.remove_window();
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .h(px(40.))
            .w_full()
            .gap_1()
            .items_center()
            .p_1()
            .rounded_lg()
            .bg(cx.theme().secondary.opacity(0.45))
            .child(
                Button::new("git-tools-worktrees")
                    .ghost()
                    .compact()
                    .icon(GitToolsTab::Worktrees.icon())
                    .label(GitToolsTab::Worktrees.label())
                    .selected(self.tab == GitToolsTab::Worktrees)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.tab = GitToolsTab::Worktrees;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("git-tools-submodules")
                    .ghost()
                    .compact()
                    .icon(GitToolsTab::Submodules.icon())
                    .label(GitToolsTab::Submodules.label())
                    .selected(self.tab == GitToolsTab::Submodules)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.tab = GitToolsTab::Submodules;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_create_form(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.3))
            .child(div().text_sm().font_semibold().child("新建 Worktree"))
            .child(Input::new(&self.destination_input).aria_label("Worktree 路径"))
            .child(Input::new(&self.branch_input).aria_label("分支名称"))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("git-worktree-new-branch")
                            .ghost()
                            .compact()
                            .label(if self.create_branch {
                                "✓ 创建新分支"
                            } else {
                                "使用已有分支"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.create_branch = !this.create_branch;
                                cx.notify();
                            })),
                    )
                    .when(self.create_branch, |row| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.start_point_input).aria_label("分支起点")),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("git-worktree-cancel-create")
                            .label("取消")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.showing_create = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-worktree-create")
                            .primary()
                            .label(if self.busy {
                                "正在创建…"
                            } else {
                                "创建"
                            })
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.create_worktree(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_removal_confirmation(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(check) = self.pending_removal.as_ref() else {
            return div().into_any_element();
        };
        let warning = check.has_uncommitted_changes;
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(if warning {
                rgb(0xef6b73).into()
            } else {
                cx.theme().border
            })
            .bg(if warning {
                rgb(0xef6b73).into()
            } else {
                cx.theme().secondary.opacity(0.3)
            })
            .child(div().text_sm().font_semibold().child(if warning {
                "删除 Worktree？未提交改动会丢失"
            } else {
                "删除 Worktree？"
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(check.path.display().to_string()),
            )
            .child(div().text_xs().child(check.summary.clone()))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("git-worktree-remove-cancel")
                            .label("取消")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.pending_removal = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("git-worktree-remove-confirm")
                            .primary()
                            .label(if warning {
                                "放弃改动并删除"
                            } else {
                                "删除"
                            })
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_remove(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_worktree_row(&self, worktree: &GitWorktree, cx: &mut Context<Self>) -> AnyElement {
        let path = worktree.path.clone();
        let delete_path = worktree.path.clone();
        h_flex()
            .w_full()
            .min_h(px(64.))
            .gap_3()
            .items_center()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.24))
            .child(
                Icon::new(if worktree.is_primary {
                    IconName::House
                } else {
                    IconName::GitBranch
                })
                .size_4()
                .text_color(if worktree.is_current {
                    cx.theme().primary
                } else {
                    cx.theme().muted_foreground
                }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div().text_sm().font_semibold().child(
                                    worktree
                                        .branch
                                        .clone()
                                        .unwrap_or_else(|| "分离 HEAD".into()),
                                ),
                            )
                            .when(worktree.is_primary, |row| {
                                row.child(status_tag("主工作区", rgb(0x5e9cff).into()))
                            })
                            .when(worktree.is_current, |row| {
                                row.child(status_tag("当前", rgb(0x55b978).into()))
                            })
                            .when_some(worktree.locked_reason.as_ref(), |row, reason| {
                                row.child(status_tag(
                                    &format!("已锁定：{reason}"),
                                    rgb(0xe5a84b).into(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(worktree.path.display().to_string()),
                    )
                    .when_some(worktree.head.as_ref(), |column, head| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(head.chars().take(12).collect::<String>()),
                        )
                    }),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "git-worktree-open-{}",
                    worktree.path.display()
                )))
                .ghost()
                .compact()
                .label("在 Kite 中打开")
                .disabled(worktree.is_current || worktree.prunable_reason.is_some())
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.open_directory(path.clone(), window, cx)
                    },
                )),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "git-worktree-delete-{}",
                    worktree.path.display()
                )))
                .ghost()
                .compact()
                .icon(IconName::Trash)
                .tooltip("删除 Worktree")
                .disabled(self.busy || worktree.is_primary || worktree.is_current)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.request_remove(delete_path.clone(), cx)
                })),
            )
            .into_any_element()
    }

    fn render_worktrees(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(div().text_sm().font_semibold().child("全部 Worktree"))
                    .child(div().flex_1())
                    .child(
                        Button::new("git-worktree-toggle-create")
                            .compact()
                            .label(if self.showing_create {
                                "取消新建"
                            } else {
                                "新建 Worktree"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.showing_create = !this.showing_create;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.showing_create, |column| {
                column.child(self.render_create_form(cx))
            })
            .when(self.pending_removal.is_some(), |column| {
                column.child(self.render_removal_confirmation(cx))
            })
            .child(
                v_flex()
                    .id("git-worktrees-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap_2()
                    .overflow_y_scroll()
                    .children(
                        self.worktrees
                            .iter()
                            .map(|worktree| self.render_worktree_row(worktree, cx)),
                    ),
            )
            .into_any_element()
    }

    fn render_submodule_row(&self, submodule: &GitSubmodule, cx: &mut Context<Self>) -> AnyElement {
        let path = submodule.path.clone();
        let color = match submodule.state {
            GitSubmoduleState::Initialized => rgb(0x55b978).into(),
            GitSubmoduleState::Uninitialized => cx.theme().muted_foreground,
            GitSubmoduleState::OutOfDate => rgb(0xe5a84b).into(),
            GitSubmoduleState::Conflicted => rgb(0xef6b73).into(),
        };
        h_flex()
            .w_full()
            .min_h(px(64.))
            .gap_3()
            .items_center()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.24))
            .child(Icon::new(IconName::Boxes).size_4().text_color(color))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .child(submodule.name.clone()),
                            )
                            .child(status_tag(submodule.state.label(), color))
                            .when(submodule.has_uncommitted_changes, |row| {
                                row.child(status_tag("有未提交改动", rgb(0xe5a84b).into()))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(submodule.relative_path.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} {}",
                                submodule
                                    .declared_commit
                                    .chars()
                                    .take(12)
                                    .collect::<String>(),
                                submodule.describe.clone().unwrap_or_default()
                            )),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "git-submodule-open-{}",
                    submodule.relative_path
                )))
                .ghost()
                .compact()
                .label("打开")
                .disabled(!submodule.path.is_dir())
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.open_directory(path.clone(), window, cx)
                    },
                )),
            )
            .into_any_element()
    }

    fn render_submodules(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("全部 Submodule"))
                    .child(div().flex_1())
                    .child(
                        Button::new("git-submodules-init")
                            .compact()
                            .label("初始化")
                            .disabled(self.busy || self.submodules.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.run_submodule_action(true, cx)
                            })),
                    )
                    .child(
                        Button::new("git-submodules-update")
                            .compact()
                            .label(if self.busy {
                                "正在更新…"
                            } else {
                                "更新全部"
                            })
                            .disabled(self.busy || self.submodules.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.run_submodule_action(false, cx)
                            })),
                    ),
            )
            .when(self.submodules.is_empty(), |column| {
                column.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Boxes)
                                .size_10()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().text_sm().font_medium().child("没有 Submodule"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("仓库根目录没有 .gitmodules 配置"),
                        ),
                )
            })
            .when(!self.submodules.is_empty(), |column| {
                column.child(
                    v_flex()
                        .id("git-submodules-scroll")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .gap_2()
                        .overflow_y_scroll()
                        .children(
                            self.submodules
                                .iter()
                                .map(|submodule| self.render_submodule_row(submodule, cx)),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for GitToolsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .gap_3()
            .p_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size_6()
                            .text_color(cx.theme().primary),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_lg().font_semibold().child("Git 工作区"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(self.repository_path.display().to_string()),
                            ),
                    )
                    .child(
                        Button::new("git-tools-refresh")
                            .ghost()
                            .compact()
                            .icon(IconName::RotateCw)
                            .tooltip("刷新")
                            .disabled(self.loading || self.busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload(cx))),
                    ),
            )
            .child(self.render_tabs(cx))
            .when_some(self.notice.as_ref(), |column, notice| {
                column.child(message_bar(notice.as_ref(), true, cx))
            })
            .when_some(self.error.as_ref(), |column, error| {
                column.child(message_bar(error.as_ref(), false, cx))
            })
            .child(if self.loading {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("正在读取 Git 工作区…"),
                    )
                    .into_any_element()
            } else {
                match self.tab {
                    GitToolsTab::Worktrees => self.render_worktrees(cx),
                    GitToolsTab::Submodules => self.render_submodules(cx),
                }
            })
    }
}

fn status_tag(label: &str, color: Hsla) -> AnyElement {
    div()
        .px_1p5()
        .py_0p5()
        .rounded_md()
        .bg(color.opacity(0.14))
        .text_xs()
        .text_color(color)
        .child(label.to_string())
        .into_any_element()
}

fn message_bar(message: &str, success: bool, cx: &App) -> AnyElement {
    let color: Hsla = if success {
        rgb(0x55b978).into()
    } else {
        rgb(0xef6b73).into()
    };
    h_flex()
        .w_full()
        .h(px(34.))
        .px_3()
        .items_center()
        .rounded_lg()
        .bg(color.opacity(0.1))
        .text_xs()
        .text_color(color)
        .child(message.to_string())
        .when(!success, |bar| {
            bar.border_1().border_color(color.opacity(0.4))
        })
        .when(success, |bar| {
            bar.border_1().border_color(cx.theme().border)
        })
        .into_any_element()
}

pub(crate) fn open_git_tools_window(
    main_app: Entity<KiteApp>,
    repository_path: PathBuf,
    cx: &mut App,
) -> Result<(), String> {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Git Worktree 与 Submodule".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(620.)), cx)),
        window_min_size: Some(size(px(680.), px(500.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitToolsWindow::new(main_app.clone(), repository_path, window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Git 工具窗口：{error}"))
}
