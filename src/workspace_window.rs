use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::fs::{EntryKind, FileEntry, load_directory};
use crate::preview_window::PreviewWindow;
use crate::shell_fs;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

#[derive(Clone, Debug)]
struct TreeRow {
    entry: FileEntry,
    depth: usize,
    ancestors: Vec<PathBuf>,
}

pub(crate) struct WorkspaceWindow {
    root: PathBuf,
    rows: Vec<TreeRow>,
    collapsed: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    preview: Option<Entity<PreviewWindow>>,
    loading: bool,
    message: Option<SharedString>,
    _load_task: Task<()>,
}

impl WorkspaceWindow {
    pub(crate) fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_selection(root, None, window, cx)
    }

    pub(crate) fn new_with_selection(
        root: PathBuf,
        selected: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            root: root.clone(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected,
            preview: None,
            loading: true,
            message: None,
            _load_task: Task::ready(()),
        };
        this.load_tree(root, window, cx);
        this
    }

    fn load_tree(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let title = format!("Kite 工作区 — {}", root.display());
        self._load_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { build_tree(&root) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(rows) => {
                        this.rows = rows;
                        this.message = None;
                        if let Some(path) = this.selected.clone() {
                            if let Some(row) = this.rows.iter().find(|row| row.entry.path == path) {
                                if row.entry.kind != EntryKind::Directory {
                                    this.preview = Some(cx.new(|cx| {
                                        PreviewWindow::new(row.entry.clone(), window, cx)
                                    }));
                                }
                            } else {
                                this.selected = None;
                            }
                        }
                    }
                    Err(error) => this.message = Some(error.into()),
                }
                cx.notify();
            });
        });
        window.set_window_title(&title);
    }

    fn open_row(&mut self, row: TreeRow, window: &mut Window, cx: &mut Context<Self>) {
        if row.entry.kind == EntryKind::Directory {
            if !self.collapsed.remove(&row.entry.path) {
                self.collapsed.insert(row.entry.path);
            }
            cx.notify();
            return;
        }
        self.selected = Some(row.entry.path.clone());
        let entry = row.entry;
        self.preview = Some(cx.new(|cx| PreviewWindow::new(entry, window, cx)));
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.load_tree(root, window, cx);
    }

    fn row_is_visible(&self, row: &TreeRow) -> bool {
        row.ancestors
            .iter()
            .all(|ancestor| !self.collapsed.contains(ancestor))
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.row_is_visible(row))
            .map(|(index, row)| {
                let selected = self
                    .selected
                    .as_ref()
                    .is_some_and(|path| path == &row.entry.path);
                let entry = row.clone();
                let click_entry = entry.clone();
                let icon = if entry.entry.kind == EntryKind::Directory {
                    IconName::Folder
                } else {
                    IconName::FileText
                };
                h_flex()
                    .id(("workspace-tree-row", index))
                    .w_full()
                    .h(px(28.))
                    .pl(px(10. + row.depth as f32 * 18.))
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(cx.theme().accent.opacity(0.28)))
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.28)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_row(click_entry.clone(), window, cx);
                    }))
                    .child(if entry.entry.kind == EntryKind::Directory {
                        div()
                            .w_4()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.collapsed.contains(&entry.entry.path) {
                                "▸"
                            } else {
                                "▾"
                            })
                    } else {
                        div().w_4()
                    })
                    .child(
                        Icon::new(icon)
                            .size_4()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(row.entry.name.clone()),
                    )
                    .into_any_element()
            });
        v_flex()
            .id("workspace-tree-scroll")
            .h_full()
            .w(px(300.))
            .flex_shrink_0()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }
}

impl Render for WorkspaceWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());
        let content = if let Some(preview) = self.preview.clone() {
            preview.into_any_element()
        } else if self.loading {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child("正在读取工作区…")
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child("从左侧选择文件")
                .into_any_element()
        };
        v_flex()
            .id("kite-workspace-root")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h(px(46.))
                    .w_full()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(IconName::FolderTree).size_5())
                    .child(div().font_medium().child(title))
                    .child(div().flex_1())
                    .child(
                        Button::new("workspace-refresh")
                            .ghost()
                            .compact()
                            .icon(IconName::RotateCw)
                            .tooltip("刷新目录树")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.refresh(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("内置 Markdown 插件 · 可扩展预览协议"),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_tree(cx))
                    .child(div().w(px(1.)).h_full().bg(cx.theme().border))
                    .child(v_flex().flex_1().min_w_0().h_full().child(content)),
            )
            .when_some(self.message.clone(), |this, message| {
                this.child(
                    div()
                        .p_2()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
    }
}

fn build_tree(root: &Path) -> Result<Vec<TreeRow>, String> {
    let mut rows = Vec::new();
    let mut visited = HashSet::new();
    append_tree(root, 0, &mut rows, 0, &mut Vec::new(), &mut visited)?;
    Ok(rows)
}

fn append_tree(
    path: &Path,
    depth: usize,
    rows: &mut Vec<TreeRow>,
    directories: usize,
    ancestors: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    if directories > 4 {
        return Ok(());
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    let snapshot = load_directory(
        path,
        crate::fs::SortSpec {
            key: crate::fs::SortKey::Name,
            ascending: true,
        },
    )?;
    for entry in snapshot.entries {
        let is_dir = entry.kind == EntryKind::Directory;
        rows.push(TreeRow {
            entry: entry.clone(),
            depth,
            ancestors: ancestors.clone(),
        });
        if is_dir && !shell_fs::is_shell_namespace_path(&entry.path) {
            ancestors.push(entry.path.clone());
            append_tree(
                &entry.path,
                depth + 1,
                rows,
                directories + 1,
                ancestors,
                visited,
            )?;
            ancestors.pop();
        }
    }
    Ok(())
}
