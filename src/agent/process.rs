use std::env;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::agent::AgentProfile;

const SAFE_ENV_NAMES: &[&str] = &[
    "PATH",
    "Path",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "USER",
    "LOGNAME",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

pub(crate) fn resolve_agent_command(profile: &AgentProfile, cwd: &Path) -> Result<PathBuf, String> {
    let command = profile.command.trim();
    if command.is_empty() {
        return Err("Agent 命令为空".to_string());
    }
    let path = PathBuf::from(command);
    let has_separator = command.contains('/') || command.contains('\\');
    if path.is_absolute() || has_separator {
        let candidate = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        return candidate
            .is_file()
            .then_some(candidate)
            .ok_or_else(|| format!("找不到 Agent 可执行文件：{}", profile.command));
    }

    let path_var = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path_var) {
        for candidate in command_candidates(&directory, command) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!("在 PATH 中找不到 Agent 命令：{}", profile.command))
}

pub(crate) fn spawn_agent_process(profile: &AgentProfile, cwd: &Path) -> Result<Child, String> {
    if !cwd.is_dir() {
        return Err(format!("Agent 工作目录不存在：{}", cwd.display()));
    }
    let executable = resolve_agent_command(profile, cwd)?;
    let mut command = if is_windows_batch(&executable) {
        let mut command = Command::new("cmd.exe");
        command
            .arg("/D")
            .arg("/S")
            .arg("/C")
            .arg(batch_command_line(&executable, &profile.args));
        command
    } else {
        let mut command = Command::new(&executable);
        command.args(&profile.args);
        command
    };

    command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for name in SAFE_ENV_NAMES {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    for variable in &profile.env {
        if !variable.value.is_empty() {
            command.env(variable.name.trim(), &variable.value);
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }

    command
        .spawn()
        .map_err(|error| format!("无法启动 Agent {}：{error}", executable.display()))
}

pub(crate) fn terminate_agent_process(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn command_candidates(directory: &Path, command: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        let mut candidates = vec![directory.join(command)];
        if Path::new(command).extension().is_none() {
            for extension in pathext.split(';').filter(|extension| !extension.is_empty()) {
                candidates.push(directory.join(format!("{command}{extension}")));
            }
        }
        candidates
    }
    #[cfg(not(windows))]
    {
        vec![directory.join(command)]
    }
}

fn is_windows_batch(executable: &Path) -> bool {
    #[cfg(windows)]
    {
        executable
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
            })
    }
    #[cfg(not(windows))]
    {
        let _ = executable;
        false
    }
}

fn batch_command_line(executable: &Path, args: &[String]) -> String {
    std::iter::once(executable.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|argument| format!("\"{}\"", argument.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_fixture_binary_from_path_or_workspace_target() {
        let profile = AgentProfile {
            command: if cfg!(windows) {
                "kite-agent-fixture.exe".to_string()
            } else {
                "kite-agent-fixture".to_string()
            },
            ..AgentProfile::default()
        };
        let cwd = env::current_dir().expect("cwd");
        let _ = resolve_agent_command(&profile, &cwd);
    }
}
