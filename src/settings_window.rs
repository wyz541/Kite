use std::path::PathBuf;

use crate::app::{KiteApp, SettingsUpdateState};
use crate::ui_preferences::{self, AppearancePreference, UiPreferences};
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, Size, StyledExt as _, Theme, ThemeMode,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::agent::{
    AgentConnectionInfo, AgentDiagnostics, AgentProfile, AgentProfileSnapshot, AgentStoreLoad,
    agent_config_path, agent_presets, diagnose_agents, load_agent_profiles, preset_profile,
    probe_agent_connection, save_agent_profiles,
};

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
            Self::FileHandling => "默认资源管理器",
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

pub(crate) struct SettingsWindow {
    main_app: Entity<KiteApp>,
    page: SettingsPage,
    preferences: UiPreferences,
    update_state: SettingsUpdateState,
    status: Option<SharedString>,
    agent_store: AgentStoreLoad,
    agent_diagnostics: AgentDiagnostics,
    agent_testing_profile: Option<String>,
    agent_test_result: Option<AgentConnectionInfo>,
    _agent_test_task: Task<()>,
    _main_app_subscription: Subscription,
}

impl SettingsWindow {
    fn new(
        main_app: Entity<KiteApp>,
        initial_update_state: SettingsUpdateState,
        initial_page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The main app is mutably leased while its click handler opens this window.
        // Use the caller's snapshot instead of reading the entity here.
        let preferences = ui_preferences::load_ui_preferences();
        let subscription = cx.observe_in(&main_app, window, |this, main_app, _, cx| {
            this.update_state = main_app.read(cx).settings_update_state();
            this.status = this.update_state.status.clone();
            cx.notify();
        });
        Self {
            main_app,
            page: initial_page,
            preferences,
            status: initial_update_state.status.clone(),
            update_state: initial_update_state,
            agent_store: load_agent_profiles(),
            agent_diagnostics: diagnose_agents(),
            agent_testing_profile: None,
            agent_test_result: None,
            _agent_test_task: Task::ready(()),
            _main_app_subscription: subscription,
        }
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
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.page = page;
                    cx.notify();
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

    fn save_agent_store(&mut self, store: AgentProfileSnapshot, cx: &mut Context<Self>) {
        match save_agent_profiles(store) {
            Ok(loaded) => {
                self.agent_store = loaded;
                self.agent_diagnostics = diagnose_agents();
                self.status = Some("Agent 配置已保存".into());
            }
            Err(error) => self.status = Some(error.into()),
        }
        cx.notify();
    }

    fn add_agent_preset(&mut self, preset_id: &str, cx: &mut Context<Self>) {
        let Some(mut profile) = preset_profile(preset_id, preset_id.to_string()) else {
            self.status = Some("未知的 Agent 预设".into());
            cx.notify();
            return;
        };
        let used_names = self
            .agent_store
            .store
            .profiles
            .iter()
            .map(|item| item.name.to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        if used_names.contains(&profile.name.to_ascii_lowercase()) {
            let base = profile.name.clone();
            for index in 2..100 {
                let candidate = format!("{base} {index}");
                if !used_names.contains(&candidate.to_ascii_lowercase()) {
                    profile.name = candidate;
                    break;
                }
            }
            profile.id = format!("{}-{}", profile.id, now_unix_ms());
        }
        let profile_id = profile.id.clone();
        self.agent_store.store.profiles.push(profile);
        if self.agent_store.store.default_profile_id.is_none() {
            self.agent_store.store.default_profile_id = Some(profile_id);
        }
        let store = self.agent_store.store.clone();
        self.save_agent_store(store, cx);
    }

    fn set_agent_default(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        self.agent_store.store.default_profile_id = Some(profile_id.to_string());
        let store = self.agent_store.store.clone();
        self.save_agent_store(store, cx);
    }

    fn delete_agent_profile(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        self.agent_store
            .store
            .profiles
            .retain(|profile| profile.id != profile_id);
        if self.agent_store.store.default_profile_id.as_deref() == Some(profile_id) {
            self.agent_store.store.default_profile_id = self
                .agent_store
                .store
                .profiles
                .first()
                .map(|profile| profile.id.clone());
        }
        let store = self.agent_store.store.clone();
        self.save_agent_store(store, cx);
    }

    fn test_agent_profile(
        &mut self,
        profile: AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile_id = profile.id.clone();
        self.agent_testing_profile = Some(profile_id.clone());
        self.agent_test_result = None;
        self.status = Some(format!("正在测试 {}…", profile.name).into());
        self._agent_test_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { probe_agent_connection(profile) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.agent_testing_profile = None;
                this.agent_test_result = Some(result.clone());
                this.agent_diagnostics = diagnose_agents();
                this.status = Some(if result.success {
                    format!("连接成功：{} {} ", result.agent_name, result.agent_version).into()
                } else {
                    result
                        .error
                        .clone()
                        .unwrap_or_else(|| "Agent 连接测试失败".to_string())
                        .into()
                });
                cx.notify();
            });
        });
        cx.notify();
    }

    fn render_agent(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut profile_rows = Vec::new();
        let profiles = self.agent_store.store.profiles.clone();
        let default_id = self.agent_store.store.default_profile_id.clone();
        if profiles.is_empty() {
            profile_rows.push(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_4()
                    .py_4()
                    .child(div().text_sm().font_medium().child("尚未配置 Agent"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("从下方预设添加，或通过配置文件接入自定义 ACP Agent。"),
                    )
                    .into_any_element(),
            );
        }
        for profile in profiles {
            let is_default = default_id.as_deref() == Some(profile.id.as_str());
            let is_testing = self.agent_testing_profile.as_deref() == Some(profile.id.as_str());
            let default_id = profile.id.clone();
            let test_profile = profile.clone();
            let delete_id = profile.id.clone();
            let can_delete = !is_default && self.agent_store.store.profiles.len() > 1;
            profile_rows.push(
                v_flex()
                    .w_full()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
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
                                                    .font_medium()
                                                    .child(profile.name.clone()),
                                            )
                                            .when(is_default, |this| {
                                                this.child(
                                                    div()
                                                        .text_xs()
                                                        .font_semibold()
                                                        .text_color(cx.theme().primary)
                                                        .child("默认"),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {} · {}",
                                                profile
                                                    .executable
                                                    .as_deref()
                                                    .unwrap_or("未设置命令"),
                                                profile
                                                    .model
                                                    .as_deref()
                                                    .unwrap_or("Agent 默认模型"),
                                                profile.permission.label()
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .flex_shrink_0()
                                    .when(!is_default, |this| {
                                        this.child(
                                            Button::new(format!("agent-default-{default_id}"))
                                                .ghost()
                                                .compact()
                                                .label("设为默认")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_agent_default(&default_id, cx)
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::new(format!("agent-test-{}", profile.id))
                                            .small()
                                            .outline()
                                            .label(if is_testing {
                                                "测试中…"
                                            } else {
                                                "测试连接"
                                            })
                                            .loading(is_testing)
                                            .disabled(self.agent_testing_profile.is_some())
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.test_agent_profile(
                                                    test_profile.clone(),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .when(can_delete, |this| {
                                        this.child(
                                            Button::new(format!("agent-delete-{delete_id}"))
                                                .ghost()
                                                .compact()
                                                .label("删除")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.delete_agent_profile(&delete_id, cx)
                                                })),
                                        )
                                    }),
                            ),
                    )
                    .into_any_element(),
            );
            profile_rows.push(self.separator(cx));
        }
        if profile_rows.last().is_some() {
            profile_rows.pop();
        }

        let mut preset_buttons = h_flex().w_full().gap_2().flex_wrap();
        for preset in agent_presets() {
            let preset_id = preset.id.clone();
            preset_buttons = preset_buttons.child(
                Button::new(format!("agent-preset-{preset_id}"))
                    .small()
                    .outline()
                    .icon(IconName::Plus)
                    .label(format!("添加 {}", preset.name))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.add_agent_preset(&preset_id, cx)),
                    ),
            );
        }

        let test_result =
            self.agent_test_result.as_ref().map(|result| {
                let color = if result.success {
                    cx.theme().success
                } else {
                    cx.theme().danger
                };
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .child(div().text_sm().font_medium().text_color(color).child(
                        if result.success {
                            "连接成功"
                        } else {
                            "连接失败"
                        },
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if result.success {
                                format!(
                                    "{} {} · 协议 v{} · {} ms · {} 项能力",
                                    result.agent_name,
                                    result.agent_version,
                                    result.protocol_version,
                                    result.latency_ms,
                                    result.capabilities.len()
                                )
                            } else {
                                result
                                    .error
                                    .clone()
                                    .unwrap_or_else(|| "未知错误".to_string())
                            }),
                    )
                    .into_any_element()
            });

        let config_button = Button::new("agent-open-config")
            .small()
            .outline()
            .label("打开配置目录")
            .on_click(cx.listener(|this, _, _, cx| {
                let path = agent_config_path();
                if let Err(error) =
                    open::that(path.parent().unwrap_or_else(|| std::path::Path::new(".")))
                {
                    this.status = Some(format!("无法打开配置目录：{error}").into());
                    cx.notify();
                }
            }))
            .into_any_element();
        let agent_log_path = self.agent_diagnostics.log_path.clone();
        let log_button = Button::new("agent-open-log")
            .small()
            .outline()
            .label("打开诊断日志")
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Err(error) = open::that(&agent_log_path) {
                    this.status = Some(format!("无法打开 Agent 日志：{error}").into());
                    cx.notify();
                }
            }))
            .into_any_element();

        let mut install_buttons = Vec::new();
        for preset in agent_presets() {
            install_buttons.push(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_4()
                    .py_3()
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
                            .child(preset.install_hint),
                    )
                    .into_any_element(),
            );
        }

        let mut diagnostic_rows = vec![
            h_flex()
                .id("agent-diagnostics-actions")
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
                        .child(div().text_sm().font_medium().child("诊断位置"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(self.agent_diagnostics.log_path.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_2()
                        .child(config_button)
                        .child(log_button),
                )
                .into_any_element(),
        ];
        for profile in &self.agent_diagnostics.profiles {
            diagnostic_rows.push(self.separator(cx));
            diagnostic_rows.push(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child(profile.profile_name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(profile.status.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(
                                profile
                                    .resolved_command
                                    .clone()
                                    .unwrap_or_else(|| profile.command.clone()),
                            ),
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
                    )
                    .into_any_element(),
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
            diagnostic_rows.push(self.separator(cx));
            diagnostic_rows.push(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
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
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_6()
            .child(self.section(
                "已配置 Agent",
                profile_rows,
                Some("测试连接会启动一次 ACP 握手；认证仍由 Agent 自身管理。"),
                cx,
            ))
            .child(self.section(
                "添加预设",
                vec![preset_buttons.into_any_element()],
                None,
                cx,
            ))
            .when_some(test_result, |this, result| this.child(result))
            .child(self.section(
                "诊断",
                diagnostic_rows,
                Some("日志会遮盖常见 token、密码和 URL 凭据。"),
                cx,
            ))
            .child(self.section("预设安装提示", install_buttons, None, cx))
            .when_some(self.agent_store.warning.clone(), |this, warning| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(warning))
            })
            .into_any_element()
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
        h_flex()
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
            )
    }
}

pub(crate) fn open_settings_window(
    main_app: Entity<KiteApp>,
    initial_update_state: SettingsUpdateState,
    cx: &mut App,
) -> Result<(), String> {
    open_settings_window_at(main_app, initial_update_state, SettingsPage::General, cx)
}

pub(crate) fn open_agent_settings_window(
    main_app: Entity<KiteApp>,
    initial_update_state: SettingsUpdateState,
    cx: &mut App,
) -> Result<(), String> {
    open_settings_window_at(main_app, initial_update_state, SettingsPage::Agent, cx)
}

fn open_settings_window_at(
    main_app: Entity<KiteApp>,
    initial_update_state: SettingsUpdateState,
    initial_page: SettingsPage,
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

fn now_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}
