#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod actions;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod archive_progress_window;
#[cfg(windows)]
mod context_menu;
#[cfg(windows)]
mod explorer_integration;
#[cfg(windows)]
mod file_clipboard;
#[cfg(windows)]
mod global_search_window;
#[cfg(windows)]
mod preview_window;
#[cfg(windows)]
mod shell_fs;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod table;
#[cfg(windows)]
mod tabs;

#[cfg(windows)]
use app::KiteApp;
#[cfg(windows)]
use gpui_kit::component::{Theme, ThemeMode};
#[cfg(windows)]
use gpui_kit::*;
#[cfg(windows)]
pub use kite_core::{
    archive, content_search, devices, favorites, fs, platform, session, text_editor,
    ui_preferences, updater, vcs,
};
#[cfg(windows)]
use single_instance::{InstanceStart, SingleInstance};
#[cfg(windows)]
use tabs::KiteWindow;

#[cfg(windows)]
fn main() {
    kite_core::perf_trace::start();
    file_clipboard::trace_drag("Kite process started");
    let launch_path = KiteApp::launch_path_override();
    let single_instance = match SingleInstance::start() {
        Ok(InstanceStart::Primary(instance)) => Some(instance),
        Ok(InstanceStart::Secondary) => {
            if let Err(error) = SingleInstance::forward(launch_path.as_deref()) {
                eprintln!("{error}");
            } else {
                return;
            }
            None
        }
        Err(error) => {
            eprintln!("{error}");
            None
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let preferences = kite_core::ui_preferences::load_ui_preferences();
            let theme_mode = match preferences.theme_mode {
                kite_core::ui_preferences::ThemePreference::Dark => ThemeMode::Dark,
                kite_core::ui_preferences::ThemePreference::Light => ThemeMode::Light,
            };
            Theme::change(theme_mode, None, cx);
            KiteApp::bind_keys(cx);
            KiteApp::install_menus(cx);

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Kite".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::centered(size(px(1240.), px(800.)), cx)),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| KiteWindow::new(single_instance, window, cx))
            })
            .expect("failed to open Kite window");
            cx.activate(true);
        });
}

#[cfg(not(windows))]
fn main() {}
