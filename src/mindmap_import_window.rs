use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::assets::IconName;

use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::mindmap_ffi::{
    self, MindmapConversionOptionsDto, MindmapConversionPreviewDto, MindmapImportCodeStrategyDto,
    MindmapImportImageStrategyDto, MindmapImportPresetDto, MindmapImportSourceDto,
    MindmapImportTableStrategyDto,
};

pub(crate) fn open_mindmap_import_window(path: PathBuf, cx: &mut App) -> Result<(), String> {
    let source = mindmap_ffi::detect_mindmap_import_source(path.display().to_string())
        .ok_or_else(|| "该文件类型没有可用的 Mindmap 转换适配器".to_string())?;
    let title = format!("Kite Mindmap 转换 · {}", source.file_name);
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1040.), px(780.)), cx)),
        window_min_size: Some(size(px(760.), px(580.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| MindmapImportWindow::new(source.clone(), window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Mindmap 转换窗口：{error}"))
}

pub struct MindmapImportWindow {
    source: MindmapImportSourceDto,
    options: MindmapConversionOptionsDto,
    preview: Option<MindmapConversionPreviewDto>,
    output_input: Entity<InputState>,
    loading: bool,
    converting: bool,
    status: SharedString,
    error: Option<SharedString>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _preview_task: Task<()>,
    _convert_task: Task<()>,
}

impl MindmapImportWindow {
    fn new(source: MindmapImportSourceDto, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default_output_path = source.default_output_path.clone();
        let output_input = cx.new(move |cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("输出 .xmind 路径")
                .clean_on_escape();
            input.set_value(default_output_path, window, cx);
            input
        });
        let mut this = Self {
            source,
            options: MindmapConversionOptionsDto::default(),
            preview: None,
            output_input,
            loading: true,
            converting: false,
            status: "正在分析源文档…".into(),
            error: None,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _preview_task: Task::ready(()),
            _convert_task: Task::ready(()),
        };
        this.request_preview(cx);
        this
    }

    fn request_preview(&mut self, cx: &mut Context<Self>) {
        let source_path = self.source.path.clone();
        let options = self.options.clone();
        self.loading = true;
        self.error = None;
        self.status = "正在生成转换预览…".into();
        cx.notify();
        self._preview_task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(140))
                .await;
            let result = cx
                .background_spawn(async move {
                    mindmap_ffi::preview_mindmap_conversion(source_path, options)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(preview) => {
                        this.status = format!("预览已就绪 · {} 个主题", preview.node_count).into();
                        this.preview = Some(preview);
                        this.error = None;
                    }
                    Err(error) => {
                        this.preview = None;
                        this.error = Some(error.to_string().into());
                        this.status = "预览失败".into();
                    }
                }
                cx.notify();
            });
        });
    }

    fn set_preset(&mut self, preset: MindmapImportPresetDto, cx: &mut Context<Self>) {
        self.options.preset = preset;
        match preset {
            MindmapImportPresetDto::StructureFirst => {
                self.options.split_content = true;
                self.options.merge_short_fragments = true;
            }
            MindmapImportPresetDto::ExpandedContent => {
                self.options.split_content = true;
                self.options.merge_short_fragments = false;
            }
            MindmapImportPresetDto::StrictOutline => {
                self.options.split_content = false;
                self.options.merge_short_fragments = true;
            }
            MindmapImportPresetDto::SentenceSplit => {
                self.options.split_content = true;
                self.options.merge_short_fragments = false;
            }
        }
        self.request_preview(cx);
    }

    fn choose_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = Path::new(&self.source.path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(&self.source.path));
        let name = Path::new(&self.source.default_output_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let receiver = cx.prompt_for_new_path(&directory, name.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.output_input.update(cx, |input, cx| {
                    input.set_value(path.display().to_string(), window, cx)
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn convert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        let output_path = self.output_input.read(cx).value().trim().to_string();
        if output_path.is_empty() {
            self.error = Some("请选择输出路径".into());
            cx.notify();
            return;
        }
        let source_path = self.source.path.clone();
        let options = self.options.clone();
        let expected_sha = preview.source_sha256.clone();
        self.converting = true;
        self.error = None;
        self.status = "正在生成 .xmind…".into();
        cx.notify();
        self._convert_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    mindmap_ffi::convert_mindmap_file(
                        source_path,
                        expected_sha,
                        output_path,
                        false,
                        options,
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.converting = false;
                match result {
                    Ok(result) => {
                        match crate::mindmap_window::open_mindmap_window(
                            PathBuf::from(&result.output_path),
                            cx,
                        ) {
                            Ok(()) => {
                                this.error = None;
                                this.status =
                                    format!("已生成并打开：{}", result.output_path).into();
                                window.remove_window();
                            }
                            Err(error) => {
                                this.error = Some(error.into());
                                this.status = "已生成，但无法打开编辑器".into();
                            }
                        }
                    }
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        this.status = "转换失败".into();
                    }
                }
                cx.notify();
            });
        });
    }

    fn render_header(&self, cx: &App) -> AnyElement {
        h_flex()
            .h(px(54.))
            .w_full()
            .px_3()
            .gap_2()
            .items_center()
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Icon::new(IconName::FileText)
                    .size_5()
                    .text_color(cx.theme().primary),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.source.file_name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                self.source.format.display_name, self.source.path
                            )),
                    ),
            )
            .child(div().flex_1())
            .when(self.loading, |this| {
                this.child(div().text_xs().child("正在预览…"))
            })
            .when(self.converting, |this| {
                this.child(div().text_xs().child("正在生成…"))
            })
            .into_any_element()
    }

    fn render_options(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let presets = [
            (MindmapImportPresetDto::StructureFirst, "结构优先"),
            (MindmapImportPresetDto::ExpandedContent, "正文展开"),
            (MindmapImportPresetDto::StrictOutline, "严格大纲"),
            (MindmapImportPresetDto::SentenceSplit, "按句拆散"),
        ];
        v_flex()
            .w(px(360.))
            .flex_shrink_0()
            .h_full()
            .p_4()
            .gap_4()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar.opacity(0.76))
            .child(section("转换规则"))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .children(presets.into_iter().map(|(preset, label)| {
                        Button::new(SharedString::from(format!("preset-{label}")))
                            .small()
                            .outline()
                            .selected(self.options.preset == preset)
                            .label(label)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_preset(preset, cx)
                            }))
                    })),
            )
            .child(toggle_row(
                "拆分正文",
                self.options.split_content,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.options.split_content = !this.options.split_content;
                    this.request_preview(cx);
                }),
            ))
            .child(toggle_row(
                "保留原始段落备注",
                self.options.preserve_original_paragraphs,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.options.preserve_original_paragraphs =
                        !this.options.preserve_original_paragraphs;
                    this.request_preview(cx);
                }),
            ))
            .child(toggle_row(
                "合并短片段",
                self.options.merge_short_fragments,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.options.merge_short_fragments = !this.options.merge_short_fragments;
                    this.request_preview(cx);
                }),
            ))
            .child(section("表格 / 代码 / 图片"))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        Button::new("table-strategy")
                            .small()
                            .outline()
                            .label(match self.options.table_strategy {
                                MindmapImportTableStrategyDto::Note => "表格→备注",
                                MindmapImportTableStrategyDto::RowsAsTopics => "表格→主题",
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.options.table_strategy = match this.options.table_strategy {
                                    MindmapImportTableStrategyDto::Note => {
                                        MindmapImportTableStrategyDto::RowsAsTopics
                                    }
                                    MindmapImportTableStrategyDto::RowsAsTopics => {
                                        MindmapImportTableStrategyDto::Note
                                    }
                                };
                                this.request_preview(cx);
                            })),
                    )
                    .child(
                        Button::new("code-strategy")
                            .small()
                            .outline()
                            .label(match self.options.code_strategy {
                                MindmapImportCodeStrategyDto::TopicNote => "代码→主题",
                                MindmapImportCodeStrategyDto::ParentNote => "代码→父备注",
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.options.code_strategy = match this.options.code_strategy {
                                    MindmapImportCodeStrategyDto::TopicNote => {
                                        MindmapImportCodeStrategyDto::ParentNote
                                    }
                                    MindmapImportCodeStrategyDto::ParentNote => {
                                        MindmapImportCodeStrategyDto::TopicNote
                                    }
                                };
                                this.request_preview(cx);
                            })),
                    )
                    .child(
                        Button::new("image-strategy")
                            .small()
                            .outline()
                            .label(match self.options.image_strategy {
                                MindmapImportImageStrategyDto::TopicLink => "图片→主题",
                                MindmapImportImageStrategyDto::ParentNote => "图片→父备注",
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.options.image_strategy = match this.options.image_strategy {
                                    MindmapImportImageStrategyDto::TopicLink => {
                                        MindmapImportImageStrategyDto::ParentNote
                                    }
                                    MindmapImportImageStrategyDto::ParentNote => {
                                        MindmapImportImageStrategyDto::TopicLink
                                    }
                                };
                                this.request_preview(cx);
                            })),
                    ),
            )
            .child(section("输出"))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Input::new(&self.output_input)
                            .aria_label("输出路径")
                            .cleanable(true),
                    )
                    .child(
                        Button::new("choose-output")
                            .small()
                            .outline()
                            .label("选择输出位置…")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.choose_output(window, cx)
                            })),
                    ),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-import")
                            .outline()
                            .label("取消")
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("generate-xmind")
                            .primary()
                            .label(if self.converting {
                                "正在生成…"
                            } else {
                                "生成 Mindmap"
                            })
                            .disabled(self.loading || self.converting || self.preview.is_none())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.convert(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_preview(&self, cx: &App) -> AnyElement {
        let body = if let Some(preview) = self.preview.as_ref() {
            let warnings = preview
                .warnings
                .iter()
                .map(|warning| {
                    let color = if warning.severity == "error" {
                        cx.theme().danger
                    } else if warning.severity == "warning" {
                        rgb(0xD5A143).into()
                    } else {
                        cx.theme().muted_foreground
                    };
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(color)
                                .child(warning.severity.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(color)
                                .child(warning.message.clone()),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let sample = serde_json::from_str::<serde_json::Value>(&preview.sample_tree_json)
                .ok()
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or_else(|| preview.sample_tree_json.clone());
            let sample = sample.chars().take(12_000).collect::<String>();
            v_flex()
                .gap_4()
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(metric("主题", preview.node_count.to_string(), cx))
                        .child(metric(
                            "结构节点",
                            preview.structure_node_count.to_string(),
                            cx,
                        ))
                        .child(metric(
                            "内容节点",
                            preview.content_node_count.to_string(),
                            cx,
                        ))
                        .child(metric("最大层级", preview.max_depth.to_string(), cx))
                        .child(metric("备注", preview.note_count.to_string(), cx))
                        .child(metric("警告", preview.warnings.len().to_string(), cx)),
                )
                .child(section("有界样本树"))
                .child(
                    div()
                        .w_full()
                        .max_h(px(280.))
                        .overflow_y_scrollbar()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(rgb(0x111316))
                        .p_3()
                        .child(div().text_xs().font_family("monospace").child(sample)),
                )
                .when(!warnings.is_empty(), |this| {
                    this.child(section("警告"))
                        .child(v_flex().gap_1().children(warnings))
                })
                .into_any_element()
        } else if self.loading {
            centered("正在分析文档并生成预览…", cx)
        } else {
            centered("没有可用预览", cx)
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .id("mindmap-import-preview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_5()
                    .child(body),
            )
            .child(
                h_flex()
                    .h(px(30.))
                    .px_3()
                    .items_center()
                    .bg(cx.theme().status_bar)
                    .border_t_1()
                    .border_color(cx.theme().status_bar_border)
                    .text_xs()
                    .when(self.error.is_some(), |this| {
                        this.text_color(cx.theme().danger)
                    })
                    .child(self.error.clone().unwrap_or_else(|| self.status.clone())),
            )
            .into_any_element()
    }
}

impl Focusable for MindmapImportWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MindmapImportWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let options = self.render_options(cx);
        let preview = self.render_preview(cx);
        v_flex()
            .id("mindmap-import-root")
            .key_context("KiteMindmapImport")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(options)
                    .child(preview),
            )
    }
}

fn toggle_row(
    label: &'static str,
    selected: bool,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(
            Button::new(SharedString::from(format!("toggle-{label}")))
                .small()
                .outline()
                .selected(selected)
                .label(if selected { "开启" } else { "关闭" })
                .on_click(listener),
        )
        .child(div().text_xs().child(label))
        .into_any_element()
}

fn metric(label: &'static str, value: String, cx: &App) -> AnyElement {
    v_flex()
        .min_w(px(110.))
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().title_bar.opacity(0.68))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .font_family("monospace")
                .child(value),
        )
        .into_any_element()
}

fn section(label: &'static str) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(0x8F9BA8))
        .child(label)
        .into_any_element()
}

fn centered(message: &'static str, cx: &App) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message),
        )
        .into_any_element()
}
