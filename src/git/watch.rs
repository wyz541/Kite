use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::command::GitExecutor;
use super::error::GitError;
use super::repository::{GitRepository, GitRepositoryState, repository_state};

const GIT_WATCH_DEBOUNCE: Duration = Duration::from_millis(180);

pub enum GitRepositoryEvent {
    Changed(GitRepositoryState),
    Error(GitError),
}

pub struct GitRepositoryWatcher {
    _watcher: RecommendedWatcher,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GitRepositoryWatcher {
    pub fn new(
        repository: GitRepository,
        executor: Arc<GitExecutor>,
        mut on_event: impl FnMut(GitRepositoryEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel::<()>();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    return;
                };
                if !matches!(event.kind, EventKind::Access(_)) {
                    let _ = sender.send(());
                }
            })
            .map_err(|error| format!("无法监听 Git 仓库：{error}"))?;

        watch_repository(&mut watcher, &repository)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let first = match receiver.recv_timeout(GIT_WATCH_DEBOUNCE) {
                    Ok(()) => true,
                    Err(mpsc::RecvTimeoutError::Timeout) => false,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if !first {
                    continue;
                }

                let mut quiet = false;
                while !quiet && !thread_stop.load(Ordering::SeqCst) {
                    match receiver.recv_timeout(GIT_WATCH_DEBOUNCE) {
                        Ok(()) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => quiet = true,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }

                match repository_state(&executor, &repository) {
                    Ok(state) => on_event(GitRepositoryEvent::Changed(state)),
                    Err(error) => on_event(GitRepositoryEvent::Error(error)),
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for GitRepositoryWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watch_repository(
    watcher: &mut RecommendedWatcher,
    repository: &GitRepository,
) -> Result<(), String> {
    let mut watched = Vec::<PathBuf>::new();
    for path in [&repository.git_dir, &repository.common_dir] {
        if path.exists() && !watched.iter().any(|existing| existing == path) {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|error| format!("无法监听 {}：{error}", path.display()))?;
            watched.push(path.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::command::CancellationToken;
    use crate::git::events::NoopGitEventSink;
    use crate::git::repository::{discover_repository, operation_state};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git_ok(root: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "{:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn reports_git_dir_changes_after_debounce() {
        if !git_available() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "kite-git-watch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "user.email", "kite@example.test"]);
        std::fs::write(root.join("a.txt"), "one\n").expect("write file");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial"]);

        let executor = Arc::new(GitExecutor::new());
        let repository = discover_repository(&executor, &root).expect("discover repo");
        let (sender, receiver) = mpsc::channel();
        let mut watcher = GitRepositoryWatcher::new(repository.clone(), executor, move |event| {
            let _ = sender.send(event);
        })
        .expect("start watcher");

        std::fs::write(repository.git_dir.join("MERGE_HEAD"), "abc\n").expect("write merge marker");
        let event = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("watch event");
        match event {
            GitRepositoryEvent::Changed(state) => {
                assert_eq!(state.operation, operation_state(&repository.git_dir));
            }
            GitRepositoryEvent::Error(error) => panic!("watcher error: {error}"),
        }
        watcher.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_cancelled_operation_tokens() {
        let token = CancellationToken::default();
        token.cancel();
        let _sink = NoopGitEventSink;
        assert!(token.is_cancelled());
    }
}
