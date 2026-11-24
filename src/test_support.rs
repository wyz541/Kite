use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// A uniquely named temporary directory that removes itself when the test ends.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(label: &str) -> std::io::Result<Self> {
        let label = label
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kite-{label}-{}-{nanos}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path.join(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub(crate) fn git_output(root: &Path, args: &[&str]) -> std::io::Result<Output> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("LC_ALL", "C")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.output()
}

pub(crate) fn git_ok(root: &Path, args: &[&str]) -> Output {
    let output = git_output(root, args).expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub(crate) fn configure_repo(root: &Path) {
    git_ok(root, &["config", "user.name", "Kite Test"]);
    git_ok(root, &["config", "user.email", "kite@example.invalid"]);
    git_ok(root, &["config", "commit.gpgsign", "false"]);
    git_ok(root, &["config", "core.autocrlf", "false"]);
}

pub(crate) fn init_repo(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    git_ok(root, &["init", "--quiet", "-b", "main"]);
    configure_repo(root);
    Ok(())
}

pub(crate) fn init_bare_repo(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let path_text = path.to_str().expect("UTF-8 fixture path");
    git_ok(
        parent,
        &["init", "--bare", "--quiet", "-b", "main", path_text],
    );
    Ok(())
}

pub(crate) fn write_file(root: &Path, relative: impl AsRef<Path>, contents: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, contents).expect("write fixture file");
}

pub(crate) fn commit_all(root: &Path, message: &str) -> String {
    git_ok(root, &["add", "-A"]);
    git_ok(root, &["commit", "--quiet", "-m", message]);
    head(root)
}

pub(crate) fn head(root: &Path) -> String {
    String::from_utf8_lossy(&git_ok(root, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// A loopback HTTP endpoint that answers Git's first request with `401` and
/// never asks for credentials. Tests use this to exercise authentication
/// failure without contacting a real remote.
pub(crate) struct UnauthorizedHttpServer {
    address: SocketAddr,
    handle: Option<JoinHandle<()>>,
}

impl UnauthorizedHttpServer {
    pub(crate) fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let handle = thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let response = concat!(
                "HTTP/1.1 401 Unauthorized\r\n",
                "WWW-Authenticate: Basic realm=\"kite-test\"\r\n",
                "Content-Length: 0\r\n",
                "Connection: close\r\n",
                "\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
        });
        Ok(Self {
            address,
            handle: Some(handle),
        })
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }
}

impl Drop for UnauthorizedHttpServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = TcpStream::connect(self.address);
            let _ = handle.join();
        }
    }
}
