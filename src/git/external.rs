use std::path::{Path, PathBuf};
use std::process::Command;

use super::GitService;

/// Opens a native Git history viewer. The text query remains the portable
/// fallback when neither TortoiseGit nor gitk is installed.
pub fn open_log_window(target: &Path) -> Result<String, String> {
    let target = dunce::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let repository = GitService::shared()
        .discover_repository(&target)
        .map_err(|error| error.to_string())?;
    if spawn_tortoise_log("TortoiseGitProc.exe", "TortoiseGit", &target) {
        return Ok("Git 日志窗口已打开".to_string());
    }

    let mut command = crate::vcs::configured_command("gitk", &repository.work_tree);
    command.arg("--all");
    if let Ok(relative) = repository.relative_path(&target)
        && !relative.as_os_str().is_empty()
    {
        command.arg("--").arg(relative);
    }
    command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "未找到 TortoiseGitProc.exe 或 gitk。请安装 Git GUI。".to_string()
        } else {
            format!("无法打开 Git 日志窗口：{error}")
        }
    })?;
    Ok("Git 日志窗口已打开".to_string())
}

fn spawn_tortoise_log(executable: &str, product_dir: &str, target: &Path) -> bool {
    gui_executable_candidates(executable, product_dir)
        .into_iter()
        .any(|candidate| {
            Command::new(candidate)
                .arg("/command:log")
                .arg(format!("/path:{}", target.display()))
                .spawn()
                .is_ok()
        })
}

fn gui_executable_candidates(executable: &str, product_dir: &str) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(executable)];
    if let Some(base) = std::env::var_os("ProgramFiles") {
        candidates.push(
            PathBuf::from(base)
                .join(product_dir)
                .join("bin")
                .join(executable),
        );
    }
    if let Some(base) = std::env::var_os("ProgramFiles(x86)") {
        candidates.push(
            PathBuf::from(base)
                .join(product_dir)
                .join("bin")
                .join(executable),
        );
    }
    if let Some(base) = std::env::var_os("LocalAppData") {
        candidates.push(
            PathBuf::from(base)
                .join("Programs")
                .join(product_dir)
                .join("bin")
                .join(executable),
        );
    }
    candidates
}
