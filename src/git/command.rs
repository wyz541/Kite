use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::error::{GitError, GitErrorKind};
use super::events::{
    GitCredentialProvider, GitCredentialRequest, GitEvent, GitEventSink, GitOutputStream,
};

const CONSOLE_CAPACITY: usize = 256;
const CAPTURE_LIMIT_BYTES: usize = 128 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;
const ASKPASS_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub raw: String,
}

pub const MIN_GIT_VERSION: GitVersion = GitVersion {
    major: 2,
    minor: 30,
    patch: 0,
    raw: String::new(),
};

impl GitVersion {
    pub fn is_supported(&self) -> bool {
        (self.major, self.minor, self.patch)
            >= (
                MIN_GIT_VERSION.major,
                MIN_GIT_VERSION.minor,
                MIN_GIT_VERSION.patch,
            )
    }

    pub fn display(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    pub(super) fn parse(output: &str) -> Option<Self> {
        let raw = output.trim().to_string();
        let value = raw
            .strip_prefix("git version ")
            .unwrap_or(raw.as_str())
            .split_whitespace()
            .next()?;
        let mut components = value.split('.');
        let major = components.next()?.parse().ok()?;
        let minor = components.next()?.parse().ok()?;
        let patch = components
            .next()
            .and_then(|value| {
                value
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .unwrap_or(0);
        Some(Self {
            major,
            minor,
            patch,
            raw,
        })
    }
}

pub fn detect_git_version() -> Result<GitVersion, GitError> {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut command = crate::vcs::configured_command("git", &current_dir);
    command.args(["--no-optional-locks", "--version"]);
    command.stdin(Stdio::null());
    let output = command.output().map_err(GitError::from)?;
    if !output.status.success() {
        return Err(GitError::classified(
            &String::from_utf8_lossy(&output.stderr),
            &String::from_utf8_lossy(&output.stdout),
            output.status.code(),
            "git --version",
            current_dir,
        ));
    }
    GitVersion::parse(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        GitError::new(
            GitErrorKind::UnsupportedVersion,
            "无法识别当前安装的 Git 版本。",
        )
        .with_detail(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

#[derive(Clone, Debug)]
pub struct GitConsoleEntry {
    pub recorded_unix_ms: i64,
    pub label: String,
    pub command: String,
    pub repository: PathBuf,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Default)]
pub struct GitConsole {
    entries: Mutex<VecDeque<GitConsoleEntry>>,
}

impl GitConsole {
    pub fn entries(&self) -> Vec<GitConsoleEntry> {
        self.entries
            .lock()
            .map(|entries| entries.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    fn record(&self, entry: GitConsoleEntry) {
        if let Ok(mut entries) = self.entries.lock() {
            while entries.len() >= CONSOLE_CAPACITY {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }
}

#[derive(Clone, Debug)]
pub struct GitCommandOptions {
    pub label: String,
    /// Fetch, pull, push and clone send `--progress` and stream progress lines.
    pub show_progress: bool,
}

impl GitCommandOptions {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            show_progress: false,
        }
    }

    pub fn with_progress(mut self, show_progress: bool) -> Self {
        self.show_progress = show_progress;
        self
    }
}

#[derive(Clone, Debug)]
pub struct GitCommandResult {
    pub label: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub progress_lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct GitExecutor {
    console: Arc<GitConsole>,
    program: OsString,
    prefix_args: Arc<Vec<OsString>>,
    inject_git_args: bool,
}

impl Default for GitExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl GitExecutor {
    pub fn new() -> Self {
        Self {
            console: Arc::new(GitConsole::default()),
            program: OsString::from("git"),
            prefix_args: Arc::new(Vec::new()),
            inject_git_args: true,
        }
    }

    pub fn with_console(console: Arc<GitConsole>) -> Self {
        Self {
            console,
            ..Self::new()
        }
    }

    #[cfg(test)]
    fn with_test_program(
        console: Arc<GitConsole>,
        program: &str,
        prefix_args: Vec<OsString>,
    ) -> Self {
        Self {
            console,
            program: OsString::from(program),
            prefix_args: Arc::new(prefix_args),
            inject_git_args: false,
        }
    }

    pub fn console(&self) -> Arc<GitConsole> {
        self.console.clone()
    }

    pub fn run(
        &self,
        repository: &Path,
        args: impl IntoIterator<Item = OsString>,
        options: GitCommandOptions,
        cancellation: &CancellationToken,
        events: &dyn GitEventSink,
        credentials: Option<&dyn GitCredentialProvider>,
    ) -> Result<GitCommandResult, GitError> {
        if cancellation.is_cancelled() {
            return Err(GitError::cancelled().with_repository(repository));
        }

        let args = args.into_iter().collect::<Vec<_>>();
        let command_text = render_command(&args);
        events.emit(GitEvent::Started {
            label: options.label.clone(),
            repository: repository.to_path_buf(),
            command: command_text.clone(),
        });

        let mut command = if self.inject_git_args {
            let mut command = crate::vcs::configured_command("git", repository);
            command.args(["--no-optional-locks"]);
            command
        } else {
            let mut command = Command::new(&self.program);
            command
                .current_dir(repository)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C")
                .stdin(Stdio::null());
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            command
        };
        command.args(self.prefix_args.iter());
        command.args(&args);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&directory)?.permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&directory, permissions)?;
        }

        let askpass = credentials
            .map(|_| AskpassBridge::new())
            .transpose()
            .map_err(|error| {
                GitError::new(GitErrorKind::Io, "无法启动 Git 凭据桥。")
                    .with_detail(error.to_string())
                    .with_repository(repository)
            })?;
        if let Some(bridge) = askpass.as_ref() {
            bridge.configure(&mut command);
        }

        let started = Instant::now();
        let mut child = command.spawn().map_err(|error| {
            let error = GitError::from(error)
                .with_command(command_text.clone())
                .with_repository(repository);
            events.emit(GitEvent::Failed {
                kind: error.kind,
                message: error.message.clone(),
                duration_ms: started.elapsed().as_millis() as u64,
            });
            error
        })?;

        let stdout = child.stdout.take().expect("piped git stdout");
        let stderr = child.stderr.take().expect("piped git stderr");
        let (sender, receiver) = mpsc::channel::<StreamMessage>();
        spawn_stream_reader(stdout, GitOutputStream::Stdout, sender.clone());
        spawn_stream_reader(stderr, GitOutputStream::Stderr, sender);

        let mut stdout_text = String::new();
        let mut stderr_text = String::new();
        let mut progress_lines = Vec::new();
        let mut readers_done = 0usize;
        let mut status = None;
        let mut cancel_error = false;

        loop {
            if cancellation.is_cancelled() {
                cancel_error = true;
                let _ = child.kill();
                break;
            }

            if let Some(bridge) = askpass.as_ref()
                && let Some(provider) = credentials
                && let Err(error) = bridge.poll(provider, events, cancellation)
            {
                let _ = child.kill();
                let _ = child.wait();
                let duration_ms = started.elapsed().as_millis() as u64;
                record_console(
                    &self.console,
                    ConsoleRecord {
                        label: &options.label,
                        command: &command_text,
                        repository,
                        duration_ms,
                        exit_code: None,
                        stdout: &stdout_text,
                        stderr: &stderr_text,
                    },
                );
                events.emit(GitEvent::Failed {
                    kind: error.kind,
                    message: error.message.clone(),
                    duration_ms,
                });
                return Err(error.with_command(command_text).with_repository(repository));
            }

            match receiver.recv_timeout(ASKPASS_POLL_INTERVAL) {
                Ok(StreamMessage::Line { stream, text }) => {
                    append_bounded(&mut stdout_text, &mut stderr_text, stream.clone(), &text);
                    if stream == GitOutputStream::Stderr
                        && options.show_progress
                        && looks_like_progress(&text)
                    {
                        if progress_lines.len() >= 256 {
                            progress_lines.remove(0);
                        }
                        if progress_lines.len() >= 256 {
                            progress_lines.remove(0);
                        }
                        progress_lines.push(text.clone());
                        events.emit(GitEvent::Progress { text: text.clone() });
                    }
                    events.emit(GitEvent::Output { stream, text });
                }
                Ok(StreamMessage::Done) => readers_done += 1,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    readers_done = 2;
                }
            }

            if status.is_none() {
                match child.try_wait() {
                    Ok(exit) => status = exit,
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(GitError::from(error)
                            .with_command(command_text)
                            .with_repository(repository));
                    }
                }
            }

            if status.is_some() && readers_done >= 2 {
                while let Ok(message) = receiver.try_recv() {
                    if let StreamMessage::Line { stream, text } = message {
                        append_bounded(&mut stdout_text, &mut stderr_text, stream.clone(), &text);
                        if stream == GitOutputStream::Stderr
                            && options.show_progress
                            && looks_like_progress(&text)
                        {
                            progress_lines.push(text.clone());
                            events.emit(GitEvent::Progress { text: text.clone() });
                        }
                        events.emit(GitEvent::Output { stream, text });
                    }
                }
                break;
            }
        }

        if cancel_error && let Some(exit) = child.try_wait().ok().flatten() {
            status = Some(exit);
        }
        if status.is_none() {
            status = child.wait().ok();
        }
        let duration_ms = started.elapsed().as_millis() as u64;
        let exit_code = status.and_then(|status| status.code());
        record_console(
            &self.console,
            ConsoleRecord {
                label: &options.label,
                command: &command_text,
                repository,
                duration_ms,
                exit_code,
                stdout: &stdout_text,
                stderr: &stderr_text,
            },
        );

        if cancel_error {
            let error = GitError::cancelled()
                .with_exit_code(exit_code)
                .with_command(command_text)
                .with_repository(repository)
                .with_detail(stderr_text);
            events.emit(GitEvent::Failed {
                kind: error.kind,
                message: error.message.clone(),
                duration_ms,
            });
            return Err(error);
        }

        let success = status.is_some_and(|status| status.success());
        if success {
            events.emit(GitEvent::Finished {
                duration_ms,
                exit_code,
            });
            return Ok(GitCommandResult {
                label: options.label,
                stdout: stdout_text,
                stderr: stderr_text,
                exit_code,
                duration_ms,
                progress_lines,
            });
        }

        let error = GitError::classified(
            &stderr_text,
            &stdout_text,
            exit_code,
            command_text,
            repository,
        );
        events.emit(GitEvent::Failed {
            kind: error.kind,
            message: error.message.clone(),
            duration_ms,
        });
        Err(error)
    }
}

fn append_bounded(stdout: &mut String, stderr: &mut String, stream: GitOutputStream, text: &str) {
    let destination = match stream {
        GitOutputStream::Stdout => stdout,
        GitOutputStream::Stderr => stderr,
    };
    if destination.len() >= CAPTURE_LIMIT_BYTES {
        return;
    }
    let remaining = CAPTURE_LIMIT_BYTES - destination.len();
    if text.len() <= remaining {
        destination.push_str(text);
        destination.push('\n');
    } else {
        let mut end = remaining.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        destination.push_str(&text[..end]);
        destination.push('\n');
    }
}

fn looks_like_progress(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains('%')
        || lower.contains("counting objects")
        || lower.contains("compressing objects")
        || lower.contains("receiving objects")
        || lower.contains("resolving deltas")
        || lower.contains("writing objects")
        || lower.contains("remote:")
        || lower.contains("enumerating objects")
        || lower.contains("updating files")
}

enum StreamMessage {
    Line {
        stream: GitOutputStream,
        text: String,
    },
    Done,
}

fn spawn_stream_reader<R>(
    mut reader: R,
    stream: GitOutputStream,
    sender: mpsc::Sender<StreamMessage>,
) where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut line = Vec::new();
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    for byte in &chunk[..count] {
                        if *byte == b'\n' || *byte == b'\r' {
                            if !line.is_empty() {
                                let text = String::from_utf8_lossy(&line).into_owned();
                                line.clear();
                                if sender
                                    .send(StreamMessage::Line {
                                        stream: stream.clone(),
                                        text,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        } else {
                            line.push(*byte);
                            if line.len() >= MAX_LINE_BYTES {
                                let text = String::from_utf8_lossy(&line).into_owned();
                                line.clear();
                                if sender
                                    .send(StreamMessage::Line {
                                        stream: stream.clone(),
                                        text,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
        if !line.is_empty() {
            let _ = sender.send(StreamMessage::Line {
                stream,
                text: String::from_utf8_lossy(&line).into_owned(),
            });
        }
        let _ = sender.send(StreamMessage::Done);
    });
}

struct ConsoleRecord<'a> {
    label: &'a str,
    command: &'a str,
    repository: &'a Path,
    duration_ms: u64,
    exit_code: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
}

fn record_console(console: &GitConsole, record: ConsoleRecord<'_>) {
    let recorded_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    console.record(GitConsoleEntry {
        recorded_unix_ms,
        label: record.label.to_string(),
        command: redact_credentials(record.command),
        repository: record.repository.to_path_buf(),
        duration_ms: record.duration_ms,
        exit_code: record.exit_code,
        stdout: record.stdout.to_string(),
        stderr: record.stderr.to_string(),
    });
}

fn render_command(args: &[OsString]) -> String {
    let mut rendered = String::from("git --no-optional-locks");
    for arg in args {
        rendered.push(' ');
        rendered.push_str(&quote_argument(arg));
    }
    rendered
}

fn quote_argument(arg: &OsStr) -> String {
    let value = arg.to_string_lossy();
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_./:@{}[]=,+".contains(character))
    {
        value.into_owned()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn redact_credentials(command: &str) -> String {
    let mut result = command.to_string();
    if let Some(scheme) = result.find("://") {
        let authority_start = scheme + 3;
        let authority_end = result[authority_start..]
            .find('/')
            .map(|offset| authority_start + offset)
            .unwrap_or(result.len());
        if let Some(at) = result[authority_start..authority_end].rfind('@') {
            result.replace_range(authority_start..authority_start + at, "***");
        }
    }
    result
}

struct AskpassBridge {
    directory: PathBuf,
    _id: u64,
}

impl AskpassBridge {
    fn new() -> std::io::Result<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("kite-git-askpass-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&directory)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = directory.join("askpass.sh");
            std::fs::write(
                &script,
                r#"#!/bin/sh
dir="$KITE_GIT_ASKPASS_DIR"
id="$-$(date +%s)"
request="$dir/request-$id"
response="$dir/response-$id"
printf '%s' "$1" > "$request"
while [ ! -f "$response" ]; do sleep 0.02; done
cat "$response"
rm -f "$request" "$response"
"#,
            )?;
            let mut permissions = std::fs::metadata(&script)?.permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&script, permissions)?;
        }

        #[cfg(windows)]
        {
            let script = directory.join("askpass.cmd");
            std::fs::write(
                &script,
                "@echo off\r\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File \"%~dp0askpass.ps1\" %*\r\n",
            )?;
            std::fs::write(
                directory.join("askpass.ps1"),
                r#"$id = [guid]::NewGuid().ToString("N")
$request = Join-Path $env:KITE_GIT_ASKPASS_DIR "request-$id.txt"
$response = Join-Path $env:KITE_GIT_ASKPASS_DIR "response-$id.txt"
[IO.File]::WriteAllText($request, $args[0])
while (-not (Test-Path -LiteralPath $response)) {
    Start-Sleep -Milliseconds 25
}
[Console]::Out.Write([IO.File]::ReadAllText($response))
Remove-Item -LiteralPath $request, $response -Force -ErrorAction SilentlyContinue
"#,
            )?;
        }

        Ok(Self { directory, _id: id })
    }

    fn script_path(&self) -> PathBuf {
        #[cfg(windows)]
        {
            self.directory.join("askpass.cmd")
        }
        #[cfg(not(windows))]
        {
            self.directory.join("askpass.sh")
        }
    }

    fn configure(&self, command: &mut Command) {
        let script = self.script_path();
        command
            .env("KITE_GIT_ASKPASS_DIR", &self.directory)
            .env("GIT_ASKPASS", &script)
            .env("SSH_ASKPASS", &script)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("PINENTRY_USER_DATA", self.directory.as_os_str());
        if std::env::var_os("DISPLAY").is_none() {
            command.env("DISPLAY", ":0");
        }
    }

    fn poll(
        &self,
        provider: &dyn GitCredentialProvider,
        events: &dyn GitEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(), GitError> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) => return Err(GitError::from(error)),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            let Some(id) = name.strip_prefix("request-") else {
                continue;
            };
            let response = self.directory.join(format!("response-{id}"));
            if response.exists() {
                continue;
            }
            let prompt = match std::fs::read_to_string(&path) {
                Ok(prompt) if !prompt.is_empty() => prompt,
                _ => continue,
            };
            let request = GitCredentialRequest::classify(prompt);
            events.emit(GitEvent::CredentialRequested {
                kind: request.kind,
                prompt: request.prompt.clone(),
            });
            let answer = provider.request_credential(&request);
            let Some(answer) = answer else {
                cancellation.cancel();
                return Ok(());
            };
            std::fs::write(&response, answer).map_err(GitError::from)?;
        }
        Ok(())
    }
}

impl Drop for AskpassBridge {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::events::{
        GitCredentialRequest, GitEvent, GitEventSink, NoopGitCredentialProvider, NoopGitEventSink,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn parses_common_git_version_formats() {
        assert_eq!(
            GitVersion::parse("git version 2.39.5 (Apple Git-154)"),
            Some(GitVersion {
                major: 2,
                minor: 39,
                patch: 5,
                raw: "git version 2.39.5 (Apple Git-154)".into(),
            })
        );
        assert_eq!(
            GitVersion::parse("git version 2.45.1.windows.1").map(|value| value.display()),
            Some("2.45.1".into())
        );
        assert!(GitVersion::parse("not git").is_none());
    }

    #[test]
    fn version_gate_rejects_old_git() {
        let old = GitVersion {
            major: 2,
            minor: 29,
            patch: 0,
            raw: "git version 2.29.0".into(),
        };
        assert!(!old.is_supported());
        assert!(MIN_GIT_VERSION.is_supported());
    }

    #[test]
    fn redacts_inline_url_credentials() {
        assert_eq!(
            redact_credentials("git clone https://user:secret@example.test/repo.git"),
            "git clone https://***@example.test/repo.git"
        );
    }

    struct RecordingSink(Arc<Mutex<Vec<GitEvent>>>);
    impl GitEventSink for RecordingSink {
        fn emit(&self, event: GitEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[test]
    fn cancellation_token_is_shared_between_threads() {
        let token = CancellationToken::default();
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn askpass_classifies_common_prompts() {
        assert_eq!(
            GitCredentialRequest::classify("Username for 'https://example.test': ").kind,
            crate::git::events::GitCredentialKind::Https
        );
        assert_eq!(
            GitCredentialRequest::classify("Enter passphrase for key '/home/me/.ssh/id_ed25519':")
                .kind,
            crate::git::events::GitCredentialKind::SshPassphrase
        );
        assert_eq!(
            GitCredentialRequest::classify("GPG signing key passphrase:").kind,
            crate::git::events::GitCredentialKind::GpgPinentry
        );
    }

    #[test]
    fn noop_credential_provider_declines() {
        let provider = NoopGitCredentialProvider;
        let request = GitCredentialRequest::classify("Password:");
        assert!(provider.request_credential(&request).is_none());
    }

    #[test]
    fn console_is_bounded() {
        let console = GitConsole::default();
        for index in 0..(CONSOLE_CAPACITY + 3) {
            console.record(GitConsoleEntry {
                recorded_unix_ms: index as i64,
                label: index.to_string(),
                command: "git".into(),
                repository: PathBuf::from("."),
                duration_ms: 1,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            });
        }
        let entries = console.entries();
        assert_eq!(entries.len(), CONSOLE_CAPACITY);
        assert_eq!(entries.first().unwrap().label, "3");
    }

    fn fake_git_executor(label: &str) -> (GitExecutor, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "kite-git-command-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create fake command temp dir");

        #[cfg(windows)]
        {
            let script = root.join("git-probe.ps1");
            std::fs::write(
                &script,
                r#"param([string]$Mode)
switch ($Mode) {
  "progress" {
    [Console]::Error.Write("Receiving objects: 50%`r")
    [Console]::Error.Flush()
    Start-Sleep -Milliseconds 40
    [Console]::Error.Write("Receiving objects: 100%`r")
    [Console]::Error.Flush()
    exit 0
  }
  "auth" {
    [Console]::Error.WriteLine("fatal: Authentication failed for 'https://example.test/repo.git/'")
    exit 1
  }
  "askpass" {
    $answer = & $env:GIT_ASKPASS "Username for 'https://example.test':"
    if ($answer -eq "kite-user") { exit 0 }
    [Console]::Error.WriteLine("unexpected askpass answer: $answer")
    exit 2
  }
  "cancel" {
    Start-Sleep -Seconds 30
    exit 0
  }
  default { exit 0 }
}
"#,
            )
            .expect("write fake git script");
            let executor = GitExecutor::with_test_program(
                Arc::new(GitConsole::default()),
                "powershell.exe",
                vec![
                    OsString::from("-NoProfile"),
                    OsString::from("-ExecutionPolicy"),
                    OsString::from("Bypass"),
                    OsString::from("-File"),
                    script.into_os_string(),
                ],
            );
            (executor, root)
        }

        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = root.join("git-probe.sh");
            std::fs::write(
                &script,
                r#"#!/bin/sh
case "$1" in
  progress)
    printf 'Receiving objects: 50%\r'
    sleep 0.04
    printf 'Receiving objects: 100%\r'
    exit 0
    ;;
  auth)
    printf "fatal: Authentication failed for 'https://example.test/repo.git/'\n" >&2
    exit 1
    ;;
  askpass)
    answer="$("$GIT_ASKPASS" "Username for 'https://example.test':")"
    [ "$answer" = "kite-user" ] && exit 0
    printf 'unexpected askpass answer: %s\n' "$answer" >&2
    exit 2
    ;;
  cancel)
    sleep 30
    exit 0
    ;;
esac
exit 0
"#,
            )
            .expect("write fake git script");
            let mut permissions = std::fs::metadata(&script)
                .expect("fake script metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&script, permissions).expect("chmod fake script");
            let executor = GitExecutor::with_test_program(
                Arc::new(GitConsole::default()),
                "sh",
                vec![script.into_os_string()],
            );
            (executor, root)
        }
    }

    fn run_fake(
        executor: &GitExecutor,
        root: &Path,
        mode: &str,
        show_progress: bool,
        sink: &dyn GitEventSink,
        cancellation: &CancellationToken,
    ) -> Result<GitCommandResult, crate::git::GitError> {
        executor.run(
            root,
            vec![OsString::from(mode)],
            GitCommandOptions::new("Fake Git").with_progress(show_progress),
            cancellation,
            sink,
            Some(&NoopGitCredentialProvider),
        )
    }

    #[test]
    fn classifies_authentication_failures_from_a_command() {
        let (executor, root) = fake_git_executor("auth");
        let sink = NoopGitEventSink;
        let error = run_fake(
            &executor,
            &root,
            "auth",
            false,
            &sink,
            &CancellationToken::default(),
        )
        .expect_err("authentication should fail");
        assert_eq!(error.kind, crate::git::GitErrorKind::Authentication);
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|value| value.contains("Authentication failed"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn streams_progress_lines_from_stderr() {
        let (executor, root) = fake_git_executor("progress");
        let sink = Arc::new(RecordingSink(Arc::new(Mutex::new(Vec::new()))));
        run_fake(
            &executor,
            &root,
            "progress",
            true,
            sink.as_ref(),
            &CancellationToken::default(),
        )
        .expect("progress command");
        let events = sink.0.lock().unwrap();
        let progress = events
            .iter()
            .filter(|event| matches!(event, GitEvent::Progress { .. }))
            .count();
        assert!(
            progress >= 2,
            "expected two progress events, got {progress}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn routes_askpass_requests_through_the_credential_provider() {
        let bridge = AskpassBridge::new().expect("create askpass bridge");
        let request_path = bridge.directory.join("request-test.txt");
        std::fs::write(&request_path, "Username for 'https://example.test':")
            .expect("write askpass request");

        let sink = Arc::new(RecordingSink(Arc::new(Mutex::new(Vec::new()))));
        let provider = |request: &GitCredentialRequest| {
            assert_eq!(request.kind, crate::git::GitCredentialKind::Https);
            Some("kite-user".to_string())
        };
        bridge
            .poll(&provider, sink.as_ref(), &CancellationToken::default())
            .expect("answer askpass request");

        assert_eq!(
            std::fs::read_to_string(bridge.directory.join("response-test.txt"))
                .expect("read askpass response"),
            "kite-user"
        );
        assert!(matches!(
            sink.0.lock().unwrap().first(),
            Some(GitEvent::CredentialRequested {
                kind: crate::git::GitCredentialKind::Https,
                ..
            })
        ));
    }

    #[test]
    fn cancels_a_running_git_command() {
        let (executor, root) = fake_git_executor("cancel");
        let cancellation = CancellationToken::default();
        let worker_cancel = cancellation.clone();
        let sink = Arc::new(RecordingSink(Arc::new(Mutex::new(Vec::new()))));
        let worker_root = root.clone();
        let worker = thread::spawn(move || {
            run_fake(
                &executor,
                &worker_root,
                "cancel",
                false,
                sink.as_ref(),
                &worker_cancel,
            )
        });
        thread::sleep(Duration::from_millis(150));
        let started = Instant::now();
        cancellation.cancel();
        let error = worker
            .join()
            .expect("join fake git")
            .expect_err("cancel should fail");
        assert_eq!(error.kind, crate::git::GitErrorKind::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(20));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recording_sink_is_usable_from_trait_object() {
        let sink = Arc::new(RecordingSink(Arc::new(Mutex::new(Vec::new()))));
        let dynamic: &dyn GitEventSink = sink.as_ref();
        dynamic.emit(GitEvent::Progress {
            text: "Receiving objects: 50%".into(),
        });
        assert_eq!(sink.0.lock().unwrap().len(), 1);
    }
}
