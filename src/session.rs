use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SESSION_SCHEMA_VERSION: u32 = 1;
const MAX_SESSION_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SESSION_TABS: usize = 64;
const MAX_GIT_CLIENT_WINDOWS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct SessionTabDto {
    pub current_directory: String,
    pub view_mode: String,
    pub show_path_bar: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct SessionStateDto {
    pub schema_version: u32,
    pub active_tab: u32,
    pub tabs: Vec<SessionTabDto>,
    #[serde(default)]
    pub git_client_windows: Vec<GitClientWindowSessionDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct GitClientWindowSessionDto {
    pub repository_root: String,
    pub width: u32,
    pub height: u32,
    /// Percentage of the content width, expressed in basis points.
    pub changes_ratio_bp: u16,
    /// Percentage of the content width, expressed in basis points.
    pub details_ratio_bp: u16,
    /// Percentage of the content height, expressed in basis points.
    pub console_ratio_bp: u16,
    pub console_expanded: bool,
    pub last_view: String,
}

pub fn load_session() -> Option<SessionStateDto> {
    let path = session_path()?;
    load_session_at(&path)
}

pub fn save_session(state: &SessionStateDto) -> Result<(), String> {
    let path = session_path().ok_or_else(|| "cannot resolve the session directory".to_string())?;
    save_session_at(&path, state)
}

pub fn load_git_client_window_state(repository_root: &Path) -> Option<GitClientWindowSessionDto> {
    let state = load_session()?;
    state
        .git_client_windows
        .into_iter()
        .find(|window| same_repository(&window.repository_root, repository_root))
}

pub fn save_git_client_window_state(state: &GitClientWindowSessionDto) -> Result<(), String> {
    let state = normalize_git_client_window(state)
        .ok_or_else(|| "Git client window state is invalid".to_string())?;
    let mut session = load_session().unwrap_or_else(empty_session_state);
    if let Some(existing) = session
        .git_client_windows
        .iter_mut()
        .find(|window| same_repository(&window.repository_root, Path::new(&state.repository_root)))
    {
        *existing = state;
    } else {
        session.git_client_windows.push(state);
    }
    session.git_client_windows.truncate(MAX_GIT_CLIENT_WINDOWS);
    save_session(&session)
}

#[cfg(feature = "performance")]
pub fn load_session_for_performance(path: &Path) -> Option<SessionStateDto> {
    load_session_at(path)
}

#[cfg(feature = "performance")]
pub fn save_session_for_performance(path: &Path, state: &SessionStateDto) -> Result<(), String> {
    save_session_at(path, state)
}

fn session_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KITE_SESSION_PATH") {
        return Some(PathBuf::from(path));
    }
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .map(|path| path.join("Kite").join("session.json"))
}

fn normalize_state(state: &SessionStateDto) -> Option<SessionStateDto> {
    if state.schema_version != SESSION_SCHEMA_VERSION {
        return None;
    }

    let mut tabs = Vec::new();
    for tab in state.tabs.iter().take(MAX_SESSION_TABS) {
        let current_directory = tab.current_directory.trim().to_string();
        if current_directory.is_empty() {
            continue;
        }
        let view_mode = if tab.view_mode == "list" {
            "list"
        } else {
            "columns"
        };
        tabs.push(SessionTabDto {
            current_directory,
            view_mode: view_mode.to_string(),
            show_path_bar: tab.show_path_bar,
        });
    }

    let git_client_windows = state
        .git_client_windows
        .iter()
        .take(MAX_GIT_CLIENT_WINDOWS)
        .filter_map(normalize_git_client_window)
        .fold(Vec::new(), |mut windows, window| {
            if !windows.iter().any(|existing: &GitClientWindowSessionDto| {
                same_repository(
                    &existing.repository_root,
                    Path::new(&window.repository_root),
                )
            }) {
                windows.push(window);
            }
            windows
        });

    if tabs.is_empty() && git_client_windows.is_empty() {
        return None;
    }

    let active_tab = if tabs.is_empty() {
        0
    } else {
        usize::try_from(state.active_tab)
            .unwrap_or(usize::MAX)
            .min(tabs.len() - 1) as u32
    };
    Some(SessionStateDto {
        schema_version: SESSION_SCHEMA_VERSION,
        active_tab,
        tabs,
        git_client_windows,
    })
}

fn normalize_git_client_window(
    state: &GitClientWindowSessionDto,
) -> Option<GitClientWindowSessionDto> {
    let repository_root = state.repository_root.trim();
    if repository_root.is_empty() {
        return None;
    }
    let repository_root = PathBuf::from(repository_root);
    let repository_root = dunce::canonicalize(&repository_root)
        .unwrap_or(repository_root)
        .to_string_lossy()
        .into_owned();

    let mut changes_ratio_bp = state.changes_ratio_bp.clamp(1_400, 4_200);
    let mut details_ratio_bp = state.details_ratio_bp.clamp(1_600, 4_600);
    if changes_ratio_bp + details_ratio_bp > 7_400 {
        let overflow = changes_ratio_bp + details_ratio_bp - 7_400;
        let details_trim = overflow.min(details_ratio_bp.saturating_sub(1_600));
        details_ratio_bp -= details_trim;
        changes_ratio_bp -= overflow - details_trim;
    }
    let last_view = match state.last_view.as_str() {
        "changes" | "log" | "details" => state.last_view.as_str(),
        _ => "log",
    }
    .to_string();

    Some(GitClientWindowSessionDto {
        repository_root,
        width: state.width.clamp(760, 3_840),
        height: state.height.clamp(520, 2_160),
        changes_ratio_bp,
        details_ratio_bp,
        console_ratio_bp: state.console_ratio_bp.clamp(1_200, 4_500),
        console_expanded: state.console_expanded,
        last_view,
    })
}

fn empty_session_state() -> SessionStateDto {
    SessionStateDto {
        schema_version: SESSION_SCHEMA_VERSION,
        active_tab: 0,
        tabs: Vec::new(),
        git_client_windows: Vec::new(),
    }
}

fn same_repository(left: &str, right: &Path) -> bool {
    let left = left.trim();
    let right = right.to_string_lossy();
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right.as_ref())
    } else {
        left == right
    }
}

fn load_session_at(path: &Path) -> Option<SessionStateDto> {
    let backup = backup_path(path);
    for candidate in [path, backup.as_path()] {
        let Ok(metadata) = fs::metadata(candidate) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_SESSION_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(candidate) else {
            continue;
        };
        let Ok(state) = serde_json::from_slice::<SessionStateDto>(&bytes) else {
            continue;
        };
        if let Some(state) = normalize_state(&state) {
            return Some(state);
        }
    }
    None
}

fn save_session_at(path: &Path, state: &SessionStateDto) -> Result<(), String> {
    let state = normalize_state(state).ok_or_else(|| "session state is invalid".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create session directory: {error}"))?;
    }

    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|error| format!("cannot serialize session state: {error}"))?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err("session state is too large".to_string());
    }

    let temporary = temporary_path(path);
    let backup = backup_path(path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create temporary session file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("cannot write temporary session file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync temporary session file: {error}"))?;
    }

    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| format!("cannot replace session backup: {error}"))?;
        }
        fs::rename(path, &backup)
            .map_err(|error| format!("cannot rotate session backup: {error}"))?;
    }

    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() && !path.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot replace session file: {error}"));
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_session_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "kite-session-{label}-{}-{unique}",
                std::process::id()
            ))
            .join("session.json")
    }

    fn tab(path: &str) -> SessionTabDto {
        SessionTabDto {
            current_directory: path.to_string(),
            view_mode: "columns".to_string(),
            show_path_bar: true,
        }
    }

    #[test]
    fn saves_and_loads_a_normalized_session() {
        let path = temp_session_path("roundtrip");
        let state = SessionStateDto {
            schema_version: SESSION_SCHEMA_VERSION,
            active_tab: 99,
            tabs: vec![tab("C:/one"), tab("C:/two")],
            git_client_windows: Vec::new(),
        };

        save_session_at(&path, &state).expect("save session");
        let loaded = load_session_at(&path).expect("load session");

        assert_eq!(loaded.schema_version, SESSION_SCHEMA_VERSION);
        assert_eq!(loaded.active_tab, 1);
        assert_eq!(loaded.tabs.len(), 2);
        let _ = fs::remove_dir_all(path.parent().expect("session parent"));
    }

    #[test]
    fn falls_back_to_the_previous_session_when_the_primary_is_corrupt() {
        let path = temp_session_path("backup");
        let first = SessionStateDto {
            schema_version: SESSION_SCHEMA_VERSION,
            active_tab: 0,
            tabs: vec![tab("C:/first")],
            git_client_windows: Vec::new(),
        };
        let second = SessionStateDto {
            schema_version: SESSION_SCHEMA_VERSION,
            active_tab: 0,
            tabs: vec![tab("C:/second")],
            git_client_windows: Vec::new(),
        };

        save_session_at(&path, &first).expect("save first session");
        save_session_at(&path, &second).expect("save second session");
        fs::write(&path, b"not json").expect("corrupt primary session");

        let loaded = load_session_at(&path).expect("load backup session");
        assert_eq!(loaded.tabs[0].current_directory, "C:/first");
        let _ = fs::remove_dir_all(path.parent().expect("session parent"));
    }

    #[test]
    fn rejects_an_empty_or_future_session() {
        let path = temp_session_path("invalid");
        let empty = SessionStateDto {
            schema_version: SESSION_SCHEMA_VERSION,
            active_tab: 0,
            tabs: Vec::new(),
            git_client_windows: Vec::new(),
        };
        assert!(save_session_at(&path, &empty).is_err());

        let future = SessionStateDto {
            schema_version: SESSION_SCHEMA_VERSION + 1,
            active_tab: 0,
            tabs: vec![tab("C:/future")],
            git_client_windows: Vec::new(),
        };
        assert!(save_session_at(&path, &future).is_err());
        let _ = fs::remove_dir_all(path.parent().expect("session parent"));
    }

    #[test]
    fn normalizes_git_client_window_state() {
        let state = GitClientWindowSessionDto {
            repository_root: "  C:/repo  ".to_string(),
            width: 10,
            height: 9_000,
            changes_ratio_bp: 9_000,
            details_ratio_bp: 9_000,
            console_ratio_bp: 10,
            console_expanded: true,
            last_view: "unknown".to_string(),
        };
        let normalized = normalize_git_client_window(&state).expect("valid state");
        assert_eq!(normalized.width, 760);
        assert_eq!(normalized.height, 2_160);
        assert!(normalized.changes_ratio_bp >= 1_400);
        assert!(normalized.details_ratio_bp >= 1_600);
        assert!(normalized.changes_ratio_bp + normalized.details_ratio_bp <= 7_400);
        assert_eq!(normalized.console_ratio_bp, 1_200);
        assert_eq!(normalized.last_view, "log");
    }
}
