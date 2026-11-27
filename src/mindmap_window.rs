use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kite_core::mindmap_ffi::{
    self, MindmapConnectorKindDto, MindmapDateKindDto, MindmapDatePlacementDto,
    MindmapDatePrecisionDto, MindmapDateRequestDto, MindmapDateStyleDto, MindmapDateTargetDto,
    MindmapDocumentDto, MindmapDocumentStyleDto, MindmapDocumentStylePatchDto, MindmapEditDto,
    MindmapGraphStructureDto, MindmapTodoDto, MindmapTodoStatusDto, MindmapTopicMarkersDto,
    MindmapTopicStyleDto,
};
use serde_json::Value;

use crate::actions::*;
use crate::mindmap_actions::MindmapToggleTodo;
use crate::mindmap_ui_model::{
    MindmapLayout, MindmapNodeLayout, MindmapOutlineRow, MindmapTopic, MindmapTree,
    letter_spacing_preview, measurement_number, missing_font_family, resolve_render_font,
    system_date_pattern, system_font_families, topic_json,
};

#[derive(Clone, Debug)]
struct MindmapSnapshot {
    content_json: String,
    source_sha256: String,
}

impl MindmapSnapshot {
    fn from_document(document: &MindmapDocumentDto) -> Self {
        Self {
            content_json: document.content_json.clone(),
            source_sha256: document.source_sha256.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct GraphDraft {
    structure: MindmapGraphStructureDto,
    connector: MindmapConnectorKindDto,
    corner_radius: u16,
    width: String,
    color: String,
    pattern: String,
}

impl Default for GraphDraft {
    fn default() -> Self {
        Self {
            structure: MindmapGraphStructureDto::LogicRight,
            connector: MindmapConnectorKindDto::Curved,
            corner_radius: 12,
            width: "2pt".to_string(),
            color: "#8F9BA8".to_string(),
            pattern: "solid".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
struct DateDraft {
    style: MindmapDateStyleDto,
    precision: MindmapDatePrecisionDto,
    target: MindmapDateTargetDto,
    placement: MindmapDatePlacementDto,
    kind: MindmapDateKindDto,
}

impl Default for DateDraft {
    fn default() -> Self {
        Self {
            style: MindmapDateStyleDto::Iso,
            precision: MindmapDatePrecisionDto::DateOnly,
            target: MindmapDateTargetDto::Title,
            placement: MindmapDatePlacementDto::End,
            kind: MindmapDateKindDto::Date,
        }
    }
}

#[derive(Clone, Debug)]
struct EdgeRender {
    from_x: f32,
    from_y: f32,
    to_x: f32,
    to_y: f32,
    width: f32,
    color: Hsla,
    connector: MindmapConnectorKindDto,
    corner_radius: f32,
    pattern: String,
}

pub struct MindmapWindow {
    path: PathBuf,
    document: MindmapDocumentDto,
    tree: MindmapTree,
    layout: MindmapLayout,
    selected_id: Option<String>,
    collapsed: BTreeSet<String>,
    search_input: Entity<InputState>,
    inline_input: Entity<InputState>,
    date_input: Entity<InputState>,
    time_input: Entity<InputState>,
    editing_id: Option<String>,
    graph: GraphDraft,
    style_draft: MindmapTopicStyleDto,
    markers: MindmapTopicMarkersDto,
    todo: MindmapTodoDto,
    date: DateDraft,
    zoom: f32,
    pan: Point<Pixels>,
    panning: bool,
    last_pointer: Option<Point<Pixels>>,
    canvas_size: (f32, f32),
    outline_scroll: ScrollHandle,
    inspector_scroll: ScrollHandle,
    show_inspector: bool,
    zen_mode: bool,
    copied_topic_json: Option<String>,
    copied_style: Option<MindmapTopicStyleDto>,
    system_fonts: Vec<String>,
    undo_stack: Vec<MindmapSnapshot>,
    redo_stack: Vec<MindmapSnapshot>,
    status: SharedString,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _search_subscription: Subscription,
    _inline_subscription: Subscription,
}

pub(crate) fn open_mindmap_window(path: PathBuf, cx: &mut App) -> Result<(), String> {
    let title = path
        .file_name()
        .map(|name| format!("Kite Mindmap · {}", name.to_string_lossy()))
        .unwrap_or_else(|| "Kite Mindmap".to_string());
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1320.), px(860.)), cx)),
        window_min_size: Some(size(px(860.), px(600.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| MindmapWindow::new(path.clone(), window, cx))
    })
    .map(|_| ())
    .map_err(|error| format!("无法打开 Mindmap 编辑器：{error}"))
}

impl MindmapWindow {
    fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索主题")
                .clean_on_escape()
        });
        let inline_input = cx.new(|cx| InputState::new(window, cx).clean_on_escape());
        let now = Local::now();
        let date_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("YYYY-MM-DD")
                .clean_on_escape();
            input.set_value(now.format("%Y-%m-%d").to_string(), window, cx);
            input
        });
        let time_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("HH:MM:SS")
                .clean_on_escape();
            input.set_value(now.format("%H:%M:%S").to_string(), window, cx);
            input
        });
        let search_subscription = cx.subscribe_in(
            &search_input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                    this.rebuild_layout();
                    cx.notify();
                }
            },
        );
        let inline_subscription = cx.subscribe_in(
            &inline_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => {
                    this.commit_inline_edit(window, cx);
                }
                InputEvent::Blur if this.editing_id.is_some() => {
                    this.commit_inline_edit(window, cx);
                }
                _ => {}
            },
        );
        let mut this = Self {
            path: path.clone(),
            document: MindmapDocumentDto {
                path: path.display().to_string(),
                content_json: "[]".to_string(),
                source_sha256: String::new(),
                sheet_count: 0,
                entry_count: 0,
            },
            tree: MindmapTree::default(),
            layout: MindmapLayout::default(),
            selected_id: None,
            collapsed: BTreeSet::new(),
            search_input,
            inline_input,
            date_input,
            time_input,
            editing_id: None,
            graph: GraphDraft::default(),
            style_draft: MindmapTopicStyleDto::default(),
            markers: empty_markers(),
            todo: MindmapTodoDto {
                status: MindmapTodoStatusDto::NotStarted,
                progress_eighths: None,
                is_complete: false,
                task_presentation: None,
                todo_status: Some("todo".to_string()),
                todo_progress: Some(0.0),
            },
            date: DateDraft::default(),
            zoom: 0.82,
            pan: point(px(0.), px(0.)),
            panning: false,
            last_pointer: None,
            canvas_size: (980.0, 680.0),
            outline_scroll: ScrollHandle::new(),
            inspector_scroll: ScrollHandle::new(),
            show_inspector: true,
            zen_mode: false,
            copied_topic_json: None,
            copied_style: None,
            system_fonts: system_font_families(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            status: "正在通过 Rust Core 读取 XMind…".into(),
            error: None,
            focus_handle: cx.focus_handle(),
            _search_subscription: search_subscription,
            _inline_subscription: inline_subscription,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        match mindmap_ffi::open_mindmap_document(self.path.display().to_string()) {
            Ok(document) => match MindmapTree::from_content_json(&document.content_json) {
                Ok(tree) => {
                    self.selected_id = tree.root_id.clone();
                    self.document = document;
                    self.tree = tree;
                    self.error = None;
                    self.status = format!(
                        "Rust Core 已读取 {} 个主题 · {} 个包条目 · 自动保存",
                        self.tree.topics.len(),
                        self.document.entry_count
                    )
                    .into();
                    self.load_graph(cx);
                    self.load_inspector(cx);
                    self.rebuild_layout();
                }
                Err(error) => self.fail(error),
            },
            Err(error) => self.fail(error.to_string()),
        }
        cx.notify();
    }

    fn fail(&mut self, error: impl Into<SharedString>) {
        self.error = Some(error.into());
        self.status = "操作失败".into();
    }

    fn load_graph(&mut self, _cx: &mut Context<Self>) {
        match mindmap_ffi::read_mindmap_document_style(self.path.display().to_string(), 0) {
            Ok(style) => self.graph = graph_draft_from_style(style),
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn load_inspector(&mut self, _cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            self.style_draft = MindmapTopicStyleDto::default();
            self.markers = empty_markers();
            return;
        };
        let path = self.path.display().to_string();
        match mindmap_ffi::read_mindmap_topic_style(path.clone(), 0, topic_id.clone()) {
            Ok(style) => self.style_draft = style,
            Err(error) => self.fail(error.to_string()),
        }
        match mindmap_ffi::read_mindmap_topic_markers(path.clone(), 0, topic_id.clone()) {
            Ok(markers) => self.markers = markers,
            Err(error) => self.fail(error.to_string()),
        }
        match mindmap_ffi::read_mindmap_topic_todo(path, 0, topic_id) {
            Ok(mut todo) => {
                if let Some(topic) = self
                    .tree
                    .topics
                    .get(&self.selected_id.clone().unwrap_or_default())
                {
                    merge_topic_task_fields(&mut todo, topic);
                }
                self.todo = todo;
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn selected_topic(&self) -> Option<&MindmapTopic> {
        self.tree.selected(self.selected_id.as_deref())
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected_id = id;
        self.editing_id = None;
        self.load_inspector(cx);
        cx.notify();
    }

    fn rebuild_layout(&mut self) {
        self.layout = self
            .tree
            .layout(&self.collapsed, self.graph.structure, self.zoom);
        if let Some(selected) = self.selected_id.as_deref()
            && self.layout.node(selected).is_none()
        {
            self.selected_id = self.tree.root_id.clone();
        }
    }

    fn record_success(&mut self, previous: MindmapDocumentDto, next: MindmapDocumentDto) {
        self.undo_stack
            .push(MindmapSnapshot::from_document(&previous));
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        let previous_ids = self.tree.topics.keys().cloned().collect::<BTreeSet<_>>();
        self.document = next;
        match MindmapTree::from_content_json(&self.document.content_json) {
            Ok(tree) => {
                let new_id = tree
                    .topics
                    .keys()
                    .find(|id| !previous_ids.contains(*id))
                    .cloned();
                self.tree = tree;
                if let Some(new_id) = new_id {
                    self.selected_id = Some(new_id);
                } else if self
                    .selected_id
                    .as_ref()
                    .is_some_and(|id| !self.tree.topics.contains_key(id))
                {
                    self.selected_id = self.tree.root_id.clone();
                }
                self.rebuild_layout();
                self.load_inspector_detached();
                self.error = None;
                self.status = "已通过 Rust Core 原子保存".into();
            }
            Err(error) => self.fail(error),
        }
    }

    fn load_inspector_detached(&mut self) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let path = self.path.display().to_string();
        if let Ok(style) = mindmap_ffi::read_mindmap_topic_style(path.clone(), 0, topic_id.clone())
        {
            self.style_draft = style;
        }
        if let Ok(markers) =
            mindmap_ffi::read_mindmap_topic_markers(path.clone(), 0, topic_id.clone())
        {
            self.markers = markers;
        }
        if let Ok(mut todo) = mindmap_ffi::read_mindmap_topic_todo(path, 0, topic_id.clone()) {
            if let Some(topic) = self.tree.topics.get(&topic_id) {
                merge_topic_task_fields(&mut todo, topic);
            }
            self.todo = todo;
        }
    }

    fn run_edit(&mut self, edit: MindmapEditDto, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.document.clone();
        match mindmap_ffi::edit_mindmap_document(
            self.path.display().to_string(),
            previous.source_sha256.clone(),
            edit,
        ) {
            Ok(next) => {
                self.record_success(previous, next);
                self.focus_canvas(window, cx);
            }
            Err(error) => self.fail(error.to_string()),
        }
        cx.notify();
    }

    fn run_document_operation<F>(&mut self, operation: F, cx: &mut Context<Self>) -> bool
    where
        F: FnOnce(String) -> Result<MindmapDocumentDto, kite_core::ffi::KiteError>,
    {
        let previous = self.document.clone();
        let succeeded = match operation(previous.source_sha256.clone()) {
            Ok(next) => {
                self.record_success(previous, next);
                true
            }
            Err(error) => {
                self.fail(error.to_string());
                false
            }
        };
        cx.notify();
        succeeded
    }

    fn focus_canvas(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
    }

    fn select_root(&mut self, cx: &mut Context<Self>) {
        self.select(self.tree.root_id.clone(), cx);
        self.pan = point(px(0.), px(0.));
        cx.notify();
    }

    fn fit_canvas(&mut self, cx: &mut Context<Self>) {
        self.rebuild_layout();
        if self.layout.width > 0.0 && self.layout.height > 0.0 {
            let scale = ((self.canvas_size.0 - 160.0) / self.layout.width)
                .min((self.canvas_size.1 - 120.0) / self.layout.height)
                .clamp(0.35, 1.6);
            self.zoom = (self.zoom * scale).clamp(0.25, 2.2);
            self.pan = point(px(0.), px(0.));
            self.rebuild_layout();
        }
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom * factor).clamp(0.25, 2.5);
        self.rebuild_layout();
        cx.notify();
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        self.pan = point(px(0.), px(0.));
        self.rebuild_layout();
        cx.notify();
    }

    fn toggle_branch(&mut self, expand_all: bool, cx: &mut Context<Self>) {
        let Some(selected) = self.selected_id.clone() else {
            return;
        };
        if expand_all {
            let mut stack = vec![selected];
            while let Some(id) = stack.pop() {
                self.collapsed.remove(&id);
                if let Some(topic) = self.tree.topics.get(&id) {
                    stack.extend(topic.child_ids.iter().cloned());
                }
            }
        } else if !self.collapsed.remove(&selected) {
            self.collapsed.insert(selected);
        }
        self.rebuild_layout();
        cx.notify();
    }

    fn begin_inline_edit(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic) = self.tree.topics.get(&id) else {
            return;
        };
        let title = topic.title.clone();
        self.selected_id = Some(id.clone());
        self.editing_id = Some(id);
        self.inline_input.update(cx, |input, cx| {
            input.set_value(title, window, cx);
            input.focus(window, cx);
        });
        self.load_inspector(cx);
        cx.notify();
    }

    fn commit_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing_id.take() else {
            return;
        };
        let title = self.inline_input.read(cx).value().trim().to_string();
        let current = self.tree.topics.get(&id).map(|topic| topic.title.clone());
        if !title.is_empty() && current.as_deref() != Some(title.as_str()) {
            self.run_edit(
                MindmapEditDto::Rename {
                    topic_id: id,
                    title,
                },
                window,
                cx,
            );
        } else {
            self.focus_canvas(window, cx);
            cx.notify();
        }
    }

    fn add_child(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent_id) = self
            .selected_id
            .clone()
            .or_else(|| self.tree.root_id.clone())
        else {
            return;
        };
        self.run_edit(
            MindmapEditDto::AddChild {
                parent_id,
                title: "新主题".to_string(),
            },
            window,
            cx,
        );
    }

    fn add_sibling(&mut self, before: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let edit = if before {
            MindmapEditDto::InsertSiblingBefore {
                topic_id,
                title: "新主题".to_string(),
            }
        } else {
            MindmapEditDto::AddSibling {
                topic_id,
                title: "新主题".to_string(),
            }
        };
        self.run_edit(edit, window, cx);
    }

    fn new_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        self.run_edit(
            MindmapEditDto::NewParent {
                topic_id,
                title: "新父主题".to_string(),
            },
            window,
            cx,
        );
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        if self.tree.root_id.as_deref() == Some(topic_id.as_str()) {
            self.error = Some("中心主题不能删除".into());
            cx.notify();
            return;
        }
        self.run_edit(MindmapEditDto::Delete { topic_id }, window, cx);
    }

    fn move_selected(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        self.run_edit(
            if up {
                MindmapEditDto::MoveUp { topic_id }
            } else {
                MindmapEditDto::MoveDown { topic_id }
            },
            window,
            cx,
        );
    }

    fn copy_selected(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let Some(json) = topic_json(&self.document.content_json, &topic_id) else {
            self.error = Some("无法复制当前主题".into());
            cx.notify();
            return;
        };
        self.copied_topic_json = Some(json.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(json));
        if cut {
            self.delete_selected(window, cx);
        } else {
            self.status = "已复制主题子树".into();
            cx.notify();
        }
    }

    fn paste_selected(&mut self, plain_text: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent_id) = self
            .selected_id
            .clone()
            .or_else(|| self.tree.root_id.clone())
        else {
            return;
        };
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .or_else(|| self.copied_topic_json.clone());
        let Some(text) = clipboard else {
            self.status = "剪贴板为空".into();
            cx.notify();
            return;
        };
        let topic_json = if plain_text {
            None
        } else {
            serde_json::from_str::<Value>(&text)
                .ok()
                .filter(|value| value.get("id").is_some() && value.get("title").is_some())
                .map(|value| value.to_string())
        };
        let edit = if let Some(topic_json) = topic_json {
            MindmapEditDto::Paste {
                parent_id,
                topic_json,
            }
        } else {
            let title = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("粘贴的主题")
                .chars()
                .take(160)
                .collect::<String>();
            MindmapEditDto::AddChild { parent_id, title }
        };
        self.run_edit(edit, window, cx);
    }

    fn copy_style(&mut self, cx: &mut Context<Self>) {
        if self.selected_id.is_some() {
            self.copied_style = Some(self.style_draft.clone());
            self.status = "已复制主题样式".into();
        } else {
            self.status = "请先选择主题".into();
        }
        cx.notify();
    }

    fn paste_style(&mut self, cx: &mut Context<Self>) {
        let Some(style) = self.copied_style.clone() else {
            self.status = "尚未复制主题样式".into();
            cx.notify();
            return;
        };
        self.update_style(style, cx);
    }

    fn update_style(&mut self, style: MindmapTopicStyleDto, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            self.status = "请先选择主题".into();
            cx.notify();
            return;
        };
        let path = self.path.display().to_string();
        self.run_document_operation(
            |expected| mindmap_ffi::update_mindmap_topic_style(path, expected, 0, topic_id, style),
            cx,
        );
    }

    fn mutate_style(
        &mut self,
        mutate: impl FnOnce(&mut MindmapTopicStyleDto),
        cx: &mut Context<Self>,
    ) {
        let mut style = self.style_draft.clone();
        mutate(&mut style);
        self.update_style(style, cx);
    }

    fn update_graph(&mut self, graph: GraphDraft, cx: &mut Context<Self>) {
        let patch = MindmapDocumentStylePatchDto {
            structure: graph.structure,
            connector_kind: graph.connector,
            connector_corner_radius: Some(graph.corner_radius),
            connector_width: Some(graph.width.clone()),
            connector_color: Some(graph.color.clone()),
            connector_pattern: Some(graph.pattern.clone()),
        };
        let path = self.path.display().to_string();
        let previous_graph = self.graph.clone();
        self.graph = graph;
        if !self.run_document_operation(
            |expected| mindmap_ffi::update_mindmap_document_style(path, expected, 0, patch),
            cx,
        ) {
            self.graph = previous_graph;
        }
        self.rebuild_layout();
    }

    fn set_priority(&mut self, level: u8, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let path = self.path.display().to_string();
        self.run_document_operation(
            |expected| mindmap_ffi::set_mindmap_priority(path, expected, 0, topic_id, level),
            cx,
        );
    }

    fn set_progress(&mut self, marker_id: &str, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let marker_id = marker_id.to_string();
        let path = self.path.display().to_string();
        self.run_document_operation(
            |expected| {
                mindmap_ffi::set_mindmap_task_progress(path, expected, 0, topic_id, marker_id)
            },
            cx,
        );
    }

    fn add_marker(&mut self, marker_id: &str, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let marker_id = marker_id.to_string();
        let path = self.path.display().to_string();
        self.run_document_operation(
            |expected| mindmap_ffi::add_mindmap_marker(path, expected, 0, topic_id, marker_id),
            cx,
        );
    }

    fn remove_marker(&mut self, marker_id: &str, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let marker_id = marker_id.to_string();
        let path = self.path.display().to_string();
        self.run_document_operation(
            |expected| mindmap_ffi::remove_mindmap_marker(path, expected, 0, topic_id, marker_id),
            cx,
        );
    }

    fn toggle_todo(&mut self, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let path = self.path.display().to_string();
        self.run_document_operation(
            |expected| mindmap_ffi::toggle_mindmap_todo(path, expected, 0, topic_id),
            cx,
        );
    }

    fn shift_date(&mut self, days: i64, window: &mut Window, cx: &mut Context<Self>) {
        let today = Local::now().date_naive();
        let current = self.date_input.read(cx).value().trim().to_string();
        let base = if days == 0 {
            today
        } else {
            chrono::NaiveDate::parse_from_str(&current, "%Y-%m-%d").unwrap_or(today)
        };
        let next = base
            .checked_add_signed(chrono::Duration::days(days))
            .unwrap_or(today);
        self.date_input.update(cx, |input, cx| {
            input.set_value(next.format("%Y-%m-%d").to_string(), window, cx);
        });
        cx.notify();
    }

    fn insert_date(&mut self, cx: &mut Context<Self>) {
        let Some(topic_id) = self.selected_id.clone() else {
            return;
        };
        let date = self.date_input.read(cx).value().trim().to_string();
        let time = if matches!(self.date.precision, MindmapDatePrecisionDto::DateOnly) {
            None
        } else {
            Some(self.time_input.read(cx).value().trim().to_string())
        };
        let precision = self.date.precision;
        let (system_date_order, system_date_separator) =
            if matches!(self.date.style, MindmapDateStyleDto::System) {
                let (order, separator) = system_date_pattern();
                (Some(order), Some(separator))
            } else {
                (None, None)
            };
        let request = MindmapDateRequestDto {
            date,
            time,
            style: self.date.style,
            precision,
            target: self.date.target,
            placement: self.date.placement,
            kind: self.date.kind,
            system_date_order,
            system_date_separator,
        };
        let previous = self.document.clone();
        match mindmap_ffi::insert_mindmap_date(
            self.path.display().to_string(),
            previous.source_sha256.clone(),
            0,
            topic_id,
            request,
        ) {
            Ok(result) => {
                self.record_success(previous, result.document);
                self.status = if matches!(self.date.kind, MindmapDateKindDto::Due) {
                    format!("已插入截止文本：{}（未映射 XMind deadline）", result.text).into()
                } else {
                    format!("已插入日期：{}", result.text).into()
                };
            }
            Err(error) => self.fail(error.to_string()),
        }
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        let current = MindmapSnapshot::from_document(&self.document);
        match mindmap_ffi::save_mindmap_content(
            self.path.display().to_string(),
            current.source_sha256.clone(),
            snapshot.content_json.clone(),
        ) {
            Ok(_) => {
                self.redo_stack.push(current);
                match mindmap_ffi::open_mindmap_document(self.path.display().to_string()) {
                    Ok(document) => {
                        self.apply_restored_document(document, cx);
                        self.status = "已撤销".into();
                    }
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Err(error) => {
                self.undo_stack.push(snapshot);
                self.fail(error.to_string());
            }
        }
        cx.notify();
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.redo_stack.pop() else {
            return;
        };
        let current = MindmapSnapshot::from_document(&self.document);
        match mindmap_ffi::save_mindmap_content(
            self.path.display().to_string(),
            current.source_sha256.clone(),
            snapshot.content_json.clone(),
        ) {
            Ok(_) => {
                self.undo_stack.push(current);
                match mindmap_ffi::open_mindmap_document(self.path.display().to_string()) {
                    Ok(document) => {
                        self.apply_restored_document(document, cx);
                        self.status = "已重做".into();
                    }
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Err(error) => {
                self.redo_stack.push(snapshot);
                self.fail(error.to_string());
            }
        }
        cx.notify();
    }

    fn apply_restored_document(&mut self, document: MindmapDocumentDto, cx: &mut Context<Self>) {
        let previous_structure = self.graph.structure;
        self.document = document;
        match MindmapTree::from_content_json(&self.document.content_json) {
            Ok(tree) => {
                if self
                    .selected_id
                    .as_ref()
                    .is_some_and(|id| !tree.topics.contains_key(id))
                {
                    self.selected_id = tree.root_id.clone();
                }
                self.tree = tree;
                self.load_graph(cx);
                self.rebuild_layout();
                self.load_inspector_detached();
                self.error = None;
                if self.graph.structure != previous_structure {
                    self.fit_canvas(cx);
                }
            }
            Err(error) => self.fail(error),
        }
    }

    fn save_now(&mut self, cx: &mut Context<Self>) {
        match mindmap_ffi::save_mindmap_content(
            self.path.display().to_string(),
            self.document.source_sha256.clone(),
            self.document.content_json.clone(),
        ) {
            Ok(report) => {
                self.error = None;
                self.status = if report.byte_identical {
                    "保存校验通过：文件字节一致".into()
                } else {
                    format!("保存完成：{} 字节", report.bytes_written).into()
                };
            }
            Err(error) => self.fail(error.to_string()),
        }
        cx.notify();
    }

    fn choose_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("打开 XMind 文件".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, _, cx| {
                match open_mindmap_window(path, cx) {
                    Ok(()) => this.status = "已打开新的 Mindmap 窗口".into(),
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择要转换为 Mindmap 的文档".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, _, cx| {
                match crate::mindmap_import_window::open_mindmap_import_window(path, cx) {
                    Ok(()) => this.status = "已打开转换预览".into(),
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reveal_in_folder(&mut self, cx: &mut Context<Self>) {
        cx.reveal_path(&self.path);
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string());
        h_flex()
            .h(px(54.))
            .flex_shrink_0()
            .w_full()
            .px_3()
            .gap_2()
            .items_center()
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .size(px(28.))
                    .rounded_md()
                    .bg(rgb(0xB85D28))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(0xFFFFFF))
                    .font_weight(FontWeight::BOLD)
                    .child("M"),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(cx.theme().muted_foreground)
                            .child("XMind · Rust Core · 原子保存"),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("mindmap-undo")
                    .ghost()
                    .compact()
                    .icon(IconName::Undo)
                    .tooltip("撤销 Ctrl+Z")
                    .disabled(self.undo_stack.is_empty())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(cx))),
            )
            .child(
                Button::new("mindmap-redo")
                    .ghost()
                    .compact()
                    .icon(IconName::Redo)
                    .tooltip("重做 Ctrl+Y")
                    .disabled(self.redo_stack.is_empty())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.redo(cx))),
            )
            .child(
                Button::new("mindmap-open")
                    .ghost()
                    .compact()
                    .icon(IconName::FolderOpen)
                    .label("打开")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.choose_open_file(window, cx)
                    })),
            )
            .child(
                Button::new("mindmap-import")
                    .ghost()
                    .compact()
                    .icon(IconName::FileText)
                    .label("转换")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.choose_import_file(window, cx)
                    })),
            )
            .child(
                Button::new("mindmap-reveal")
                    .ghost()
                    .compact()
                    .icon(IconName::FolderOpen)
                    .label("定位")
                    .tooltip("在资源管理器中显示")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reveal_in_folder(cx))),
            )
            .child(
                Button::new("mindmap-save")
                    .ghost()
                    .compact()
                    .icon(IconName::Save)
                    .label("保存")
                    .tooltip("保存 Ctrl+S")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_now(cx))),
            )
            .child(div().w(px(1.)).h(px(22.)).bg(cx.theme().border))
            .child(
                Button::new("mindmap-fit")
                    .ghost()
                    .compact()
                    .label("适应")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.fit_canvas(cx))),
            )
            .child(
                Button::new("mindmap-inspector")
                    .ghost()
                    .compact()
                    .icon(IconName::PanelRightOpen)
                    .tooltip("显示或隐藏检查器 Ctrl+Alt+F")
                    .selected(self.show_inspector)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_inspector = !this.show_inspector;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_outline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self
            .tree
            .outline_rows(&self.search_input.read(cx).value(), &self.collapsed);
        let selected = self.selected_id.clone();
        let row_elements = rows
            .into_iter()
            .map(|row| {
                let is_selected = selected.as_deref() == Some(row.id.as_str());
                let row = decorate_outline_row(row, &self.collapsed);
                let id = row.id.clone();
                h_flex()
                    .id(SharedString::from(format!("outline-{}", row.id)))
                    .h(px(28.))
                    .w_full()
                    .pl(px(8. + row.depth.min(12) as f32 * 12.))
                    .pr_2()
                    .gap_1()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_selected, |this| {
                        this.bg(cx.theme().primary.opacity(0.18))
                    })
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.28)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select(Some(id.clone()), cx)
                    }))
                    .child(
                        div()
                            .w(px(14.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if row.child_count == 0 {
                                "·"
                            } else if row.is_collapsed {
                                "▸"
                            } else {
                                "▾"
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .when(row.is_complete, |this| {
                                this.line_through().text_color(cx.theme().muted_foreground)
                            })
                            .child(row.title),
                    )
                    .when(!row.marker_summary.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(cx.theme().primary)
                                .child(row.marker_summary),
                        )
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        v_flex()
            .h_full()
            .min_h_0()
            .w(px(286.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar.opacity(0.92))
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .child("OUTLINE"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{}", self.tree.topics.len())),
                            ),
                    )
                    .child(
                        Input::new(&self.search_input)
                            .aria_label("搜索主题")
                            .cleanable(true),
                    ),
            )
            .child(
                div()
                    .id("mindmap-outline-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_2()
                    .overflow_y_scroll()
                    .track_scroll(&self.outline_scroll)
                    .children(row_elements),
            )
            .into_any_element()
    }

    fn render_inspector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(topic) = self.selected_topic() else {
            return v_flex()
                .w(px(300.))
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("选择一个主题"),
                )
                .into_any_element();
        };
        let topic_id = topic.id.clone();
        let title = topic.title.clone();
        let plain = self.style_draft.clone();
        let body = v_flex()
            .gap_4()
            .child(section_title("主题"))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("edit-selected-topic")
                            .outline()
                            .small()
                            .label("编辑标题 F2")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.begin_inline_edit(topic_id.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new("toggle-todo")
                            .outline()
                            .small()
                            .selected(self.todo.is_complete)
                            .label(if self.todo.is_complete {
                                "已完成 ✓"
                            } else {
                                "标记完成"
                            })
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_todo(cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .min_w_0()
                    .when(self.todo.is_complete, |this| this.line_through())
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(todo_status_label(&self.todo)),
            )
            .child(section_title("图结构与连线"))
            .child(structure_controls(self, cx))
            .child(connector_controls(self, cx))
            .child(section_title("标记"))
            .child(priority_controls(self, cx))
            .child(progress_controls(self, cx))
            .child(generic_marker_controls(self, cx))
            .when(self.markers.opaque_count > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(format!(
                            "保留 {} 个未知/非对象 marker；Kite 不会改写其内容",
                            self.markers.opaque_count
                        )),
                )
            })
            .child(section_title("日期"))
            .child(date_controls(self, cx))
            .child(section_title("字体与节点格式"))
            .child(font_controls(self, cx))
            .child(color_controls(self, cx))
            .child(node_controls(self, cx));
        v_flex()
            .w(px(310.))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar.opacity(0.92))
            .child(
                h_flex()
                    .h(px(44.))
                    .px_3()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .child("INSPECTOR"),
                    )
                    .child(
                        div()
                            .max_w(px(170.))
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                plain
                                    .font_family
                                    .clone()
                                    .unwrap_or_else(|| "系统字体".to_string()),
                            ),
                    ),
            )
            .child(
                div()
                    .id("mindmap-inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.inspector_scroll)
                    .p_3()
                    .child(body),
            )
            .into_any_element()
    }

    fn render_canvas(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let viewport = window.viewport_size();
        self.canvas_size = (
            (viewport.width.as_f32() - 286.0 - if self.show_inspector { 310.0 } else { 0.0 })
                .max(320.0),
            (viewport.height.as_f32() - 112.0).max(260.0),
        );
        let layout = self.layout.clone();
        let origin_x =
            (self.canvas_size.0 - layout.width) / 2.0 - layout.min_x + self.pan.x.as_f32();
        let origin_y =
            (self.canvas_size.1 - layout.height) / 2.0 - layout.min_y + self.pan.y.as_f32();
        let edges = self.render_edges(origin_x, origin_y, cx);
        let node_topics = layout
            .nodes
            .iter()
            .filter_map(|node| {
                self.tree
                    .topics
                    .get(&node.id)
                    .cloned()
                    .map(|topic| (node.clone(), topic))
            })
            .collect::<Vec<_>>();
        let nodes = node_topics
            .into_iter()
            .map(|(node, topic)| self.render_node(&node, origin_x, origin_y, &topic, cx))
            .collect::<Vec<_>>();
        let has_visible = self.layout.nodes.len();
        let empty = has_visible == 0;
        div()
            .id("mindmap-canvas")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(rgb(0x141619))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.panning = true;
                    this.last_pointer = Some(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.panning = false;
                    this.last_pointer = None;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.panning {
                    if let Some(last) = this.last_pointer {
                        this.pan.x += event.position.x - last.x;
                        this.pan.y += event.position.y - last.y;
                    }
                    this.last_pointer = Some(event.position);
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if event.modifiers.secondary() {
                    this.zoom_by(if delta.y.as_f32() > 0.0 { 1.08 } else { 0.92 }, cx);
                } else {
                    this.pan.x -= delta.x;
                    this.pan.y -= delta.y;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .child(
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, _| {
                        for edge in &edges {
                            let mut builder = PathBuilder::stroke(px(edge.width));
                            if let Some(dash_array) =
                                connector_dash_array(&edge.pattern, edge.width)
                            {
                                builder = builder.dash_array(&dash_array);
                            }
                            let start = point(
                                px(bounds.origin.x.as_f32() + edge.from_x),
                                px(bounds.origin.y.as_f32() + edge.from_y),
                            );
                            let end = point(
                                px(bounds.origin.x.as_f32() + edge.to_x),
                                px(bounds.origin.y.as_f32() + edge.to_y),
                            );
                            builder.move_to(start);
                            match edge.connector {
                                MindmapConnectorKindDto::Straight => builder.line_to(end),
                                MindmapConnectorKindDto::Curved => {
                                    let control = point(
                                        px((start.x.as_f32() + end.x.as_f32()) / 2.0),
                                        px(start.y.as_f32()),
                                    );
                                    builder.curve_to(end, control);
                                }
                                MindmapConnectorKindDto::Elbow => {
                                    let mid_x = (start.x.as_f32() + end.x.as_f32()) / 2.0;
                                    builder.line_to(point(
                                        px(bounds.origin.x.as_f32() + mid_x),
                                        start.y,
                                    ));
                                    builder.line_to(point(
                                        px(bounds.origin.x.as_f32() + mid_x),
                                        end.y,
                                    ));
                                    builder.line_to(end);
                                }
                                MindmapConnectorKindDto::RoundedElbow
                                | MindmapConnectorKindDto::Unknown => {
                                    let mid_x = (start.x.as_f32() + end.x.as_f32()) / 2.0;
                                    let r = edge
                                        .corner_radius
                                        .min((end.x.as_f32() - start.x.as_f32()).abs() / 2.0)
                                        .max(0.0);
                                    if r > 0.5 {
                                        builder.line_to(point(
                                            px(bounds.origin.x.as_f32() + mid_x - r),
                                            start.y,
                                        ));
                                        builder.curve_to(
                                            point(
                                                px(bounds.origin.x.as_f32() + mid_x),
                                                px(start.y.as_f32() + r),
                                            ),
                                            point(px(bounds.origin.x.as_f32() + mid_x), start.y),
                                        );
                                        builder.line_to(point(
                                            px(bounds.origin.x.as_f32() + mid_x),
                                            px(end.y.as_f32() - r),
                                        ));
                                        builder.curve_to(
                                            point(px(bounds.origin.x.as_f32() + mid_x + r), end.y),
                                            point(px(bounds.origin.x.as_f32() + mid_x), end.y),
                                        );
                                        builder.line_to(end);
                                    } else {
                                        builder.line_to(point(
                                            px(bounds.origin.x.as_f32() + mid_x),
                                            start.y,
                                        ));
                                        builder.line_to(point(
                                            px(bounds.origin.x.as_f32() + mid_x),
                                            end.y,
                                        ));
                                        builder.line_to(end);
                                    }
                                }
                            }
                            if let Ok(path) = builder.build() {
                                window.paint_path(path, edge.color);
                            }
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(nodes)
            .when(empty, |this| {
                this.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .child("没有可见主题"),
                )
            })
            .child(
                h_flex()
                    .absolute()
                    .right_2()
                    .bottom_2()
                    .gap_1()
                    .p_1()
                    .rounded_md()
                    .bg(cx.theme().title_bar.opacity(0.92))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("zoom-out")
                            .ghost()
                            .compact()
                            .label("−")
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.zoom_by(0.9, cx)),
                            ),
                    )
                    .child(
                        div()
                            .w(px(46.))
                            .text_center()
                            .text_xs()
                            .font_family("monospace")
                            .child(format!("{:.0}%", self.zoom * 100.0)),
                    )
                    .child(
                        Button::new("zoom-in")
                            .ghost()
                            .compact()
                            .label("+")
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.zoom_by(1.1, cx)),
                            ),
                    )
                    .child(
                        Button::new("center-root")
                            .ghost()
                            .compact()
                            .label("中心")
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.select_root(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_node(
        &mut self,
        node: &MindmapNodeLayout,
        origin_x: f32,
        origin_y: f32,
        topic: &MindmapTopic,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = node.id.clone();
        let x = node.center_x + origin_x;
        let y = node.center_y + origin_y;
        let width = node.width;
        let height = node.height;
        let selected = self.selected_id.as_deref() == Some(id.as_str());
        let editing = self.editing_id.as_deref() == Some(id.as_str());
        let style = &topic.style;
        let fill = style_color(
            style.fill_color.as_deref(),
            if selected {
                rgb(0x2C3138).into()
            } else {
                rgb(0x202328).into()
            },
        );
        let border = style_color(
            style.border_color.as_deref(),
            if selected {
                cx.theme().primary
            } else {
                rgb(0x4B535D).into()
            },
        );
        let text = style_color(
            style.text_color.as_deref(),
            if topic.is_complete {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            },
        );
        let font_size = style.font_size.unwrap_or(13.0).clamp(8.0, 48.0) as f32;
        let line_height = measurement_number(style.line_height.as_deref())
            .unwrap_or(1.25)
            .clamp(0.8, 2.5) as f32;
        let letter_spacing = measurement_number(style.letter_spacing.as_deref());
        let display_title = letter_spacing_preview(&topic.title, letter_spacing);
        let alignment = text_align_for(style.alignment.as_deref());
        let render_font =
            resolve_render_font(style.font_family.as_deref(), &self.system_fonts, "Segoe UI");
        let marker = topic.marker_summary();
        let left = x - width / 2.0;
        let top = y - height / 2.0;
        let checkbox_id = id.clone();
        let checkbox = div()
            .id(SharedString::from(format!("mindmap-todo-{id}")))
            .flex_shrink_0()
            .size(px(15.))
            .rounded_sm()
            .border_1()
            .border_color(if topic.is_complete { text } else { border })
            .when(topic.is_complete, |this| this.bg(text))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(rgb(0x111315))
            .cursor_pointer()
            .when(topic.is_complete, |this| this.child("✓"))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.selected_id = Some(checkbox_id.clone());
                this.toggle_todo(cx);
            }));
        let body = if editing {
            Input::new(&self.inline_input)
                .aria_label("主题标题")
                .w_full()
                .text_size(px(font_size))
                .into_any_element()
        } else {
            h_flex()
                .min_w_0()
                .w_full()
                .gap_1p5()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .child(
                            div()
                                .w_full()
                                .max_w_full()
                                .truncate()
                                .text_size(px(font_size))
                                .line_height(relative(line_height))
                                .text_align(alignment)
                                .text_color(text)
                                .font_family(render_font)
                                .when(style.italic, |this| this.italic())
                                .when(style.underline, |this| this.underline())
                                .when(style.strike || topic.is_complete, |this| {
                                    this.line_through()
                                })
                                .when(is_bold_weight(style.font_weight.as_deref()), |this| {
                                    this.font_weight(FontWeight::BOLD)
                                })
                                .when(topic.is_complete, |this| this.opacity(0.62))
                                .child(display_title),
                        )
                        .when(!marker.is_empty(), |this| {
                            this.child(
                                div()
                                    .max_w_full()
                                    .truncate()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(if topic.is_complete {
                                        cx.theme().muted_foreground
                                    } else {
                                        cx.theme().primary
                                    })
                                    .child(marker),
                            )
                        }),
                )
                .child(checkbox)
                .into_any_element()
        };

        let border_width = style
            .border_width
            .unwrap_or(if selected { 2.0 } else { 1.0 })
            .clamp(0.0, 12.0) as f32
            * self.zoom;
        let border_pattern = style.border_pattern.as_deref().unwrap_or("solid");
        let corner_radius = style.corner_radius.unwrap_or(8) as f32 * self.zoom;

        div()
            .id(SharedString::from(format!("mindmap-node-{id}")))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height))
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(corner_radius))
            .bg(if topic.is_complete {
                fill.opacity(0.55)
            } else {
                fill
            })
            .when(border_width <= 0.0 || border_pattern == "none", |this| {
                this.border_0()
            })
            .when(border_width > 0.0 && border_pattern != "none", |this| {
                this.border(px(border_width))
                    .border_color(if selected { cx.theme().primary } else { border })
                    .when(
                        matches!(border_pattern, "dash" | "dot" | "dash-dot"),
                        |this| this.border_dashed(),
                    )
            })
            .shadow_sm()
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    this.begin_inline_edit(id.clone(), window, cx);
                } else {
                    this.select(Some(id.clone()), cx);
                }
            }))
            .child(body)
            .into_any_element()
    }

    fn render_edges(&self, origin_x: f32, origin_y: f32, cx: &App) -> Vec<EdgeRender> {
        self.layout
            .nodes
            .iter()
            .filter_map(|node| {
                let parent_id = node.parent_id.as_ref()?;
                let anchors =
                    self.layout
                        .edge_anchors(parent_id, &node.id, self.graph.structure)?;
                let topic = self.tree.topics.get(&node.id)?;
                let mut edge_color =
                    parse_hex_color(&self.graph.color).unwrap_or_else(|| cx.theme().border);
                if topic.is_complete {
                    edge_color = edge_color.opacity(0.38);
                }
                let base_width = measurement_number(Some(&self.graph.width))
                    .unwrap_or(2.0)
                    .clamp(0.5, 12.0) as f32;
                Some(EdgeRender {
                    from_x: anchors.start_x + origin_x,
                    from_y: anchors.start_y + origin_y,
                    to_x: anchors.end_x + origin_x,
                    to_y: anchors.end_y + origin_y,
                    width: base_width * self.zoom,
                    color: edge_color,
                    connector: self.graph.connector,
                    corner_radius: self.graph.corner_radius as f32 * self.zoom,
                    pattern: self.graph.pattern.clone(),
                })
            })
            .collect()
    }

    fn render_status(&self, cx: &App) -> AnyElement {
        h_flex()
            .h(px(28.))
            .w_full()
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .bg(cx.theme().status_bar)
            .border_t_1()
            .border_color(cx.theme().status_bar_border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .when(self.error.is_some(), |this| {
                        this.text_color(cx.theme().danger)
                    })
                    .child(self.error.clone().unwrap_or_else(|| self.status.clone())),
            )
            .child(format!(
                "{} 主题 · {} 可见 · 撤销 {} / 重做 {}",
                self.tree.topics.len(),
                self.layout.nodes.len(),
                self.undo_stack.len(),
                self.redo_stack.len()
            ))
            .child(self.path.display().to_string())
            .into_any_element()
    }
}

impl Focusable for MindmapWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MindmapWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = (!self.zen_mode).then(|| self.render_header(cx));
        let outline = (!self.zen_mode).then(|| self.render_outline(cx));
        let inspector = (self.show_inspector && !self.zen_mode).then(|| self.render_inspector(cx));
        let status = (!self.zen_mode).then(|| self.render_status(cx));
        let canvas = self.render_canvas(window, cx);
        v_flex()
            .id("kite-mindmap-root")
            .key_context("KiteMindmap")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(rgb(0x101214))
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &MindmapAddSibling, window, cx| {
                this.add_sibling(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &MindmapAddChild, window, cx| this.add_child(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MindmapInsertSiblingBefore, window, cx| {
                    this.add_sibling(true, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &MindmapNewParent, window, cx| this.new_parent(window, cx)),
            )
            .on_action(cx.listener(|this, _: &MindmapEditTopic, window, cx| {
                if let Some(id) = this.selected_id.clone() {
                    this.begin_inline_edit(id, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &MindmapDeleteTopic, window, cx| {
                this.delete_selected(window, cx)
            }))
            .on_action(cx.listener(|this, _: &MindmapMoveTopicUp, window, cx| {
                this.move_selected(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MindmapMoveTopicDown, window, cx| {
                this.move_selected(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MindmapCopyTopic, window, cx| {
                this.copy_selected(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MindmapCutTopic, window, cx| {
                this.copy_selected(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MindmapPasteTopic, window, cx| {
                this.paste_selected(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &MindmapPasteTextAsTopic, window, cx| {
                    this.paste_selected(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &MindmapCopyTopicStyle, _, cx| this.copy_style(cx)))
            .on_action(cx.listener(|this, _: &MindmapPasteTopicStyle, _, cx| this.paste_style(cx)))
            .on_action(cx.listener(|this, _: &MindmapToggleTodo, _, cx| this.toggle_todo(cx)))
            .on_action(cx.listener(|this, _: &MindmapZoomIn, _, cx| this.zoom_by(1.1, cx)))
            .on_action(cx.listener(|this, _: &MindmapZoomOut, _, cx| this.zoom_by(0.9, cx)))
            .on_action(cx.listener(|this, _: &MindmapResetZoom, _, cx| this.reset_zoom(cx)))
            .on_action(cx.listener(|this, _: &MindmapFitCanvas, _, cx| this.fit_canvas(cx)))
            .on_action(cx.listener(|this, _: &MindmapCenterRoot, _, cx| this.select_root(cx)))
            .on_action(
                cx.listener(|this, _: &MindmapToggleBranch, _, cx| this.toggle_branch(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MindmapExpandBranch, _, cx| this.toggle_branch(true, cx)),
            )
            .on_action(cx.listener(|this, _: &MindmapToggleZenMode, _, cx| {
                this.zen_mode = !this.zen_mode;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MindmapToggleInspector, _, cx| {
                this.show_inspector = !this.show_inspector;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MindmapFindTopic, window, cx| {
                this.search_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &MindmapSave, _, cx| this.save_now(cx)))
            .on_action(cx.listener(|this, _: &MindmapUndo, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &MindmapRedo, _, cx| this.redo(cx)))
            .on_action(
                cx.listener(|this, _: &MindmapAddRelation, _, cx| {
                    this.not_implemented("关系线", cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &MindmapAddBoundary, _, cx| this.not_implemented("外框", cx)),
            )
            .on_action(
                cx.listener(|this, _: &MindmapAddSummary, _, cx| this.not_implemented("概要", cx)),
            )
            .on_action(cx.listener(|this, _: &MindmapInsertNote, _, cx| {
                this.not_implemented("备注编辑", cx)
            }))
            .on_action(cx.listener(|this, _: &MindmapInsertLink, _, cx| {
                this.not_implemented("超链接编辑", cx)
            }))
            .when_some(header, |this, header| this.child(header))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when_some(outline, |this, outline| this.child(outline))
                    .child(canvas)
                    .when_some(inspector, |this, inspector| this.child(inspector)),
            )
            .when_some(status, |this, status| this.child(status))
    }
}

impl MindmapWindow {
    fn not_implemented(&mut self, feature: &str, cx: &mut Context<Self>) {
        self.error = None;
        self.status = format!("{feature}尚未在当前 Windows 切片实现").into();
        cx.notify();
    }
}

fn graph_draft_from_style(style: MindmapDocumentStyleDto) -> GraphDraft {
    GraphDraft {
        structure: style.structure,
        connector: style.connector_kind,
        corner_radius: style.connector_corner_radius.unwrap_or(12),
        width: style.connector_width.unwrap_or_else(|| "2pt".to_string()),
        color: style
            .connector_color
            .unwrap_or_else(|| "#8F9BA8".to_string()),
        pattern: style
            .connector_pattern
            .unwrap_or_else(|| "solid".to_string()),
    }
}

fn empty_markers() -> MindmapTopicMarkersDto {
    MindmapTopicMarkersDto {
        priority_marker_id: None,
        task_progress_marker_id: None,
        markers: Vec::new(),
        opaque_count: 0,
    }
}

fn merge_topic_task_fields(todo: &mut MindmapTodoDto, topic: &MindmapTopic) {
    if todo.task_presentation.is_none() {
        todo.task_presentation = topic.task_presentation.clone();
    }
    if todo.todo_status.is_none() {
        todo.todo_status = topic.todo_status.clone();
    }
    if todo.todo_progress.is_none() {
        todo.todo_progress = topic.todo_progress;
    }
    if topic.is_complete {
        todo.status = MindmapTodoStatusDto::Done;
        todo.is_complete = true;
        todo.progress_eighths = Some(8);
        todo.todo_status = Some("done".to_string());
        todo.todo_progress = Some(1.0);
    }
}

fn parse_hex_color(value: &str) -> Option<Hsla> {
    let value = value.trim().trim_start_matches('#');
    match value.len() {
        6 => u32::from_str_radix(value, 16)
            .ok()
            .map(|value| rgb(value).into()),
        8 => u32::from_str_radix(value, 16)
            .ok()
            .map(rgba)
            .map(Into::into),
        _ => None,
    }
}

fn style_color(value: Option<&str>, fallback: Hsla) -> Hsla {
    value.and_then(parse_hex_color).unwrap_or(fallback)
}

fn section_title(label: &'static str) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(0x8F9BA8))
        .child(label)
        .into_any_element()
}

fn structure_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    let structures = [
        (MindmapGraphStructureDto::MindMap, "思维导图"),
        (MindmapGraphStructureDto::LogicRight, "逻辑右"),
        (MindmapGraphStructureDto::LogicLeft, "逻辑左"),
        (MindmapGraphStructureDto::OrgChart, "组织架构"),
        (MindmapGraphStructureDto::Tree, "树形"),
    ];
    h_flex()
        .flex_wrap()
        .gap_1()
        .children(structures.into_iter().map(|(structure, label)| {
            let mut graph = view.graph.clone();
            graph.structure = structure;
            Button::new(SharedString::from(format!("structure-{label}")))
                .small()
                .outline()
                .selected(view.graph.structure == structure)
                .label(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.update_graph(graph.clone(), cx);
                }))
        }))
        .into_any_element()
}

fn connector_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    let connectors = [
        (MindmapConnectorKindDto::Curved, "曲线"),
        (MindmapConnectorKindDto::Straight, "直线"),
        (MindmapConnectorKindDto::Elbow, "直角"),
        (MindmapConnectorKindDto::RoundedElbow, "圆角"),
    ];
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .children(connectors.into_iter().map(|(connector, label)| {
                    let mut graph = view.graph.clone();
                    graph.connector = connector;
                    Button::new(SharedString::from(format!("connector-{label}")))
                        .small()
                        .outline()
                        .selected(view.graph.connector == connector)
                        .label(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.update_graph(graph.clone(), cx);
                        }))
                })),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().text_xs().text_color(rgb(0x8F9BA8)).child("圆角"))
                .child(
                    Button::new("corner-minus")
                        .small()
                        .ghost()
                        .label("−")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let mut graph = this.graph.clone();
                            graph.corner_radius = graph.corner_radius.saturating_sub(4);
                            this.update_graph(graph, cx);
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family("monospace")
                        .child(view.graph.corner_radius.to_string()),
                )
                .child(
                    Button::new("corner-plus")
                        .small()
                        .ghost()
                        .label("+")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let mut graph = this.graph.clone();
                            graph.corner_radius = (graph.corner_radius + 4).min(256);
                            this.update_graph(graph, cx);
                        })),
                ),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .items_center()
                .child(div().text_xs().text_color(rgb(0x8F9BA8)).child("线宽"))
                .child(
                    Button::new("connector-width")
                        .small()
                        .outline()
                        .label(view.graph.width.clone())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let mut graph = this.graph.clone();
                            graph.width = next_graph_width(&graph.width);
                            this.update_graph(graph, cx);
                        })),
                )
                .child(
                    Button::new("connector-color")
                        .small()
                        .outline()
                        .label("线色")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let mut graph = this.graph.clone();
                            graph.color = next_color(
                                Some(&graph.color),
                                &["#8F9BA8", "#7FB4FF", "#B85D28", "#5A8F73", "#B76E79AA"],
                            );
                            this.update_graph(graph, cx);
                        })),
                )
                .child(
                    Button::new("connector-pattern")
                        .small()
                        .outline()
                        .selected(view.graph.pattern != "solid")
                        .label(match view.graph.pattern.as_str() {
                            "dash" => "虚线",
                            "dot" => "点线",
                            "dash-dot" => "点划线",
                            _ => "实线",
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let mut graph = this.graph.clone();
                            graph.pattern = next_graph_pattern(&graph.pattern);
                            this.update_graph(graph, cx);
                        })),
                ),
        )
        .into_any_element()
}

fn priority_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    let current = view.markers.priority_marker_id.clone();
    h_flex()
        .flex_wrap()
        .gap_1()
        .children((0u8..=3).map(|level| {
            let current = current.clone();
            let priority_id = format!("priority-{level}");
            let selected = if level == 0 {
                current.is_none()
            } else {
                current.as_deref() == Some(priority_id.as_str())
            };
            Button::new(SharedString::from(format!("priority-{level}")))
                .small()
                .outline()
                .selected(selected)
                .label(if level == 0 {
                    "P—".to_string()
                } else {
                    format!("P{level}")
                })
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_priority(level, cx)),
                )
        }))
        .into_any_element()
}

fn progress_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    let current = view.markers.task_progress_marker_id.clone();
    let options = [
        ("", "无"),
        ("task-start", "0%"),
        ("task-quarter", "25%"),
        ("task-half", "50%"),
        ("task-3quar", "75%"),
        ("task-done", "100%"),
        ("task-pause", "暂停"),
    ];
    h_flex()
        .flex_wrap()
        .gap_1()
        .children(options.into_iter().map(|(marker, label)| {
            let selected = current.as_deref().unwrap_or_default() == marker;
            let marker = marker.to_string();
            Button::new(SharedString::from(format!("progress-{label}")))
                .small()
                .outline()
                .selected(selected)
                .label(label)
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_progress(&marker, cx)),
                )
        }))
        .into_any_element()
}

fn generic_marker_controls(
    view: &mut MindmapWindow,
    cx: &mut Context<MindmapWindow>,
) -> AnyElement {
    let markers = view.markers.markers.clone();
    let has = |id: &str| markers.iter().any(|marker| marker.marker_id == id);
    let choices = [
        ("flag-red", "⚑"),
        ("star-yellow", "★"),
        ("tag-purple", "#"),
        ("symbol-wrong", "阻滞"),
        ("symbol-attention", "风险"),
        ("symbol-pause", "等待"),
    ];
    h_flex()
        .flex_wrap()
        .gap_1()
        .children(choices.into_iter().map(|(id, label)| {
            let present = has(id);
            let id = id.to_string();
            Button::new(SharedString::from(format!("marker-{label}")))
                .small()
                .outline()
                .selected(present)
                .label(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if present {
                        this.remove_marker(&id, cx);
                    } else {
                        this.add_marker(&id, cx);
                    }
                }))
        }))
        .into_any_element()
}

fn date_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().text_xs().text_color(rgb(0x8F9BA8)).child("格式"))
                .child(
                    Button::new("date-style")
                        .small()
                        .outline()
                        .label(date_style_label(view.date.style))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.date.style = next_date_style(this.date.style);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("date-target")
                        .small()
                        .outline()
                        .label(if matches!(view.date.target, MindmapDateTargetDto::Title) {
                            "标题"
                        } else {
                            "备注"
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.date.target = match this.date.target {
                                MindmapDateTargetDto::Title => MindmapDateTargetDto::Note,
                                MindmapDateTargetDto::Note => MindmapDateTargetDto::Title,
                            };
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("date-kind")
                        .small()
                        .outline()
                        .selected(matches!(view.date.kind, MindmapDateKindDto::Due))
                        .label(match view.date.kind {
                            MindmapDateKindDto::Date => "普通日期",
                            MindmapDateKindDto::Due => "截止文本",
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.date.kind = match this.date.kind {
                                MindmapDateKindDto::Date => MindmapDateKindDto::Due,
                                MindmapDateKindDto::Due => MindmapDateKindDto::Date,
                            };
                            cx.notify();
                        })),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(Input::new(&view.date_input).aria_label("日期 YYYY-MM-DD"))
                .child(Input::new(&view.time_input).aria_label("时间 HH:MM:SS")),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .items_center()
                .child(
                    Button::new("date-minus-day")
                        .small()
                        .ghost()
                        .label("−1天")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.shift_date(-1, window, cx)
                        })),
                )
                .child(
                    Button::new("date-today")
                        .small()
                        .ghost()
                        .label("今天")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.shift_date(0, window, cx)
                        })),
                )
                .child(
                    Button::new("date-plus-day")
                        .small()
                        .ghost()
                        .label("+1天")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.shift_date(1, window, cx)
                        })),
                )
                .child(
                    Button::new("date-precision")
                        .small()
                        .outline()
                        .label(date_precision_label(view.date.precision))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.date.precision = next_date_precision(this.date.precision);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("date-placement")
                        .small()
                        .outline()
                        .label(
                            if matches!(view.date.placement, MindmapDatePlacementDto::Start) {
                                "插入开头"
                            } else {
                                "插入结尾"
                            },
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.date.placement = match this.date.placement {
                                MindmapDatePlacementDto::Start => MindmapDatePlacementDto::End,
                                MindmapDatePlacementDto::End => MindmapDatePlacementDto::Start,
                            };
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("insert-date")
                        .small()
                        .outline()
                        .label("插入")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.insert_date(cx))),
                ),
        )
        .when(matches!(view.date.kind, MindmapDateKindDto::Due), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child("“截止”当前写为可编辑文本前缀，不冒充 XMind 结构化 deadline。"),
            )
        })
        .into_any_element()
}

fn font_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    let font = view
        .style_draft
        .font_family
        .clone()
        .unwrap_or_else(|| "Segoe UI".to_string());
    let size = view.style_draft.font_size.unwrap_or(13.0);
    let system_fonts = view.system_fonts.clone();
    let missing_font = missing_font_family(view.style_draft.font_family.as_deref(), &system_fonts);
    let font_for_picker = system_fonts.clone();
    let line_height = measurement_number(view.style_draft.line_height.as_deref()).unwrap_or(1.25);
    let letter_spacing =
        measurement_number(view.style_draft.letter_spacing.as_deref()).unwrap_or(0.0);
    let alignment = view
        .style_draft
        .alignment
        .clone()
        .unwrap_or_else(|| "center".to_string());
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    Button::new("font-family")
                        .small()
                        .outline()
                        .label(format!("{font}（系统）"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let fonts = font_for_picker.clone();
                            this.mutate_style(
                                |style| {
                                    style.font_family = Some(next_system_font(
                                        style.font_family.as_deref(),
                                        &fonts,
                                    ));
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("font-minus")
                        .small()
                        .ghost()
                        .label("A−")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.font_size = Some(
                                        (style.font_size.unwrap_or(13.0) - 1.0).clamp(8.0, 48.0),
                                    );
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("font-size")
                        .small()
                        .ghost()
                        .label(format!("{size:.0}")),
                )
                .child(
                    Button::new("font-plus")
                        .small()
                        .ghost()
                        .label("A+")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.font_size = Some(
                                        (style.font_size.unwrap_or(13.0) + 1.0).clamp(8.0, 48.0),
                                    );
                                },
                                cx,
                            );
                        })),
                ),
        )
        .when_some(missing_font, |this, missing| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(format!(
                        "字体缺失：{missing}；当前显示回退到系统字体，保存仍保留原字体族"
                    )),
            )
        })
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(
                    Button::new("font-bold")
                        .small()
                        .outline()
                        .selected(view.style_draft.font_weight.as_deref() == Some("bold"))
                        .label("B")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.font_weight =
                                        Some(if style.font_weight.as_deref() == Some("bold") {
                                            "normal".to_string()
                                        } else {
                                            "bold".to_string()
                                        });
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("font-italic")
                        .small()
                        .outline()
                        .selected(view.style_draft.italic.unwrap_or(false))
                        .label("I")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.italic = Some(!style.italic.unwrap_or(false));
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("font-underline")
                        .small()
                        .outline()
                        .selected(view.style_draft.underline.unwrap_or(false))
                        .label("U")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.underline = Some(!style.underline.unwrap_or(false));
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("font-strike")
                        .small()
                        .outline()
                        .selected(view.style_draft.strike.unwrap_or(false))
                        .label("S")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.strike = Some(!style.strike.unwrap_or(false));
                                },
                                cx,
                            );
                        })),
                ),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .items_center()
                .child(
                    Button::new("text-alignment")
                        .small()
                        .outline()
                        .label(format!("对齐：{}", alignment_label(&alignment)))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    style.alignment =
                                        Some(next_alignment(style.alignment.as_deref()))
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("line-height-minus")
                        .small()
                        .ghost()
                        .label("行高−")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    let value = measurement_number(style.line_height.as_deref())
                                        .unwrap_or(1.25);
                                    style.line_height =
                                        Some((value - 0.1).clamp(0.8, 2.5).to_string());
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family("monospace")
                        .child(format!("{line_height:.2}")),
                )
                .child(
                    Button::new("line-height-plus")
                        .small()
                        .ghost()
                        .label("行高+")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    let value = measurement_number(style.line_height.as_deref())
                                        .unwrap_or(1.25);
                                    style.line_height =
                                        Some((value + 0.1).clamp(0.8, 2.5).to_string());
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("letter-spacing-minus")
                        .small()
                        .ghost()
                        .label("字距−")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    let value = measurement_number(style.letter_spacing.as_deref())
                                        .unwrap_or(0.0);
                                    style.letter_spacing =
                                        Some(format!("{}pt", (value - 0.05).clamp(0.0, 2.0)));
                                },
                                cx,
                            );
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family("monospace")
                        .child(format!("{letter_spacing:.2}pt")),
                )
                .child(
                    Button::new("letter-spacing-plus")
                        .small()
                        .ghost()
                        .label("字距+")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mutate_style(
                                |style| {
                                    let value = measurement_number(style.letter_spacing.as_deref())
                                        .unwrap_or(0.0);
                                    style.letter_spacing =
                                        Some(format!("{}pt", (value + 0.05).clamp(0.0, 2.0)));
                                },
                                cx,
                            );
                        })),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("行高/字距为 Kite UI 实验能力；XMind 27.1 静态资源未确认视觉支持。"),
        )
        .into_any_element()
}

fn color_controls(_view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    h_flex()
        .flex_wrap()
        .gap_1()
        .child(
            Button::new("text-color")
                .small()
                .outline()
                .label("文字色")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.text_color = Some(next_color(
                                style.text_color.as_deref(),
                                &["#E7ECF2", "#F8C471", "#7FB4FF", "#EF8A8A"],
                            ))
                        },
                        cx,
                    );
                })),
        )
        .child(
            Button::new("fill-color")
                .small()
                .outline()
                .label("填充")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.node_fill = Some(next_color(
                                style.node_fill.as_deref(),
                                &["#202328", "#2C3138", "#2D3B32", "#3B2D32"],
                            ))
                        },
                        cx,
                    );
                })),
        )
        .child(
            Button::new("border-color")
                .small()
                .outline()
                .label("边框色")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.border_color = Some(next_color(
                                style.border_color.as_deref(),
                                &["#4B535D", "#B85D28", "#5A8F73", "#7B6BB3"],
                            ))
                        },
                        cx,
                    );
                })),
        )
        .into_any_element()
}

fn node_controls(view: &mut MindmapWindow, cx: &mut Context<MindmapWindow>) -> AnyElement {
    let width = view.style_draft.border_width.unwrap_or(1.0);
    let radius = view.style_draft.corner_radius.unwrap_or(4);
    h_flex()
        .flex_wrap()
        .gap_1()
        .items_center()
        .child(div().text_xs().text_color(rgb(0x8F9BA8)).child("边框"))
        .child(
            Button::new("border-minus")
                .small()
                .ghost()
                .label("−")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.border_width =
                                Some((style.border_width.unwrap_or(1.0) - 1.0).max(0.0))
                        },
                        cx,
                    );
                })),
        )
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .child(format!("{width:.0}")),
        )
        .child(
            Button::new("border-plus")
                .small()
                .ghost()
                .label("+")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.border_width =
                                Some((style.border_width.unwrap_or(1.0) + 1.0).min(12.0))
                        },
                        cx,
                    );
                })),
        )
        .child(
            Button::new("border-pattern")
                .small()
                .outline()
                .label(border_pattern_label(
                    view.style_draft.border_pattern.as_deref(),
                ))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.border_pattern =
                                Some(next_border_pattern(style.border_pattern.as_deref()))
                        },
                        cx,
                    );
                })),
        )
        .child(div().text_xs().text_color(rgb(0x8F9BA8)).child("圆角"))
        .child(
            Button::new("node-corner-minus")
                .small()
                .ghost()
                .label("−")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.corner_radius =
                                Some(style.corner_radius.unwrap_or(4).saturating_sub(2))
                        },
                        cx,
                    );
                })),
        )
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .child(radius.to_string()),
        )
        .child(
            Button::new("node-corner-plus")
                .small()
                .ghost()
                .label("+")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.mutate_style(
                        |style| {
                            style.corner_radius =
                                Some((style.corner_radius.unwrap_or(4) + 2).min(64))
                        },
                        cx,
                    );
                })),
        )
        .into_any_element()
}

fn next_system_font(current: Option<&str>, fonts: &[String]) -> String {
    if fonts.is_empty() {
        return "Segoe UI".to_string();
    }
    let current = current
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .unwrap_or_default();
    let index = fonts
        .iter()
        .position(|font| font.eq_ignore_ascii_case(current))
        .map_or(0, |index| (index + 1) % fonts.len());
    fonts[index].clone()
}

fn next_color(current: Option<&str>, palette: &[&str]) -> String {
    let index = current
        .and_then(|value| {
            palette
                .iter()
                .position(|color| color.eq_ignore_ascii_case(value))
        })
        .unwrap_or(0);
    palette[(index + 1) % palette.len()].to_string()
}

fn next_date_style(style: MindmapDateStyleDto) -> MindmapDateStyleDto {
    match style {
        MindmapDateStyleDto::Iso => MindmapDateStyleDto::Chinese,
        MindmapDateStyleDto::Chinese => MindmapDateStyleDto::EnglishUs,
        MindmapDateStyleDto::EnglishUs => MindmapDateStyleDto::System,
        MindmapDateStyleDto::System => MindmapDateStyleDto::Iso,
    }
}

fn date_style_label(style: MindmapDateStyleDto) -> &'static str {
    match style {
        MindmapDateStyleDto::Iso => "ISO",
        MindmapDateStyleDto::Chinese => "中文",
        MindmapDateStyleDto::EnglishUs => "English US",
        MindmapDateStyleDto::System => "系统",
    }
}

fn next_date_precision(value: MindmapDatePrecisionDto) -> MindmapDatePrecisionDto {
    match value {
        MindmapDatePrecisionDto::DateOnly => MindmapDatePrecisionDto::Minutes,
        MindmapDatePrecisionDto::Minutes => MindmapDatePrecisionDto::Seconds,
        MindmapDatePrecisionDto::Seconds => MindmapDatePrecisionDto::DateOnly,
    }
}

fn date_precision_label(value: MindmapDatePrecisionDto) -> &'static str {
    match value {
        MindmapDatePrecisionDto::DateOnly => "仅日期",
        MindmapDatePrecisionDto::Minutes => "到分钟",
        MindmapDatePrecisionDto::Seconds => "到秒",
    }
}

fn todo_status_label(todo: &MindmapTodoDto) -> String {
    let legacy = match todo.status {
        MindmapTodoStatusDto::NotStarted => "待办 · 未开始".to_string(),
        MindmapTodoStatusDto::InProgress => format!(
            "待办 · 进行中 {}%",
            todo.progress_eighths.unwrap_or(0) * 100 / 8
        ),
        MindmapTodoStatusDto::Paused => "待办 · 已暂停（marker）".to_string(),
        MindmapTodoStatusDto::Done => "待办 · 已完成".to_string(),
    };
    let mut parts = vec![legacy];
    if let Some(status) = todo.todo_status.as_deref() {
        let progress = todo
            .todo_progress
            .map(|value| format!(" {:.0}%", value.clamp(0.0, 1.0) * 100.0))
            .unwrap_or_default();
        parts.push(format!("27.1 todoStatus={status}{progress}"));
    }
    if let Some(presentation) = todo.task_presentation.as_deref() {
        parts.push(format!("taskPresentation={presentation}"));
    }
    parts.join(" · ")
}

fn text_align_for(alignment: Option<&str>) -> TextAlign {
    match alignment.unwrap_or("center") {
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        _ => TextAlign::Center,
    }
}

fn alignment_label(alignment: &str) -> &'static str {
    match alignment {
        "inherited" => "继承",
        "left" => "左",
        "right" => "右",
        "justify" => "两端",
        _ => "居中",
    }
}

fn next_alignment(current: Option<&str>) -> String {
    match current.unwrap_or("center") {
        "inherited" => "left",
        "left" => "center",
        "center" => "right",
        "right" => "justify",
        _ => "left",
    }
    .to_string()
}

fn is_bold_weight(weight: Option<&str>) -> bool {
    match weight.unwrap_or_default() {
        "bold" | "bolder" | "semibold" => true,
        value => value.parse::<u16>().is_ok_and(|weight| weight >= 600),
    }
}

fn next_graph_width(current: &str) -> String {
    const WIDTHS: &[&str] = &["1pt", "2pt", "3pt", "4pt", "6pt"];
    let index = WIDTHS
        .iter()
        .position(|width| width.eq_ignore_ascii_case(current))
        .unwrap_or(1);
    WIDTHS[(index + 1) % WIDTHS.len()].to_string()
}

fn next_graph_pattern(current: &str) -> String {
    match current {
        "solid" => "dash",
        "dash" => "dot",
        "dot" => "dash-dot",
        _ => "solid",
    }
    .to_string()
}

fn next_border_pattern(current: Option<&str>) -> String {
    match current.unwrap_or("solid") {
        "solid" => "dash",
        "dash" => "dot",
        "dot" => "none",
        _ => "solid",
    }
    .to_string()
}

fn border_pattern_label(pattern: Option<&str>) -> &'static str {
    match pattern.unwrap_or("solid") {
        "dash" => "虚线",
        "dot" => "点线",
        "dash-dot" => "点划线",
        "none" => "无边框",
        _ => "实线",
    }
}

fn connector_dash_array(pattern: &str, width: f32) -> Option<Vec<Pixels>> {
    let unit = width.max(1.0);
    match pattern {
        "dash" => Some(vec![px(unit * 3.0), px(unit * 2.0)]),
        "dot" => Some(vec![px((unit * 0.5).max(1.0)), px(unit * 2.0)]),
        "dash-dot" => Some(vec![
            px(unit * 4.0),
            px(unit * 2.0),
            px((unit * 0.5).max(1.0)),
            px(unit * 2.0),
        ]),
        _ => None,
    }
}

fn decorate_outline_row(
    mut row: MindmapOutlineRow,
    collapsed: &BTreeSet<String>,
) -> MindmapOutlineRow {
    row.is_collapsed = collapsed.contains(&row.id);
    row
}
