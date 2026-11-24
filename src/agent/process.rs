use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::agent::model::{AgentError, AgentErrorKind};

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
