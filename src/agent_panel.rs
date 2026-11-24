use std::path::{Path, PathBuf};

use crate::app::KiteApp;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::agent_context::{
    AgentContext, AgentContextMode, AgentLaunchRequest, AgentPath, AgentSessionPhase,
    default_working_directory,
};

pub(crate) struct AgentPanel {
    context: Option<AgentContext>,
    mode: AgentContextMode,
    phase: AgentSessionPhase,
    prompt_input: Entity<InputState>,
    main_app: Option<WeakEntity<KiteApp>>,
    notice: Option<SharedString>,
}

impl AgentPanel {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("添加问题或指令。内容会先停留在 Kite 中。")
                .clean_on_escape()
        });
        Self {
            context: None,
            mode: AgentContextMode::Fixed,
            phase: AgentSessionPhase::Empty,
            prompt_input,
            main_app: None,
            notice: None,
        }
    }

    pub(crate) fn set_main_app(&mut self, main_app: WeakEntity<KiteApp>) {
        self.main_app = Some(main_app);
    }

    pub(crate) fn present(
        &mut self,
        request: AgentLaunchRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mode = AgentContextMode::Fixed;
        self.phase = AgentSessionPhase::Unconfigured;
        self.prompt_input.update(cx, |input, cx| {
            input.set_value(request.prompt, window, cx);
        });
        self.context = Some(request.context);
        self.notice = None;
        cx.notify();
    }

    pub(crate) fn follows_selection(&self) -> bool {
        self.mode == AgentContextMode::FollowSelection
    }

    pub(crate) fn replace_followed_selection(
        &mut self,
        paths: Vec<AgentPath>,
        current_directory: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if !self.follows_selection() || paths.is_empty() {
            return;
        }
        let primary = paths[0].clone();
        let working_directory = if primary.is_dir {
            primary.path.clone()
        } else {
            current_directory
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| default_working_directory(&primary.path))
        };
        match AgentContext::new(&primary.path, paths, Some(working_directory)) {
            Ok(context) => {
                self.context = Some(context);
                self.notice = Some("Agent 上下文已跟随当前选择".into());
            }
            Err(error) => {
                self.context = None;
                self.phase = AgentSessionPhase::Error;
                self.notice = Some(error.into());
            }
        }
        cx.notify();
    }

    pub(crate) fn add_paths(&mut self, paths: Vec<AgentPath>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if let Some(context) = self.context.as_mut() {
            let added = context.append(paths);
            self.notice = Some(if added == 0 {
                "所选路径已在上下文中".into()
            } else {
                format!("已添加 {added} 个上下文路径").into()
            });
        } else {
            let primary = paths[0].clone();
            let working_directory = if primary.is_dir {
                primary.path.clone()
            } else {
                default_working_directory(&primary.path)
            };
            match AgentContext::new(&primary.path, paths, Some(working_directory)) {
                Ok(context) => {
                    self.context = Some(context);
                    self.phase = AgentSessionPhase::Unconfigured;
                    self.notice = Some("已添加上下文路径".into());
                }
                Err(error) => {
                    self.phase = AgentSessionPhase::Error;
                    self.notice = Some(error.into());
                }
            }
        }
        cx.notify();
    }

    fn remove_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(context) = self.context.as_mut() else {
            return;
        };
        if context.remove(path) && context.paths.is_empty() {
            self.context = None;
            self.phase = AgentSessionPhase::Empty;
        }
        self.notice = Some("已从 Agent 上下文移除".into());
        cx.notify();
    }

    fn append_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("添加到 Agent 上下文".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.phase = AgentSessionPhase::Error;
                        this.notice = Some(format!("无法选择上下文路径：{error}").into());
                        cx.notify();
                    });
                    return;
                }
                Err(_) => return,
            };
            let paths = selected
                .into_iter()
                .map(|path| AgentPath::new(path.clone(), path.is_dir()))
                .collect::<Vec<_>>();
            let _ = this.update_in(cx, |this, _, cx| this.add_paths(paths, cx));
        })
        .detach();
    }

    fn reveal_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(main_app) = self.main_app.as_ref().and_then(|entity| entity.upgrade()) else {
            return;
        };
        main_app.update(cx, |app, cx| app.reveal_agent_path(path, window, cx));
    }

    fn toggle_follow_selection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.mode = match self.mode {
            AgentContextMode::Fixed => AgentContextMode::FollowSelection,
            AgentContextMode::FollowSelection => AgentContextMode::Fixed,
        };
        if self.mode == AgentContextMode::FollowSelection
            && let Some(main_app) = self.main_app.as_ref().and_then(|entity| entity.upgrade())
        {
            main_app.update(cx, |app, cx| app.sync_agent_follow_selection(cx));
        }
        cx.notify();
    }

    fn import_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(main_app) = self.main_app.as_ref().and_then(|entity| entity.upgrade()) {
            main_app.update(cx, |app, cx| app.open_agent_review_file(window, cx));
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(main_app) = self.main_app.as_ref().and_then(|entity| entity.upgrade()) {
            main_app.update(cx, |app, cx| app.open_agent_settings(window, cx));
        }
    }

    fn render_status(&self, cx: &App) -> AnyElement {
        let (status_color, connection_label) = match self.phase {
            AgentSessionPhase::Empty | AgentSessionPhase::Unconfigured => {
                (cx.theme().muted_foreground, "未连接")
            }
            AgentSessionPhase::Loading => (cx.theme().primary, "正在连接"),
            AgentSessionPhase::Running => (cx.theme().success, "已连接"),
            AgentSessionPhase::Error => (cx.theme().danger, "连接失败"),
            AgentSessionPhase::Ended => (cx.theme().muted_foreground, "已结束"),
        };
        let working_directory = self
            .context
            .as_ref()
            .map(|context| context.working_directory.display().to_string())
            .unwrap_or_else(|| "—".to_string());
        v_flex()
            .id("agent-status")
            .w_full()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar.opacity(0.86))
            .p_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(cx.theme().primary.opacity(0.14))
                            .child(
                                Icon::new(IconName::Sparkles)
                                    .size_4()
                                    .text_color(cx.theme().primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_semibold().child("未配置 Agent"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("模型 — · 只读 · Kite 宿主"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().size(px(7.)).rounded_full().bg(status_color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(status_color)
                                    .child(connection_label),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("工作目录"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .truncate()
                            .child(working_directory),
                    ),
            )
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            })
            .into_any_element()
    }

    fn render_context_section(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(context) = self.context.as_ref() else {
            return div().into_any_element();
        };
        let mut chips = Vec::new();
        for path in &context.paths {
            let path_for_reveal = path.path.clone();
            let path_for_remove = path.path.clone();
            let is_primary = path.path == context.primary_path;
            let label = if is_primary {
                format!("主目标 · {}", path.display_name())
            } else {
                path.display_name()
            };
            chips.push(
                h_flex()
                    .max_w(px(250.))
                    .h(px(28.))
                    .gap_1()
                    .pl_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if is_primary {
                        cx.theme().primary.opacity(0.38)
                    } else {
                        cx.theme().border
                    })
                    .bg(if is_primary {
                        cx.theme().primary.opacity(0.10)
                    } else {
                        cx.theme().muted.opacity(0.45)
                    })
                    .child(
                        Icon::new(if path.is_dir {
                            IconName::Folder
                        } else {
                            IconName::FileText
                        })
                        .size_3p5()
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Button::new(format!("agent-context-{}", path.path.display()))
                            .ghost()
                            .compact()
                            .label(label)
                            .tooltip(path.path.display().to_string())
                            .accessibility_label(format!("定位 {}", path.path.display()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reveal_path(path_for_reveal.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(format!("agent-context-remove-{}", path.path.display()))
                            .ghost()
                            .compact()
                            .icon(IconName::X)
                            .tooltip("从上下文移除")
                            .accessibility_label(format!("从上下文移除 {}", path.path.display()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_path(&path_for_remove, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        v_flex()
            .id("agent-context")
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().font_semibold().child("上下文"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} 个", context.paths.len())),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("agent-append-context")
                            .ghost()
                            .compact()
                            .icon(IconName::Plus)
                            .label("添加")
                            .tooltip("添加上下文路径")
                            .accessibility_label("添加上下文路径")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.append_context(window, cx)),
                            ),
                    ),
            )
            .child(h_flex().w_full().gap_1p5().flex_wrap().children(chips))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Switch::new("agent-follow-selection")
                            .checked(self.follows_selection())
                            .small()
                            .accessibility_label("跟随当前选择")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_follow_selection(window, cx)
                            })),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_xs().font_medium().child(self.mode.label()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.follows_selection() {
                                        "切换目录或选择时，先更新这份上下文；发送前仍需确认。"
                                    } else {
                                        "这份快照不会因切换目录而改变。"
                                    }),
                            ),
                    ),
            )
            .when(window.viewport_size().width < px(900.), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("窄窗口中 Agent 以覆盖式抽屉显示，分栏仍可继续浏览。"),
                )
            })
            .into_any_element()
    }

    fn render_prompt(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("agent-prompt")
            .w_full()
            .gap_2()
            .child(div().text_xs().font_semibold().child("发送准备"))
            .child(
                Input::new(&self.prompt_input)
                    .aria_label("Agent 问题或指令")
                    .cleanable(true)
                    .w_full(),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("agent-import-result")
                            .small()
                            .ghost()
                            .icon(IconName::FilePen)
                            .label("审阅结果")
                            .tooltip("审阅结构化 Agent 结果并确认写入")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.import_result(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("agent-open-settings")
                            .small()
                            .outline()
                            .icon(IconName::Settings)
                            .label("Agent 设置")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("agent-send")
                            .small()
                            .primary()
                            .label("发送")
                            .disabled(true)
                            .tooltip("发送和流式输出由下一阶段接入"),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("当前阶段只准备上下文和提示，不会启动外部 Agent。"),
            )
            .into_any_element()
    }

    fn render_empty(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(
                div()
                    .size(px(52.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(cx.theme().primary.opacity(0.10))
                    .child(
                        Icon::new(IconName::Sparkles)
                            .size_6()
                            .text_color(cx.theme().primary),
                    ),
            )
            .child(div().text_sm().font_semibold().child("从文件或文件夹开始"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .text_center()
                    .child(
                        "在任意项目上右键并选择“使用 Agent”。Kite 会先展示上下文，不会自动发送。",
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("agent-empty-review")
                            .small()
                            .outline()
                            .icon(IconName::FilePen)
                            .label("审阅已有结果")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.import_result(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("agent-empty-settings")
                            .small()
                            .outline()
                            .icon(IconName::Settings)
                            .label("打开 Agent 设置")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.context.is_none() {
            self.render_empty(cx)
        } else {
            let status = self.render_status(cx);
            let context = self.render_context_section(window, cx);
            let prompt = self.render_prompt(cx);
            let phase = self.phase;
            v_flex()
                .id("agent-panel-content")
                .w_full()
                .gap_3()
                .child(status)
                .child(context)
                .when(phase == AgentSessionPhase::Unconfigured, |this| {
                    this.child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().primary.opacity(0.06))
                            .p_3()
                            .child(
                                Icon::new(IconName::Info)
                                    .size_4()
                                    .text_color(cx.theme().primary),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(div().text_sm().font_medium().child("尚未配置 Agent"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("上下文和提示已经准备好。打开设置接入 Agent 后，发送流程会在下一阶段启用。"),
                                    ),
                            ),
                    )
                })
                .child(prompt)
                .into_any_element()
        };

        v_flex()
            .id("agent-panel")
            .size_full()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(content),
            )
            .into_any_element()
    }
}
