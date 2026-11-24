use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use gpui_kit::assets::IconName;
use gpui_kit::base::SelectableText;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::agent_review::{
    self, AgentAuditOutcome, AgentFileChange, AgentPermissionDecision, AgentPermissionRequest,
    AgentReview, AgentReviewError, AgentReviewErrorKind, AgentTransport,
};
use kite_core::fs::{EntryKind, FileEntry, open_file};

use crate::app::KiteApp;
use crate::preview_window::PreviewWindow;

#[derive(Clone)]
struct PendingOverwrite {
    text: String,
    change_index: Option<usize>,
    target: PathBuf,
}

pub(crate) struct AgentReviewWindow {
    review: AgentReview,
    task_id: String,
    main_app: Option<WeakEntity<KiteApp>>,
    permission_sender: Option<Sender<String>>,
    applied_changes: HashSet<usize>,
    busy: bool,
    notice: Option<SharedString>,
    error: Option<SharedString>,
    pending_overwrite: Option<PendingOverwrite>,
    allow_close: bool,
}

impl AgentReviewWindow {
    pub(crate) fn new(
        review: AgentReview,
        task_id: String,
        main_app: Option<WeakEntity<KiteApp>>,
        permission_sender: Option<Sender<String>>,
    ) -> Self {
        let this = Self {
            review,
            task_id,
            main_app,
            permission_sender,
            applied_changes: HashSet::new(),
            busy: false,
            notice: None,
            error: None,
            pending_overwrite: None,
            allow_close: false,
        };
        this.record_audit(false, AgentAuditOutcome::Prepared);
        this
    }

    fn record_audit(&self, user_confirmed_write: bool, outcome: AgentAuditOutcome) {
        let event = agent_review::build_audit_event(&self.review, user_confirmed_write, outcome);
        let _ = agent_review::append_audit_event(&event, None);
    }

    fn copy_text(&mut self, text: String, label: &'static str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some(format!("已复制{label}").into());
        self.error = None;
        cx.notify();
    }

    fn reveal_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(main_app) = self.main_app.as_ref().and_then(WeakEntity::upgrade) else {
            self.error = Some("Agent 栏已关闭，无法在分栏中定位文件".into());
            cx.notify();
            return;
        };
        main_app.update(cx, |app, cx| {
            app.reveal_agent_path(path, window, cx);
        });
        self.error = None;
    }

    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { open_file(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.error = None;
                        this.notice = Some("已交给默认应用打开".into());
                    }
                    Err(error) => {
                        this.notice = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn insert_answer_into_editor(&mut self, cx: &mut Context<Self>) {
        let directory = match agent_review::default_draft_directory() {
            Ok(directory) => directory,
            Err(error) => {
                self.error = Some(error.redacted_message().into());
                cx.notify();
                return;
            }
        };
        let path = match agent_review::materialize_text_draft(
            &self.review.answer,
            &directory,
            "agent-result.md",
        ) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error.redacted_message().into());
                cx.notify();
                return;
            }
        };
        let metadata = std::fs::metadata(&path).ok();
        let entry = FileEntry {
            path: path.clone(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Agent 结果".into()),
            kind: EntryKind::File,
            size: metadata.as_ref().map_or(0, std::fs::Metadata::len),
            modified: metadata.as_ref().and_then(|value| value.modified().ok()),
            extension: path
                .extension()
                .map(|value| value.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
            git_status: None,
            svn_status: None,
        };
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Kite 编辑器 — Agent 结果".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(size(px(980.), px(760.)), cx)),
            window_min_size: Some(size(px(640.), px(480.))),
            ..Default::default()
        };
        match gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| PreviewWindow::new(entry, window, cx))
        }) {
            Ok(_) => {
                self.record_audit(false, AgentAuditOutcome::SavedAs);
                self.error = None;
                self.notice = Some("已在编辑器中打开 Agent 回答".into());
            }
            Err(error) => {
                self.error = Some(format!("无法打开编辑器：{error}").into());
            }
        }
        cx.notify();
    }

    fn choose_save_directory(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change_index: Option<usize>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择 Agent 结果保存位置".into()),
        });
        self.pending_overwrite = None;
        self.error = None;
        self.notice = Some("请选择保存文件夹…".into());
        let text = change_index
            .and_then(|index| self.review.file_changes.get(index))
            .map_or_else(
                || self.review.answer.clone(),
                |change| change.proposed_text.clone(),
            );
        let suggested = change_index
            .and_then(|index| self.review.file_changes.get(index))
            .and_then(|change| Path::new(&change.relative_path).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent-result.md".to_string());
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.notice = None;
                        this.error = Some(format!("无法打开保存位置选择器：{error}").into());
                        cx.notify();
                    });
                    return;
                }
                Err(_) => return,
            };
            let Some(directory) = selected else {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.notice = None;
                    cx.notify();
                });
                return;
            };
            let target = directory.join(suggested);
            let change = change_index.and_then(|index| {
                this.read_with(cx, |this, _| this.review.file_changes.get(index).cloned())
                    .ok()
                    .flatten()
            });
            let result = match change_index {
                Some(_) => {
                    change.map(|change| agent_review::save_file_change_as(&change, &target, false))
                }
                None => Some(agent_review::save_answer_as(&text, &target, false)),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Some(Ok(outcome)) => {
                        this.record_audit(true, AgentAuditOutcome::SavedAs);
                        this.error = None;
                        this.notice = Some(outcome.message.into());
                        this.pending_overwrite = None;
                    }
                    Some(Err(error))
                        if error.kind() == AgentReviewErrorKind::ConfirmationRequired =>
                    {
                        this.pending_overwrite = Some(PendingOverwrite {
                            text,
                            change_index,
                            target,
                        });
                        this.error = None;
                        this.notice = Some("目标文件已存在，需再次确认覆盖".into());
                    }
                    Some(Err(error)) => {
                        this.notice = None;
                        this.error = Some(error.redacted_message().into());
                    }
                    None => {
                        this.notice = None;
                        this.error = Some("找不到要保存的 Agent 文件修改".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_overwrite(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_overwrite.clone() else {
            return;
        };
        let result = match pending.change_index {
            Some(index) => self
                .review
                .file_changes
                .get(index)
                .ok_or_else(|| AgentReviewError::InvalidInput("找不到文件修改".into()))
                .and_then(|change| {
                    agent_review::save_file_change_as(change, &pending.target, true)
                }),
            None => agent_review::save_answer_as(&pending.text, &pending.target, true),
        };
        match result {
            Ok(outcome) => {
                self.record_audit(true, AgentAuditOutcome::SavedAs);
                self.pending_overwrite = None;
                self.error = None;
                self.notice = Some(outcome.message.into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.redacted_message().into());
            }
        }
        cx.notify();
    }

    fn apply_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.review.file_changes.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.notice = Some("正在应用 Agent 修改…".into());
        cx.notify();
        let changes = self.review.file_changes.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { agent_review::apply_review_changes(&changes, true) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(outcomes) => {
                        this.applied_changes
                            .extend(0..this.review.file_changes.len());
                        this.record_audit(true, AgentAuditOutcome::Applied);
                        this.error = None;
                        this.notice = Some(
                            format!("已应用 {} 个文件修改；文件列表将刷新", outcomes.len()).into(),
                        );
                        if let Some(main_app) = this.main_app.as_ref().and_then(WeakEntity::upgrade)
                        {
                            main_app.update(cx, |app, cx| {
                                app.refresh_agent_review_changes(window, cx);
                            });
                        }
                    }
                    Err(error) => {
                        this.record_audit(true, AgentAuditOutcome::Failed);
                        this.notice = None;
                        this.error = Some(error.redacted_message().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reconfirm_change(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(change) = self.review.file_changes.get(index) else {
            return;
        };
        match agent_review::reconfirm_file_change(change) {
            Ok(updated) => {
                self.review.file_changes[index] = updated;
                self.applied_changes.remove(&index);
                self.error = None;
                self.notice = Some("已按最新文件内容重新生成 diff，请再次确认".into());
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.redacted_message().into());
            }
        }
        cx.notify();
    }

    fn install_close_guard(&self, window: &mut Window, cx: &mut Context<Self>) {
        let weak_entity = cx.weak_entity();
        window.on_window_should_close(cx, move |window, app| {
            let Some(entity) = weak_entity.upgrade() else {
                return true;
            };
            let (has_pending, allow, busy) = {
                let state = entity.read(app);
                (
                    state.review.file_changes.len() > state.applied_changes.len(),
                    state.allow_close,
                    state.busy,
                )
            };
            if busy {
                return false;
            }
            if allow || !has_pending {
                return true;
            }
            if window.has_active_dialog(app) {
                return false;
            }
            let dialog_entity = entity.downgrade();
            entity.update(app, |this, cx| {
                this.allow_close = false;
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let entity = dialog_entity.clone();
                    alert
                        .title("仍有未应用的 Agent 修改")
                        .description("关闭窗口不会写入文件。你可以保留审阅，或放弃并关闭。")
                        .confirm()
                        .ok_text("放弃并关闭")
                        .cancel_text("继续审阅")
                        .on_ok(move |_, window, app| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, _| {
                                    this.allow_close = true;
                                    this.record_audit(false, AgentAuditOutcome::Discarded);
                                });
                            }
                            window.remove_window();
                            true
                        })
                });
            });
            false
        });
    }

    fn respond_permission(
        &mut self,
        request: AgentPermissionRequest,
        decision: AgentPermissionDecision,
        cx: &mut Context<Self>,
    ) {
        match agent_review::respond_to_permission(
            &request,
            decision,
            &self.task_id,
            self.review.transport,
        ) {
            Ok(response) => {
                let delivered = self
                    .permission_sender
                    .as_ref()
                    .is_some_and(|sender| sender.send(response.response_json.clone()).is_ok());
                let outcome = if response.allowed {
                    AgentAuditOutcome::PermissionAllowed
                } else {
                    AgentAuditOutcome::PermissionDenied
                };
                self.record_audit(false, outcome);
                self.error = None;
                self.notice = Some(if delivered {
                    format!("{}，已返回给 Agent", response.message).into()
                } else {
                    format!("{}；结构化回执已生成，但当前未连接会话", response.message).into()
                });
            }
            Err(error) => {
                self.notice = None;
                self.error = Some(error.redacted_message().into());
            }
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.review.model.as_deref().unwrap_or("Agent 默认模型");
        let permission = match self.review.transport {
            AgentTransport::Acp => "写入需确认",
            AgentTransport::Cli => "权限由 Agent 管理",
        };
        v_flex()
            .w_full()
            .gap_2()
            .p_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .size(px(38.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .bg(cx.theme().primary.opacity(0.14))
                            .child(
                                Icon::new(IconName::Sparkles)
                                    .size_5()
                                    .text_color(cx.theme().primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .child(format!("{} 的结果", self.review.agent)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{model} · {permission}")),
                            ),
                    )
                    .child(
                        Button::new("agent-review-copy-answer")
                            .outline()
                            .compact()
                            .label("复制回答")
                            .disabled(self.review.answer.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.copy_text(this.review.answer.clone(), " Agent 回答", cx);
                            })),
                    )
                    .child(
                        Button::new("agent-review-insert-editor")
                            .outline()
                            .compact()
                            .label("插入编辑器")
                            .disabled(self.review.answer.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.insert_answer_into_editor(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "工作目录：{} · 上下文 {} 项",
                        self.review.working_directory.display(),
                        self.review.context_paths.len() + self.review.references.len()
                    )),
            )
            .into_any_element()
    }

    fn render_warning(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        (self.review.transport == AgentTransport::Cli).then(|| {
            h_flex()
                .w_full()
                .gap_2()
                .items_start()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().warning.opacity(0.35))
                .bg(cx.theme().warning.opacity(0.08))
                .child(
                    Icon::new(IconName::CircleAlert)
                        .size_4()
                        .text_color(cx.theme().warning),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .child(self.review.boundary_notice.clone()),
                )
                .into_any_element()
        })
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(error) = self.error.as_ref() {
            return Some(
                div()
                    .m_4()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().danger.opacity(0.35))
                    .bg(cx.theme().danger.opacity(0.08))
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        self.notice.as_ref().map(|notice| {
            div()
                .m_4()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().title_bar.opacity(0.7))
                .text_sm()
                .child(notice.clone())
                .into_any_element()
        })
    }

    fn render_answer(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(section_title("回答", Some(IconName::MessageSquare), cx))
            .child(
                div()
                    .w_full()
                    .max_h(px(300.))
                    .overflow_y_scrollbar()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().title_bar.opacity(0.5))
                    .p_3()
                    .text_sm()
                    .child(SelectableText::new(
                        "agent-review-answer",
                        if self.review.answer.is_empty() {
                            "Agent 没有返回文字回答".to_string()
                        } else {
                            self.review.answer.clone()
                        },
                    )),
            )
            .into_any_element()
    }

    fn render_references(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::new();
        for (index, reference) in self.review.references.iter().enumerate() {
            let path = reference.path.clone();
            let open_path = reference.path.clone();
            let missing = !reference.exists;
            rows.push(
                h_flex()
                    .id(("agent-review-reference", index))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        Icon::new(if reference.is_directory {
                            IconName::Folder
                        } else {
                            IconName::File
                        })
                        .size_4()
                        .text_color(if missing {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().primary
                        }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_medium().child(reference.label.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(if missing {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(if missing {
                                        format!("路径已失效：{}", reference.path.display())
                                    } else {
                                        reference.path.display().to_string()
                                    }),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("agent-review-locate-{index}")))
                            .ghost()
                            .compact()
                            .label("定位")
                            .disabled(missing)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.reveal_path(path.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("agent-review-open-{index}")))
                            .ghost()
                            .compact()
                            .label("打开")
                            .disabled(missing)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.open_path(open_path.clone(), cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .gap_2()
            .child(section_title("引用文件", Some(IconName::Link), cx))
            .children(rows)
            .into_any_element()
    }

    fn render_commands(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::new();
        for (index, command) in self.review.commands.iter().enumerate() {
            let command_text = command.command.clone();
            rows.push(
                v_flex()
                    .id(("agent-review-command", index))
                    .w_full()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().title_bar.opacity(0.45))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family("monospace")
                                    .text_xs()
                                    .child(SelectableText::new(
                                        format!("agent-review-command-text-{index}"),
                                        command.command.clone(),
                                    )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-review-copy-command-{index}"
                                )))
                                .ghost()
                                .compact()
                                .label("复制")
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.copy_text(command_text.clone(), "命令", cx);
                                    },
                                )),
                            ),
                    )
                    .when(!command.rationale.is_empty(), |card| {
                        card.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(command.rationale.clone()),
                        )
                    })
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .gap_2()
            .child(section_title(
                "命令建议（仅文本，不会自动执行）",
                Some(IconName::Terminal),
                cx,
            ))
            .children(rows)
            .into_any_element()
    }

    fn render_permissions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::new();
        for (index, request) in self.review.permission_requests.iter().enumerate() {
            let allow_once = request.clone();
            let allow_task = request.clone();
            let deny = request.clone();
            rows.push(
                v_flex()
                    .id(("agent-review-permission", index))
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.35))
                    .bg(cx.theme().warning.opacity(0.06))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(IconName::ShieldCheck)
                                    .size_4()
                                    .text_color(cx.theme().warning),
                            )
                            .child(div().text_sm().font_semibold().child(request.title.clone())),
                    )
                    .when(!request.detail.is_empty(), |card| {
                        card.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(request.detail.clone()),
                        )
                    })
                    .when(!request.target_paths.is_empty(), |card| {
                        card.child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    request
                                        .target_paths
                                        .iter()
                                        .map(|path| path.display().to_string())
                                        .collect::<Vec<_>>()
                                        .join(" · "),
                                ),
                        )
                    })
                    .when(self.review.transport == AgentTransport::Cli, |card| {
                        card.child(div().text_xs().text_color(cx.theme().warning).child(
                            "普通 CLI 无法接收 Kite 的交互式权限回复；请在 CLI 自身界面处理。",
                        ))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-review-allow-once-{index}"
                                )))
                                .outline()
                                .compact()
                                .label("仅本次允许")
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.respond_permission(
                                            allow_once.clone(),
                                            AgentPermissionDecision::AllowOnce,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-review-allow-task-{index}"
                                )))
                                .outline()
                                .compact()
                                .label("本任务允许")
                                .disabled(!request.can_allow_task)
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.respond_permission(
                                            allow_task.clone(),
                                            AgentPermissionDecision::AllowTask,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-review-deny-{index}"
                                )))
                                .outline()
                                .compact()
                                .label("拒绝")
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.respond_permission(
                                            deny.clone(),
                                            AgentPermissionDecision::Deny,
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    )
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .gap_2()
            .child(section_title("权限请求", Some(IconName::ShieldCheck), cx))
            .children(rows)
            .into_any_element()
    }

    fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::new();
        for (index, change) in self.review.file_changes.iter().enumerate() {
            rows.push(self.render_change(index, change, cx));
        }
        v_flex()
            .w_full()
            .gap_2()
            .child(section_title("文件修改", Some(IconName::FilePen), cx))
            .children(rows)
            .into_any_element()
    }

    fn render_change(
        &self,
        index: usize,
        change: &AgentFileChange,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let applied = self.applied_changes.contains(&index);
        let action_id = SharedString::from(format!("agent-review-save-change-{index}"));
        let reconfirm_id = SharedString::from(format!("agent-review-reconfirm-{index}"));
        let visible_diff_truncated =
            change.diff_truncated || change.unified_diff.lines().count() > 1_500;
        let mut diff_rows = Vec::new();
        for (line_index, line) in change.unified_diff.lines().take(1_500).enumerate() {
            let (color, background) = if line.starts_with('+') && !line.starts_with("+++") {
                (cx.theme().success, Some(cx.theme().success.opacity(0.09)))
            } else if line.starts_with('-') && !line.starts_with("---") {
                (cx.theme().danger, Some(cx.theme().danger.opacity(0.09)))
            } else if line.starts_with("@@") {
                (cx.theme().primary, Some(cx.theme().primary.opacity(0.08)))
            } else {
                (cx.theme().foreground, None)
            };
            diff_rows.push(
                div()
                    .id(("agent-review-diff-line", index * 10_000 + line_index))
                    .w_full()
                    .px_2()
                    .when_some(background, |row, background| row.bg(background))
                    .font_family("monospace")
                    .text_xs()
                    .text_color(color)
                    .child(SelectableText::new(
                        SharedString::from(format!("agent-review-diff-{index}-{line_index}")),
                        line.to_string(),
                    )),
            );
        }
        v_flex()
            .id(("agent-review-change", index))
            .w_full()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(if change.requires_reconfirmation {
                cx.theme().danger.opacity(0.45)
            } else {
                cx.theme().border
            })
            .bg(cx.theme().title_bar.opacity(0.55))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::FileCode).size_4().text_color(cx.theme().primary))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_semibold().truncate().child(change.relative_path.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "写入：{} · +{} −{} · {}",
                                        change.target_path.display(),
                                        change.added_lines,
                                        change.removed_lines,
                                        if change.overwrite_risk { "覆盖已有文件" } else { "创建新文件" }
                                    )),
                            ),
                    )
                    .when(applied, |row| {
                        row.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(Icon::new(IconName::Check).size_4().text_color(cx.theme().success))
                                .child(div().text_xs().text_color(cx.theme().success).child("已应用")),
                        )
                    }),
            )
            .when(change.requires_reconfirmation, |card| {
                card.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .p_2()
                        .rounded_md()
                        .bg(cx.theme().danger.opacity(0.08))
                        .child(Icon::new(IconName::TriangleAlert).size_4().text_color(cx.theme().danger))
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child("文件在审核后发生变化。Kite 不会覆盖；请重新生成 diff 并再次确认。"),
                        )
                        .child(
                            Button::new(reconfirm_id)
                                .outline()
                                .compact()
                                .label("重新审核")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.reconfirm_change(index, cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .max_h(px(360.))
                    .overflow_y_scrollbar()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .children(diff_rows)
                    .when(visible_diff_truncated, |diff| {
                        diff.child(
                            div()
                                .p_2()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child("diff 已截断；应用前请确认受影响文件。"),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("agent-review-refresh-{index}")))
                            .ghost()
                            .compact()
                            .label("刷新审核")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.reconfirm_change(index, cx);
                            })),
                    )
                    .child(
                        Button::new(action_id)
                            .ghost()
                            .compact()
                            .label("另存为…")
                            .disabled(applied || change.requires_reconfirmation || self.busy)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.choose_save_directory(window, cx, Some(index));
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .when_some(self.pending_overwrite.as_ref(), |footer, pending| {
                footer.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .p_2()
                        .rounded_md()
                        .bg(cx.theme().warning.opacity(0.08))
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .child(format!("目标已存在：{}", pending.target.display())),
                        )
                        .child(
                            Button::new("agent-review-confirm-overwrite")
                                .outline()
                                .compact()
                                .label("确认覆盖")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.confirm_overwrite(cx);
                                })),
                        ),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("任何写入都必须先查看上方 diff。Agent 建议的命令不会自动执行。"),
                    )
                    .child(
                        Button::new("agent-review-save-answer")
                            .ghost()
                            .compact()
                            .label("回答另存为…")
                            .disabled(self.review.answer.is_empty() || self.busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.choose_save_directory(window, cx, None);
                            })),
                    )
                    .child(
                        Button::new("agent-review-apply-all")
                            .primary()
                            .compact()
                            .label(if self.busy {
                                "正在应用…"
                            } else {
                                "应用全部修改"
                            })
                            .disabled(self.busy || self.review.file_changes.is_empty())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.apply_changes(window, cx);
                            })),
                    )
                    .child(
                        Button::new("agent-review-discard")
                            .ghost()
                            .compact()
                            .label("放弃")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.allow_close = true;
                                this.record_audit(false, AgentAuditOutcome::Discarded);
                                window.remove_window();
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for AgentReviewWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let warning = self.render_warning(cx);
        let notice = self.render_notice(cx);
        v_flex()
            .id("agent-review-window")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_header(cx))
            .when_some(warning, |view, warning| {
                view.child(div().px_4().pt_3().child(warning))
            })
            .when_some(notice, |view, notice| view.child(notice))
            .child(
                div().flex_1().min_h_0().overflow_y_scrollbar().p_4().child(
                    v_flex()
                        .w_full()
                        .gap_4()
                        .child(self.render_answer(cx))
                        .when(!self.review.references.is_empty(), |body| {
                            body.child(self.render_references(cx))
                        })
                        .when(!self.review.file_changes.is_empty(), |body| {
                            body.child(self.render_changes(cx))
                        })
                        .when(!self.review.permission_requests.is_empty(), |body| {
                            body.child(self.render_permissions(cx))
                        })
                        .when(!self.review.commands.is_empty(), |body| {
                            body.child(self.render_commands(cx))
                        }),
                ),
            )
            .child(self.render_footer(cx))
    }
}

pub(crate) fn open_agent_review_window(
    review: AgentReview,
    task_id: Option<String>,
    main_app: Option<WeakEntity<KiteApp>>,
    permission_sender: Option<Sender<String>>,
    cx: &mut App,
) -> Result<AnyWindowHandle, String> {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(format!("Kite Agent 结果 — {}", review.agent).into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(980.), px(760.)), cx)),
        window_min_size: Some(size(px(720.), px(520.))),
        ..Default::default()
    };
    let task_id = task_id.unwrap_or_else(|| review.id.clone());
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| {
            let review_window =
                AgentReviewWindow::new(review, task_id, main_app, permission_sender);
            review_window.install_close_guard(window, cx);
            review_window
        })
    })
    .map(|(handle, _)| handle)
    .map_err(|error| format!("无法打开 Agent 结果窗口：{error}"))
}

fn section_title(label: &'static str, icon: Option<IconName>, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(div().text_sm().font_semibold().child(label))
        .when_some(icon, |row, icon| {
            row.child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(cx.theme().muted_foreground),
            )
        })
        .into_any_element()
}
