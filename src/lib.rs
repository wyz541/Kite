uniffi::setup_scaffolding!();

pub mod agent;
#[cfg(windows)]
pub mod agent_permission_ui;
pub mod agent_review;
pub mod archive;
pub mod content_search;
pub mod control;
pub mod core_api;
pub mod devices;
pub mod diff;
pub mod favorites;
pub mod ffi;
#[cfg(windows)]
pub mod file_watcher;
pub mod fs;
pub mod git;
pub mod git_branches;
pub mod git_changes;
pub mod git_ops;
pub mod git_rebase;
pub mod global_name_search;
pub mod indexed_search;
pub mod mindmap;
pub mod mindmap_ffi;
pub mod mindmap_import;
pub mod name_index;
pub mod perf_trace;
pub mod platform;
pub mod plugins;
pub mod preview;
#[cfg(windows)]
pub mod recycle_bin;
pub mod search_filters;
pub mod session;
pub mod text_editor;
pub mod ui_preferences;
pub mod update_notes;
pub mod updater;
#[cfg(windows)]
pub mod usn_journal;
pub mod vcs;
