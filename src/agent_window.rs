use crate::agent_panel::AgentPanel;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

pub(crate) struct AgentWindow {
    panel: Entity<AgentPanel>,
}

impl AgentWindow {
    pub(crate) fn new(panel: Entity<AgentPanel>, _cx: &mut Context<Self>) -> Self {
        Self { panel }
    }
}

impl Render for AgentWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("agent-window")
            .size_full()
            .gap_2()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_3()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .child(div().text_sm().font_semibold().child("Kite Agent"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("共享同一上下文；关闭窗口会保留会话"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().title_bar.opacity(0.82))
                    .p_3()
                    .child(self.panel.clone()),
            )
    }
}

pub(crate) fn open_agent_window(
    panel: Entity<AgentPanel>,
    cx: &mut App,
) -> Result<AnyWindowHandle, String> {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Kite Agent".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(560.), px(760.)), cx)),
        window_min_size: Some(size(px(440.), px(560.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |_window, cx| {
        cx.new(|cx| AgentWindow::new(panel.clone(), cx))
    })
    .map(|(handle, _)| handle)
    .map_err(|error| format!("无法打开 Agent 窗口：{error}"))
}
