use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{DiffNextChange, DiffPreviousChange};
use crate::fs::format_size;
use crate::preview_window::preview_render_image;
use kite_core::diff::{
    DiffContentKind, DiffEndpoint, DiffLine, DiffRow, DiffRowKind, DiffSyntaxKind, DiffSyntaxSpan,
    GitDiffDocument, GitDiffRequest, git_diff,
};
use kite_core::preview::PreviewImageData;

const DIFF_WINDOW_WIDTH: f32 = 1080.0;
const DIFF_WINDOW_HEIGHT: f32 = 720.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffViewMode {
    Unified,
    SideBySide,
}

pub(crate) fn open_diff_viewer_window(
    path: PathBuf,
    old: DiffEndpoint,
    new: DiffEndpoint,
    cx: &mut App,
) -> Result<(), String> {
    let title = format!(
        "Diff 查看器 — {}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("文件")
    );
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(
            size(px(DIFF_WINDOW_WIDTH), px(DIFF_WINDOW_HEIGHT)),
            cx,
        )),
        window_min_size: Some(size(px(820.), px(540.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| DiffViewerWindow::new(path, old, new, window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Diff 查看器：{error}"))
}

struct DiffViewerWindow {
    path: PathBuf,
    old: DiffEndpoint,
    new: DiffEndpoint,
    old_input: Entity<InputState>,
    new_input: Entity<InputState>,
    ignore_whitespace: bool,
    hide_unchanged: bool,
    mode: DiffViewMode,
    document: Option<GitDiffDocument>,
    loading: bool,
    error: Option<SharedString>,
    current_change: usize,
    generation: u64,
    scroll: ScrollHandle,
    _load_task: Task<()>,
}

impl DiffViewerWindow {
    fn new(
        path: PathBuf,
        old: DiffEndpoint,
        new: DiffEndpoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let old_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("旧端点：HEAD、main、暂存区…")
                .clean_on_escape()
        });
        let new_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("新端点：HEAD、feature、工作区…")
                .clean_on_escape()
        });
        old_input.update(cx, |input, cx| {
            input.set_value(endpoint_input_value(&old), window, cx);
        });
        new_input.update(cx, |input, cx| {
            input.set_value(endpoint_input_value(&new), window, cx);
        });

        let mut this = Self {
            path,
            old,
            new,
            old_input,
            new_input,
            ignore_whitespace: false,
            hide_unchanged: false,
            mode: DiffViewMode::Unified,
            document: None,
            loading: true,
            error: None,
            current_change: 0,
            generation: 0,
            scroll: ScrollHandle::new(),
            _load_task: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    fn set_preset(&mut self, old: &str, new: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.old_input.update(cx, |input, cx| {
            input.set_value(old, window, cx);
        });
        self.new_input.update(cx, |input, cx| {
            input.set_value(new, window, cx);
        });
        self.reload_from_inputs(cx);
    }

    fn reload_from_inputs(&mut self, cx: &mut Context<Self>) {
        self.old = endpoint_from_input(self.old_input.read(cx).value().as_ref());
        self.new = endpoint_from_input(self.new_input.read(cx).value().as_ref());
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        self.document = None;
        self.current_change = 0;
        let request = GitDiffRequest {
            path: self.path.clone(),
            old: self.old.clone(),
            new: self.new.clone(),
            ignore_whitespace: self.ignore_whitespace,
        };
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { git_diff(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(document) => {
                        this.current_change = 0;
                        this.document = Some(document);
                        this.error = None;
                    }
                    Err(error) => {
                        this.document = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn toggle_ignore_whitespace(&mut self, cx: &mut Context<Self>) {
        self.ignore_whitespace = !self.ignore_whitespace;
        self.reload(cx);
    }

    fn move_change(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        if document.change_rows.is_empty() {
            return;
        }
        let count = document.change_rows.len() as isize;
        let next = (self.current_change as isize + delta).rem_euclid(count) as usize;
        self.current_change = next;
        if let Some(row) = document.change_rows.get(next) {
            self.scroll.scroll_to_item(*row as usize);
        }
        cx.notify();
    }

    fn on_next_change(&mut self, _: &DiffNextChange, _: &mut Window, cx: &mut Context<Self>) {
        self.move_change(1, cx);
    }

    fn on_previous_change(
        &mut self,
        _: &DiffPreviousChange,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_change(-1, cx);
    }

    fn change_position(&self) -> String {
        let Some(document) = self.document.as_ref() else {
            return "无改动".to_string();
        };
        if document.change_rows.is_empty() {
            "无改动".to_string()
        } else {
            format!(
                "{} / {}",
                self.current_change + 1,
                document.change_rows.len()
            )
        }
    }
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(gpui_kit::assets::IconName::FileCode)
                            .size_4()
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .min_w(px(96.))
                            .max_w(px(150.))
                            .flex_1()
                            .child(Input::new(&self.old_input).aria_label("旧端点")),
                    )
                    .child(div().text_sm().text_color(muted).child("→"))
                    .child(
                        div()
                            .min_w(px(96.))
                            .max_w(px(150.))
                            .flex_1()
                            .child(Input::new(&self.new_input).aria_label("新端点")),
                    )
                    .child(
                        Button::new("diff-reload")
                            .outline()
                            .compact()
                            .label("比较")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.reload_from_inputs(cx);
                            })),
                    )
                    .child(div().w(px(8.)))
                    .child(
                        Button::new("diff-head-worktree")
                            .ghost()
                            .compact()
                            .label("HEAD ↔ 工作区")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.set_preset("HEAD", "工作区", window, cx);
                            })),
                    )
                    .child(
                        Button::new("diff-head-index")
                            .ghost()
                            .compact()
                            .label("HEAD ↔ 暂存区")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.set_preset("HEAD", "暂存区", window, cx);
                            })),
                    )
                    .child(
                        Button::new("diff-index-worktree")
                            .ghost()
                            .compact()
                            .label("暂存区 ↔ 工作区")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.set_preset("暂存区", "工作区", window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("diff-unified")
                            .compact()
                            .label(if self.mode == DiffViewMode::Unified {
                                "✓ 统一"
                            } else {
                                "统一"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.mode = DiffViewMode::Unified;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("diff-side-by-side")
                            .compact()
                            .label(if self.mode == DiffViewMode::SideBySide {
                                "✓ 并排"
                            } else {
                                "并排"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.mode = DiffViewMode::SideBySide;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("diff-ignore-whitespace")
                            .ghost()
                            .compact()
                            .label(if self.ignore_whitespace {
                                "忽略空白：开"
                            } else {
                                "忽略空白：关"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.toggle_ignore_whitespace(cx);
                            })),
                    )
                    .child(
                        Button::new("diff-fold-context")
                            .ghost()
                            .compact()
                            .label(if self.hide_unchanged {
                                "未修改：折叠"
                            } else {
                                "未修改：显示"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.hide_unchanged = !this.hide_unchanged;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("diff-previous")
                            .ghost()
                            .compact()
                            .label("⇧F7")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.move_change(-1, cx);
                            })),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_xs()
                            .text_color(muted)
                            .child(self.change_position()),
                    )
                    .child(
                        Button::new("diff-next")
                            .ghost()
                            .compact()
                            .label("F7")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.move_change(1, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_summary(&self, document: &GitDiffDocument, cx: &App) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .px_4()
            .py_2()
            .bg(cx.theme().muted.opacity(0.18))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .font_family("monospace")
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(format!("{} → {}", document.old_label, document.new_label)),
                    )
                    .when(!document.large_file, |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{}", document.stats.additions)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("-{}", document.stats.deletions)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} hunk", document.stats.hunks)),
                        )
                        .when(document.stats.files > 1, |this| {
                            this.child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} 文件", document.stats.files)),
                            )
                        })
                    })
                    .when(document.large_file, |this| {
                        let old = document
                            .stats
                            .old_bytes
                            .map(format_size)
                            .unwrap_or_else(|| "—".to_string());
                        let new = document
                            .stats
                            .new_bytes
                            .map(format_size)
                            .unwrap_or_else(|| "—".to_string());
                        this.child(div().text_color(cx.theme().warning).child("大文件预览"))
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{old} → {new}")),
                            )
                    }),
            )
            .when_some(document.message.as_ref(), |this, message| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                )
            })
            .into_any_element()
    }

    fn render_document(&self, document: &GitDiffDocument, cx: &App) -> AnyElement {
        match document.content_kind {
            DiffContentKind::Binary => centered_message(
                document
                    .message
                    .as_deref()
                    .unwrap_or("二进制文件已变化，无法显示文本差异。"),
                cx,
            ),
            DiffContentKind::Image => self.render_images(document, cx),
            DiffContentKind::Text => {
                if document.rows.is_empty() {
                    centered_message("没有可显示的差异", cx)
                } else {
                    v_flex()
                        .id("diff-scroll")
                        .size_full()
                        .overflow_scroll()
                        .track_scroll(&self.scroll)
                        .py_1()
                        .children(document.rows.iter().enumerate().filter_map(|(index, row)| {
                            if self.hide_unchanged && row.kind == DiffRowKind::Context {
                                return None;
                            }
                            Some(
                                div()
                                    .id(index)
                                    .w_full()
                                    .child(self.render_row(index, row, cx)),
                            )
                        }))
                        .into_any_element()
                }
            }
        }
    }
    fn render_row(&self, index: usize, row: &DiffRow, cx: &App) -> AnyElement {
        let current = row
            .change_index
            .is_some_and(|change| change as usize == self.current_change);
        match row.kind {
            DiffRowKind::File => v_flex()
                .w_full()
                .px_3()
                .py_2()
                .bg(cx.theme().muted.opacity(0.32))
                .text_xs()
                .font_family("monospace")
                .text_color(cx.theme().muted_foreground)
                .child(format!("文件  {}", row.text))
                .into_any_element(),
            DiffRowKind::Hunk => div()
                .w_full()
                .px_3()
                .py_1()
                .bg(cx.theme().accent.opacity(0.08))
                .text_xs()
                .font_family("monospace")
                .text_color(cx.theme().muted_foreground)
                .whitespace_nowrap()
                .child(row.text.clone())
                .into_any_element(),
            DiffRowKind::Context => {
                if self.mode == DiffViewMode::Unified {
                    self.render_unified_line(row.left.as_ref(), DiffLineTone::Context, false, cx)
                } else {
                    self.render_side_by_side(
                        row.left.as_ref(),
                        row.right.as_ref(),
                        DiffLineTone::Context,
                        false,
                        cx,
                    )
                }
            }
            DiffRowKind::Modification => {
                if self.mode == DiffViewMode::Unified {
                    v_flex()
                        .id(("unified-change", index))
                        .w_full()
                        .child(self.render_unified_line(
                            row.left.as_ref(),
                            DiffLineTone::Removed,
                            current,
                            cx,
                        ))
                        .child(self.render_unified_line(
                            row.right.as_ref(),
                            DiffLineTone::Added,
                            current,
                            cx,
                        ))
                        .into_any_element()
                } else {
                    self.render_side_by_side(
                        row.left.as_ref(),
                        row.right.as_ref(),
                        DiffLineTone::Modified,
                        current,
                        cx,
                    )
                }
            }
            DiffRowKind::Deletion => {
                if self.mode == DiffViewMode::Unified {
                    self.render_unified_line(row.left.as_ref(), DiffLineTone::Removed, current, cx)
                } else {
                    self.render_side_by_side(
                        row.left.as_ref(),
                        None,
                        DiffLineTone::Removed,
                        current,
                        cx,
                    )
                }
            }
            DiffRowKind::Addition => {
                if self.mode == DiffViewMode::Unified {
                    self.render_unified_line(row.right.as_ref(), DiffLineTone::Added, current, cx)
                } else {
                    self.render_side_by_side(
                        None,
                        row.right.as_ref(),
                        DiffLineTone::Added,
                        current,
                        cx,
                    )
                }
            }
        }
    }

    fn render_unified_line(
        &self,
        line: Option<&DiffLine>,
        tone: DiffLineTone,
        current: bool,
        cx: &App,
    ) -> AnyElement {
        let (prefix, color, background) = line_tone(tone, cx);
        let old_number = line
            .and_then(|line| line.old_line)
            .map(|number| number.to_string())
            .unwrap_or_default();
        let new_number = line
            .and_then(|line| line.new_line)
            .map(|number| number.to_string())
            .unwrap_or_default();
        h_flex()
            .w_full()
            .min_h(px(19.))
            .px_2()
            .when(current, |this| {
                this.border_l_2().border_color(cx.theme().accent)
            })
            .when_some(background, |this, background| this.bg(background))
            .font_family("monospace")
            .text_xs()
            .child(
                div()
                    .w(px(46.))
                    .flex_shrink_0()
                    .pr_1()
                    .text_right()
                    .text_color(color.opacity(0.85))
                    .child(old_number),
            )
            .child(
                div()
                    .w(px(46.))
                    .flex_shrink_0()
                    .pr_1()
                    .text_right()
                    .text_color(color.opacity(0.85))
                    .child(new_number),
            )
            .child(
                div()
                    .w(px(15.))
                    .flex_shrink_0()
                    .text_color(color)
                    .child(prefix),
            )
            .child(self.highlighted_line(line, color, cx))
            .when(line.is_some_and(|line| line.missing_newline), |this| {
                this.child(div().text_color(cx.theme().muted_foreground).child(" ⏎"))
            })
            .into_any_element()
    }

    fn render_side_by_side(
        &self,
        left: Option<&DiffLine>,
        right: Option<&DiffLine>,
        tone: DiffLineTone,
        current: bool,
        cx: &App,
    ) -> AnyElement {
        h_flex()
            .w_full()
            .min_h(px(19.))
            .child(self.render_side_panel(left, DiffSide::Left, tone, current, cx))
            .child(div().w(px(1.)).self_stretch().bg(cx.theme().border))
            .child(self.render_side_panel(right, DiffSide::Right, tone, current, cx))
            .into_any_element()
    }

    fn render_side_panel(
        &self,
        line: Option<&DiffLine>,
        side: DiffSide,
        tone: DiffLineTone,
        current: bool,
        cx: &App,
    ) -> AnyElement {
        let changed_for_side = matches!(
            (side, tone),
            (
                DiffSide::Left,
                DiffLineTone::Removed | DiffLineTone::Modified
            ) | (
                DiffSide::Right,
                DiffLineTone::Added | DiffLineTone::Modified
            )
        );
        let color = match side {
            DiffSide::Left => cx.theme().danger,
            DiffSide::Right => cx.theme().success,
        };
        let background = if changed_for_side {
            Some(color.opacity(if current { 0.2 } else { 0.1 }))
        } else if tone == DiffLineTone::Context {
            None
        } else {
            Some(cx.theme().muted.opacity(0.08))
        };
        let number = line
            .and_then(|line| match side {
                DiffSide::Left => line.old_line,
                DiffSide::Right => line.new_line,
            })
            .map(|number| number.to_string())
            .unwrap_or_default();
        h_flex()
            .flex_1()
            .min_w_0()
            .min_h(px(19.))
            .px_2()
            .when(current, |this| {
                this.border_l_2().border_color(cx.theme().accent)
            })
            .when_some(background, |this, background| this.bg(background))
            .font_family("monospace")
            .text_xs()
            .child(
                div()
                    .w(px(46.))
                    .flex_shrink_0()
                    .pr_1()
                    .text_right()
                    .text_color(if changed_for_side {
                        color
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(number),
            )
            .child(self.highlighted_line(
                line,
                if changed_for_side {
                    color
                } else {
                    cx.theme().foreground
                },
                cx,
            ))
            .into_any_element()
    }

    fn highlighted_line(&self, line: Option<&DiffLine>, base: Hsla, cx: &App) -> AnyElement {
        let Some(line) = line else {
            return div().flex_1().into_any_element();
        };
        let segments = highlighted_segments(&line.text, &line.highlighted, &line.syntax);
        h_flex()
            .flex_1()
            .min_w_0()
            .whitespace_nowrap()
            .children(segments.into_iter().map(|segment| {
                let color = segment
                    .syntax
                    .map(|kind| diff_syntax_color(kind, cx))
                    .unwrap_or(base);
                div()
                    .when(segment.changed, |this| {
                        this.bg(base.opacity(0.2)).font_weight(FontWeight::BOLD)
                    })
                    .text_color(color)
                    .child(segment.text)
            }))
            .into_any_element()
    }

    fn render_images(&self, document: &GitDiffDocument, cx: &App) -> AnyElement {
        let old = document
            .old_image
            .as_deref()
            .and_then(render_image_from_bytes);
        let new = document
            .new_image
            .as_deref()
            .and_then(render_image_from_bytes);
        h_flex()
            .size_full()
            .gap_3()
            .p_4()
            .items_stretch()
            .child(self.render_image_panel("旧", old, cx))
            .child(self.render_image_panel("新", new, cx))
            .into_any_element()
    }

    fn render_image_panel(
        &self,
        label: &'static str,
        image: Option<Arc<RenderImage>>,
        cx: &App,
    ) -> AnyElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(cx.theme().muted.opacity(0.14))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(if let Some(image) = image {
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .child(
                        img(image)
                            .max_w_full()
                            .max_h_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element()
            } else {
                centered_message("此端点没有可显示的图片", cx)
            })
            .into_any_element()
    }
}

impl Render for DiffViewerWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.loading {
            centered_message("正在计算差异…", cx)
        } else if let Some(error) = self.error.as_ref() {
            centered_message(error, cx)
        } else if let Some(document) = self.document.as_ref() {
            v_flex()
                .size_full()
                .child(self.render_summary(document, cx))
                .child(self.render_document(document, cx))
                .into_any_element()
        } else {
            centered_message("没有可显示的差异", cx)
        };

        v_flex()
            .key_context("KiteDiff")
            .on_action(cx.listener(Self::on_next_change))
            .on_action(cx.listener(Self::on_previous_change))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_toolbar(cx))
            .child(body)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffLineTone {
    Context,
    Removed,
    Added,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffSide {
    Left,
    Right,
}

fn line_tone(tone: DiffLineTone, cx: &App) -> (&'static str, Hsla, Option<Hsla>) {
    match tone {
        DiffLineTone::Context => (" ", cx.theme().foreground, None),
        DiffLineTone::Removed => ("-", cx.theme().danger, Some(cx.theme().danger.opacity(0.1))),
        DiffLineTone::Added => (
            "+",
            cx.theme().success,
            Some(cx.theme().success.opacity(0.1)),
        ),
        DiffLineTone::Modified => (" ", cx.theme().foreground, None),
    }
}

struct HighlightSegment {
    text: String,
    changed: bool,
    syntax: Option<DiffSyntaxKind>,
}

fn highlighted_segments(
    text: &str,
    word_ranges: &[kite_core::diff::DiffWordRange],
    syntax_spans: &[DiffSyntaxSpan],
) -> Vec<HighlightSegment> {
    let word_bytes: Vec<_> = word_ranges
        .iter()
        .map(|range| {
            (
                utf16_offset_to_byte(text, range.start_utf16),
                utf16_offset_to_byte(text, range.end_utf16),
            )
        })
        .collect();
    let syntax_bytes: Vec<_> = syntax_spans
        .iter()
        .map(|span| {
            (
                utf16_offset_to_byte(text, span.start_utf16),
                utf16_offset_to_byte(text, span.end_utf16),
                span.kind,
            )
        })
        .collect();
    let mut boundaries = vec![0, text.len()];
    for (start, end) in &word_bytes {
        boundaries.extend([*start, *end]);
    }
    for (start, end, _) in &syntax_bytes {
        boundaries.extend([*start, *end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut segments = Vec::new();
    for bounds in boundaries.windows(2) {
        let start = bounds[0].min(text.len());
        let end = bounds[1].min(text.len());
        if start >= end {
            continue;
        }
        let changed = word_bytes
            .iter()
            .any(|(range_start, range_end)| *range_start <= start && start < *range_end);
        let syntax = syntax_bytes
            .iter()
            .find(|(range_start, range_end, _)| *range_start <= start && start < *range_end)
            .map(|(_, _, kind)| *kind);
        segments.push(HighlightSegment {
            text: text[start..end].to_string(),
            changed,
            syntax,
        });
    }
    if segments.is_empty() {
        segments.push(HighlightSegment {
            text: text.to_string(),
            changed: false,
            syntax: None,
        });
    }
    segments
}

fn diff_syntax_color(kind: DiffSyntaxKind, cx: &App) -> Hsla {
    match kind {
        DiffSyntaxKind::Keyword => cx.theme().accent,
        DiffSyntaxKind::String => cx.theme().success,
        DiffSyntaxKind::Number => cx.theme().warning,
        DiffSyntaxKind::Comment => cx.theme().muted_foreground,
        DiffSyntaxKind::TypeName => cx.theme().accent.opacity(0.86),
        DiffSyntaxKind::Function => cx.theme().success.opacity(0.9),
    }
}

fn utf16_offset_to_byte(text: &str, utf16_offset: u32) -> usize {
    let mut utf16 = 0_u32;
    for (byte, character) in text.char_indices() {
        if utf16 >= utf16_offset {
            return byte;
        }
        utf16 += character.len_utf16() as u32;
    }
    text.len()
}

fn render_image_from_bytes(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let width = decoded.width();
    let height = decoded.height();
    let mut bgra = decoded.into_raw();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(preview_render_image(PreviewImageData {
        width,
        height,
        bgra,
    }))
}

fn endpoint_input_value(endpoint: &DiffEndpoint) -> &str {
    match endpoint {
        DiffEndpoint::Revision(revision) => revision,
        DiffEndpoint::Index => "暂存区",
        DiffEndpoint::WorkingTree => "工作区",
    }
}

fn endpoint_from_input(value: &str) -> DiffEndpoint {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("工作区") || trimmed.eq_ignore_ascii_case("worktree") {
        DiffEndpoint::WorkingTree
    } else if trimmed.eq_ignore_ascii_case("暂存区") || trimmed.eq_ignore_ascii_case("index") {
        DiffEndpoint::Index
    } else if trimmed.is_empty() {
        DiffEndpoint::Revision("HEAD".to_string())
    } else {
        DiffEndpoint::Revision(trimmed.to_string())
    }
}

fn centered_message(message: &str, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message.to_string()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{highlighted_segments, utf16_offset_to_byte};
    use kite_core::diff::{DiffSyntaxKind, DiffSyntaxSpan, DiffWordRange};

    #[test]
    fn converts_utf16_ranges_to_utf8_segments() {
        assert_eq!(utf16_offset_to_byte("a😀b", 3), 5);
        let segments = highlighted_segments(
            "one 世界 two",
            &[DiffWordRange {
                start_utf16: 4,
                end_utf16: 6,
            }],
            &[DiffSyntaxSpan {
                start_utf16: 7,
                end_utf16: 10,
                kind: DiffSyntaxKind::Keyword,
            }],
        );
        assert_eq!(segments.len(), 4);
        assert!(segments[1].changed);
        assert_eq!(segments[1].text, "世界");
        assert_eq!(segments[3].syntax, Some(DiffSyntaxKind::Keyword));
        assert_eq!(segments[3].text, "two");
    }
}
