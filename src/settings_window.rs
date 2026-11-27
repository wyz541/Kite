use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use kite_core::agent::{
    AgentCapabilities, AgentConnectionResult, AgentConnectionStatus, AgentDiagnostics,
    AgentEnvironmentVariable, AgentProfile, AgentProfileSnapshot, AgentProfileStore,
    AgentTransport, PermissionPolicy, WorkingDirectoryPolicy, agent_presets,
    clear_agent_diagnostics, diagnose_agents, preset_profile,
};
use kite_core::tool_registry::{ToolPlatform, ToolPolicy, builtin_tool_definitions};

use crate::app::{KiteApp, SettingsUpdateState};
use crate::ui_preferences::{self, AppearancePreference, UiPreferences};
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, Size, StyledExt as _, Theme, ThemeMode,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    General,
    Appearance,
    Layout,
    FileHandling,
    Agent,
    Updates,
    About,
}

impl SettingsPage {
    const ALL: [Self; 7] = [
        Self::General,
        Self::Appearance,
        Self::Layout,
        Self::FileHandling,
        Self::Agent,
        Self::Updates,
        Self::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Appearance => "外观",
            Self::Layout => "布局",
            Self::FileHandling => "文件处理",
            Self::Agent => "Agent",
            Self::Updates => "更新",
            Self::About => "关于",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::General => "启动和窗口恢复",
            Self::Appearance => "主题和界面外观",
            Self::Layout => "侧边栏、预览和详细信息",
            Self::FileHandling => "默认资源管理器与工具",
            Self::Agent => "连接、权限与默认 Agent",
            Self::Updates => "版本、更新与诊断",
            Self::About => "Kite 版本信息",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Settings,
            Self::Appearance => IconName::Palette,
            Self::Layout => IconName::LayoutPanelLeft,
            Self::FileHandling => IconName::Folder,
            Self::Agent => IconName::Sparkles,
            Self::Updates => IconName::RefreshCw,
            Self::About => IconName::Info,
        }
    }

    fn tint(self) -> Hsla {
        match self {
            Self::General => rgb(0x8e9aab).into(),
            Self::Appearance => rgb(0xf47a32).into(),
            Self::Layout => rgb(0x5e9cff).into(),
            Self::FileHandling => rgb(0x45b8d8).into(),
            Self::Agent => rgb(0xf28c4c).into(),
            Self::Updates => rgb(0x55b978).into(),
            Self::About => rgb(0xa675df).into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AgentWorkingDirectoryChoice {
    SelectedItem,
    FileParent,
    RepositoryRoot,
    Fixed,
    #[default]
    Ask,
}

impl AgentWorkingDirectoryChoice {
    fn from_policy(policy: &WorkingDirectoryPolicy) -> Self {
        match policy {
            WorkingDirectoryPolicy::SelectedItem => Self::SelectedItem,
            WorkingDirectoryPolicy::FileParent => Self::FileParent,
            WorkingDirectoryPolicy::RepositoryRoot => Self::RepositoryRoot,
            WorkingDirectoryPolicy::Fixed { .. } => Self::Fixed,
            WorkingDirectoryPolicy::Ask => Self::Ask,
        }
    }

    fn to_policy(self, fixed_path: Option<String>) -> WorkingDirectoryPolicy {
        match self {
            Self::SelectedItem => WorkingDirectoryPolicy::SelectedItem,
            Self::FileParent => WorkingDirectoryPolicy::FileParent,
            Self::RepositoryRoot => WorkingDirectoryPolicy::RepositoryRoot,
            Self::Fixed => WorkingDirectoryPolicy::Fixed {
                path: fixed_path.unwrap_or_default(),
            },
            Self::Ask => WorkingDirectoryPolicy::Ask,
        }
    }
}

struct AgentEditorState {
    profile_id: String,
    is_new: bool,
    original_credential_ref: Option<String>,
    has_stored_credential: bool,
    transport: AgentTransport,
    permission: PermissionPolicy,
    capabilities: AgentCapabilities,
    working_directory_policy: AgentWorkingDirectoryChoice,
    clear_credential: bool,
    name_input: Entity<InputState>,
    executable_input: Entity<InputState>,
    endpoint_input: Entity<InputState>,
    arguments_input: Entity<TextareaState>,
    environment_input: Entity<TextareaState>,
    model_input: Entity<InputState>,
    working_directory_input: Entity<InputState>,
    credential_env_input: Entity<InputState>,
    credential_input: Entity<InputState>,
    timeout_input: Entity<InputState>,
    output_limit_input: Entity<InputState>,
    initial_snapshot: String,
    test_result: Option<AgentConnectionResult>,
    testing: bool,
    error: Option<SharedString>,
}

impl AgentEditorState {
    fn new(
        profile: AgentProfile,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let name_input = text_input(window, cx, "例如：本地 Agent");
        let executable_input = text_input(window, cx, "绝对路径或 PATH 中的命令名称");
        set_input_value(
            &executable_input,
            profile.executable.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        let endpoint_input = text_input(window, cx, "https://localhost:7777/acp");
        set_input_value(
            &endpoint_input,
            profile.endpoint.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        set_input_value(&name_input, &profile.name, window, cx);
        let arguments_input = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx)
                .placeholder("每行一个启动参数；不要在这里填写 token 或密码");
            input.set_value(profile.arguments.join("\n"), window, cx);
            input
        });
        let environment_input = cx.new(|cx| {
            let mut input =
                TextareaState::new(window, cx).placeholder("每行 KEY=VALUE；敏感值请使用凭据栏");
            input.set_value(environment_text(&profile.environment), window, cx);
            input
        });
        let model_input = text_input(window, cx, "留空时使用 Agent 自身配置");
        set_input_value(
            &model_input,
            profile.model.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        let working_directory_input = text_input(window, cx, "固定工作目录");
        set_input_value(
            &working_directory_input,
            profile.working_directory.fixed_path().unwrap_or_default(),
            window,
            cx,
        );
        let credential_env_input = text_input(window, cx, "KITE_AGENT_TOKEN");
        set_input_value(
            &credential_env_input,
            profile.credential_env_var.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        let credential_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Token 或密码，保存到系统钥匙串")
                .masked(true)
                .clean_on_escape()
        });
        let timeout_input = text_input(window, cx, "30");
        set_input_value(
            &timeout_input,
            &profile.timeout_seconds.to_string(),
            window,
            cx,
        );
        let output_limit_input = text_input(window, cx, "512");
        set_input_value(
            &output_limit_input,
            &(profile.output_limit_bytes / 1024).to_string(),
            window,
            cx,
        );

        let mut editor = Self {
            profile_id: profile.id.clone(),
            is_new,
            original_credential_ref: profile.credential_ref.clone(),
            has_stored_credential: profile.credential_ref.is_some(),
            transport: profile.transport,
            permission: profile.permission,
            capabilities: profile.capabilities,
            working_directory_policy: AgentWorkingDirectoryChoice::from_policy(
                &profile.working_directory,
            ),
            clear_credential: false,
            name_input,
            executable_input,
            endpoint_input,
            arguments_input,
            environment_input,
            model_input,
            working_directory_input,
            credential_env_input,
            credential_input,
            timeout_input,
            output_limit_input,
            initial_snapshot: String::new(),
            test_result: None,
            testing: false,
            error: None,
        };
        editor.initial_snapshot = editor.snapshot(cx);
        editor
    }

    fn snapshot(&self, cx: &App) -> String {
        let timeout = self.timeout_input.read(cx).value();
        let output_limit = self.output_limit_input.read(cx).value();
        format!(
            "{:?}\n{:?}\n{:?}\n{:?}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.transport,
            self.permission,
            self.capabilities,
            self.working_directory_policy,
            self.clear_credential,
            self.name_input.read(cx).value(),
            self.executable_input.read(cx).value(),
            self.endpoint_input.read(cx).value(),
            self.arguments_input.read(cx).value(),
            self.environment_input.read(cx).value(),
            self.model_input.read(cx).value(),
            self.working_directory_input.read(cx).value(),
            self.credential_env_input.read(cx).value(),
            self.credential_input.read(cx).value(),
            timeout,
            output_limit,
        )
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.snapshot(cx) != self.initial_snapshot
    }

    fn credential(&self, cx: &App) -> String {
        self.credential_input.read(cx).value().trim().to_string()
    }

    fn current_profile(&self, cx: &App) -> Result<AgentProfile, String> {
        let timeout_seconds = self
            .timeout_input
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .map_err(|_| "超时必须是整数秒".to_string())?;
        let output_limit_kib = self
            .output_limit_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .map_err(|_| "输出上限必须是整数 KiB".to_string())?;
        let output_limit_bytes = output_limit_kib
            .checked_mul(1024)
            .ok_or_else(|| "输出上限过大".to_string())?;
        let fixed_path = if self.working_directory_policy == AgentWorkingDirectoryChoice::Fixed {
            Some(
                self.working_directory_input
                    .read(cx)
                    .value()
                    .trim()
                    .to_string(),
            )
        } else {
            None
        };
        let is_http = self.transport == AgentTransport::AcpHttp;
        let endpoint = if is_http {
            optional_value(&self.endpoint_input.read(cx).value())
        } else {
            None
        };
        Ok(AgentProfile {
            id: self.profile_id.clone(),
            name: self.name_input.read(cx).value().trim().to_string(),
            transport: self.transport,
            executable: if is_http {
                None
            } else {
                optional_value(&self.executable_input.read(cx).value())
            },
            endpoint,
            arguments: if is_http {
                Vec::new()
            } else {
                non_empty_lines(&self.arguments_input.read(cx).value())
            },
            environment: if is_http {
                Vec::new()
            } else {
                parse_environment(&self.environment_input.read(cx).value())?
            },
            model: if is_http {
                None
            } else {
                optional_value(&self.model_input.read(cx).value())
            },
            working_directory: self.working_directory_policy.to_policy(fixed_path),
            permission: self.permission,
            timeout_seconds,
            output_limit_bytes,
            credential_ref: if self.clear_credential {
                None
            } else {
                self.original_credential_ref.clone()
            },
            credential_env_var: optional_value(&self.credential_env_input.read(cx).value()),
            capabilities: self.capabilities.clone(),
        })
    }
}

#[derive(Clone)]
struct AgentPendingDelete {
    profile: AgentProfile,
    replacements: Vec<AgentProfile>,
}

fn text_input(
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
    placeholder: &'static str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .clean_on_escape()
    })
}

fn set_input_value(
    input: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) {
    if !value.is_empty() {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

fn optional_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn non_empty_lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_environment(value: &str) -> Result<Vec<AgentEnvironmentVariable>, String> {
    let mut entries = Vec::new();
    for (index, line) in value.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("环境变量第 {} 行缺少 =", index + 1));
        };
        entries.push(AgentEnvironmentVariable {
            name: key.trim().to_string(),
            value: value.to_string(),
        });
    }
    Ok(entries)
}

fn environment_text(environment: &[AgentEnvironmentVariable]) -> String {
    environment
        .iter()
        .map(|entry| format!("{}={}", entry.name, entry.value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn agent_transport_choice_label(transport: AgentTransport) -> String {
    let capability = transport.capabilities();
    if capability.profile_save_supported {
        capability.label
    } else {
        format!("{}（仅测试）", capability.label)
    }
}

fn agent_connection_target(profile: &AgentProfile) -> String {
    if profile.transport == AgentTransport::AcpHttp {
        profile
            .endpoint
            .clone()
            .unwrap_or_else(|| "未设置端点".to_string())
    } else {
        profile
            .executable
            .clone()
            .unwrap_or_else(|| "未设置命令".to_string())
    }
}

fn working_directory_detail(profile: &AgentProfile) -> String {
    match &profile.working_directory {
        WorkingDirectoryPolicy::Fixed { path } => path.clone(),
        policy => policy.label().to_string(),
    }
}

fn agent_status_color(status: AgentConnectionStatus, cx: &App) -> Hsla {
    if matches!(
        status,
        AgentConnectionStatus::AuthenticationRequired
            | AgentConnectionStatus::AuthenticationFailed
            | AgentConnectionStatus::IncompatibleVersion
    ) {
        cx.theme().warning
    } else if status.is_success() {
        cx.theme().success
    } else {
        cx.theme().danger
    }
}

fn agent_working_directory_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    policy: AgentWorkingDirectoryChoice,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    Button::new(id)
        .ghost()
        .compact()
        .label(label)
        .selected(selected)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Some(editor) = this.agent_editor.as_mut() {
                editor.working_directory_policy = policy;
                editor.test_result = None;
            }
            cx.notify();
        }))
        .into_any_element()
}

pub(crate) struct SettingsWindow {
    main_app: Entity<KiteApp>,
    page: SettingsPage,
    preferences: UiPreferences,
    update_state: SettingsUpdateState,
    status: Option<SharedString>,
    agent_config: AgentProfileSnapshot,
    agent_diagnostics: AgentDiagnostics,
    agent_store: Arc<AgentProfileStore>,
    agent_warning: Option<SharedString>,
    agent_message: Option<SharedString>,
    agent_editor: Option<AgentEditorState>,
    agent_pending_delete: Option<AgentPendingDelete>,
    agent_tests: BTreeMap<String, AgentConnectionResult>,
    agent_testing_id: Option<String>,
    _agent_test_task: Task<()>,
    allow_close: bool,
    _main_app_subscription: Subscription,
}

impl SettingsWindow {
    fn new(
        main_app: Entity<KiteApp>,
        initial_update_state: SettingsUpdateState,
        initial_page: SettingsPage,
        agent_store: Arc<AgentProfileStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The main app is mutably leased while its click handler opens this window.
        // Use the caller's snapshot instead of reading the entity here.
        let preferences = ui_preferences::load_ui_preferences();
        let agent_snapshot = agent_store.snapshot();
        let subscription = cx.observe_in(&main_app, window, |this, main_app, _, cx| {
            this.update_state = main_app.read(cx).settings_update_state();
            this.status = this.update_state.status.clone();
            cx.notify();
        });
        let this = Self {
            main_app,
            page: initial_page,
            preferences,
            status: initial_update_state.status.clone(),
            update_state: initial_update_state,
            agent_config: agent_snapshot.clone(),
            agent_diagnostics: diagnose_agents(),
            agent_store,
            agent_warning: agent_snapshot
                .warning
                .as_ref()
                .map(|warning| SharedString::from(warning.message())),
            agent_message: None,
            agent_editor: None,
            agent_pending_delete: None,
            agent_tests: BTreeMap::new(),
            agent_testing_id: None,
            _agent_test_task: Task::ready(()),
            allow_close: false,
            _main_app_subscription: subscription,
        };
        this.install_close_guard(window, cx);
        this
    }

    fn save_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match ui_preferences::save_ui_preferences(self.preferences.clone()) {
            Ok(()) => self.status = Some("设置已保存".into()),
            Err(error) => self.status = Some(error.into()),
        }
        self.main_app.update(cx, |app, cx| {
            app.apply_ui_preferences(self.preferences.clone(), cx);
        });
        let mode = match self.preferences.appearance {
            AppearancePreference::System => ThemeMode::from(window.appearance()),
            AppearancePreference::Light => ThemeMode::Light,
            AppearancePreference::Dark => ThemeMode::Dark,
        };
        Theme::change(mode, Some(window), cx);
        cx.notify();
    }

    fn install_close_guard(&self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.weak_entity();
        window.on_window_should_close(cx, move |window, app| {
            let Some(entity) = entity.upgrade() else {
                return true;
            };
            let dirty = {
                let this = entity.read(app);
                this.agent_editor
                    .as_ref()
                    .is_some_and(|editor| editor.is_dirty(app))
            };
            if entity.read(app).allow_close || !dirty {
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
                        .title("有未保存的 Agent 更改")
                        .description("关闭设置会丢弃当前表单修改。")
                        .confirm()
                        .ok_text("放弃并关闭")
                        .cancel_text("继续编辑")
                        .on_ok(move |_, window, app| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(app, |this, _| {
                                    this.allow_close = true;
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

    fn request_page_change(
        &mut self,
        page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page == page {
            return;
        }
        let dirty = self
            .agent_editor
            .as_ref()
            .is_some_and(|editor| editor.is_dirty(cx));
        if !dirty {
            self.page = page;
            self.agent_editor = None;
            self.agent_pending_delete = None;
            cx.notify();
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        let entity = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            alert
                .title("有未保存的 Agent 更改")
                .description("切换设置页面会丢弃当前表单修改。")
                .confirm()
                .ok_text("放弃并切换")
                .cancel_text("继续编辑")
                .on_ok(move |_, _, app| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.agent_editor = None;
                            this.page = page;
                            this.agent_pending_delete = None;
                            cx.notify();
                        });
                    }
                    true
                })
        });
    }

    fn reload_agents(&mut self) {
        let snapshot = self.agent_store.snapshot();
        self.agent_warning = snapshot
            .warning
            .as_ref()
            .map(|warning| SharedString::from(warning.message()));
        self.agent_config = snapshot;
        self.agent_diagnostics = diagnose_agents();
    }

    fn notify_agents_changed(&self, cx: &mut Context<Self>) {
        let snapshot = self.agent_store.snapshot();
        self.main_app
            .update(cx, |app, cx| app.apply_agent_profiles(snapshot, cx));
    }

    fn open_new_agent(
        &mut self,
        transport: AgentTransport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = AgentProfile {
            transport,
            permission: if transport == AgentTransport::Cli {
                PermissionPolicy::AgentManaged
            } else {
                PermissionPolicy::ReadOnly
            },
            ..AgentProfile::default()
        };
        self.agent_editor = Some(AgentEditorState::new(profile, true, window, cx));
        self.agent_pending_delete = None;
        self.agent_message = None;
        if let Some(editor) = &self.agent_editor {
            editor
                .name_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn add_agent_preset(&mut self, preset_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut profile) = preset_profile(preset_id, kite_core::agent::new_profile_id())
        else {
            self.agent_message = Some("未知的 Agent 预设".into());
            cx.notify();
            return;
        };
        let base_name = profile.name.clone();
        let mut suffix = 2;
        while self
            .agent_config
            .profiles
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&profile.name))
        {
            profile.name = format!("{base_name} ({suffix})");
            suffix += 1;
        }
        self.agent_editor = Some(AgentEditorState::new(profile, true, window, cx));
        self.agent_pending_delete = None;
        self.agent_message = Some("请检查预设字段并测试连接后再保存".into());
        cx.notify();
    }

    fn open_edit_agent(
        &mut self,
        profile: AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agent_editor = Some(AgentEditorState::new(profile, false, window, cx));
        self.agent_pending_delete = None;
        self.agent_message = None;
        cx.notify();
    }

    fn close_agent_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dirty = self
            .agent_editor
            .as_ref()
            .is_some_and(|editor| editor.is_dirty(cx));
        if !dirty {
            self.agent_editor = None;
            self.agent_message = None;
            cx.notify();
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        let entity = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            alert
                .title("放弃未保存的 Agent 更改？")
                .description("这些更改尚未写入 agent.json，也没有更新系统钥匙串。")
                .confirm()
                .ok_text("放弃更改")
                .cancel_text("继续编辑")
                .on_ok(move |_, _, app| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(app, |this, cx| {
                            this.agent_editor = None;
                            this.agent_message = None;
                            cx.notify();
                        });
                    }
                    true
                })
        });
    }

    fn save_agent_editor(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.agent_editor.as_ref() else {
            return;
        };
        let profile = match editor.current_profile(cx) {
            Ok(profile) => profile,
            Err(error) => {
                if let Some(editor) = self.agent_editor.as_mut() {
                    editor.error = Some(error.to_string().into());
                }
                cx.notify();
                return;
            }
        };
        let credential = {
            let value = editor.credential(cx);
            (!value.is_empty()).then_some(value)
        };
        let clear_credential = editor.clear_credential;
        match self
            .agent_store
            .save_profile_with_credential(profile, credential, clear_credential)
        {
            Ok(config) => {
                self.agent_config = config;
                self.agent_warning = None;
                self.agent_message = Some("Agent 配置已保存".into());
                self.agent_editor = None;
                self.agent_pending_delete = None;
                self.reload_agents();
                self.notify_agents_changed(cx);
            }
            Err(error) => {
                if let Some(editor) = self.agent_editor.as_mut() {
                    editor.error = Some(error.to_string().into());
                }
            }
        }
        cx.notify();
    }

    fn duplicate_agent(&mut self, profile_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let before = self
            .agent_config
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        match self.agent_store.duplicate_profile(profile_id.to_string()) {
            Ok(config) => {
                let duplicate = config
                    .profiles
                    .iter()
                    .find(|profile| !before.contains(&profile.id))
                    .cloned();
                self.agent_config = config;
                self.agent_warning = None;
                self.agent_message = Some("已创建副本，请检查名称和凭据".into());
                self.agent_diagnostics = diagnose_agents();
                self.notify_agents_changed(cx);
                if let Some(duplicate) = duplicate {
                    self.open_edit_agent(duplicate, window, cx);
                }
            }
            Err(error) => self.agent_message = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn set_default_agent(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        match self.agent_store.set_default_profile(profile_id.to_string()) {
            Ok(config) => {
                self.agent_config = config;
                self.agent_warning = None;
                self.agent_message = Some("默认 Agent 已更新".into());
                self.agent_diagnostics = diagnose_agents();
                self.notify_agents_changed(cx);
            }
            Err(error) => self.agent_message = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn begin_delete_agent(&mut self, profile: AgentProfile, cx: &mut Context<Self>) {
        let is_default =
            self.agent_config.default_profile_id.as_deref() == Some(profile.id.as_str());
        let replacements = if is_default {
            self.agent_config
                .profiles
                .iter()
                .filter(|candidate| {
                    candidate.id != profile.id && candidate.transport.can_be_default()
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        self.agent_pending_delete = Some(AgentPendingDelete {
            profile,
            replacements,
        });
        cx.notify();
    }

    fn confirm_delete_agent(
        &mut self,
        replacement_default_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.agent_pending_delete.as_ref() else {
            return;
        };
        let profile = pending.profile.clone();
        match self
            .agent_store
            .delete_profile_with_replacement(profile.id.clone(), replacement_default_id)
        {
            Ok(config) => {
                self.agent_config = config;
                self.agent_warning = None;
                self.agent_message = Some(format!("已删除 {}", profile.name).into());
                self.agent_pending_delete = None;
                self.agent_tests.remove(&profile.id);
                self.agent_diagnostics = diagnose_agents();
                self.notify_agents_changed(cx);
            }
            Err(error) => self.agent_message = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn test_saved_agent(&mut self, profile: AgentProfile, cx: &mut Context<Self>) {
        if self.agent_testing_id.as_deref() == Some(profile.id.as_str()) {
            return;
        }
        self.agent_testing_id = Some(profile.id.clone());
        self.agent_message = Some(format!("正在测试 {}…", profile.name).into());
        cx.notify();
        self._agent_test_task = cx.spawn(async move |this, cx| {
            let profile_id = profile.id.clone();
            let result = cx
                .background_spawn(
                    async move { kite_core::agent::test_agent_connection(profile, None) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.agent_testing_id.as_deref() == Some(profile_id.as_str()) {
                    this.agent_testing_id = None;
                }
                this.agent_tests.insert(profile_id, result);
                this.agent_diagnostics = diagnose_agents();
                this.agent_message = None;
                cx.notify();
            });
        });
    }

    fn test_agent_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.agent_editor.as_mut() else {
            return;
        };
        let profile = match editor.current_profile(cx) {
            Ok(profile) => profile,
            Err(error) => {
                editor.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let credential = {
            let value = editor.credential(cx);
            (!value.is_empty()).then_some(value)
        };
        editor.testing = true;
        editor.test_result = None;
        editor.error = None;
        let profile_key = profile.id.clone();
        cx.notify();
        self._agent_test_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    kite_core::agent::test_agent_connection(profile, credential)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(editor) = this.agent_editor.as_mut()
                    && editor.profile_id == profile_key
                {
                    editor.testing = false;
                    editor.test_result = Some(result);
                }
                this.agent_diagnostics = diagnose_agents();
                cx.notify();
            });
        });
    }

    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            SettingsPage::General => self.render_general(cx),
            SettingsPage::Appearance => self.render_appearance(cx),
            SettingsPage::Layout => self.render_layout(cx),
            SettingsPage::FileHandling => self.render_file_handling(window, cx),
            SettingsPage::Agent => self.render_agent(cx),
            SettingsPage::Updates => self.render_updates(window, cx),
            SettingsPage::About => self.render_about(cx),
        }
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = v_flex().gap_1();
        for page in SettingsPage::ALL {
            let selected = self.page == page;
            let tint = page.tint();
            rows = rows.child(
                Button::new(SharedString::from(format!(
                    "settings-page-{}",
                    page.title()
                )))
                .ghost()
                .compact()
                .selected(selected)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.request_page_change(page, window, cx);
                }))
                .child(
                    h_flex()
                        .h(px(38.))
                        .w_full()
                        .gap_2p5()
                        .items_center()
                        .px_2()
                        .child(
                            div()
                                .size(px(25.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .bg(tint.opacity(if selected { 0.24 } else { 0.13 }))
                                .child(Icon::new(page.icon()).size_4().text_color(tint)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_medium()
                                .child(page.title()),
                        ),
                ),
            );
        }

        v_flex()
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar.opacity(0.76))
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .px_5()
                    .pt_5()
                    .pb_4()
                    .child(
                        div()
                            .size(px(38.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(10.))
                            .bg(cx.theme().primary.opacity(0.16))
                            .child(
                                Icon::new(IconName::Wind)
                                    .size_6()
                                    .text_color(cx.theme().primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_sm().font_semibold().child("Kite"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("设置"),
                            ),
                    ),
            )
            .child(div().px_2p5().child(rows))
            .child(div().flex_1())
            .child(
                div()
                    .px_5()
                    .pb_4()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Kite {}", env!("CARGO_PKG_VERSION"))),
            )
            .into_any_element()
    }

    fn render_header(&self, cx: &App) -> AnyElement {
        v_flex()
            .gap_1()
            .child(div().text_2xl().font_semibold().child(self.page.title()))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.page.subtitle()),
            )
            .into_any_element()
    }

    fn render_general(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.section(
            "启动",
            vec![
                self.row(
                    "general-restore-session",
                    "恢复上次会话",
                    Some("重新打开 Kite 时恢复页签和目录。"),
                    Switch::new("restore-session")
                        .checked(self.preferences.restore_session)
                        .small()
                        .accessibility_label("恢复上次会话")
                        .on_click(cx.listener(|this, checked, window, cx| {
                            this.preferences.restore_session = *checked;
                            this.save_preferences(window, cx);
                        }))
                        .into_any_element(),
                    cx,
                ),
                self.separator(cx),
                self.row(
                    "general-git-auto-fetch",
                    "定时获取 Git 更新",
                    Some("每 15 分钟在后台获取当前仓库，不会自动合并。"),
                    Switch::new("git-auto-fetch")
                        .checked(self.preferences.git_auto_fetch)
                        .small()
                        .accessibility_label("定时获取 Git 更新")
                        .on_click(cx.listener(|this, checked, window, cx| {
                            this.preferences.git_auto_fetch = *checked;
                            this.save_preferences(window, cx);
                        }))
                        .into_any_element(),
                    cx,
                ),
            ],
            Some("关闭会话恢复后，Kite 下次启动时打开默认文件夹。定时获取默认关闭。"),
            cx,
        )
    }

    fn render_appearance(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let buttons = h_flex()
            .gap_1()
            .p_1()
            .rounded_md()
            .bg(cx.theme().muted.opacity(0.22))
            .child(self.appearance_button(
                "appearance-system",
                "跟随系统",
                AppearancePreference::System,
                cx,
            ))
            .child(self.appearance_button(
                "appearance-light",
                "浅色",
                AppearancePreference::Light,
                cx,
            ))
            .child(self.appearance_button(
                "appearance-dark",
                "深色",
                AppearancePreference::Dark,
                cx,
            ))
            .into_any_element();

        self.section(
            "主题",
            vec![self.row(
                "appearance-theme",
                "外观",
                Some("选择 Kite 窗口使用的明暗外观。"),
                buttons,
                cx,
            )],
            Some("Kite 的所有窗口会立即应用相同外观。"),
            cx,
        )
    }

    fn appearance_button(
        &self,
        id: &'static str,
        label: &'static str,
        preference: AppearancePreference,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Button::new(id)
            .ghost()
            .compact()
            .label(label)
            .selected(self.preferences.appearance == preference)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.preferences.appearance = preference;
                this.save_preferences(window, cx);
            }))
            .into_any_element()
    }

    fn render_layout(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.section(
            "窗口组件",
            vec![
                self.row(
                    "layout-sidebar",
                    "显示侧边栏",
                    Some("显示快速访问、收藏和磁盘。"),
                    Switch::new("sidebar-visible")
                        .checked(self.preferences.sidebar_visible)
                        .small()
                        .accessibility_label("显示侧边栏")
                        .on_click(cx.listener(|this, checked, window, cx| {
                            this.preferences.sidebar_visible = *checked;
                            this.save_preferences(window, cx);
                        }))
                        .into_any_element(),
                    cx,
                ),
                self.separator(cx),
                self.row(
                    "layout-preview",
                    "显示右侧栏",
                    Some("显示预览或 Agent 上下文表面。"),
                    Switch::new("preview-visible")
                        .checked(self.preferences.preview_visible)
                        .small()
                        .accessibility_label("显示预览")
                        .on_click(cx.listener(|this, checked, window, cx| {
                            this.preferences.preview_visible = *checked;
                            this.save_preferences(window, cx);
                        }))
                        .into_any_element(),
                    cx,
                ),
                self.separator(cx),
                self.row(
                    "layout-details",
                    "显示详细信息列",
                    Some("在活动栏显示日期、类型和大小。"),
                    Switch::new("details-visible")
                        .checked(self.preferences.column_details_visible)
                        .small()
                        .accessibility_label("显示详细信息列")
                        .on_click(cx.listener(|this, checked, window, cx| {
                            this.preferences.column_details_visible = *checked;
                            this.save_preferences(window, cx);
                        }))
                        .into_any_element(),
                    cx,
                ),
            ],
            None,
            cx,
        )
    }

    fn render_agent(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let add_acp = Button::new("agent-add-acp")
            .small()
            .primary()
            .label("添加 Agent")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_new_agent(AgentTransport::AcpStdio, window, cx);
            }))
            .into_any_element();
        let add_cli = Button::new("agent-add-cli")
            .small()
            .outline()
            .label("测试通用 CLI")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_new_agent(AgentTransport::Cli, window, cx);
            }))
            .into_any_element();

        let mut content = v_flex().gap_6().child(
            v_flex()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "只有 ACP / JSON-RPC stdio 可保存并启动会话；ACP HTTP 与通用 CLI 仅用于连接测试。",
                        ),
                )
                .child(h_flex().justify_end().gap_2().child(add_cli).child(add_acp)),
        );

        if let Some(warning) = self.agent_warning.clone() {
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.5))
                    .bg(cx.theme().warning.opacity(0.08))
                    .p_3()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size_4()
                            .text_color(cx.theme().warning),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(warning),
                    ),
            );
        }
        if let Some(message) = self.agent_message.clone() {
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.08))
                    .p_3()
                    .child(
                        Icon::new(IconName::Info)
                            .size_4()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(message),
                    ),
            );
        }

        if let Some(pending) = self.agent_pending_delete.clone() {
            content = content.child(self.render_agent_delete_prompt(pending, cx));
        }

        if !self.agent_config.profiles.is_empty() && self.agent_config.default_profile_id.is_none()
        {
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.45))
                    .bg(cx.theme().warning.opacity(0.07))
                    .p_3()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size_4()
                            .text_color(cx.theme().warning),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child("当前没有可运行的默认 Agent。请编辑一个配置并将接入方式改为 ACP / JSON-RPC stdio。"),
                    ),
            );
        }

        if self.agent_config.profiles.is_empty() {
            content = content.child(
                v_flex()
                    .items_center()
                    .gap_3()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().title_bar.opacity(0.86))
                    .px_8()
                    .py_12()
                    .child(
                        div()
                            .size(px(52.))
                            .rounded(px(14.))
                            .bg(SettingsPage::Agent.tint().opacity(0.14))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::Bot)
                                    .size_7()
                                    .text_color(SettingsPage::Agent.tint()),
                            ),
                    )
                    .child(div().text_lg().font_semibold().child("尚未配置 Agent"))
                    .child(
                        div()
                            .max_w(px(420.))
                            .text_center()
                            .text_sm()
                            .line_height(relative(1.4))
                            .text_color(cx.theme().muted_foreground)
                            .child("添加一个 ACP / JSON-RPC stdio Agent。ACP HTTP 和通用 CLI 可从上方按钮进入仅测试模式；Kite 不会扫描或导入其他工具的配置。"),
                    )
                    .child(
                        h_flex()
                            .mt_2()
                            .gap_2()
                            .child(
                                Button::new("agent-empty-add")
                                    .primary()
                                    .label("添加 Agent")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.open_new_agent(AgentTransport::AcpStdio, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("agent-empty-preset")
                                    .outline()
                                    .label("测试通用 CLI")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.open_new_agent(AgentTransport::Cli, window, cx);
                                    })),
                            ),
                    ),
            );
        } else {
            if let Some(default) = self
                .agent_config
                .default_profile_id
                .as_deref()
                .and_then(|id| {
                    self.agent_config
                        .profiles
                        .iter()
                        .find(|profile| profile.id == id)
                })
                .cloned()
            {
                content = content.child(self.render_default_agent(default, cx));
            }
            let mut rows = v_flex();
            for (index, profile) in self.agent_config.profiles.iter().cloned().enumerate() {
                if index > 0 {
                    rows = rows.child(self.separator(cx));
                }
                rows = rows.child(self.render_agent_profile_row(profile, cx));
            }
            content =
                content.child(self.section("Agent 配置", vec![rows.into_any_element()], None, cx));
        }

        content
            .child(self.render_agent_presets(cx))
            .child(self.render_agent_diagnostics(cx))
            .child(
                div()
                    .text_xs()
                    .line_height(relative(1.45))
                    .text_color(cx.theme().muted_foreground)
                    .child("Token 或密码只写入 Windows 凭据管理器 / macOS 钥匙串；agent.json 仅保存 keychain:// 引用。通用 CLI 的实际权限由 Agent 自身管理。"),
            )
            .into_any_element()
    }

    fn render_agent_presets(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = v_flex();
        for (index, preset) in agent_presets().into_iter().enumerate() {
            if index > 0 {
                rows = rows.child(self.separator(cx));
            }
            let preset_id = preset.id.clone();
            rows = rows.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_6()
                    .px_4()
                    .py_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(preset.name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(preset.description),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(preset.install_hint),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("agent-preset-{preset_id}")))
                            .small()
                            .outline()
                            .label("添加预设")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.add_agent_preset(&preset_id, window, cx);
                            })),
                    ),
            );
        }
        self.section(
            "Agent 预设",
            vec![rows.into_any_element()],
            Some("预设只填写接入元数据，不覆盖 Agent 原生认证或用户配置。"),
            cx,
        )
    }

    fn render_agent_diagnostics(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = v_flex();
        let config_path = self.agent_diagnostics.config_path.clone();
        let log_path = self.agent_diagnostics.log_path.clone();
        rows = rows.child(
            h_flex()
                .w_full()
                .items_center()
                .gap_6()
                .px_4()
                .py_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().font_medium().child("诊断位置"))
                        .child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(log_path.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("agent-open-config-dir")
                                .small()
                                .outline()
                                .label("打开配置目录")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let directory = std::path::Path::new(&config_path)
                                        .parent()
                                        .unwrap_or_else(|| std::path::Path::new("."));
                                    if let Err(error) = open::that(directory) {
                                        this.agent_message =
                                            Some(format!("无法打开配置目录：{error}").into());
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(
                            Button::new("agent-clear-diagnostics")
                                .small()
                                .ghost()
                                .label("清除日志")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    match clear_agent_diagnostics() {
                                        Ok(()) => {
                                            this.agent_diagnostics = diagnose_agents();
                                            this.agent_message =
                                                Some("Agent 诊断日志已清除".into());
                                        }
                                        Err(error) => {
                                            this.agent_message =
                                                Some(format!("无法清除诊断日志：{error}").into());
                                        }
                                    }
                                    cx.notify();
                                })),
                        ),
                ),
        );
        for profile in self.agent_diagnostics.profiles.clone() {
            rows = rows.child(self.separator(cx)).child(
                v_flex()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_sm().font_medium().child(profile.profile_name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(profile.status),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(profile.resolved_command.unwrap_or(profile.command)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {} · {} 项能力",
                                profile.protocol,
                                profile.version.as_deref().unwrap_or("版本待测试"),
                                profile.capabilities.len()
                            )),
                    ),
            );
        }
        for entry in self
            .agent_diagnostics
            .recent_entries
            .iter()
            .rev()
            .filter(|entry| matches!(entry.level.as_str(), "error" | "warning"))
            .take(8)
        {
            rows = rows.child(self.separator(cx)).child(
                v_flex()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().danger)
                                    .child(entry.event.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(entry.level.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(entry.message.clone()),
                    ),
            );
        }
        self.section(
            "诊断",
            vec![rows.into_any_element()],
            Some("日志会遮盖常见 token、密码和 URL 内嵌凭据。"),
            cx,
        )
    }

    fn render_default_agent(&self, profile: AgentProfile, cx: &mut Context<Self>) -> AnyElement {
        let status = self.agent_tests.get(&profile.id).cloned();
        let test_profile = profile.clone();
        let test_button = Button::new(SharedString::from(format!(
            "agent-test-default-{}",
            profile.id
        )))
        .small()
        .outline()
        .label(
            if self.agent_testing_id.as_deref() == Some(profile.id.as_str()) {
                "测试中…"
            } else {
                "测试连接"
            },
        )
        .loading(self.agent_testing_id.as_deref() == Some(profile.id.as_str()))
        .disabled(self.agent_testing_id.as_deref() == Some(profile.id.as_str()))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.test_saved_agent(test_profile.clone(), cx);
        }))
        .into_any_element();
        v_flex()
            .gap_2p5()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child("默认 Agent"),
            )
            .child(
                v_flex()
                    .gap_3()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(SettingsPage::Agent.tint().opacity(0.32))
                    .bg(SettingsPage::Agent.tint().opacity(0.06))
                    .p_4()
                    .child(
                        h_flex()
                            .items_start()
                            .gap_4()
                            .child(
                                div()
                                    .size(px(36.))
                                    .flex_shrink_0()
                                    .rounded(px(9.))
                                    .bg(SettingsPage::Agent.tint().opacity(0.16))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::new(IconName::Bot)
                                            .size_5()
                                            .text_color(SettingsPage::Agent.tint()),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .child(profile.name.clone()),
                                            )
                                            .child(self.agent_badge("默认", cx.theme().primary))
                                            .child(self.agent_badge(
                                                profile.transport.display_label(),
                                                cx.theme().muted_foreground,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family("monospace")
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(agent_connection_target(&profile)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "工作目录：{} · 权限：{}",
                                                working_directory_detail(&profile),
                                                profile.permission.label()
                                            )),
                                    ),
                            )
                            .child(test_button),
                    )
                    .when_some(status, |this, result| {
                        this.child(self.render_agent_test_result(result, cx))
                    }),
            )
            .into_any_element()
    }

    fn render_agent_profile_row(
        &self,
        profile: AgentProfile,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_default =
            self.agent_config.default_profile_id.as_deref() == Some(profile.id.as_str());
        let status = self.agent_tests.get(&profile.id).cloned();
        let edit_profile = profile.clone();
        let duplicate_id = profile.id.clone();
        let default_id = profile.id.clone();
        let delete_profile = profile.clone();
        let test_profile = profile.clone();
        let testing = self.agent_testing_id.as_deref() == Some(profile.id.as_str());
        v_flex()
            .gap_3()
            .px_4()
            .py_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div().text_sm().font_medium().child(profile.name.clone()),
                                    )
                                    .when(is_default, |this| {
                                        this.child(self.agent_badge("默认", cx.theme().primary))
                                    })
                                    .child(self.agent_badge(
                                        profile.transport.display_label(),
                                        SettingsPage::Agent.tint(),
                                    ))
                                    .when(!profile.transport.supports_session(), |this| {
                                        this.child(self.agent_badge("仅测试", cx.theme().warning))
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(agent_connection_target(&profile)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} · {} · {}{}",
                                        profile.permission.label(),
                                        working_directory_detail(&profile),
                                        if profile.has_stored_credential() {
                                            "凭据已保存"
                                        } else {
                                            "无保存凭据"
                                        },
                                        if profile.credential_env_var.is_some() {
                                            " · 环境变量注入"
                                        } else {
                                            ""
                                        }
                                    )),
                            )
                            .when_some(status, |this, result| {
                                this.child(self.render_agent_test_result(result, cx))
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-test-{}",
                                    profile.id
                                )))
                                .small()
                                .ghost()
                                .label(if testing { "测试中…" } else { "测试" })
                                .loading(testing)
                                .disabled(testing)
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.test_saved_agent(test_profile.clone(), cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-edit-{}",
                                    profile.id
                                )))
                                .small()
                                .outline()
                                .label("编辑")
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, window, cx| {
                                        this.open_edit_agent(edit_profile.clone(), window, cx);
                                    },
                                )),
                            )
                            .when(profile.transport.supports_profile_save(), |this| {
                                this.child(
                                    Button::new(SharedString::from(format!(
                                        "agent-duplicate-{}",
                                        profile.id
                                    )))
                                    .small()
                                    .ghost()
                                    .label("复制")
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.duplicate_agent(&duplicate_id, window, cx);
                                        },
                                    )),
                                )
                            })
                            .when(!is_default && profile.transport.can_be_default(), |this| {
                                this.child(
                                    Button::new(SharedString::from(format!(
                                        "agent-default-{}",
                                        profile.id
                                    )))
                                    .small()
                                    .ghost()
                                    .label("设为默认")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.set_default_agent(&default_id, cx);
                                    })),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!(
                                    "agent-delete-{}",
                                    profile.id
                                )))
                                .small()
                                .ghost()
                                .label("删除")
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.begin_delete_agent(delete_profile.clone(), cx);
                                    },
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_agent_delete_prompt(
        &self,
        pending: AgentPendingDelete,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile_name = pending.profile.name.clone();
        let mut body = v_flex()
            .gap_3()
            .rounded(px(12.))
            .border_1()
            .border_color(cx.theme().danger.opacity(0.45))
            .bg(cx.theme().danger.opacity(0.06))
            .p_4()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(cx.theme().danger)
                    .child(format!("删除 {profile_name}？")),
            );
        if pending.replacements.is_empty() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.agent_config.profiles.len() > 1 {
                        "删除后将没有可运行的默认 Agent；其余配置仅用于连接测试。"
                    } else {
                        "这会让 Agent 列表变为空。"
                    }),
            );
        } else {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("这是当前默认 Agent。请选择新的默认 Agent，然后删除。"),
            );
            let mut choices = v_flex().gap_1();
            for candidate in pending.replacements.clone() {
                let id = candidate.id.clone();
                choices = choices.child(
                    Button::new(SharedString::from(format!(
                        "agent-delete-replacement-{}",
                        candidate.id
                    )))
                    .small()
                    .outline()
                    .label(format!("删除并设为 {}", candidate.name))
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.confirm_delete_agent(Some(id.clone()), cx);
                        },
                    )),
                );
            }
            body = body.child(choices);
        }
        if pending.replacements.is_empty() {
            body = body.child(
                Button::new("agent-delete-confirm")
                    .small()
                    .danger()
                    .label("确认删除")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.confirm_delete_agent(None, cx);
                    })),
            );
        }
        body.child(
            Button::new("agent-delete-cancel")
                .small()
                .ghost()
                .label("取消")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.agent_pending_delete = None;
                    cx.notify();
                })),
        )
        .into_any_element()
    }

    fn agent_badge(&self, label: &str, color: Hsla) -> AnyElement {
        div()
            .rounded_full()
            .bg(color.opacity(0.12))
            .px_2()
            .py_0p5()
            .text_xs()
            .text_color(color)
            .child(label.to_string())
            .into_any_element()
    }

    fn render_agent_test_result(&self, result: AgentConnectionResult, cx: &App) -> AnyElement {
        let color = agent_status_color(result.status, cx);
        let mut details = Vec::new();
        if let Some(name) = result.agent_name.clone() {
            details.push(name);
        }
        if let Some(version) = result.agent_version.clone() {
            details.push(format!("版本 {version}"));
        }
        if let Some(protocol) = result.protocol_version {
            details.push(format!("ACP {protocol}"));
        }
        if !result.capabilities.is_empty() {
            details.push(format!("能力：{}", result.capabilities.join("、")));
        }
        let detail = if details.is_empty() {
            result.message.clone()
        } else {
            format!("{} · {}", result.message, details.join(" · "))
        };
        let session_notice = (!result.session_supported).then_some(
            "此接入方式仅用于连接测试，无论结果如何都不能启动 Agent 会话；请改用 ACP / JSON-RPC stdio。",
        );
        v_flex()
            .gap_1()
            .rounded(px(8.))
            .bg(color.opacity(0.08))
            .px_2p5()
            .py_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(if result.success {
                            IconName::CircleCheck
                        } else {
                            IconName::TriangleAlert
                        })
                        .size_3p5()
                        .text_color(color),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(color)
                            .child(result.status.label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} ms", result.elapsed_ms)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(relative(1.35))
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
            .when_some(session_notice, |this, notice| {
                this.child(
                    div()
                        .text_xs()
                        .line_height(relative(1.35))
                        .text_color(cx.theme().warning)
                        .child(notice),
                )
            })
            .into_any_element()
    }

    fn render_agent_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.agent_editor.as_ref() else {
            return div().into_any_element();
        };

        let name = Input::new(&editor.name_input)
            .aria_label("Agent 名称")
            .cleanable(true)
            .w_full();
        let executable = Input::new(&editor.executable_input)
            .aria_label("Agent 可执行文件")
            .cleanable(true)
            .w_full();
        let endpoint = Input::new(&editor.endpoint_input)
            .aria_label("ACP HTTP 端点")
            .cleanable(true)
            .w_full();
        let arguments = Textarea::new(&editor.arguments_input).h(px(74.)).w_full();
        let environment = Textarea::new(&editor.environment_input).h(px(74.)).w_full();
        let model = Input::new(&editor.model_input)
            .aria_label("Agent 模型")
            .cleanable(true)
            .w_full();
        let working_directory = Input::new(&editor.working_directory_input)
            .aria_label("固定工作目录")
            .cleanable(true)
            .w_full();
        let credential_env = Input::new(&editor.credential_env_input)
            .aria_label("凭据环境变量")
            .cleanable(true)
            .w_full();
        let credential = Input::new(&editor.credential_input)
            .aria_label("Agent 凭据")
            .cleanable(true)
            .w_full();
        let timeout = Input::new(&editor.timeout_input)
            .aria_label("连接超时秒数")
            .w_full();
        let output_limit = Input::new(&editor.output_limit_input)
            .aria_label("输出上限 KiB")
            .w_full();

        let transport_choices = h_flex()
            .gap_1()
            .child(
                Button::new("agent-transport-acp")
                    .ghost()
                    .compact()
                    .label(agent_transport_choice_label(AgentTransport::AcpStdio))
                    .selected(editor.transport == AgentTransport::AcpStdio)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.transport = AgentTransport::AcpStdio;
                            editor.test_result = None;
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("agent-transport-http")
                    .ghost()
                    .compact()
                    .label(agent_transport_choice_label(AgentTransport::AcpHttp))
                    .selected(editor.transport == AgentTransport::AcpHttp)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.transport = AgentTransport::AcpHttp;
                            editor.test_result = None;
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("agent-transport-cli")
                    .ghost()
                    .compact()
                    .label(agent_transport_choice_label(AgentTransport::Cli))
                    .selected(editor.transport == AgentTransport::Cli)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.transport = AgentTransport::Cli;
                            editor.permission = PermissionPolicy::AgentManaged;
                            editor.test_result = None;
                        }
                        cx.notify();
                    })),
            )
            .into_any_element();

        let permission_choices = h_flex()
            .gap_1()
            .child(
                Button::new("agent-permission-readonly")
                    .ghost()
                    .compact()
                    .label("只读")
                    .selected(editor.permission == PermissionPolicy::ReadOnly)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.permission = PermissionPolicy::ReadOnly;
                            editor.test_result = None;
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("agent-permission-confirm")
                    .ghost()
                    .compact()
                    .label("写入前确认")
                    .selected(editor.permission == PermissionPolicy::ConfirmWrites)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.permission = PermissionPolicy::ConfirmWrites;
                            editor.test_result = None;
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("agent-permission-managed")
                    .ghost()
                    .compact()
                    .label("由 Agent 管理")
                    .selected(editor.permission == PermissionPolicy::AgentManaged)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.permission = PermissionPolicy::AgentManaged;
                            editor.test_result = None;
                        }
                        cx.notify();
                    })),
            )
            .into_any_element();

        let workspace_choices = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .child(agent_working_directory_button(
                        "agent-wd-selected",
                        "所选项目",
                        editor.working_directory_policy
                            == AgentWorkingDirectoryChoice::SelectedItem,
                        AgentWorkingDirectoryChoice::SelectedItem,
                        cx,
                    ))
                    .child(agent_working_directory_button(
                        "agent-wd-parent",
                        "文件所在目录",
                        editor.working_directory_policy == AgentWorkingDirectoryChoice::FileParent,
                        AgentWorkingDirectoryChoice::FileParent,
                        cx,
                    ))
                    .child(agent_working_directory_button(
                        "agent-wd-repository",
                        "Git 根目录",
                        editor.working_directory_policy
                            == AgentWorkingDirectoryChoice::RepositoryRoot,
                        AgentWorkingDirectoryChoice::RepositoryRoot,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(agent_working_directory_button(
                        "agent-wd-fixed",
                        "固定目录",
                        editor.working_directory_policy == AgentWorkingDirectoryChoice::Fixed,
                        AgentWorkingDirectoryChoice::Fixed,
                        cx,
                    ))
                    .child(agent_working_directory_button(
                        "agent-wd-ask",
                        "每次询问",
                        editor.working_directory_policy == AgentWorkingDirectoryChoice::Ask,
                        AgentWorkingDirectoryChoice::Ask,
                        cx,
                    )),
            )
            .into_any_element();

        let mut basic_rows = vec![
            self.agent_field(
                "agent-field-name",
                "名称",
                Some("在设置、菜单和 Agent 栏中显示；名称必须唯一。"),
                name.into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.agent_field(
                "agent-field-transport",
                "接入方式",
                Some("只有 ACP / JSON-RPC stdio 可保存并启动会话；ACP HTTP 和通用 CLI 仅执行连接测试。"),
                transport_choices,
                cx,
            ),
        ];
        if editor.transport == AgentTransport::AcpHttp {
            basic_rows.push(self.separator(cx));
            basic_rows.push(self.agent_field(
                "agent-field-endpoint",
                "ACP HTTP 端点",
                Some("使用绝对 HTTP 或 HTTPS URL。凭据通过系统钥匙串和 Authorization 头注入。"),
                endpoint.into_any_element(),
                cx,
            ));
        } else {
            basic_rows.push(self.separator(cx));
            basic_rows.push(self.agent_field(
                "agent-field-executable",
                "可执行文件",
                Some("使用绝对路径，或只填写 PATH 中的命令名称。参数不会经过 shell。"),
                executable.into_any_element(),
                cx,
            ));
            basic_rows.push(self.separator(cx));
            basic_rows.push(self.agent_field(
                "agent-field-arguments",
                "启动参数",
                Some("每行一个参数。禁止在参数中直接填写 token、密码或带凭据的 URL。"),
                arguments.into_any_element(),
                cx,
            ));
            basic_rows.push(self.separator(cx));
            basic_rows.push(self.agent_field(
                "agent-field-environment",
                "环境变量",
                Some("每行 KEY=VALUE。敏感环境变量会被拒绝，请使用下方凭据栏。"),
                environment.into_any_element(),
                cx,
            ));
            basic_rows.push(self.separator(cx));
            basic_rows.push(self.agent_field(
                "agent-field-model",
                "模型",
                Some("可选。留空时由 Agent 自身配置决定。"),
                model.into_any_element(),
                cx,
            ));
        }

        let mut workspace_rows = vec![self.agent_field(
            "agent-field-working-directory-policy",
            "工作目录策略",
            Some("设置页无法获知本次选中项；固定目录会在保存时校验。"),
            workspace_choices,
            cx,
        )];
        if editor.working_directory_policy == AgentWorkingDirectoryChoice::Fixed {
            workspace_rows.push(self.separator(cx));
            workspace_rows.push(self.agent_field(
                "agent-field-working-directory",
                "固定工作目录",
                Some("保存前会确认目录存在。"),
                working_directory.into_any_element(),
                cx,
            ));
        }
        workspace_rows.push(self.separator(cx));
        workspace_rows.push(self.agent_field(
            "agent-field-permission",
            "权限摘要",
            Some(editor.permission.summary()),
            permission_choices,
            cx,
        ));

        let has_new_credential = !editor.credential_input.read(cx).value().trim().is_empty();
        let credential_status = if has_new_credential {
            div()
                .text_xs()
                .text_color(cx.theme().success)
                .child("保存后会写入系统钥匙串并替换现有凭据。")
                .into_any_element()
        } else if editor.clear_credential {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child("保存时会从系统钥匙串删除凭据。"),
                )
                .child(
                    Button::new("agent-credential-undo-clear")
                        .small()
                        .ghost()
                        .label("撤销")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(editor) = this.agent_editor.as_mut() {
                                editor.clear_credential = false;
                            }
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else if editor.has_stored_credential {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().success)
                        .child("已保存到系统钥匙串。留空会保留现有凭据。"),
                )
                .child(
                    Button::new("agent-credential-clear")
                        .small()
                        .ghost()
                        .label("清除已保存凭据")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(editor) = this.agent_editor.as_mut() {
                                editor.clear_credential = true;
                            }
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("尚未保存凭据。输入后会写入系统钥匙串，不会进入 agent.json。")
                .into_any_element()
        };

        let capability_rows = vec![
            self.row(
                "agent-capability-selection",
                "读取选中项",
                Some("允许 Agent 使用本次显式提供的文件或目录上下文。"),
                Switch::new("agent-cap-selection")
                    .checked(editor.capabilities.read_selection)
                    .small()
                    .accessibility_label("读取选中项")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.capabilities.read_selection = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.row(
                "agent-capability-workspace",
                "读取工作区",
                Some("声明 Agent 是否需要读取工作目录中的其他文件。"),
                Switch::new("agent-cap-workspace")
                    .checked(editor.capabilities.read_workspace)
                    .small()
                    .accessibility_label("读取工作区")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.capabilities.read_workspace = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.row(
                "agent-capability-permissions",
                "权限请求",
                Some("允许 Agent 在需要额外权限时先向用户请求确认。"),
                Switch::new("agent-cap-permissions")
                    .checked(editor.capabilities.permission_requests)
                    .small()
                    .accessibility_label("权限请求")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.capabilities.permission_requests = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.row(
                "agent-capability-write",
                "写入文件",
                Some("参与受管权限请求过滤；不代表 Kite 可以沙箱化任意 Agent。"),
                Switch::new("agent-cap-write")
                    .checked(editor.capabilities.write_files)
                    .small()
                    .accessibility_label("写入文件")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.capabilities.write_files = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.row(
                "agent-capability-commands",
                "执行命令",
                Some("参与受管权限请求过滤；Kite 不会自动执行 Agent 的建议命令。"),
                Switch::new("agent-cap-commands")
                    .checked(editor.capabilities.execute_commands)
                    .small()
                    .accessibility_label("执行命令")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.capabilities.execute_commands = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.row(
                "agent-capability-network",
                "网络访问",
                Some("参与受管网络授权过滤；Kite 不做强制网络隔离。"),
                Switch::new("agent-cap-network")
                    .checked(editor.capabilities.network)
                    .small()
                    .accessibility_label("网络访问")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.agent_editor.as_mut() {
                            editor.capabilities.network = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ),
        ];

        let credential_rows = vec![
            self.agent_field(
                "agent-field-credential",
                "Token / 密码",
                Some("仅用于本次测试或保存到系统钥匙串。测试不会保存表单值。"),
                v_flex()
                    .gap_2()
                    .child(credential)
                    .child(credential_status)
                    .into_any_element(),
                cx,
            ),
            self.separator(cx),
            self.agent_field(
                "agent-field-credential-env",
                "凭据环境变量",
                Some("启动 Agent 时把钥匙串中的凭据注入此环境变量；默认 KITE_AGENT_TOKEN。"),
                credential_env.into_any_element(),
                cx,
            ),
        ];

        let advanced_rows = vec![
            self.agent_field(
                "agent-field-limits",
                "超时与输出上限",
                Some("超时单位为秒；输出上限单位为 KiB。ACP 响应超过上限会中止测试。"),
                h_flex()
                    .gap_3()
                    .child(v_flex().flex_1().gap_1().child("超时").child(timeout))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child("输出上限")
                            .child(output_limit),
                    )
                    .into_any_element(),
                cx,
            ),
        ];

        let mut body = v_flex()
            .gap_5()
            .child(self.section("基本", basic_rows, None, cx))
            .child(self.section("工作目录与权限", workspace_rows, None, cx))
            .child(self.section("能力声明", capability_rows, None, cx))
            .child(self.section("凭据边界", credential_rows, None, cx))
            .child(self.section("高级", advanced_rows, None, cx));
        if !editor.transport.supports_session() {
            let capability = editor.transport.capabilities();
            let note = capability
                .limitation
                .unwrap_or_else(|| capability.summary.clone());
            body = body.child(
                div()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.45))
                    .bg(cx.theme().warning.opacity(0.07))
                    .p_3()
                    .text_xs()
                    .line_height(relative(1.4))
                    .text_color(cx.theme().warning)
                    .child(format!("{} {}", capability.summary, note)),
            );
        }
        if let Some(result) = editor.test_result.clone() {
            body = body.child(self.render_agent_test_result(result, cx));
        }
        if let Some(error) = editor.error.clone() {
            body = body.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .rounded(px(8.))
                    .bg(cx.theme().danger.opacity(0.08))
                    .p_2p5()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size_4()
                            .text_color(cx.theme().danger),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(error),
                    ),
            );
        }

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .p_5()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background.opacity(0.94))
            .child(
                v_flex()
                    .w(px(720.))
                    .h_full()
                    .max_h(px(760.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_lg()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .px_5()
                            .py_4()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div().text_lg().font_semibold().child(if editor.is_new {
                                            "添加 Agent"
                                        } else {
                                            "编辑 Agent"
                                        }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("测试使用当前表单值，但不会保存；只有“保存”会写入 agent.json。"),
                                    ),
                            )
                            .child(
                                Button::new("agent-editor-close")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.close_agent_editor(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(div().p_5().child(body)),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .gap_3()
                            .px_5()
                            .py_4()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("仅 ACP / JSON-RPC stdio 可保存为可运行配置；其他接入方式只执行连接测试。"),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("agent-editor-cancel")
                                            .outline()
                                            .label("取消")
                                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                                this.close_agent_editor(window, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("agent-editor-test")
                                            .outline()
                                            .label(if editor.testing { "测试中…" } else { "测试连接" })
                                            .loading(editor.testing)
                                            .disabled(editor.testing || !editor.transport.supports_connection_test())
                                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                this.test_agent_editor(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("agent-editor-save")
                                            .primary()
                                            .label("保存")
                                            .disabled(!editor.transport.supports_profile_save())
                                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                                this.save_agent_editor(window, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_file_handling(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let set_default = Button::new("set-default-explorer")
            .small()
            .outline()
            .label("设为默认…")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.main_app.update(cx, |app, cx| {
                    app.set_default_explorer_from_settings(window, cx);
                });
            }))
            .into_any_element();
        let restore_default = Button::new("restore-default-explorer")
            .small()
            .outline()
            .label("恢复…")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.main_app.update(cx, |app, cx| {
                    app.restore_default_explorer_from_settings(window, cx);
                });
            }))
            .into_any_element();
        let shortcut = Button::new("create-start-menu-shortcut")
            .small()
            .outline()
            .label("创建快捷方式")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.main_app.update(cx, |app, cx| {
                    app.create_start_menu_shortcut_from_settings(window, cx);
                });
            }))
            .into_any_element();

        let mut content = v_flex()
            .gap_6()
            .child(self.section(
                "默认资源管理器",
                vec![
                    self.row(
                        "file-default",
                        "使用 Kite 打开文件夹",
                        Some("写入当前用户的文件夹打开命令。"),
                        set_default,
                        cx,
                    ),
                    self.separator(cx),
                    self.row(
                        "file-restore",
                        "恢复之前的打开方式",
                        Some("恢复 Kite 保存的命令。"),
                        restore_default,
                        cx,
                    ),
                ],
                Some("Windows 桌面、任务栏和文件对话框仍由 Windows 管理。"),
                cx,
            ))
            .child(self.render_tool_settings(cx))
            .child(self.section(
                "系统入口",
                vec![self.row(
                    "file-shortcut",
                    "开始菜单快捷方式",
                    Some("创建当前用户的 Kite 入口。"),
                    shortcut,
                    cx,
                )],
                None,
                cx,
            ));
        if let Some(status) = self.status.clone() {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(status),
            );
        }
        content.into_any_element()
    }

    fn render_tool_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let definitions = builtin_tool_definitions(ToolPlatform::Windows)
            .into_iter()
            .filter(|tool| {
                tool.platform == ToolPlatform::Any || tool.platform == ToolPlatform::Windows
            })
            .collect::<Vec<_>>();
        let tool_ids = definitions
            .iter()
            .map(|tool| tool.tool_id.clone())
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        for (index, tool) in definitions.iter().enumerate() {
            if index > 0 {
                rows.push(self.separator(cx));
            }
            let tool_id = tool.tool_id.clone();
            let pinned = self.preferences.tool_policy.is_pinned(&tool_id);
            let hidden = self.preferences.tool_policy.is_hidden(&tool_id);
            let display_name = tool.display_name.clone();
            let description = tool.description.clone();
            let pin_id = tool_id.clone();
            let hide_id = tool_id.clone();
            let up_id = tool_id.clone();
            let down_id = tool_id.clone();
            let up_ids = tool_ids.clone();
            let down_ids = tool_ids.clone();
            let controls = h_flex()
                .gap_2()
                .child(
                    Switch::new(SharedString::from(format!("tool-pin-{tool_id}")))
                        .checked(pinned)
                        .small()
                        .accessibility_label(format!("固定 {display_name}"))
                        .on_click(cx.listener(move |this, checked, window, cx| {
                            this.preferences.tool_policy.set_pinned(&pin_id, *checked);
                            this.save_preferences(window, cx);
                        })),
                )
                .child(
                    Switch::new(SharedString::from(format!("tool-hide-{tool_id}")))
                        .checked(hidden)
                        .small()
                        .accessibility_label(format!("隐藏 {display_name}"))
                        .on_click(cx.listener(move |this, checked, window, cx| {
                            this.preferences.tool_policy.set_hidden(&hide_id, *checked);
                            this.save_preferences(window, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("tool-up-{tool_id}")))
                        .ghost()
                        .compact()
                        .label("↑")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            move_tool_in_policy(
                                &mut this.preferences.tool_policy,
                                &up_id,
                                -1,
                                &up_ids,
                            );
                            this.save_preferences(window, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("tool-down-{tool_id}")))
                        .ghost()
                        .compact()
                        .label("↓")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            move_tool_in_policy(
                                &mut this.preferences.tool_policy,
                                &down_id,
                                1,
                                &down_ids,
                            );
                            this.save_preferences(window, cx);
                        })),
                )
                .into_any_element();
            rows.push(self.dynamic_row(
                format!("tool-policy-{tool_id}"),
                display_name,
                Some(description),
                controls,
                cx,
            ));
        }
        let tool_controls = h_flex()
            .gap_2()
            .child(
                Button::new("tool-refresh-discovery")
                    .small()
                    .outline()
                    .label("重新发现")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        crate::context_menu::tool_registry_windows::refresh();
                        this.status = Some("工具发现缓存已清除".into());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tool-clear-recent")
                    .small()
                    .outline()
                    .label("清空最近使用")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.preferences.tool_policy.recent_tool_ids.clear();
                        this.save_preferences(window, cx);
                    })),
            )
            .into_any_element();
        rows.push(self.separator(cx));
        rows.push(self.row(
            "tool-recent-policy",
            "最近使用",
            Some("只影响工具子菜单内部顺序，不改变一级菜单顺序。"),
            tool_controls,
            cx,
        ));
        self.section(
            "工具",
            rows,
            Some("固定工具优先；隐藏工具不出现在“在工具中打开”和“比较…”中。启动工具时使用精确参数数组，不经过 Shell。"),
            cx,
        )
    }

    fn render_updates(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let update_in_progress = self.update_state.in_progress;
        let check = Button::new("check-updates")
            .small()
            .outline()
            .icon(IconName::RefreshCw)
            .label("检查更新…")
            .loading(update_in_progress)
            .disabled(update_in_progress)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.main_app.update(cx, |app, cx| {
                    app.check_for_updates_from_settings(window, cx);
                });
            }))
            .into_any_element();
        let log = Button::new("show-update-log")
            .small()
            .outline()
            .label("打开日志位置")
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                if let Err(error) = open_update_log() {
                    this.status = Some(error.into());
                    cx.notify();
                }
            }))
            .into_any_element();

        let mut rows = vec![self.row(
            "updates-version",
            "当前版本",
            Some(env!("CARGO_PKG_VERSION")),
            check,
            cx,
        )];
        if let (Some(version), Some(current)) = (
            self.update_state.available_version.clone(),
            self.update_state.current_version.clone(),
        ) {
            let install = Button::new("download-update")
                .small()
                .primary()
                .label("下载并安装")
                .loading(update_in_progress)
                .disabled(update_in_progress)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.main_app.update(cx, |app, cx| {
                        app.start_update_download_from_settings(window, cx);
                    });
                }))
                .into_any_element();
            let notes = Button::new("show-update-release-notes")
                .small()
                .outline()
                .label("查看发布说明")
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.main_app.update(cx, |app, cx| {
                        app.open_update_release_notes_from_settings(cx)
                    });
                }))
                .into_any_element();
            rows.push(self.separator(cx));
            rows.push(self.update_available_row(version, current, install, notes, cx));
            if let Some(notes) = self.update_state.notes.clone() {
                rows.push(self.separator(cx));
                rows.push(self.update_notes_row(notes, cx));
            }
        }

        if let Some(progress) = self.update_state.download_progress {
            rows.push(self.separator(cx));
            rows.push(self.update_progress_row(progress, cx));
        } else if (self.update_state.available_version.is_none()
            || self.update_state.status_is_error)
            && let Some(status) = self.update_state.status.clone()
        {
            rows.push(self.separator(cx));
            rows.push(self.update_status_row(status, self.update_state.status_is_error, cx));
        }
        rows.push(self.separator(cx));
        rows.push(self.row(
            "updates-log",
            "更新诊断日志",
            Some("查看下载、校验和安装记录。"),
            log,
            cx,
        ));

        v_flex()
            .gap_6()
            .child(self.section(
                "版本",
                rows,
                Some("Kite 只在用户选择“检查更新…”后访问更新服务。"),
                cx,
            ))
            .into_any_element()
    }

    fn render_about(&self, cx: &App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(
                h_flex()
                    .gap_5()
                    .items_center()
                    .py_1()
                    .child(
                        div()
                            .size(px(74.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(18.))
                            .bg(cx.theme().primary.opacity(0.14))
                            .border_1()
                            .border_color(cx.theme().primary.opacity(0.18))
                            .child(
                                Icon::new(IconName::Wind)
                                    .size(px(48.))
                                    .text_color(cx.theme().primary),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_3xl().font_semibold().child("Kite"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("版本 {}", env!("CARGO_PKG_VERSION"))),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("原生 Windows 文件导航器"),
                            ),
                    ),
            )
            .child(self.section(
                "产品",
                vec![
                    self.info_row(
                        "about-navigation",
                        "导航先于保管",
                        "Kite 把文件系统显示为可牵引的层级路径。",
                        cx,
                    ),
                    self.separator(cx),
                    self.info_row(
                        "about-system-integration",
                        "系统集成",
                        "Kite 支持文件剪贴板、回收站和系统右键菜单。",
                        cx,
                    ),
                    self.separator(cx),
                    self.info_row(
                        "about-local",
                        "本地优先",
                        "Kite 不上传文件内容，也不收集浏览记录。",
                        cx,
                    ),
                ],
                None,
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("MIT License · Kite {}", env!("CARGO_PKG_VERSION"))),
            )
            .into_any_element()
    }

    fn info_row(
        &self,
        id: &'static str,
        title: &'static str,
        detail: &'static str,
        cx: &App,
    ) -> AnyElement {
        v_flex()
            .id(id)
            .w_full()
            .gap_1()
            .px_4()
            .py_3()
            .child(div().text_sm().font_medium().child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
            .into_any_element()
    }

    fn update_status_row(&self, status: SharedString, is_error: bool, cx: &App) -> AnyElement {
        let color = if is_error {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        h_flex()
            .id("updates-status")
            .w_full()
            .gap_3()
            .items_start()
            .px_4()
            .py_3()
            .child(
                Icon::new(if is_error {
                    IconName::TriangleAlert
                } else {
                    IconName::Info
                })
                .size_4()
                .mt(px(1.))
                .text_color(color),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .line_height(relative(1.35))
                    .text_color(color)
                    .child(status),
            )
            .into_any_element()
    }

    fn update_available_row(
        &self,
        version: SharedString,
        current: SharedString,
        install: AnyElement,
        release_notes: AnyElement,
        cx: &App,
    ) -> AnyElement {
        h_flex()
            .id("updates-available")
            .w_full()
            .gap_6()
            .items_center()
            .px_4()
            .py_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(format!("Kite {version} 可用")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("当前版本：{current}")),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .child(release_notes)
                    .child(install),
            )
            .into_any_element()
    }

    fn update_notes_row(&self, notes: SharedString, cx: &App) -> AnyElement {
        v_flex()
            .id("updates-notes")
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .child(div().text_sm().font_medium().child("更新内容"))
            .child(
                div()
                    .text_xs()
                    .line_height(relative(1.45))
                    .text_color(cx.theme().muted_foreground)
                    .child(notes),
            )
            .into_any_element()
    }

    fn update_progress_row(
        &self,
        progress: crate::updater::UpdateDownloadProgress,
        cx: &App,
    ) -> AnyElement {
        let status = self
            .update_state
            .status
            .clone()
            .unwrap_or_else(|| "正在下载 Kite 更新…".into());
        v_flex()
            .id("updates-progress")
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .child(div().text_sm().child(status))
            .child(
                Progress::new("settings-update-progress")
                    .when_some(progress.fraction(), |this, fraction| {
                        this.value((fraction * 100.0) as f32)
                    })
                    .when(progress.fraction().is_none(), |this| this.loading(true))
                    .color(cx.theme().primary)
                    .with_size(Size::Small)
                    .accessibility_label("Kite 更新下载进度"),
            )
            .into_any_element()
    }

    fn section(
        &self,
        title: &'static str,
        rows: Vec<AnyElement>,
        footer: Option<&'static str>,
        cx: &App,
    ) -> AnyElement {
        v_flex()
            .gap_2p5()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .child(
                v_flex()
                    .w_full()
                    .overflow_hidden()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().title_bar.opacity(0.86))
                    .children(rows),
            )
            .when_some(footer, |this, footer| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(footer),
                )
            })
            .into_any_element()
    }

    fn agent_field(
        &self,
        id: &'static str,
        title: &'static str,
        detail: Option<&'static str>,
        control: AnyElement,
        cx: &App,
    ) -> AnyElement {
        v_flex()
            .id(id)
            .w_full()
            .gap_1p5()
            .px_4()
            .py_3()
            .child(div().text_sm().font_medium().child(title))
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
            })
            .child(div().w_full().child(control))
            .into_any_element()
    }

    fn row(
        &self,
        id: &'static str,
        title: &'static str,
        detail: Option<&'static str>,
        control: AnyElement,
        cx: &App,
    ) -> AnyElement {
        h_flex()
            .id(id)
            .w_full()
            .min_h(px(58.))
            .gap_6()
            .items_center()
            .px_4()
            .py_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().font_medium().child(title))
                    .when_some(detail, |this, detail| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(detail),
                        )
                    }),
            )
            .child(div().flex_shrink_0().child(control))
            .into_any_element()
    }

    fn dynamic_row(
        &self,
        id: String,
        title: String,
        detail: Option<String>,
        control: AnyElement,
        cx: &App,
    ) -> AnyElement {
        h_flex()
            .id(SharedString::from(id))
            .w_full()
            .min_h(px(58.))
            .gap_6()
            .items_center()
            .px_4()
            .py_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().font_medium().child(title))
                    .when_some(detail, |this, detail| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(detail),
                        )
                    }),
            )
            .child(div().flex_shrink_0().child(control))
            .into_any_element()
    }

    fn separator(&self, cx: &App) -> AnyElement {
        div()
            .ml(px(16.))
            .h(px(1.))
            .bg(cx.theme().border)
            .into_any_element()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self
            .agent_editor
            .is_some()
            .then(|| self.render_agent_editor(cx));
        let content = h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(
                        v_flex()
                            .w_full()
                            .gap_6()
                            .px(px(30.))
                            .pt(px(26.))
                            .pb(px(20.))
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(self.render_header(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .overflow_y_scrollbar()
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_6()
                                    .px(px(30.))
                                    .pt(px(22.))
                                    .pb(px(30.))
                                    .child(self.render_page(window, cx)),
                            ),
                    ),
            );
        div().relative().size_full().child(content).children(editor)
    }
}

fn move_tool_in_policy(policy: &mut ToolPolicy, tool_id: &str, delta: isize, tool_ids: &[String]) {
    let mut order = policy.user_order.clone();
    for id in tool_ids {
        if !order
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(id))
        {
            order.push(id.clone());
        }
    }
    let Some(index) = order
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(tool_id))
    else {
        return;
    };
    let Some(destination) = index.checked_add_signed(delta) else {
        return;
    };
    if destination >= order.len() {
        return;
    }
    order.swap(index, destination);
    policy.user_order = order;
}

pub(crate) fn open_settings_window(
    main_app: Entity<KiteApp>,
    initial_update_state: SettingsUpdateState,
    agent_store: Arc<AgentProfileStore>,
    cx: &mut App,
) -> Result<(), String> {
    open_settings_window_at(
        main_app,
        initial_update_state,
        SettingsPage::General,
        agent_store,
        cx,
    )
}

pub(crate) fn open_agent_settings_window(
    main_app: Entity<KiteApp>,
    initial_update_state: SettingsUpdateState,
    agent_store: Arc<AgentProfileStore>,
    cx: &mut App,
) -> Result<(), String> {
    open_settings_window_at(
        main_app,
        initial_update_state,
        SettingsPage::Agent,
        agent_store,
        cx,
    )
}

fn open_settings_window_at(
    main_app: Entity<KiteApp>,
    initial_update_state: SettingsUpdateState,
    initial_page: SettingsPage,
    agent_store: Arc<AgentProfileStore>,
    cx: &mut App,
) -> Result<(), String> {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Kite 设置".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(560.)), cx)),
        window_min_size: Some(size(px(680.), px(500.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| {
            SettingsWindow::new(
                main_app.clone(),
                initial_update_state,
                initial_page,
                agent_store.clone(),
                window,
                cx,
            )
        })
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开设置窗口：{error}"))
}

fn open_update_log() -> Result<(), String> {
    let updates = dirs::data_local_dir()
        .ok_or_else(|| "无法确定更新诊断日志目录".to_string())?
        .join("Kite")
        .join("updates");
    std::fs::create_dir_all(&updates)
        .map_err(|error| format!("无法创建更新诊断日志目录：{error}"))?;
    let log = updates.join("update.log");
    let target: PathBuf = if log.is_file() { log } else { updates };
    open::that(target).map_err(|error| format!("无法打开更新诊断日志：{error}"))
}
