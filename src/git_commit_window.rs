use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{
    self, GitCommitOptions, GitCommitState, GitSigningMode, commit_subject_warning,
};
use kite_core::vcs::{VcsTool, open_vcs_log_window};

pub(crate) struct GitCommitWindow {
    path: PathBuf,
    state: Option<GitCommitState>,
    loading: bool,
    busy: bool,
    message_editor: Entity<EditorState>,
    author_input: Entity<InputState>,
    amend: bool,
    signoff: bool,
    signing: GitSigningMode,
    skip_hooks: bool,
    push: bool,
    notice: Option<SharedString>,
    error: Option<SharedString>,
    output: Option<SharedString>,
    message_override: Option<String>,
    _message_subscription: Subscription,
    _load_task: Task<()>,
    _commit_task: Task<()>,
}

impl GitCommitWindow {
    pub(crate) fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message_editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value("")
                .language("plaintext")
        });
        let author_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("作者（可选），例如 Name <email>")
                .clean_on_escape()
        });
        let message_subscription = cx.subscribe_in(
            &message_editor,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: true,
                        ..
                    }
                ) {
                    this.submit(window, cx);
                }
                cx.notify();
            },
        );
        let mut this = Self {
            path,
            state: None,
            loading: true,
            busy: false,
            message_editor,
            author_input,
            amend: false,
            signoff: false,
            signing: GitSigningMode::GitConfig,
            skip_hooks: false,
            push: false,
            notice: None,
            error: None,
            output: None,
            message_override: None,
            _message_subscription: message_subscription,
            _load_task: Task::ready(()),
            _commit_task: Task::ready(()),
        };
        this.reload(window, cx);
        this
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let path = self.path.clone();
        let message_override = self.message_override.take();
        self._load_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { git::git_commit_state(&path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(state) => {
                        let current = this.message_editor.read(cx).text().to_string();
                        let next_message = message_override
                            .or_else(|| {
                                (current.trim().is_empty())
                                    .then(|| state.commit_template.clone())
                                    .flatten()
                            })
                            .unwrap_or(current);
                        this.message_editor.update(cx, |editor, cx| {
                            editor.set_value(next_message, window, cx);
                            editor.focus(window, cx);
                        });
                        this.state = Some(state);
                    }
                    Err(error) => {
                        this.state = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(state) = self.state.clone() else {
            return;
        };
        let message = self.message_editor.read(cx).text().to_string();
        if message.trim().is_empty() {
            self.error = Some("请输入提交说明".into());
            cx.notify();
            return;
        }
        let author = self.author_input.read(cx).value().trim().to_string();
        let options = GitCommitOptions {
            repository_path: state.repository_root,
            message,
            amend: self.amend,
            signoff: self.signoff,
            signing: self.signing,
            skip_hooks: self.skip_hooks,
            author: (!author.is_empty()).then_some(author),
            push: self.push,
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.output = None;
        cx.notify();

        self._commit_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { git::commit(options) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(result) => {
                        this.notice = Some(
                            if result.pushed {
                                format!("已提交并推送 {}", result.short_hash)
                            } else {
                                format!("已创建提交 {}", result.short_hash)
                            }
                            .into(),
                        );
                        this.output = combine_output(
                            result.command_output.as_deref(),
                            result.push_output.as_deref(),
                        );
                        if let Some(push_error) = result.push_error {
                            this.error = Some(push_error.into());
                        }
                        this.message_override = Some(if result.amended {
                            result.subject
                        } else {
                            String::new()
                        });
                        this.amend = false;
                        this.reload(window, cx);
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn set_message(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.message_editor.update(cx, |editor, cx| {
            editor.set_value(message, window, cx);
            editor.focus(window, cx);
        });
        cx.notify();
    }

    fn toggle_amend(&mut self, checked: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = checked;
        if checked
            && let Some(previous) = self
                .state
                .as_ref()
                .and_then(|state| state.previous_message.clone())
        {
            self.set_message(previous, window, cx);
            return;
        }
        cx.notify();
    }

    fn signing_label(&self, state: Option<&GitCommitState>) -> String {
        match self.signing {
            GitSigningMode::GitConfig => {
                if state.is_some_and(|state| state.signing_default) {
                    "签名 · 跟随 Git（开）".to_string()
                } else {
                    "签名 · 跟随 Git（关）".to_string()
                }
            }
            GitSigningMode::Enabled => "签名 · 强制开启".to_string(),
            GitSigningMode::Disabled => "签名 · 强制关闭".to_string(),
        }
    }

    fn open_log(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { open_vcs_log_window(VcsTool::Git, &path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {}
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_signing_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.signing;
        let button = |id: &'static str,
                      label: &'static str,
                      mode: GitSigningMode,
                      cx: &mut Context<Self>| {
            Button::new(id)
                .ghost()
                .compact()
                .label(label)
                .selected(current == mode)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.signing = mode;
                    cx.notify();
                }))
                .into_any_element()
        };
        h_flex()
            .gap_1()
            .p_1()
            .rounded_md()
            .bg(cx.theme().muted.opacity(0.22))
            .child(button(
                "git-signing-config",
                "跟随 Git",
                GitSigningMode::GitConfig,
                cx,
            ))
            .child(button(
                "git-signing-on",
                "签名",
                GitSigningMode::Enabled,
                cx,
            ))
            .child(button(
                "git-signing-off",
                "不签名",
                GitSigningMode::Disabled,
                cx,
            ))
            .into_any_element()
    }

    fn render_staged_files(&self, cx: &App) -> AnyElement {
        let Some(state) = self.state.as_ref() else {
            return div().into_any_element();
        };
        let (files, added, removed, heading, empty_label) = if self.amend {
            (
                &state.amend_files,
                state.amend_lines_added,
                state.amend_lines_removed,
                "Amend 后的改动",
                "上一次提交中没有可显示的改动。",
            )
        } else {
            (
                &state.staged_files,
                state.staged_lines_added,
                state.staged_lines_removed,
                "待提交改动",
                "没有已暂存的文件。",
            )
        };
        let mut list = v_flex().gap_1();
        if files.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(empty_label),
            );
        } else {
            for path in files {
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("staged-{path}")))
                        .gap_2()
                        .items_center()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(cx.theme().muted.opacity(0.18))
                        .child(
                            Icon::new(IconName::FilePen)
                                .size(px(13.))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().min_w_0().truncate().text_sm().child(path.clone())),
                );
            }
        }
        v_flex()
            .w_full()
            .max_h(px(190.))
            .flex_shrink_0()
            .gap_3()
            .p_3()
            .rounded_lg()
            .bg(cx.theme().title_bar)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_semibold().child(heading))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} 个文件 · +{} −{}", files.len(), added, removed)),
                    ),
            )
            .child(
                v_flex()
                    .id("git-staged-files")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .into_any_element()
    }

    fn render_message_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = self.state.as_ref() else {
            return div().into_any_element();
        };
        let message = self.message_editor.read(cx).text().to_string();
        let warning = commit_subject_warning(&message);
        let disabled = self.busy
            || !state.identity_configured
            || !state.unresolved_conflicts.is_empty()
            || (!self.amend && state.staged_files.is_empty())
            || (self.amend && state.head_short_hash.is_none())
            || message.trim().is_empty();
        let count = if self.amend {
            state.amend_files.len()
        } else {
            state.staged_files.len()
        };
        let button_label = if self.push {
            format!("提交并推送 · {count} 个文件")
        } else if self.amend {
            format!("Amend · {count} 个文件")
        } else {
            format!("提交 · {count} 个文件")
        };

        let recent = state
            .recent_messages
            .iter()
            .take(6)
            .enumerate()
            .map(|(index, message)| {
                let message = message.clone();
                let label = message.lines().next().unwrap_or("提交信息").to_string();
                Button::new(("git-recent-message", index))
                    .ghost()
                    .compact()
                    .label(label)
                    .tooltip(message.clone())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_message(message.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let options = h_flex()
            .gap_3()
            .flex_wrap()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Switch::new("git-commit-amend")
                            .checked(self.amend)
                            .small()
                            .disabled(state.head_short_hash.is_none())
                            .accessibility_label("Amend 上一次提交")
                            .on_click(cx.listener(|this, checked, window, cx| {
                                this.toggle_amend(*checked, window, cx)
                            })),
                    )
                    .child(div().text_sm().child("Amend")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Switch::new("git-commit-signoff")
                            .checked(self.signoff)
                            .small()
                            .accessibility_label("添加 Signed-off-by")
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.signoff = *checked;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child("Signed-off-by")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Switch::new("git-commit-skip-hooks")
                            .checked(self.skip_hooks)
                            .small()
                            .accessibility_label("跳过 Git hooks")
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.skip_hooks = *checked;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child("跳过 hooks")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Switch::new("git-commit-push")
                            .checked(self.push)
                            .small()
                            .accessibility_label("提交后推送")
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.push = *checked;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child("提交并推送")),
            )
            .into_any_element();

        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().font_semibold().child("提交说明"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Ctrl/Cmd+Enter 提交"),
                    )
                    .when_some(warning, |this, warning| {
                        this.child(
                            div()
                                .ml_auto()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(warning),
                        )
                    }),
            )
            .child(
                Editor::new(&self.message_editor)
                    .aria_label("提交说明")
                    .h(px(230.))
                    .into_any_element(),
            )
            .when(!recent.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("最近提交信息"),
                        )
                        .child(h_flex().gap_1().flex_wrap().children(recent)),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("作者"),
                    )
                    .child(Input::new(&self.author_input).cleanable(true).w_full()),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(options)
                    .child(div().flex_1())
                    .child(self.render_signing_buttons(cx)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.signing_label(Some(state))),
            )
            .child(div().flex_1())
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .p_2()
                        .rounded_md()
                        .bg(cx.theme().success.opacity(0.12))
                        .child(div().text_sm().child(notice))
                        .child(
                            Button::new("git-commit-open-log")
                                .ghost()
                                .compact()
                                .label("查看日志")
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| this.open_log(cx)),
                                ),
                        ),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when_some(self.output.clone(), |this, output| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Git / hooks 输出"),
                        )
                        .child(
                            div()
                                .id("git-commit-output")
                                .max_h(px(150.))
                                .overflow_y_scrollbar()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().muted.opacity(0.2))
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .child(output),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("git-commit-cancel")
                            .outline()
                            .label("关闭")
                            .on_click(cx.listener(|_, _: &ClickEvent, window, _| {
                                window.remove_window();
                            })),
                    )
                    .child(
                        Button::new("git-commit-submit")
                            .primary()
                            .label(if self.busy {
                                "正在提交…"
                            } else {
                                button_label.as_str()
                            })
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_blockers(&self, cx: &App) -> Option<AnyElement> {
        let state = self.state.as_ref()?;
        if !state.identity_configured {
            return Some(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().warning.opacity(0.12))
                    .text_sm()
                    .child(
                        "尚未配置 Git 用户身份。请先在终端运行：\n\
                         git config --global user.name \"Your Name\"\n\
                         git config --global user.email \"you@example.com\"",
                    )
                    .into_any_element(),
            );
        }
        if !state.unresolved_conflicts.is_empty() {
            return Some(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().danger.opacity(0.12))
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(format!(
                        "仍有未解决的冲突：{}",
                        state.unresolved_conflicts.join("、")
                    ))
                    .into_any_element(),
            );
        }
        None
    }
}

impl Render for GitCommitWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.state.as_ref().map(|state| {
            let repo = Path::new(&state.repository_root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| state.repository_root.display().to_string());
            let branch = state
                .branch
                .clone()
                .unwrap_or_else(|| "分离 HEAD".to_string());
            h_flex()
                .gap_3()
                .items_center()
                .child(
                    Icon::new(IconName::GitBranch)
                        .size(px(18.))
                        .text_color(cx.theme().accent),
                )
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(div().font_semibold().child(repo))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if let Some(upstream) = &state.upstream {
                                    format!("{branch} → {upstream}")
                                } else {
                                    branch
                                }),
                        ),
                )
                .into_any_element()
        });

        v_flex()
            .id("git-commit-window")
            .size_full()
            .p_4()
            .gap_3()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when_some(header, |this, header| this.child(header))
            .when_some(self.render_blockers(cx), |this, blocker| {
                this.child(blocker)
            })
            .child(if self.loading && self.state.is_none() {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("正在读取 Git 提交信息…")
                    .into_any_element()
            } else if self.state.is_some() {
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .gap_3()
                            .child(self.render_staged_files(cx))
                            .child(self.render_message_form(cx)),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(
                        self.error
                            .clone()
                            .unwrap_or_else(|| "无法读取 Git 仓库".into()),
                    )
                    .into_any_element()
            })
    }
}

fn combine_output(command: Option<&str>, push: Option<&str>) -> Option<SharedString> {
    let mut parts = [command, push]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts.dedup();
    (!parts.is_empty()).then(|| parts.join("\n\n").into())
}
