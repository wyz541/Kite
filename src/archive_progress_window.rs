use std::path::{Path, PathBuf};
use std::time::Instant;

use futures_channel::mpsc::{self, UnboundedSender};
use gpui_kit::component::button::Button;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::archive::{ArchiveFormat, ArchivePhase, ArchiveProgress};
use crate::fs::format_size;

enum Update {
    Destination(PathBuf),
    Bytes {
        phase: ArchivePhase,
        processed: u64,
        total: Option<u64>,
    },
    Finishing,
    Finished(Result<PathBuf, String>),
}

pub(crate) struct ProgressSender(UnboundedSender<Update>);

impl ProgressSender {
    pub(crate) fn report(&self, progress: ArchiveProgress<'_>) {
        let update = match progress {
            ArchiveProgress::Destination(path) => Update::Destination(path.to_path_buf()),
            ArchiveProgress::Bytes {
                phase,
                processed,
                total,
            } => Update::Bytes {
                phase,
                processed,
                total,
            },
            ArchiveProgress::Finishing => Update::Finishing,
        };
        let _ = self.0.unbounded_send(update);
    }

    pub(crate) fn finish(self, result: &Result<PathBuf, String>) {
        let _ = self.0.unbounded_send(Update::Finished(result.clone()));
    }
}

pub(crate) fn open(
    operation: &'static str,
    format: Option<ArchiveFormat>,
    source: &Path,
    destination: Option<&Path>,
    cx: &mut App,
) -> ProgressSender {
    let (sender, mut receiver) = mpsc::unbounded();
    let view = ArchiveProgressWindow {
        operation,
        format: match format {
            Some(ArchiveFormat::Zip) => "ZIP",
            Some(ArchiveFormat::TarGz) => "TAR.GZ",
            None => "归档",
        },
        source: source.to_path_buf(),
        destination: destination.map(Path::to_path_buf),
        status: "正在准备…".into(),
        bytes: None,
        result: None,
        started: Instant::now(),
        ended: None,
    };
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(format!("Kite {operation}进度").into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(560.), px(320.)), cx)),
        window_min_size: Some(size(px(500.), px(310.))),
        ..Default::default()
    };

    if let Err(error) = gpui_kit::open_window(options, cx, move |_, cx| {
        cx.new(|cx| {
            cx.spawn(async move |this, cx| {
                while let Ok(update) = receiver.recv().await {
                    if this
                        .update(cx, |view: &mut ArchiveProgressWindow, cx| {
                            view.apply(update);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            view
        })
    }) {
        eprintln!("无法打开{operation}进度窗口：{error}");
    }
    ProgressSender(sender)
}

struct ArchiveProgressWindow {
    operation: &'static str,
    format: &'static str,
    source: PathBuf,
    destination: Option<PathBuf>,
    status: SharedString,
    bytes: Option<(u64, Option<u64>)>,
    result: Option<Result<(), String>>,
    started: Instant,
    ended: Option<Instant>,
}

impl ArchiveProgressWindow {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Destination(path) => self.destination = Some(path),
            Update::Bytes {
                phase,
                processed,
                total,
            } => {
                self.bytes = Some((processed, total));
                self.status = match phase {
                    ArchivePhase::Checking => "正在检查压缩包…".into(),
                    ArchivePhase::Working => format!("正在{}…", self.operation).into(),
                };
            }
            Update::Finishing => self.status = "正在保存压缩包…".into(),
            Update::Finished(result) => {
                self.ended = Some(Instant::now());
                self.status = format!(
                    "{}{}",
                    self.operation,
                    if result.is_ok() { "完成" } else { "失败" }
                )
                .into();
                self.result = Some(result.map(|path| self.destination = Some(path)));
            }
        }
    }

    fn percentage(&self) -> Option<u8> {
        if matches!(self.result, Some(Ok(()))) {
            return Some(100);
        }
        self.bytes.and_then(|(processed, total)| {
            total
                .filter(|&total| total > 0)
                .map(|total| ((u128::from(processed) * 100 / u128::from(total)).min(99)) as u8)
        })
    }
}

impl Render for ArchiveProgressWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let percent = self.percentage();
        let failed = matches!(self.result, Some(Err(_)));
        let running = self.result.is_none();
        let elapsed = self
            .ended
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .as_secs();
        let amount = self.bytes.map(|(processed, total)| match total {
            Some(total) => format!("{} / {}", format_size(processed), format_size(total)),
            None => format!("已处理 {}", format_size(processed)),
        });

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_4()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_lg().font_semibold().child(self.status.clone()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(self.format),
                            ),
                    )
                    .child(
                        Progress::new("archive-progress")
                            .loading(running && percent.is_none())
                            .value(percent.unwrap_or(0) as f32)
                            .when(failed, |bar| bar.color(theme.danger))
                            .accessibility_label(SharedString::from(format!(
                                "{}进度",
                                self.operation
                            ))),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(div().child(percent.map_or_else(
                                || amount.clone().unwrap_or_default(),
                                |percent| {
                                    amount.as_ref().map_or_else(
                                        || format!("{percent}%"),
                                        |amount| format!("{percent}% · {amount}"),
                                    )
                                },
                            )))
                            .child(div().child(format!(
                                "用时 {:02}:{:02}",
                                elapsed / 60,
                                elapsed % 60
                            ))),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .pt_3()
                    .child(path_row(
                        "archive-source",
                        "源项目",
                        Some(&self.source),
                        theme.muted_foreground,
                    ))
                    .child(path_row(
                        "archive-target",
                        "目标",
                        self.destination.as_deref(),
                        theme.muted_foreground,
                    )),
            )
            .when_some(
                self.result
                    .as_ref()
                    .and_then(|result| result.as_ref().err())
                    .cloned(),
                |this, error| {
                    this.child(
                        div()
                            .id("archive-error")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(error),
                    )
                },
            )
            .child(
                h_flex().mt_auto().justify_end().child(
                    Button::new("archive-progress-close")
                        .outline()
                        .label(if running { "隐藏" } else { "关闭" })
                        .on_click(|_, window, _| window.remove_window()),
                ),
            )
    }
}

fn path_row(
    id: &'static str,
    label: &'static str,
    path: Option<&Path>,
    muted: Hsla,
) -> impl IntoElement {
    let path = SharedString::from(
        path.map_or_else(|| "正在确定…".to_owned(), |path| path.display().to_string()),
    );
    h_flex()
        .items_center()
        .gap_3()
        .text_sm()
        .child(
            div()
                .w(px(56.))
                .flex_shrink_0()
                .text_color(muted)
                .child(label),
        )
        .child(
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .truncate()
                .child(path.clone())
                .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::{ArchiveFormat, ArchivePhase, ArchiveProgress, ArchiveProgressWindow, open};
    use gpui_kit::base::Root;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    #[ignore = "opens a progress window on the Windows desktop"]
    fn desktop_window_receives_running_and_completed_states() {
        let (observed, result) = std::sync::mpsc::channel();
        gpui_kit::application()
            .with_assets(gpui_kit::assets::AllAssets)
            .run(move |cx| {
                gpui_kit::init(cx);
                let source = Path::new(r"C:\Kite Smoke\example.zip");
                let output = PathBuf::from(r"C:\Kite Smoke\example");
                let progress = open(
                    "解压",
                    Some(ArchiveFormat::Zip),
                    source,
                    source.parent(),
                    cx,
                );
                let view = cx.windows().first().and_then(|window| {
                    window
                        .update(cx, |_, window, cx| {
                            Root::read(window, cx)
                                .view()
                                .clone()
                                .downcast::<ArchiveProgressWindow>()
                                .ok()
                        })
                        .ok()
                        .flatten()
                });
                let Some(view) = view else {
                    cx.quit();
                    return;
                };
                progress.report(ArchiveProgress::Destination(&output));
                progress.report(ArchiveProgress::Bytes {
                    phase: ArchivePhase::Working,
                    processed: 40,
                    total: Some(100),
                });
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(280))
                        .await;
                    let running = cx.update(|cx| view.read(cx).percentage());
                    progress.finish(&Ok(output.clone()));
                    cx.background_executor()
                        .timer(Duration::from_millis(280))
                        .await;
                    cx.update(|cx| {
                        let view = view.read(cx);
                        let _ =
                            observed.send((running, view.percentage(), view.destination.clone()));
                        cx.quit();
                    });
                })
                .detach();
            });

        let (running, completed, destination) = result.try_recv().expect("observe progress window");
        assert_eq!(running, Some(40));
        assert_eq!(completed, Some(100));
        assert_eq!(
            destination.as_deref(),
            Some(Path::new(r"C:\Kite Smoke\example"))
        );
    }
}
