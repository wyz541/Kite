use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::KiteApp;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::agent::{
    AgentContext, AgentContextMode, AgentConversation, AgentLaunchRequest, AgentMessage,
    AgentMessageRole, AgentPath, AgentPermissionDecision, AgentProfile, AgentProfileSnapshot,
    AgentProfileStore, AgentRuntimeSnapshot, AgentRuntimeState, AgentSessionPhase,
    AgentWorkingDirectoryStatus, WorkingDirectoryPolicy, load_agent_sessions,
    new_agent_conversation_id, save_agent_conversation,
};
use kite_core::agent_permission_ui::{permission_action_enabled, permission_action_label};
use kite_core::core_api::{CoreClient, CoreRequest};

pub(crate) struct AgentPanel {
    context: Option<AgentContext>,
    mode: AgentContextMode,
    phase: AgentSessionPhase,
    profile: Option<AgentProfile>,
    core_client: CoreClient,
    session_id: Option<String>,
    snapshot: AgentRuntimeSnapshot,
    prompt_input: Entity<TextareaState>,
    output_view: Entity<TextViewState>,
    main_app: Option<WeakEntity<KiteApp>>,
    notice: Option<SharedString>,
    conversation_id: String,
    history: Vec<AgentMessage>,
    turn_id: u64,
    sent_prompt: String,
    auto_follow_output: bool,
    output_scroll: ScrollHandle,
    saved_terminal_snapshot: bool,
    working_directory_selection: Option<PathBuf>,
    window_size: (u32, u32),
    poll_task: Task<()>,
    _prompt_subscription: Subscription,
}

impl AgentPanel {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        agent_profiles: AgentProfileSnapshot,
    ) -> Self {
        let prompt_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("向 Agent 提问。发送前可检查工作目录、上下文和权限。")
                .auto_grow(2, 6)
                .submit_on_enter(true)
                .clean_on_escape()
        });
        let output_view = cx.new(|cx| TextViewState::markdown("", cx));
        let prompt_subscription = cx.subscribe_in(
            &prompt_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.send_prompt(window, cx);
                }
            },
        );
        let profile = agent_profiles.default_profile().cloned();
        let phase = if profile.is_some() {
            AgentSessionPhase::Ended
        } else {
            AgentSessionPhase::Unconfigured
        };
        let mut panel = Self {
            context: None,
            mode: AgentContextMode::Fixed,
            phase,
            profile,
            core_client: CoreClient::new(),
            session_id: None,
            snapshot: AgentRuntimeSnapshot::default(),
            prompt_input,
            output_view,
            main_app: None,
            notice: None,
            conversation_id: new_agent_conversation_id(),
            history: Vec::new(),
            turn_id: 0,
            sent_prompt: String::new(),
            auto_follow_output: true,
            output_scroll: ScrollHandle::new(),
            saved_terminal_snapshot: false,
            working_directory_selection: None,
            window_size: (960, 760),
            poll_task: Task::ready(()),
            _prompt_subscription: prompt_subscription,
        };
        panel.restore_session(window, cx);
        panel
    }

    fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore = load_agent_sessions();
        let Some(profile_id) = self.profile.as_ref().map(|profile| profile.id.clone()) else {
            return;
        };
        let Some(conversation) = restore
            .store
            .active_conversation_id
            .as_ref()
            .and_then(|id| {
                restore
                    .store
                    .conversations
                    .iter()
                    .find(|item| &item.id == id)
            })
            .filter(|conversation| conversation.profile_id == profile_id)
            .or_else(|| {
                restore
                    .store
                    .conversations
                    .iter()
                    .find(|conversation| conversation.profile_id == profile_id)
            })
        else {
            return;
        };
        self.window_size = (conversation.window_width, conversation.window_height);
        self.history = conversation.messages.clone();
        self.mode = if conversation.follow_selection {
            AgentContextMode::FollowSelection
        } else {
            AgentContextMode::Fixed
        };
        self.conversation_id = conversation.id.clone();
        self.prompt_input.update(cx, |input, cx| {
            input.set_value(conversation.draft.clone(), window, cx);
        });
        self.set_output(
            &conversation
                .messages
                .iter()
                .rev()
                .find(|message| message.role == AgentMessageRole::Assistant)
                .map(|message| message.content.clone())
                .unwrap_or_default(),
            cx,
        );
        let missing_paths = !restore.missing_paths.is_empty();
        if conversation.context_paths.is_empty() {
            self.context = None;
            self.phase = if missing_paths {
                AgentSessionPhase::Error
            } else {
                AgentSessionPhase::Ended
            };
            self.notice = Some(
                if missing_paths {
                    "恢复的 Agent 会话包含失效路径；可移除后重新选择"
                } else {
                    "已恢复上次 Agent 会话；请重新选择上下文"
                }
                .into(),
            );
            return;
        }
        let paths = conversation
            .context_paths
            .iter()
            .map(|path| AgentPath::new(path.clone(), Path::new(path).is_dir()))
            .collect::<Vec<_>>();
        let primary_path = paths[0].path.clone();
        let Some(profile) = self.profile.as_ref() else {
            return;
        };
        match AgentContext::new_for_profile(&primary_path, paths, profile, None) {
            Ok(context) => {
                let working_directory_message = context.working_directory_message.clone();
                let working_directory_ready = context.has_resolved_working_directory();
                self.context = Some(context);
                if missing_paths {
                    self.phase = AgentSessionPhase::Error;
                    self.notice = Some("恢复的 Agent 会话包含失效路径；可移除后重新选择".into());
                } else if !working_directory_ready {
                    self.phase = AgentSessionPhase::Error;
                    self.notice = working_directory_message.map(Into::into);
                } else {
                    self.phase = AgentSessionPhase::Ended;
                    self.notice = Some("已恢复上次 Agent 会话".into());
                }
            }
            Err(error) => {
                self.phase = AgentSessionPhase::Error;
                self.notice = Some(error.into());
            }
        }
    }

    pub(crate) fn set_main_app(&mut self, main_app: WeakEntity<KiteApp>) {
        self.main_app = Some(main_app);
    }

    pub(crate) fn apply_agent_profiles(
        &mut self,
        agent_profiles: AgentProfileSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.profile = agent_profiles.default_profile().cloned();
        let selected_working_directory = self.working_directory_selection.clone();
        if let (Some(profile), Some(context)) = (self.profile.as_ref(), self.context.as_mut()) {
            context.apply_profile_policy(profile, selected_working_directory.as_deref());
        }
        if !self.profile.as_ref().is_some_and(|profile| {
            matches!(&profile.working_directory, WorkingDirectoryPolicy::Ask)
        }) {
            self.working_directory_selection = None;
        }
        if !matches!(
            self.phase,
            AgentSessionPhase::Loading | AgentSessionPhase::Running
        ) {
            self.phase = if self.profile.is_none() {
                AgentSessionPhase::Unconfigured
            } else if self
                .context
                .as_ref()
                .is_some_and(|context| !context.has_resolved_working_directory())
            {
                AgentSessionPhase::Error
            } else {
                AgentSessionPhase::Ended
            };
        }
        cx.notify();
    }

    pub(crate) fn window_size(&self) -> (u32, u32) {
        self.window_size
    }

    pub(crate) fn draft_value(&self, cx: &App) -> String {
        self.prompt_input.read(cx).value().to_string()
    }

    pub(crate) fn persist_window_state(&mut self, width: u32, height: u32, draft: String) {
        self.window_size = (width.clamp(680, 3_840), height.clamp(560, 2_160));
        let store = load_agent_sessions().store;
        let Some(mut conversation) = store
            .conversations
            .into_iter()
            .find(|conversation| conversation.id == self.conversation_id)
        else {
            self.persist_conversation(draft);
            return;
        };
        conversation.window_width = self.window_size.0;
        conversation.window_height = self.window_size.1;
        conversation.draft = draft;
        conversation.updated_unix_ms = now_unix_ms();
        let _ = save_agent_conversation(conversation);
    }

    pub(crate) fn present(
        &mut self,
        request: AgentLaunchRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_runtime();
        self.profile = AgentProfileStore::new()
            .snapshot()
            .default_profile()
            .cloned();
        self.phase = if self.profile.is_some() {
            AgentSessionPhase::Ended
        } else {
            AgentSessionPhase::Unconfigured
        };
        self.mode = AgentContextMode::Fixed;
        self.working_directory_selection = None;
        self.context = Some(request.context);
        self.refresh_context_working_directory();
        self.sent_prompt.clear();
        self.saved_terminal_snapshot = false;
        self.notice = None;
        if self
            .context
            .as_ref()
            .is_some_and(|context| !context.has_resolved_working_directory())
        {
            self.phase = AgentSessionPhase::Error;
            self.notice = self
                .context
                .as_ref()
                .and_then(|context| context.working_directory_message.clone())
                .or_else(|| Some("Agent 工作目录不可用".to_string()))
                .map(Into::into);
        }
        self.snapshot = AgentRuntimeSnapshot::default();
        self.conversation_id = new_agent_conversation_id();
        self.history.clear();
        self.turn_id = 0;
        self.prompt_input.update(cx, |input, cx| {
            input.set_value(request.prompt, window, cx);
        });
        self.set_output("", cx);
        cx.notify();
    }

    pub(crate) fn follows_selection(&self) -> bool {
        self.mode == AgentContextMode::FollowSelection
    }

    pub(crate) fn replace_followed_selection(
        &mut self,
        paths: Vec<AgentPath>,
        cx: &mut Context<Self>,
    ) {
        if !self.follows_selection() || paths.is_empty() {
            return;
        }
        let primary = paths[0].clone();
        let Some(profile) = self.profile.clone() else {
            self.phase = AgentSessionPhase::Unconfigured;
            self.notice = Some("请先在设置中配置 Agent".into());
            cx.notify();
            return;
        };
        self.working_directory_selection = None;
        match AgentContext::new_for_profile(&primary.path, paths, &profile, None) {
            Ok(context) => {
                let working_directory_message = context.working_directory_message.clone();
                let working_directory_ready = context.has_resolved_working_directory();
                self.context = Some(context);
                self.notice = Some(
                    if working_directory_ready {
                        "Agent 上下文已跟随当前选择".into()
                    } else {
                        working_directory_message
                            .unwrap_or_else(|| "Agent 工作目录不可用".to_string())
                    }
                    .into(),
                );
            }
            Err(error) => {
                self.context = None;
                self.phase = AgentSessionPhase::Error;
                self.notice = Some(error.to_string().into());
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
            let Some(profile) = self.profile.clone() else {
                self.phase = AgentSessionPhase::Unconfigured;
                self.notice = Some("请先在设置中配置 Agent".into());
                cx.notify();
                return;
            };
            self.working_directory_selection = None;
            match AgentContext::new_for_profile(&primary.path, paths, &profile, None) {
                Ok(context) => {
                    let working_directory_message = context.working_directory_message.clone();
                    let working_directory_ready = context.has_resolved_working_directory();
                    self.context = Some(context);
                    self.notice = Some(
                        if working_directory_ready {
                            "已添加上下文路径".into()
                        } else {
                            working_directory_message
                                .unwrap_or_else(|| "Agent 工作目录不可用".to_string())
                        }
                        .into(),
                    );
                }
                Err(error) => {
                    self.phase = AgentSessionPhase::Error;
                    self.notice = Some(error.to_string().into());
                }
            }
        }
        cx.notify();
    }

    fn refresh_context_working_directory(&mut self) {
        let Some(profile) = self.profile.clone() else {
            return;
        };
        let selected_working_directory = self.working_directory_selection.clone();
        if let Some(context) = self.context.as_mut() {
            context.apply_profile_policy(&profile, selected_working_directory.as_deref());
        }
    }

    fn remove_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let primary_removed;
        let context_empty;
        {
            let Some(context) = self.context.as_mut() else {
                return;
            };
            primary_removed = context.primary_path == path;
            context.remove(path);
            context_empty = context.paths.is_empty();
        }
        if context_empty {
            self.context = None;
            self.phase = AgentSessionPhase::Empty;
        } else if primary_removed {
            self.working_directory_selection = None;
            self.refresh_context_working_directory();
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

    fn show_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(main_app) = self.main_app.as_ref().and_then(|entity| entity.upgrade()) {
            main_app.update(cx, |app, cx| app.show_preview_panel(cx));
        }
    }

    fn send_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.prompt_input.read(cx).value().trim().to_string();
        self.start_prompt(prompt, window, cx);
    }

    fn retry_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.sent_prompt.trim().to_string();
        if prompt.is_empty() {
            self.notice = Some("没有可重试的上一条提示".into());
            cx.notify();
            return;
        }
        self.start_prompt(prompt, window, cx);
    }

    fn can_retry_prompt(&self) -> bool {
        !self.sent_prompt.trim().is_empty()
            && matches!(
                self.phase,
                AgentSessionPhase::Ended | AgentSessionPhase::Error
            )
    }

    fn choose_working_directory(
        &mut self,
        prompt_to_send: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择 Agent 工作目录".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.phase = AgentSessionPhase::Error;
                        this.notice = Some(format!("无法选择工作目录：{error}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            let Some(path) = selected else {
                let _ = this.update(cx, |this, cx| {
                    this.notice = Some("尚未选择工作目录，Agent 未启动".into());
                    cx.notify();
                });
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.working_directory_selection = Some(path);
                this.refresh_context_working_directory();
                let ready = this
                    .context
                    .as_ref()
                    .is_some_and(AgentContext::has_resolved_working_directory);
                if ready {
                    if let Some(prompt) = prompt_to_send {
                        this.start_prompt(prompt, window, cx);
                    } else {
                        if this.phase == AgentSessionPhase::Error {
                            this.phase = AgentSessionPhase::Ended;
                        }
                        this.notice = Some("Agent 工作目录已选择".into());
                    }
                } else {
                    this.phase = AgentSessionPhase::Error;
                    this.notice = this
                        .context
                        .as_ref()
                        .and_then(|context| context.working_directory_message.clone())
                        .or_else(|| Some("Agent 工作目录不可用".to_string()))
                        .map(Into::into);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_prompt(&mut self, prompt: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session_id) = self.session_id.clone()
            && let Ok(snapshot) = self.core_client.session_snapshot(&session_id)
            && !matches!(
                snapshot.state,
                AgentRuntimeState::Completed
                    | AgentRuntimeState::Failed
                    | AgentRuntimeState::Cancelled
                    | AgentRuntimeState::Closed
            )
        {
            self.notice = Some("Agent 正在运行，请先停止或等待完成".into());
            cx.notify();
            return;
        }
        if self.session_id.is_some() {
            self.stop_runtime();
        }
        self.profile = AgentProfileStore::new()
            .snapshot()
            .default_profile()
            .cloned();
        let Some(profile) = self.profile.clone() else {
            self.phase = AgentSessionPhase::Unconfigured;
            self.notice = Some("请先在设置中配置 Agent".into());
            cx.notify();
            return;
        };
        if prompt.is_empty() {
            self.notice = Some("提示不能为空".into());
            cx.notify();
            return;
        }
        self.refresh_context_working_directory();
        let Some(context) = self.context.clone() else {
            self.notice = Some("请先选择文件或文件夹作为上下文".into());
            cx.notify();
            return;
        };
        match context.working_directory_status {
            AgentWorkingDirectoryStatus::Ready => {}
            AgentWorkingDirectoryStatus::NeedsSelection
                if matches!(&profile.working_directory, WorkingDirectoryPolicy::Ask) =>
            {
                self.choose_working_directory(Some(prompt), window, cx);
                return;
            }
            AgentWorkingDirectoryStatus::NeedsSelection | AgentWorkingDirectoryStatus::Invalid => {
                self.phase = AgentSessionPhase::Error;
                self.notice = context
                    .working_directory_message
                    .clone()
                    .or_else(|| Some("Agent 工作目录不可用".to_string()))
                    .map(Into::into);
                cx.notify();
                return;
            }
        }
        let context_paths = context
            .paths
            .iter()
            .map(|path| path.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let prompt_context_paths = context_paths.clone();
        self.turn_id = now_unix_ms();
        let request = CoreRequest::AgentSessionStart {
            profile_id: Some(profile.id),
            working_directory: Some(context.working_directory.to_string_lossy().into_owned()),
            context_paths,
            restored_messages: self.history.clone(),
        };
        match self.core_client.invoke_request(request) {
            Ok(result) => {
                let Some(session_id) = result
                    .data
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                else {
                    self.phase = AgentSessionPhase::Error;
                    self.notice = Some("Core API 未返回 Agent session id".into());
                    cx.notify();
                    return;
                };
                self.sent_prompt = prompt.clone();
                self.auto_follow_output = true;
                self.saved_terminal_snapshot = false;
                self.snapshot = self
                    .core_client
                    .session_snapshot(&session_id)
                    .unwrap_or_else(|_| AgentRuntimeSnapshot::default());
                self.session_id = Some(session_id.clone());
                self.phase = AgentSessionPhase::Loading;
                self.notice = Some("Agent 已启动".into());
                self.poll_session(session_id, prompt, prompt_context_paths, cx);
            }
            Err(error) => {
                self.phase = AgentSessionPhase::Error;
                self.notice = Some(error.to_string().into());
            }
        }
        cx.notify();
    }

    fn poll_session(
        &mut self,
        session_id: String,
        prompt: String,
        context_paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let core_client = self.core_client.clone();
        self.poll_task = cx.spawn(async move |this, cx| {
            let mut prompt_sent = false;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let snapshot = match core_client.session_snapshot(&session_id) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        let _ = this.update(cx, |this, cx| {
                            this.phase = AgentSessionPhase::Error;
                            this.notice = Some(error.to_string().into());
                            cx.notify();
                        });
                        break;
                    }
                };
                let terminal = matches!(
                    snapshot.state,
                    AgentRuntimeState::Completed
                        | AgentRuntimeState::Failed
                        | AgentRuntimeState::Cancelled
                        | AgentRuntimeState::Closed
                );
                if !terminal && !prompt_sent && snapshot.state == AgentRuntimeState::Ready {
                    match core_client.invoke_request(CoreRequest::AgentSessionPrompt {
                        session_id: session_id.clone(),
                        prompt: prompt.clone(),
                        context_paths: Some(context_paths.clone()),
                        wait_ready_ms: None,
                    }) {
                        Ok(_) => {
                            prompt_sent = true;
                            let _ = this.update(cx, |this, cx| {
                                this.notice = Some("Agent 运行中".into());
                                cx.notify();
                            });
                        }
                        Err(error) => {
                            let _ = core_client.invoke_request(CoreRequest::AgentSessionClose {
                                session_id: session_id.clone(),
                            });
                            let _ = this.update(cx, |this, cx| {
                                this.phase = AgentSessionPhase::Error;
                                this.notice = Some(error.to_string().into());
                                cx.notify();
                            });
                            break;
                        }
                    }
                }
                if terminal {
                    let _ = core_client.invoke_request(CoreRequest::AgentSessionClose {
                        session_id: session_id.clone(),
                    });
                }
                let output = snapshot.output.clone();
                let terminal_notice = match snapshot.state {
                    AgentRuntimeState::Completed => "Agent 已完成".to_string(),
                    AgentRuntimeState::Cancelled => "Agent 已取消".to_string(),
                    AgentRuntimeState::Closed => "Agent 已关闭".to_string(),
                    AgentRuntimeState::Failed => snapshot
                        .error
                        .clone()
                        .unwrap_or_else(|| "Agent 运行失败".to_string()),
                    _ => String::new(),
                };
                let _ = this.update(cx, |this, cx| {
                    this.apply_snapshot(snapshot, cx);
                    if terminal {
                        this.notice = Some(terminal_notice.into());
                        this.session_id = None;
                        if !this.saved_terminal_snapshot {
                            this.saved_terminal_snapshot = true;
                            let draft = this.prompt_input.read(cx).value().to_string();
                            this.persist_conversation(draft);
                        }
                    }
                    let _ = &output;
                    cx.notify();
                });
                if terminal {
                    break;
                }
            }
        });
    }

    fn apply_snapshot(&mut self, snapshot: AgentRuntimeSnapshot, cx: &mut Context<Self>) {
        self.phase = match snapshot.state {
            AgentRuntimeState::Starting | AgentRuntimeState::Initializing => {
                AgentSessionPhase::Loading
            }
            AgentRuntimeState::Ready
            | AgentRuntimeState::Running
            | AgentRuntimeState::WaitingPermission
            | AgentRuntimeState::Cancelling => AgentSessionPhase::Running,
            AgentRuntimeState::Completed
            | AgentRuntimeState::Cancelled
            | AgentRuntimeState::Closed => AgentSessionPhase::Ended,
            AgentRuntimeState::Failed => AgentSessionPhase::Error,
        };
        let output = snapshot.output.clone();
        self.snapshot = snapshot;
        self.set_output(&output, cx);
    }

    fn set_output(&mut self, output: &str, cx: &mut Context<Self>) {
        let output = output.to_string();
        self.output_view
            .update(cx, |view, cx| view.set_text(&output, cx));
        if self.auto_follow_output && !output.is_empty() {
            self.output_scroll.scroll_to_bottom();
        }
    }

    fn resume_auto_follow(&mut self, cx: &mut Context<Self>) {
        self.auto_follow_output = true;
        self.output_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn persist_conversation(&mut self, draft: String) {
        let Some(profile) = self.profile.as_ref() else {
            return;
        };
        let Some(context) = self.context.as_ref() else {
            return;
        };
        if !context.has_resolved_working_directory() {
            return;
        }
        let now = now_unix_ms();
        let mut messages = self.history.clone();
        let user_message_id = format!("{}:user:{}", self.conversation_id, self.turn_id);
        let assistant_message_id = format!("{}:assistant:{}", self.conversation_id, self.turn_id);
        messages
            .retain(|message| message.id != user_message_id && message.id != assistant_message_id);
        if !self.sent_prompt.trim().is_empty() {
            messages.push(AgentMessage {
                id: user_message_id,
                role: AgentMessageRole::User,
                content: self.sent_prompt.clone(),
                unix_ms: now,
                truncated: false,
            });
        }
        if !self.snapshot.output.trim().is_empty() {
            messages.push(AgentMessage {
                id: assistant_message_id,
                role: AgentMessageRole::Assistant,
                content: self.snapshot.output.clone(),
                unix_ms: now,
                truncated: self.snapshot.output_truncated,
            });
        }
        let title = self
            .sent_prompt
            .lines()
            .next()
            .unwrap_or("新的 Agent 对话")
            .chars()
            .take(48)
            .collect::<String>();
        self.history = messages.clone();
        let conversation = AgentConversation {
            id: self.conversation_id.clone(),
            title,
            profile_id: profile.id.clone(),
            model: profile.model.clone(),
            working_directory: context.working_directory.to_string_lossy().into_owned(),
            context_paths: context
                .paths
                .iter()
                .map(|path| path.path.to_string_lossy().into_owned())
                .collect(),
            follow_selection: self.follows_selection(),
            messages,
            remote_session_id: None,
            draft,
            window_width: self.window_size.0,
            window_height: self.window_size.1,
            updated_unix_ms: now,
        };
        let _ = save_agent_conversation(conversation);
    }

    fn stop_runtime(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            let _ = self
                .core_client
                .invoke_request(CoreRequest::AgentSessionClose { session_id });
        }
        self.phase = AgentSessionPhase::Ended;
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(session_id) = self.session_id.as_ref()
            && let Err(error) = self
                .core_client
                .invoke_request(CoreRequest::AgentSessionCancel {
                    session_id: session_id.clone(),
                })
        {
            self.notice = Some(error.to_string().into());
        }
        cx.notify();
    }

    fn respond_permission(
        &mut self,
        request_id: String,
        decision: AgentPermissionDecision,
        cx: &mut Context<Self>,
    ) {
        if let Some(session_id) = self.session_id.as_ref() {
            let decision = match decision {
                AgentPermissionDecision::AllowOnce => "allow_once",
                AgentPermissionDecision::AllowTask => "allow_task",
                AgentPermissionDecision::RejectOnce => "reject_once",
                AgentPermissionDecision::RejectAlways => "reject_always",
                AgentPermissionDecision::Cancel => "cancel",
            };
            if let Err(error) =
                self.core_client
                    .invoke_request(CoreRequest::AgentSessionPermission {
                        session_id: session_id.clone(),
                        request_id,
                        decision: decision.to_string(),
                    })
            {
                self.notice = Some(error.to_string().into());
            }
        }
        cx.notify();
    }

    fn render_status(&self, cx: &App) -> AnyElement {
        let (status_color, connection_label) = if self.session_id.is_some() {
            match self.snapshot.state {
                AgentRuntimeState::Starting => (cx.theme().primary, "启动中"),
                AgentRuntimeState::Initializing => (cx.theme().primary, "握手中"),
                AgentRuntimeState::Ready => (cx.theme().success, "已就绪"),
                AgentRuntimeState::Running => (cx.theme().success, "运行中"),
                AgentRuntimeState::WaitingPermission => (cx.theme().warning, "等待权限"),
                AgentRuntimeState::Cancelling => (cx.theme().warning, "取消中"),
                AgentRuntimeState::Completed => (cx.theme().success, "完成"),
                AgentRuntimeState::Failed => (cx.theme().danger, "失败"),
                AgentRuntimeState::Cancelled => (cx.theme().muted_foreground, "已取消"),
                AgentRuntimeState::Closed => (cx.theme().muted_foreground, "已关闭"),
            }
        } else {
            match self.phase {
                AgentSessionPhase::Empty | AgentSessionPhase::Unconfigured => {
                    (cx.theme().muted_foreground, "未连接")
                }
                AgentSessionPhase::Loading => (cx.theme().primary, "正在连接"),
                AgentSessionPhase::Running => (cx.theme().success, "运行中"),
                AgentSessionPhase::Error => (cx.theme().danger, "失败"),
                AgentSessionPhase::Ended => (cx.theme().muted_foreground, "已结束"),
            }
        };
        let profile_name = self
            .profile
            .as_ref()
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| "未配置 Agent".to_string());
        let model = self
            .profile
            .as_ref()
            .and_then(|profile| profile.model.clone())
            .unwrap_or_else(|| "—".to_string());
        let permission = self
            .profile
            .as_ref()
            .map(|profile| profile.permission.label().to_string())
            .unwrap_or_else(|| "只读".to_string());
        let (working_directory, working_directory_color) = match self.context.as_ref() {
            Some(context) if context.has_resolved_working_directory() => (
                context.working_directory.display().to_string(),
                cx.theme().muted_foreground,
            ),
            Some(context)
                if context.working_directory_status
                    == AgentWorkingDirectoryStatus::NeedsSelection =>
            {
                ("尚未选择（发送前选择）".to_string(), cx.theme().warning)
            }
            Some(_) => ("不可用".to_string(), cx.theme().danger),
            None => ("—".to_string(), cx.theme().muted_foreground),
        };
        let working_directory_message = self
            .context
            .as_ref()
            .and_then(|context| context.working_directory_message.clone());
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
                            .child(div().text_sm().font_semibold().child(profile_name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{model} · {permission}")),
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
                            .text_color(working_directory_color)
                            .truncate()
                            .child(working_directory),
                    )
                    .when_some(working_directory_message, |this, message| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(message),
                        )
                    }),
            )
            .when_some(self.snapshot.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
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
}

impl AgentPanel {
    fn open_window(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(main_app) = self.main_app.as_ref().and_then(|entity| entity.upgrade()) else {
            return;
        };
        let window_size = self.window_size;
        // The independent window renders this same panel, so let the current
        // AgentPanel update finish before creating it to avoid a double lease.
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
            main_app.update(cx, |app, cx| app.open_agent_window(cx, window_size));
        })
        .detach();
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id("agent-panel-header")
            .w_full()
            .h(px(32.))
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Icon::new(IconName::Sparkles)
                    .size_3p5()
                    .text_color(cx.theme().primary),
            )
            .child(div().text_xs().font_medium().child("Agent"))
            .child(div().flex_1())
            .child(
                Button::new("agent-show-preview")
                    .ghost()
                    .compact()
                    .label("预览")
                    .tooltip("切换回文件预览")
                    .on_click(cx.listener(|this, _, _, cx| this.show_preview(cx))),
            )
            .child(
                Button::new("agent-panel-popout")
                    .ghost()
                    .compact()
                    .icon(IconName::Maximize2)
                    .tooltip("在独立窗口中打开 Agent")
                    .accessibility_label("在独立窗口中打开 Agent")
                    .on_click(cx.listener(|this, _, window, cx| this.open_window(window, cx))),
            )
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
                                        "切换目录或选择时更新这份上下文；发送前仍可检查。"
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

    fn render_output(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let output_empty = self.snapshot.output.trim().is_empty();
        let auto_follow = self.auto_follow_output;
        let mut header = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().text_xs().font_semibold().child("回答"))
            .child(div().flex_1());
        if !auto_follow && !output_empty {
            header = header.child(
                Button::new("agent-resume-follow")
                    .ghost()
                    .compact()
                    .icon(IconName::ArrowDown)
                    .label("回到最新")
                    .tooltip("恢复自动跟随并滚动到最新回答")
                    .on_click(cx.listener(|this, _, _, cx| this.resume_auto_follow(cx))),
            );
        }
        if !output_empty {
            header = header.child(
                Clipboard::new("agent-copy-output")
                    .value(self.snapshot.output.clone())
                    .tooltip("复制 Agent 回答")
                    .accessibility_label("复制 Agent 回答"),
            );
        }
        v_flex()
            .id("agent-output")
            .w_full()
            .gap_2()
            .child(header)
            .child(
                div()
                    .id("agent-output-scroll")
                    .w_full()
                    .min_h(px(160.))
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .track_scroll(&self.output_scroll)
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        if delta.y.as_f32() > 0.0 {
                            this.auto_follow_output = false;
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.18))
                    .p_3()
                    .when(output_empty, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.phase == AgentSessionPhase::Running {
                                    "Agent 正在处理…"
                                } else {
                                    "发送后会在这里显示流式回答。"
                                }),
                        )
                    })
                    .when(!output_empty, |this| {
                        this.child(
                            TextView::new(&self.output_view)
                                .w_full()
                                .text_sm()
                                .selectable(true)
                                .scrollable(false),
                        )
                    }),
            )
            .when(self.snapshot.output_truncated, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child("输出超过上限，当前内容已截断。"),
                )
            })
            .into_any_element()
    }

    fn render_permissions(&self, cx: &mut Context<Self>) -> AnyElement {
        let requests = self.snapshot.permission_requests.clone();
        if requests.is_empty() {
            return div().into_any_element();
        }
        let mut cards = Vec::new();
        for request in requests {
            let allow_once_id = request.request_id.clone();
            let allow_always_id = request.request_id.clone();
            let reject_once_id = request.request_id.clone();
            let reject_always_id = request.request_id.clone();
            let cancel_id = request.request_id.clone();
            let allow_once_enabled = permission_action_enabled(
                &request,
                AgentPermissionDecision::AllowOnce,
                Some("allow_once"),
            );
            let allow_always_enabled = permission_action_enabled(
                &request,
                AgentPermissionDecision::AllowTask,
                Some("allow_always"),
            );
            let reject_once_enabled = permission_action_enabled(
                &request,
                AgentPermissionDecision::RejectOnce,
                Some("reject_once"),
            );
            let reject_always_enabled = permission_action_enabled(
                &request,
                AgentPermissionDecision::RejectAlways,
                Some("reject_always"),
            );
            let allow_once_label = permission_action_label(&request, "allow_once", "仅本次允许");
            let allow_always_label = permission_action_label(&request, "allow_always", "始终允许");
            let reject_once_label = permission_action_label(&request, "reject_once", "仅本次拒绝");
            let reject_always_label =
                permission_action_label(&request, "reject_always", "始终拒绝");
            cards.push(
                v_flex()
                    .w_full()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.42))
                    .bg(cx.theme().warning.opacity(0.08))
                    .p_3()
                    .child(div().text_sm().font_medium().child(request.title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(request.description),
                    )
                    .when_some(request.policy_notice.clone(), |this, notice| {
                        this.child(div().text_xs().text_color(cx.theme().warning).child(notice))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new(format!(
                                    "agent-permission-allow-once-{}",
                                    allow_once_id
                                ))
                                .small()
                                .primary()
                                .label(allow_once_label)
                                .disabled(!allow_once_enabled)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.respond_permission(
                                            allow_once_id.clone(),
                                            AgentPermissionDecision::AllowOnce,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                Button::new(format!(
                                    "agent-permission-allow-always-{}",
                                    allow_always_id
                                ))
                                .small()
                                .outline()
                                .label(allow_always_label)
                                .disabled(!allow_always_enabled)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.respond_permission(
                                            allow_always_id.clone(),
                                            AgentPermissionDecision::AllowTask,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                Button::new(format!(
                                    "agent-permission-reject-once-{}",
                                    reject_once_id
                                ))
                                .small()
                                .outline()
                                .label(reject_once_label)
                                .disabled(!reject_once_enabled)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.respond_permission(
                                            reject_once_id.clone(),
                                            AgentPermissionDecision::RejectOnce,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                Button::new(format!(
                                    "agent-permission-reject-always-{}",
                                    reject_always_id
                                ))
                                .small()
                                .ghost()
                                .label(reject_always_label)
                                .disabled(!reject_always_enabled)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.respond_permission(
                                            reject_always_id.clone(),
                                            AgentPermissionDecision::RejectAlways,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                Button::new(format!("agent-permission-cancel-{}", cancel_id))
                                    .ghost()
                                    .compact()
                                    .label("取消任务")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.respond_permission(
                                            cancel_id.clone(),
                                            AgentPermissionDecision::Cancel,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        v_flex().w_full().gap_2().children(cards).into_any_element()
    }

    fn render_prompt(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let active =
            self.phase == AgentSessionPhase::Loading || self.phase == AgentSessionPhase::Running;
        let configured = self.profile.is_some();
        let ask_needs_selection = self.profile.as_ref().is_some_and(|profile| {
            matches!(&profile.working_directory, WorkingDirectoryPolicy::Ask)
        }) && self.context.as_ref().is_some_and(|context| {
            context.working_directory_status == AgentWorkingDirectoryStatus::NeedsSelection
        });
        v_flex()
            .id("agent-prompt")
            .w_full()
            .gap_2()
            .child(div().text_xs().font_semibold().child("发送准备"))
            .child(
                Textarea::new(&self.prompt_input)
                    .h(px(92.))
                    .w_full()
                    .appearance(true)
                    .bordered(true),
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
                    .when(active, |this| {
                        this.child(
                            Button::new("agent-stop")
                                .small()
                                .outline()
                                .label("停止")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                    })
                    .when(self.can_retry_prompt(), |this| {
                        this.child(
                            Button::new("agent-retry")
                                .small()
                                .outline()
                                .icon(IconName::RotateCcw)
                                .label("重试")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.retry_prompt(window, cx)
                                    }),
                                ),
                        )
                    })
                    .when(ask_needs_selection, |this| {
                        this.child(
                            Button::new("agent-choose-working-directory")
                                .small()
                                .outline()
                                .icon(IconName::Folder)
                                .label("选择目录")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_working_directory(None, window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("agent-send")
                            .small()
                            .primary()
                            .label("发送")
                            .disabled(!configured || self.context.is_none() || active)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_prompt(window, cx)),
                            ),
                    ),
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
        let header = self.render_header(cx);
        let content = if self.context.is_none() {
            self.render_empty(cx)
        } else {
            let status = self.render_status(cx);
            let context = self.render_context_section(window, cx);
            let output = self.render_output(cx);
            let permissions = self.render_permissions(cx);
            let prompt = self.render_prompt(cx);
            v_flex()
                .id("agent-panel-content")
                .w_full()
                .gap_3()
                .child(status)
                .child(context)
                .child(output)
                .child(permissions)
                .child(prompt)
                .into_any_element()
        };
        v_flex()
            .id("agent-panel")
            .size_full()
            .overflow_hidden()
            .rounded_lg()
            .bg(cx.theme().title_bar)
            .border_1()
            .border_color(cx.theme().border)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .p(px(6.))
                    .child(content),
            )
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}
