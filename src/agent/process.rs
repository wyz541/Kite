use std::env;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::agent::AgentProfile;
use crate::agent::model::{AgentError, AgentErrorKind};

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

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
    // Standard Windows OS variables needed by runtimes, certificate discovery,
    // and child process launchers. Secrets remain excluded and must come from
    // the profile environment or the credential store.
    "SYSTEMDRIVE",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "COMMONPROGRAMW6432",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOGONSERVER",
    "OS",
    "COMPUTERNAME",
    "USERNAME",
    "USERDOMAIN",
    "SESSIONNAME",
    "PROCESSOR_IDENTIFIER",
    "PSModulePath",
];

pub(crate) fn resolve_agent_command(profile: &AgentProfile, cwd: &Path) -> Result<PathBuf, String> {
    let command = profile
        .executable
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
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
            .ok_or_else(|| format!("找不到 Agent 可执行文件：{command}"));
    }

    let path_var = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path_var) {
        for candidate in command_candidates(&directory, command) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!("在 PATH 中找不到 Agent 命令：{command}"))
}

pub(crate) fn build_agent_command(
    profile: &AgentProfile,
    cwd: &Path,
) -> Result<(Command, PathBuf), String> {
    let executable = resolve_agent_command(profile, cwd)?;
    let command = if is_windows_batch(&executable) {
        let mut command = Command::new("cmd.exe");
        command.arg("/D").arg("/S").arg("/C");
        #[cfg(windows)]
        command.raw_arg(format!(
            "\"{}\"",
            batch_command_line(&executable, &profile.arguments)
        ));
        #[cfg(not(windows))]
        command.arg(batch_command_line(&executable, &profile.arguments));
        command
    } else {
        let mut command = Command::new(&executable);
        command.args(&profile.arguments);
        command
    };
    Ok((command, executable))
}

pub(crate) fn spawn_agent_process(profile: &AgentProfile, cwd: &Path) -> Result<Child, String> {
    let (mut command, executable) = prepare_agent_command(profile, cwd, None)?;
    command
        .spawn()
        .map_err(|error| format!("无法启动 Agent {}：{error}", executable.display()))
}

pub(crate) fn spawn_agent_process_guard(
    profile: &AgentProfile,
    cwd: &Path,
    credential: Option<&str>,
) -> Result<SpawnedProcess, String> {
    let (command, _) = prepare_agent_command(profile, cwd, credential)?;
    ProcessGuard::spawn(command).map_err(|error| error.to_string())
}

fn prepare_agent_command(
    profile: &AgentProfile,
    cwd: &Path,
    credential: Option<&str>,
) -> Result<(Command, PathBuf), String> {
    if !cwd.is_dir() {
        return Err(format!("Agent 工作目录不存在：{}", cwd.display()));
    }
    let (mut command, executable) = build_agent_command(profile, cwd)?;

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
    for variable in &profile.environment {
        if !variable.value.is_empty() {
            command.env(variable.name.trim(), &variable.value);
        }
    }
    if let Some(credential) = credential {
        let name = profile
            .credential_env_var
            .as_deref()
            .unwrap_or("KITE_AGENT_TOKEN");
        command.env(name, credential);
    }

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }

    Ok((command, executable))
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

#[cfg(windows)]
struct WindowsJob {
    handle: usize,
}

#[cfg(windows)]
impl WindowsJob {
    fn create(child: &Child) -> Option<Arc<Self>> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows::core::PCWSTR;

        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .is_err()
            {
                let _ = CloseHandle(job);
                return None;
            }
            if AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
                let _ = CloseHandle(job);
                return None;
            }
            Some(Arc::new(Self {
                handle: job.0 as usize,
            }))
        }
    }

    fn terminate(&self) -> bool {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::TerminateJobObject;

        unsafe { TerminateJobObject(HANDLE(self.handle as *mut _), 1).is_ok() }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};

        unsafe {
            let _ = CloseHandle(HANDLE(self.handle as *mut _));
        }
    }
}

pub(crate) struct ProcessHandle {
    pid: u32,
    active: AtomicBool,
    terminating: AtomicBool,
    #[cfg(windows)]
    windows_job: Option<Arc<WindowsJob>>,
}

impl ProcessHandle {
    #[cfg(windows)]
    fn new(pid: u32, windows_job: Option<Arc<WindowsJob>>) -> Self {
        Self {
            pid,
            active: AtomicBool::new(true),
            terminating: AtomicBool::new(false),
            windows_job,
        }
    }

    #[cfg(unix)]
    fn new(pid: u32) -> Self {
        Self {
            pid,
            active: AtomicBool::new(true),
            terminating: AtomicBool::new(false),
        }
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Ask the OS to stop the whole child tree. This is intentionally independent
    /// from `Child::kill`, which only targets the root process.
    pub(crate) fn terminate_tree(&self) {
        if !self.active.load(Ordering::Acquire) || self.terminating.swap(true, Ordering::AcqRel) {
            return;
        }

        #[cfg(windows)]
        {
            let terminated = self.windows_job.as_ref().is_some_and(|job| job.terminate());
            if !terminated {
                terminate_with_taskkill(self.pid);
            }
        }
        #[cfg(unix)]
        {
            let pgid = -(self.pid as i32);
            // The process was placed in its own group at spawn time. Terminate the
            // group first, then force the group after a short grace period.
            unsafe {
                libc::kill(pgid, libc::SIGTERM);
            }
            thread::sleep(Duration::from_millis(120));
            unsafe {
                libc::kill(pgid, libc::SIGKILL);
            }
        }

        self.active.store(false, Ordering::Release);
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(windows)]
fn terminate_with_taskkill(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = Command::new("taskkill");
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let _ = command.status();
}
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if self.is_active() {
            self.terminate_tree();
        }
    }
}

pub(crate) struct SpawnedProcess {
    pub(crate) guard: ProcessGuard,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: ChildStdout,
    pub(crate) stderr: ChildStderr,
    pub(crate) handle: Arc<ProcessHandle>,
}

pub(crate) struct ProcessGuard {
    child: Child,
    handle: Arc<ProcessHandle>,
    reaped: Option<ExitStatus>,
}

impl ProcessGuard {
    pub(crate) fn spawn(mut command: Command) -> Result<SpawnedProcess, AgentError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            let kind = if error.kind() == std::io::ErrorKind::NotFound {
                AgentErrorKind::NotInstalled
            } else {
                AgentErrorKind::Io
            };
            let message = if kind == AgentErrorKind::NotInstalled {
                "未找到 Agent 可执行文件。".to_string()
            } else {
                "无法启动 Agent 进程。".to_string()
            };
            AgentError::new(kind, message).with_detail(error.to_string())
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Io, "无法连接 Agent 标准输入。"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Io, "无法连接 Agent 标准输出。"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentError::new(AgentErrorKind::Io, "无法连接 Agent 标准错误。"))?;
        #[cfg(windows)]
        let windows_job = WindowsJob::create(&child);
        #[cfg(windows)]
        let handle = Arc::new(ProcessHandle::new(child.id(), windows_job));
        #[cfg(unix)]
        let handle = Arc::new(ProcessHandle::new(child.id()));
        Ok(SpawnedProcess {
            guard: ProcessGuard {
                child,
                handle: handle.clone(),
                reaped: None,
            },
            stdin,
            stdout,
            stderr,
            handle,
        })
    }

    pub(crate) fn try_wait(&mut self) -> Option<ExitStatus> {
        if let Some(status) = self.reaped {
            return Some(status);
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.reaped = Some(status);
                Some(status)
            }
            Ok(None) => None,
            Err(_) => None,
        }
    }

    pub(crate) fn terminate_tree(&mut self, grace: Duration) {
        self.handle.terminate_tree();
        let deadline = Instant::now() + grace;
        loop {
            if self.try_wait().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.reaped = self.child.try_wait().ok().flatten();
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.handle.terminate_tree();
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.terminate_tree(Duration::from_millis(350));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_fixture_binary_from_path_or_workspace_target() {
        let profile = AgentProfile {
            executable: Some(if cfg!(windows) {
                "kite-agent-fixture.exe".to_string()
            } else {
                "kite-agent-fixture".to_string()
            }),
            ..AgentProfile::default()
        };
        let cwd = env::current_dir().expect("cwd");
        let _ = resolve_agent_command(&profile, &cwd);
    }

    #[cfg(windows)]
    #[test]
    fn spawns_batch_wrappers_from_paths_with_spaces() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "kite agent scripts {}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create batch directory");
        let script = directory.join("echo argument.cmd");
        fs::write(&script, b"@echo off\r\necho %~1\r\n").expect("write batch script");
        let profile = AgentProfile {
            executable: Some(script.to_string_lossy().into_owned()),
            arguments: vec!["hello world".to_string()],
            ..AgentProfile::default()
        };

        let child = spawn_agent_process(&profile, &directory).expect("spawn batch script");
        let output = child.wait_with_output().expect("batch output");

        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello world"));
        let _ = fs::remove_dir_all(directory);
    }
}
