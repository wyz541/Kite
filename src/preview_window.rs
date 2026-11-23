use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::actions::SaveEditor;
use crate::app::{
    is_editor_preview_entry, is_image_preview_entry, is_raster_image_preview_entry,
    is_text_preview_entry, read_log_preview,
};
use crate::fs::FileEntry;
use crate::git_log_window;
use crate::text_editor::{self, TextDocument};
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Redo, Undo};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::git::{DEFAULT_BLAME_PAGE_LINES, GitBlameLine, git_blame};
use kite_core::preview::{PreviewImageData, is_raster_image_preview_path, load_image_preview};

const PREVIEW_WINDOW_IMAGE_MAX_WIDTH: u32 = 1_920;
const PREVIEW_WINDOW_IMAGE_MAX_HEIGHT: u32 = 1_440;
const MARKDOWN_PREVIEW_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewKind {
    Editor,
    ReadOnlyText,
    Image,
    Other,
}

pub(crate) struct PreviewWindow {
    path: PathBuf,
    name: SharedString,
    path_label: SharedString,
    kind: PreviewKind,
    loading: bool,
    saving: bool,
    dirty: bool,
    allow_close: bool,
    document: Option<TextDocument>,
    editor: Option<Entity<EditorState>>,
    text_view: Option<Entity<TextViewState>>,
    markdown_preview_visible: bool,
    image: Option<Arc<RenderImage>>,
    scroll: ScrollHandle,
    message: Option<SharedString>,
    word_wrap: bool,
    line_numbers: bool,
    show_whitespace: bool,
    blame_visible: bool,
    blame_loading: bool,
    blame_error: Option<SharedString>,
    blame_lines: Vec<GitBlameLine>,
    blame_total_lines: u32,
    blame_end_line: u32,
    blame_ignore_whitespace: bool,
    selected_blame: Option<GitBlameLine>,
    _editor_subscription: Option<Subscription>,
    _load_task: Task<()>,
    _save_task: Task<()>,
    _markdown_preview_task: Task<()>,
    _blame_task: Task<()>,
}

impl PreviewWindow {
    pub(crate) fn new(entry: FileEntry, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = if is_editor_preview_entry(&entry) {
            PreviewKind::Editor
        } else if is_image_preview_entry(&entry) {
            PreviewKind::Image
        } else if is_text_preview_entry(&entry) {
            PreviewKind::ReadOnlyText
        } else {
            PreviewKind::Other
        };
        let editor = (kind == PreviewKind::Editor).then(|| {
            let language = text_editor::language_for_path(&entry.path);
            cx.new(|cx| {
                EditorState::new(window, cx)
                    .default_value("正在加载文件…")
                    .language(language)
            })
        });
        let live_markdown =
            kind == PreviewKind::Editor && text_editor::is_markdown_path(&entry.path);
        let text_view = (kind == PreviewKind::ReadOnlyText || live_markdown).then(|| {
            cx.new(|cx| {
                TextViewState::markdown("正在加载文件…", cx)
                    .selectable(true)
                    .scrollable(false)
            })
        });
        let raster_image = kind == PreviewKind::Image && is_raster_image_preview_entry(&entry);
        let mut this = Self {
            path: entry.path.clone(),
            name: entry.name.into(),
            path_label: entry.path.display().to_string().into(),
            kind,
            loading: editor.is_some() || text_view.is_some() || raster_image,
            saving: false,
            dirty: false,
            allow_close: false,
            document: None,
            editor,
            text_view,
            markdown_preview_visible: live_markdown,
            image: None,
            scroll: ScrollHandle::new(),
            message: None,
            word_wrap: false,
            line_numbers: true,
            show_whitespace: false,
            blame_visible: false,
            blame_loading: false,
            blame_error: None,
            blame_lines: Vec::new(),
            blame_total_lines: 0,
            blame_end_line: 0,
            blame_ignore_whitespace: true,
            selected_blame: None,
            _editor_subscription: None,
            _load_task: Task::ready(()),
            _save_task: Task::ready(()),
            _markdown_preview_task: Task::ready(()),
            _blame_task: Task::ready(()),
        };
        if live_markdown && let Some(editor) = &this.editor {
            this.word_wrap = true;
            editor.update(cx, |editor, cx| editor.set_soft_wrap(true, window, cx));
        }
        if let Some(editor) = this.editor.clone() {
            this._editor_subscription = Some(cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.dirty = true;
                        this.schedule_markdown_preview(cx);
                        this.update_window_title(window);
                        cx.notify();
                    }
                },
            ));
        }
        if this.loading {
            this.start_load(cx);
        }
        this.install_close_guard(window, cx);
        this
    }

    fn start_load(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        match self.kind {
            PreviewKind::Editor => self.start_editor_load(path, cx),
            PreviewKind::ReadOnlyText => self.start_text_preview(path, cx),
            PreviewKind::Image => self.start_image_load(path, cx),
            PreviewKind::Other => {}
        }
    }

    fn start_editor_load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { TextDocument::open_editable(&path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(document) => {
                        let text = document.text.clone();
                        editor.update(cx, |editor, cx| {
                            editor.set_value(text, window, cx);
                            editor.focus(window, cx);
                        });
                        this.document = Some(document);
                        this.update_markdown_preview(cx);
                        this.message = None;
                        this.dirty = false;
                        this.update_window_title(window);
                    }
                    Err(error) => {
                        this.document = None;
                        this.message = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn start_text_preview(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let log_preview = extension.eq_ignore_ascii_case("log");
        let markdown = extension == "txt" || text_editor::is_markdown_path(&path);
        let Some(text_view) = self.text_view.clone() else {
            return;
        };
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if log_preview {
                        read_log_preview(&path)
                    } else {
                        text_editor::read_preview_text(&path)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                let source = match result {
                    Ok(text) if markdown => text,
                    Ok(text) => plain_text_markdown(&text, &extension),
                    Err(error) => format!("**无法预览文件**\n\n{error}"),
                };
                text_view.update(cx, |view, cx| view.set_text(&source, cx));
                cx.notify();
            });
        });
    }

    fn start_image_load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !is_raster_image_preview_path(&path) {
            return;
        }
        self._load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    load_image_preview(
                        &path,
                        PREVIEW_WINDOW_IMAGE_MAX_WIDTH,
                        PREVIEW_WINDOW_IMAGE_MAX_HEIGHT,
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                this.loading = false;
                match result {
                    Ok(image) => {
                        this.image = Some(preview_render_image(image));
                        this.message = None;
                    }
                    Err(error) => this.message = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn install_close_guard(&self, window: &mut Window, cx: &mut Context<Self>) {
        let weak_entity = cx.weak_entity();
        window.on_window_should_close(cx, move |window, app| {
            let Some(entity) = weak_entity.upgrade() else {
                return true;
            };
            if entity.read(app).allow_close || !entity.read(app).dirty {
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
                        .title("有未保存的更改")
                        .description("关闭窗口会丢失当前修改。请先保存文件。")
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

    fn update_window_title(&self, window: &mut Window) {
        let marker = if self.dirty { "● " } else { "" };
        window.set_window_title(&format!("{marker}{} — Kite 编辑器", self.name));
    }

    fn on_save(&mut self, _: &SaveEditor, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.saving || !self.dirty {
            return;
        }
        let (Some(document), Some(editor)) = (self.document.clone(), self.editor.clone()) else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        self.saving = true;
        self.message = Some("正在保存…".into());
        cx.notify();
        self._save_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { document.save(&text) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(document) => {
                        this.dirty = editor.read(cx).value().as_ref() != document.text;
                        this.document = Some(document);
                        this.message = Some(
                            if this.dirty {
                                "还有未保存的更改"
                            } else {
                                "已保存"
                            }
                            .into(),
                        );
                        this.update_window_title(window);
                        editor.update(cx, |editor, cx| editor.focus(window, cx));
                    }
                    Err(error) => {
                        this.message = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn schedule_markdown_preview(&mut self, cx: &mut Context<Self>) {
        if !self.markdown_preview_visible {
            return;
        }
        self._markdown_preview_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MARKDOWN_PREVIEW_DELAY).await;
            let _ = this.update(cx, |this, cx| this.update_markdown_preview(cx));
        });
    }

    fn update_markdown_preview(&self, cx: &mut Context<Self>) {
        if !self.markdown_preview_visible {
            return;
        }
        if let (Some(editor), Some(preview)) = (&self.editor, &self.text_view) {
            let source = editor.read(cx).value();
            preview.update(cx, |view, cx| {
                view.set_text(
                    kite_core::preview::markdown_preview_source(source.as_ref()),
                    cx,
                );
            });
        }
    }

    fn toggle_markdown_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.markdown_preview_visible = !self.markdown_preview_visible;
        self._markdown_preview_task = Task::ready(());
        self.update_markdown_preview(cx);
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn open_search(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.focus(window, cx);
            editor.open_search(replace, cx);
        });
    }

    fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_editor(window, cx);
        window.dispatch_action(Box::new(Undo), cx);
    }

    fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_editor(window, cx);
        window.dispatch_action(Box::new(Redo), cx);
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.as_ref() {
            editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
    }

    fn toggle_word_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.word_wrap = !self.word_wrap;
        if let Some(editor) = self.editor.as_ref() {
            editor.update(cx, |editor, cx| {
                editor.set_soft_wrap(self.word_wrap, window, cx)
            });
        }
        cx.notify();
    }

    fn toggle_line_numbers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.line_numbers = !self.line_numbers;
        if let Some(editor) = self.editor.as_ref() {
            editor.update(cx, |editor, cx| {
                editor.set_line_number(self.line_numbers, window, cx)
            });
        }
        cx.notify();
    }

    fn toggle_whitespace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_whitespace = !self.show_whitespace;
        if let Some(editor) = self.editor.as_ref() {
            editor.update(cx, |editor, cx| {
                editor.set_show_whitespaces(self.show_whitespace, window, cx)
            });
        }
        cx.notify();
    }

    fn toggle_blame(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.blame_visible = !self.blame_visible;
        self.selected_blame = None;
        if self.blame_visible && self.blame_lines.is_empty() {
            self.reload_blame(cx);
        }
        cx.notify();
    }

    fn toggle_blame_whitespace(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.blame_ignore_whitespace = !self.blame_ignore_whitespace;
        if self.blame_visible {
            self.reload_blame(cx);
        }
    }

    fn reload_blame(&mut self, cx: &mut Context<Self>) {
        self.blame_lines.clear();
        self.blame_total_lines = 0;
        self.blame_end_line = 0;
        self.blame_error = None;
        self.load_blame_page(1, cx);
    }

    fn load_more_blame(&mut self, cx: &mut Context<Self>) {
        if self.blame_end_line >= self.blame_total_lines {
            return;
        }
        self.load_blame_page(self.blame_end_line.saturating_add(1), cx);
    }

    fn load_blame_page(&mut self, start_line: u32, cx: &mut Context<Self>) {
        if self.blame_loading {
            return;
        }
        let path = self.path.clone();
        let ignore_whitespace = self.blame_ignore_whitespace;
        self.blame_loading = true;
        self.blame_error = None;
        cx.notify();
        self._blame_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git_blame(
                        &path,
                        start_line,
                        DEFAULT_BLAME_PAGE_LINES,
                        ignore_whitespace,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.blame_loading = false;
                match result {
                    Ok(page) => {
                        if start_line == 1 {
                            this.blame_lines = page.lines;
                        } else {
                            this.blame_lines.extend(page.lines);
                        }
                        this.blame_total_lines = page.total_lines;
                        this.blame_end_line = page.end_line;
                    }
                    Err(error) => this.blame_error = Some(error.into()),
                }
                cx.notify();
            });
        });
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> AnyElement {
        if self.kind == PreviewKind::ReadOnlyText {
            return h_flex()
                .h(px(38.))
                .w_full()
                .flex_shrink_0()
                .px_3()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Button::new("reader-blame")
                        .ghost()
                        .compact()
                        .icon(IconName::GitBranch)
                        .label(if self.blame_visible {
                            "隐藏 Blame"
                        } else {
                            "Blame"
                        })
                        .selected(self.blame_visible)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_blame(window, cx)
                        })),
                )
                .when(self.blame_visible, |bar| {
                    bar.child(
                        Button::new("reader-blame-whitespace")
                            .ghost()
                            .compact()
                            .icon(IconName::Pilcrow)
                            .label("忽略空白")
                            .selected(self.blame_ignore_whitespace)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_blame_whitespace(window, cx)
                            })),
                    )
                })
                .into_any_element();
        }
        if self.kind != PreviewKind::Editor {
            return div().into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        h_flex()
            .h(px(38.))
            .w_full()
            .flex_shrink_0()
            .px_3()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("editor-save")
                    .ghost()
                    .compact()
                    .icon(IconName::Save)
                    .label("保存")
                    .tooltip("保存 Ctrl+S")
                    .disabled(self.saving || !self.dirty)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)),
                    ),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(cx.theme().border).mx_1())
            .child(
                Button::new("editor-undo")
                    .ghost()
                    .compact()
                    .icon(IconName::Undo)
                    .tooltip("撤销 Ctrl+Z")
                    .disabled(self.loading || self.document.is_none())
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.undo(window, cx)),
                    ),
            )
            .child(
                Button::new("editor-redo")
                    .ghost()
                    .compact()
                    .icon(IconName::Redo)
                    .tooltip("重做 Ctrl+Y")
                    .disabled(self.loading || self.document.is_none())
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.redo(window, cx)),
                    ),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(cx.theme().border).mx_1())
            .child(
                Button::new("editor-find")
                    .ghost()
                    .compact()
                    .icon(IconName::Search)
                    .label("查找")
                    .tooltip("查找 Ctrl+F")
                    .disabled(self.loading || self.document.is_none())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_search(false, window, cx)
                    })),
            )
            .child(
                Button::new("editor-replace")
                    .ghost()
                    .compact()
                    .icon(IconName::Replace)
                    .label("替换")
                    .tooltip("查找并替换 Ctrl+H")
                    .disabled(self.loading || self.document.is_none())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_search(true, window, cx)
                    })),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(cx.theme().border).mx_1())
            .child(
                Button::new("editor-word-wrap")
                    .ghost()
                    .compact()
                    .icon(IconName::TextWrap)
                    .tooltip(if self.word_wrap {
                        "关闭自动换行"
                    } else {
                        "开启自动换行"
                    })
                    .selected(self.word_wrap)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_word_wrap(window, cx)
                    })),
            )
            .child(
                Button::new("editor-line-numbers")
                    .ghost()
                    .compact()
                    .icon(IconName::ListOrdered)
                    .tooltip(if self.line_numbers {
                        "隐藏行号"
                    } else {
                        "显示行号"
                    })
                    .selected(self.line_numbers)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_line_numbers(window, cx)
                    })),
            )
            .child(
                Button::new("editor-whitespace")
                    .ghost()
                    .compact()
                    .icon(IconName::Pilcrow)
                    .tooltip(if self.show_whitespace {
                        "隐藏空白字符"
                    } else {
                        "显示空白字符"
                    })
                    .selected(self.show_whitespace)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_whitespace(window, cx)
                    })),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(cx.theme().border).mx_1())
            .child(
                Button::new("editor-blame")
                    .ghost()
                    .compact()
                    .icon(IconName::GitBranch)
                    .label(if self.blame_visible {
                        "隐藏 Blame"
                    } else {
                        "Blame"
                    })
                    .selected(self.blame_visible)
                    .disabled(self.loading || self.document.is_none())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_blame(window, cx)
                    })),
            )
            .when(self.blame_visible, |bar| {
                bar.child(
                    Button::new("editor-blame-whitespace")
                        .ghost()
                        .compact()
                        .icon(IconName::Pilcrow)
                        .label("忽略空白")
                        .selected(self.blame_ignore_whitespace)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_blame_whitespace(window, cx)
                        })),
                )
            })
            .when(self.text_view.is_some(), |bar| {
                bar.child(
                    Button::new("editor-markdown-preview")
                        .ghost()
                        .compact()
                        .icon(IconName::PanelRightOpen)
                        .label("实时预览")
                        .selected(self.markdown_preview_visible)
                        .disabled(self.document.is_none())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_markdown_preview(window, cx);
                        })),
                )
            })
            .child(
                div()
                    .ml_auto()
                    .text_xs()
                    .text_color(muted)
                    .child(self.editor_metadata_label()),
            )
            .into_any_element()
    }

    fn editor_metadata_label(&self) -> SharedString {
        let Some(document) = self.document.as_ref() else {
            return "正在识别…".into();
        };
        let language = display_language(&document.language);
        let line_ending = match document.line_ending {
            text_editor::TextLineEnding::Lf => "LF",
            text_editor::TextLineEnding::CrLf => "CRLF",
        };
        let bom = if document.has_bom { " · BOM" } else { "" };
        format!("{language} · UTF-8{bom} · {line_ending}").into()
    }

    fn render_editor_body(&self, cx: &App) -> AnyElement {
        let Some(editor) = self.editor.as_ref() else {
            return div().into_any_element();
        };
        if self.loading {
            return centered_message("正在加载文本…", cx);
        }
        if self.document.is_none() {
            return centered_message(
                self.message.as_deref().unwrap_or("无法打开此文本文件。"),
                cx,
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .p_2()
            .bg(cx.theme().background)
            .child(
                Editor::new(editor)
                    .appearance(false)
                    .bordered(false)
                    .h_full()
                    .w_full()
                    .aria_label("Kite 文本编辑器"),
            )
            .into_any_element()
    }

    fn render_read_only_text_body(&self, cx: &App) -> AnyElement {
        let Some(text_view) = self.text_view.as_ref() else {
            return div().into_any_element();
        };
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                div()
                    .id("enlarged-preview-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_8()
                    .py_6()
                    .child(div().w_full().max_w(px(920.)).mx_auto().child(
                        if self.kind == PreviewKind::Editor {
                            gpui_kit::base::TextView::new(text_view)
                                .w_full()
                                .text_sm()
                                .selectable(true)
                                .scrollable(false)
                                .image_source(|_| ImageSource::Custom(Arc::new(|_, _| None)))
                                .on_link_click(|url, _, _, cx| {
                                    if url.starts_with("https://") || url.starts_with("http://") {
                                        cx.open_url(url);
                                    }
                                })
                                .into_any_element()
                        } else {
                            TextView::new(text_view)
                                .w_full()
                                .text_sm()
                                .selectable(true)
                                .scrollable(false)
                                .into_any_element()
                        },
                    )),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll)
                        .id("enlarged-preview-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .styles(|styles| {
                            let track = cx.theme().muted.opacity(0.55);
                            let thumb = cx.theme().muted_foreground.opacity(0.75);
                            let thumb_hover = cx.theme().muted_foreground.opacity(0.95);
                            styles
                                .track(|style| style.bg(track).width(px(10.)))
                                .thumb(|style| {
                                    style.bg(thumb).width(px(6.)).inset(px(2.)).radius(px(99.))
                                })
                                .thumb_hover(|style| style.bg(thumb_hover))
                        })
                        .viewport_from_layout(),
                ),
            )
            .into_any_element()
    }

    fn render_blame(&self, cx: &Context<Self>) -> AnyElement {
        if self.blame_loading && self.blame_lines.is_empty() {
            return centered_message("正在读取 Blame…", cx);
        }
        if let Some(error) = self.blame_error.as_ref() {
            return centered_message(error.as_ref(), cx);
        }
        if self.blame_lines.is_empty() {
            return centered_message("此文件没有可显示的 Blame 信息。", cx);
        }

        let selected = self.selected_blame.as_ref().map(|line| line.line_number);
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let list = uniform_list(
            "preview-window-blame-list",
            self.blame_lines.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|row_ix| {
                        let line = this.blame_lines[row_ix].clone();
                        let line_for_click = line.clone();
                        let tooltip = blame_tooltip(&line);
                        let color = blame_line_color(&line);
                        h_flex()
                            .id(("preview-blame-row", row_ix))
                            .h(px(24.))
                            .w_full()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .cursor_pointer()
                            .when(selected == Some(line.line_number), |this| {
                                this.bg(accent.opacity(0.14))
                            })
                            .child(
                                div()
                                    .w(px(44.))
                                    .text_right()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(line.line_number.to_string()),
                            )
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_xs()
                                    .text_color(color)
                                    .child(line.short_hash.clone()),
                            )
                            .child(
                                div()
                                    .w(px(110.))
                                    .truncate()
                                    .text_xs()
                                    .text_color(color)
                                    .child(line.author.clone()),
                            )
                            .child(
                                div()
                                    .w(px(88.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(blame_date(line.author_time)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(line.code.clone()),
                            )
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                let hash = line_for_click.commit_hash.clone();
                                this.selected_blame = Some(line_for_click.clone());
                                if let Err(error) = git_log_window::open_git_log_window(
                                    this.path.clone(),
                                    Some(hash),
                                    cx,
                                ) {
                                    this.blame_error = Some(error.into());
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .w_full();

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                h_flex()
                    .h(px(34.))
                    .w_full()
                    .flex_shrink_0()
                    .px_3()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(muted)
                    .child(format!(
                        "显示 {} / {} 行",
                        self.blame_lines.len(),
                        self.blame_total_lines
                    ))
                    .when(self.blame_end_line < self.blame_total_lines, |bar| {
                        bar.child(
                            Button::new("preview-blame-more")
                                .ghost()
                                .compact()
                                .label(if self.blame_loading {
                                    "加载中…"
                                } else {
                                    "加载更多"
                                })
                                .disabled(self.blame_loading)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.load_more_blame(cx)
                                })),
                        )
                    }),
            )
            .child(list)
            .when_some(self.selected_blame.as_ref(), |this, line| {
                this.child(
                    v_flex()
                        .flex_shrink_0()
                        .w_full()
                        .gap_1()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(accent.opacity(0.08))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .child(line.short_hash.clone()),
                                )
                                .child(div().text_xs().text_color(muted).child(format!(
                                    "{} · {}",
                                    line.author,
                                    blame_date(line.author_time)
                                ))),
                        )
                        .child(div().text_sm().font_medium().child(line.summary.clone()))
                        .when_some(line.author_mail.as_ref(), |detail, mail| {
                            detail.child(div().text_xs().text_color(muted).child(mail.clone()))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_markdown_workspace(&self, cx: &App) -> AnyElement {
        let editor = self.render_editor_body(cx);
        if !self.markdown_preview_visible || self.document.is_none() {
            return editor;
        }
        let heading = |label: &'static str| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .border_b_1()
                .border_color(cx.theme().border)
                .child(label)
        };
        h_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(heading("Markdown 源码"))
                    .child(editor),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(heading("实时预览"))
                    .child(self.render_read_only_text_body(cx)),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &App) -> AnyElement {
        if self.kind != PreviewKind::Editor {
            return div().into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let dirty = if self.dirty { "未保存" } else { "已保存" };
        let line_count = self
            .editor
            .as_ref()
            .map(|editor| {
                editor
                    .read(cx)
                    .text()
                    .chars()
                    .filter(|character| *character == '\n')
                    .count()
                    + 1
            })
            .unwrap_or(1);
        h_flex()
            .h(px(28.))
            .w_full()
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(muted)
            .child(if self.dirty { "● 未保存" } else { dirty })
            .child(format!("{line_count} 行"))
            .child(self.editor_metadata_label())
            .when(self.saving, |this| this.child("正在写入磁盘…"))
            .when_some(self.message.as_ref(), |this, message| {
                this.child(div().ml_auto().truncate().child(message.clone()))
            })
            .into_any_element()
    }
}

impl Render for PreviewWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let body = if self.blame_visible
            && matches!(self.kind, PreviewKind::Editor | PreviewKind::ReadOnlyText)
        {
            self.render_blame(cx)
        } else {
            match self.kind {
                PreviewKind::Editor => self.render_markdown_workspace(cx),
                PreviewKind::ReadOnlyText => self.render_read_only_text_body(cx),
                PreviewKind::Image => {
                    if let Some(image) = self.image.clone() {
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .p_6()
                            .child(
                                img(image)
                                    .id("preview-window-image")
                                    .max_w_full()
                                    .max_h_full()
                                    .object_fit(ObjectFit::Contain),
                            )
                            .into_any_element()
                    } else if self.loading {
                        centered_message("正在生成图片预览…", cx)
                    } else if let Some(message) = self.message.clone() {
                        centered_message(&message, cx)
                    } else {
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .p_6()
                            .child(
                                img(self.path.clone())
                                    .id("preview-window-source-image")
                                    .max_w_full()
                                    .max_h_full()
                                    .object_fit(ObjectFit::Contain),
                            )
                            .into_any_element()
                    }
                }
                PreviewKind::Other => v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(Icon::new(IconName::File).size(px(72.)).text_color(muted))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("此文件没有内置预览"),
                    )
                    .into_any_element(),
            }
        };

        v_flex()
            .key_context("KiteEditor")
            .on_action(cx.listener(Self::on_save))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h(px(48.))
                    .w_full()
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Icon::new(if self.kind == PreviewKind::Editor {
                            IconName::FileCode
                        } else {
                            IconName::FileText
                        })
                        .size_5()
                        .text_color(muted),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(self.name.clone()),
                    )
                    .when(self.dirty, |this| {
                        this.child(div().text_xs().text_color(muted).child("●"))
                    })
                    .when(self.loading, |this| {
                        this.child(div().text_xs().text_color(muted).child("正在加载…"))
                    })
                    .child(
                        div()
                            .max_w(px(520.))
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(self.path_label.clone()),
                    ),
            )
            .child(self.render_toolbar(cx))
            .child(body)
            .child(self.render_status_bar(cx))
    }
}

pub(crate) fn preview_render_image(data: PreviewImageData) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_raw(data.width, data.height, data.bgra)
        .expect("preview image dimensions must match its BGRA buffer");
    Arc::new(RenderImage::new([image::Frame::new(buffer)]))
}

fn blame_line_color(line: &GitBlameLine) -> Hsla {
    if line.is_uncommitted {
        return rgb(0xef6b73).into();
    }
    let now = chrono::Utc::now().timestamp();
    let age_days = ((now - line.author_time).max(0) as f64) / 86_400.0;
    let alpha = (1.0 - (age_days / 1_460.0).min(0.58)) as f32;
    let color: Hsla = rgb(0x5e9cff).into();
    color.opacity(alpha)
}

fn blame_date(unix_seconds: i64) -> String {
    chrono::DateTime::<chrono::Local>::from(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_seconds.max(0) as u64),
    )
    .format("%Y-%m-%d")
    .to_string()
}

fn blame_tooltip(line: &GitBlameLine) -> String {
    let mut parts = vec![
        line.short_hash.clone(),
        line.author.clone(),
        blame_date(line.author_time),
        line.summary.clone(),
    ];
    if let Some(mail) = line.author_mail.as_ref() {
        parts.push(mail.clone());
    }
    parts.join(" · ")
}

fn centered_message(message: &str, cx: &App) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            div()
                .max_w(px(560.))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message.to_string()),
        )
        .into_any_element()
}

fn display_language(language: &str) -> &'static str {
    match language {
        "rust" => "Rust",
        "javascript" => "JavaScript",
        "typescript" => "TypeScript",
        "tsx" => "TSX",
        "python" => "Python",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" => "YAML",
        "markdown" => "Markdown",
        "html" => "HTML",
        "css" => "CSS",
        "c" => "C",
        "cpp" => "C++",
        "csharp" => "C#",
        "java" => "Java",
        "go" => "Go",
        "kotlin" => "Kotlin",
        "bash" => "Shell",
        "sql" => "SQL",
        "swift" => "Swift",
        "lua" => "Lua",
        "ruby" => "Ruby",
        "php" => "PHP",
        "proto" => "Protocol Buffers",
        "cmake" => "CMake",
        "make" => "Make",
        "diff" => "Diff",
        _ => "纯文本",
    }
}

fn plain_text_markdown(text: &str, extension: &str) -> String {
    let max_backticks = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((max_backticks + 1).max(3));
    let language = if extension.is_empty() {
        "text"
    } else {
        extension
    };
    format!("{fence}{language}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::{display_language, plain_text_markdown};

    #[test]
    fn plain_text_uses_a_safe_code_fence() {
        let value = plain_text_markdown("`inline` and ```block```", "rs");
        assert!(value.starts_with("````rs\n"));
        assert!(value.ends_with("\n````"));
    }

    #[test]
    fn editor_language_labels_are_stable() {
        assert_eq!(display_language("rust"), "Rust");
        assert_eq!(display_language("unknown"), "纯文本");
    }

    #[test]
    #[ignore = "opens a Markdown editor window on the Windows desktop"]
    fn live_markdown_follows_unsaved_edits_and_catches_up_when_reopened() {
        use super::{MARKDOWN_PREVIEW_DELAY, PreviewWindow};
        use crate::fs::{EntryKind, FileEntry};
        use gpui_kit::{AppContext as _, WindowOptions};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("kite-live-markdown-{stamp}.mk"));
        let original = "# Initial heading\n";
        std::fs::write(&path, original).expect("write Markdown fixture");
        let entry = FileEntry {
            path: path.clone(),
            name: "Live Markdown fixture.mk".to_string(),
            kind: EntryKind::File,
            size: original.len() as u64,
            modified: None,
            extension: "mk".to_string(),
            git_status: None,
            submodule_status: None,
            svn_status: None,
        };
        let (observed, result) = std::sync::mpsc::channel();
        gpui_kit::application()
            .with_assets(gpui_kit::assets::AllAssets)
            .run(move |cx| {
                gpui_kit::init(cx);
                let (window, view) =
                    gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                        cx.new(|cx| PreviewWindow::new(entry, window, cx))
                    })
                    .expect("open Markdown test window");
                cx.spawn(async move |cx| {
                    let check = async {
                        let rendered = || {
                            cx.update(|cx| {
                                view.read(cx)
                                    .text_view
                                    .as_ref()
                                    .map(|preview| {
                                        preview.read(cx).rendered_text().as_str().to_string()
                                    })
                                    .unwrap_or_default()
                            })
                        };
                        for (expected, next) in [
                            ("Initial heading", Some((false, "# Updated heading\n"))),
                            ("Updated heading", Some((true, "# Hidden edit\n"))),
                            ("Hidden edit", None),
                        ] {
                            if expected == "Hidden edit" {
                                if !rendered().contains("Updated heading") {
                                    return Err("hidden preview was updated");
                                }
                                cx.update(|cx| {
                                    window.update(cx, |_, window, cx| {
                                        view.update(cx, |editor, cx| {
                                            editor.toggle_markdown_preview(window, cx)
                                        });
                                    })
                                })
                                .map_err(|_| "show preview")?;
                            }
                            for _ in 0..50 {
                                if rendered().contains(expected) {
                                    break;
                                }
                                cx.background_executor()
                                    .timer(Duration::from_millis(30))
                                    .await;
                            }
                            if !rendered().contains(expected) {
                                return Err(expected);
                            }
                            if let Some((hide, next)) = next {
                                cx.update(|cx| {
                                    window.update(cx, |_, window, cx| {
                                        view.update(cx, |editor, cx| {
                                            if hide {
                                                editor.toggle_markdown_preview(window, cx);
                                            }
                                            editor.editor.as_ref().unwrap().update(
                                                cx,
                                                |state, cx| {
                                                    state.replace_all(next, window, cx);
                                                },
                                            );
                                        });
                                    })
                                })
                                .map_err(|_| "edit fixture")?;
                                if hide {
                                    cx.background_executor()
                                        .timer(MARKDOWN_PREVIEW_DELAY * 2)
                                        .await;
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    let _ = observed.send(check);
                    cx.update(|cx| cx.quit());
                })
                .detach();
            });
        let unchanged = std::fs::read_to_string(&path).expect("read fixture after editing");
        std::fs::remove_file(path).expect("remove test fixture");
        result
            .try_recv()
            .expect("observe Markdown window")
            .expect("live preview state");
        assert_eq!(unchanged, original);
    }
}
