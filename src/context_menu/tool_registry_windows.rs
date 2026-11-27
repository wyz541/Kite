//! Windows tool discovery for the shared tool registry.
//!
//! Discovery is deliberately conservative: a tool is `Available` only when an
//! executable can be resolved through PATH or the per-user/machine App Paths
//! registry. Launching always uses an argument vector, never a shell string.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use kite_core::tool_registry::{
    DiscoveredTool, ToolAvailability, ToolCatalog, ToolCatalogStatus, ToolPlatform, ToolPolicy,
    ToolRole, build_tool_invocation, builtin_tool_definitions,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::ui_preferences;

static DISCOVERED: OnceLock<Mutex<Option<Vec<DiscoveredTool>>>> = OnceLock::new();

pub(crate) fn catalog(target: &crate::context_menu::ContextMenuTarget) -> ToolCatalog {
    let definitions = builtin_tool_definitions(ToolPlatform::Windows);
    let mut tools = discovered_tools();
    for definition in &definitions {
        if definition.tool_id == "kite.text-editor" {
            tools.retain(|tool| tool.tool_id != definition.tool_id);
            tools.push(DiscoveredTool::available(
                &definition.tool_id,
                definition
                    .executable_identity
                    .as_deref()
                    .unwrap_or("kite-internal:text-editor"),
            ));
        } else if definition.tool_id == "kite.git-client" {
            tools.retain(|tool| tool.tool_id != definition.tool_id);
            if target.git_available {
                tools.push(DiscoveredTool::available(
                    &definition.tool_id,
                    definition
                        .executable_identity
                        .as_deref()
                        .unwrap_or("kite-internal:git-client"),
                ));
            } else {
                tools.push(DiscoveredTool::unavailable(
                    &definition.tool_id,
                    ToolAvailability::Unsupported,
                ));
            }
        }
    }
    ToolCatalog {
        status: ToolCatalogStatus::Ready,
        tools,
    }
}

pub(crate) fn policy() -> ToolPolicy {
    ui_preferences::load_ui_preferences().tool_policy
}

pub(crate) fn refresh() {
    let cache = DISCOVERED.get_or_init(|| Mutex::new(None));
    if let Ok(mut discovered) = cache.lock() {
        *discovered = None;
    }
}

fn discovered_tools() -> Vec<DiscoveredTool> {
    let cache = DISCOVERED.get_or_init(|| Mutex::new(None));
    if let Ok(discovered) = cache.lock()
        && let Some(tools) = discovered.as_ref()
    {
        return tools.clone();
    }
    let tools = discover_external_tools();
    if let Ok(mut discovered) = cache.lock() {
        *discovered = Some(tools.clone());
    }
    tools
}

pub(crate) fn launch(
    tool_id: &str,
    role: ToolRole,
    paths: &[PathBuf],
    working_directory: &Path,
) -> Result<(), String> {
    let definition = builtin_tool_definitions(ToolPlatform::Windows)
        .into_iter()
        .find(|definition| definition.tool_id == tool_id)
        .ok_or_else(|| format!("未知工具：{tool_id}"))?;
    if definition.launch_kind == kite_core::tool_registry::ToolLaunchKind::KiteInternal {
        return Err(format!("Kite 内建工具应由应用分发器处理：{tool_id}"));
    }
    let targets = kite_core::context_menu_model::TargetSet::new(
        paths.first().map(|path| path.display().to_string()),
        paths
            .iter()
            .map(|path| {
                let kind = if path.is_dir() {
                    kite_core::context_menu_model::TargetKind::Directory
                } else if kite_core::archive::is_supported_archive(path) {
                    kite_core::context_menu_model::TargetKind::Archive
                } else {
                    kite_core::context_menu_model::TargetKind::File
                };
                kite_core::context_menu_model::ContextTarget::new(
                    path.display().to_string(),
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string()),
                    kind,
                )
            })
            .collect(),
        working_directory.display().to_string(),
    );
    let invocation = build_tool_invocation(&definition, role, &targets, None)?;
    let mut command = Command::new(resolve_executable(&invocation.executable)?);
    command.args(invocation.arguments);
    if let Some(directory) = invocation.working_directory {
        command.current_dir(directory);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法启动 {}：{error}", definition.display_name))?;
    ui_preferences::record_tool_use(tool_id);
    Ok(())
}

fn discover_external_tools() -> Vec<DiscoveredTool> {
    let mut tools = Vec::new();
    for definition in builtin_tool_definitions(ToolPlatform::Windows) {
        if definition.launch_kind == kite_core::tool_registry::ToolLaunchKind::KiteInternal {
            continue;
        }
        let Some(identity) = definition.executable_identity.as_deref() else {
            tools.push(DiscoveredTool::unavailable(
                &definition.tool_id,
                ToolAvailability::Unsupported,
            ));
            continue;
        };
        match resolve_executable(identity) {
            Ok(path) => tools.push(DiscoveredTool::available(
                definition.tool_id,
                path.display().to_string(),
            )),
            Err(_) => tools.push(DiscoveredTool::unavailable(
                &definition.tool_id,
                ToolAvailability::NotInstalled,
            )),
        }
    }
    tools
}

fn resolve_executable(executable: &str) -> Result<PathBuf, String> {
    let path = Path::new(executable);
    if path.components().count() > 1 || path.is_absolute() {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(format!("工具不存在：{executable}"));
    }
    if let Some(path) = find_in_path(executable) {
        return Ok(path);
    }
    if let Some(path) = app_paths_lookup(executable) {
        return Ok(path);
    }
    Err(format!("未找到工具：{executable}"))
}

fn find_in_path(executable: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let candidates = executable_candidates(executable);
    std::env::split_paths(&path)
        .flat_map(|directory| {
            candidates
                .iter()
                .map(move |candidate| directory.join(candidate))
        })
        .find(|candidate| candidate.is_file())
}

fn app_paths_lookup(executable: &str) -> Option<PathBuf> {
    let candidates = executable_candidates(executable);
    for candidate in candidates {
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let root = RegKey::predef(hive);
            let key_name =
                format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{candidate}");
            let Ok(key) = root.open_subkey(&key_name) else {
                continue;
            };
            let Ok(value) = key.get_value::<String, _>("") else {
                continue;
            };
            let path = PathBuf::from(value.trim_matches('"'));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn executable_candidates(executable: &str) -> Vec<String> {
    if executable.ends_with(".exe") || executable.ends_with(".cmd") || executable.ends_with(".bat")
    {
        return vec![executable.to_string()];
    }
    vec![executable.to_string(), format!("{executable}.exe")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_candidates_cover_exe_and_bare_names() {
        assert_eq!(
            executable_candidates("Code.exe"),
            vec!["Code.exe".to_string()]
        );
        assert_eq!(
            executable_candidates("code"),
            vec!["code".to_string(), "code.exe".to_string()]
        );
    }

    #[test]
    fn app_paths_key_uses_stable_executable_name() {
        let names = executable_candidates("WinMergeU");
        assert!(names.iter().any(|name| name == "WinMergeU.exe"));
    }

    #[test]
    fn catalog_includes_only_resolved_external_tools() {
        let tools = discover_external_tools();
        assert!(tools.iter().all(|tool| {
            tool.status != ToolAvailability::Available
                || tool.executable_or_bundle.as_deref().is_some_and(|path| {
                    let path = Path::new(path);
                    path.is_absolute() || path.exists()
                })
        }));
    }
}
