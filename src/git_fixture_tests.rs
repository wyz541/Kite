#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
#[cfg(any(unix, windows))]
use std::path::PathBuf;

use crate::fs::VcsStatus;
use crate::git::{
    CancellationToken, GitErrorKind, GitOperation, GitService, NoopGitEventSink, git_path_summary,
};
use crate::test_support::{
    TempDir, UnauthorizedHttpServer, commit_all, configure_repo, git_available, git_ok, git_output,
    init_bare_repo, init_repo, write_file,
};
use crate::vcs::{VcsQuery, VcsTool, load_vcs_snapshot, run_vcs_query};

fn git_or_skip(label: &str) -> TempDir {
    assert!(git_available(), "Git is required for {label}");
    TempDir::new(label).expect("create temporary fixture")
}

#[test]
fn merge_history_and_rename_are_visible() {
    let temp = git_or_skip("git-merge-rename");
    let root = temp.path();
    init_repo(root).expect("init repository");
    write_file(root, "base.txt", b"base\n");
    commit_all(root, "base");

    git_ok(root, &["switch", "--quiet", "-c", "feature"]);
    write_file(root, "feature.txt", b"feature\n");
    commit_all(root, "feature");

    git_ok(root, &["switch", "--quiet", "main"]);
    write_file(root, "main.txt", b"main\n");
    commit_all(root, "main");
    git_ok(
        root,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "feature",
            "-m",
            "merge feature",
        ],
    );

    let log = run_vcs_query(VcsTool::Git, VcsQuery::Log, root).expect("read log");
    assert!(log.contains("merge feature"));
    assert!(log.contains("feature"));

    fs::rename(root.join("base.txt"), root.join("renamed.txt")).expect("rename fixture");
    git_ok(root, &["add", "-A"]);
    let snapshot = load_vcs_snapshot(root);
    assert!(
        snapshot.git_entries().any(|(path, status)| {
            path.ends_with("renamed.txt") && status == VcsStatus::Renamed
        })
    );
}

#[test]
fn conflicted_merge_fixture_exposes_operation_and_conflict() {
    let temp = git_or_skip("git-conflict");
    let root = temp.path();
    init_repo(root).expect("init repository");
    write_file(root, "conflict.txt", b"base\n");
    commit_all(root, "base");

    git_ok(root, &["switch", "--quiet", "-c", "feature"]);
    write_file(root, "conflict.txt", b"feature\n");
    commit_all(root, "feature edit");

    git_ok(root, &["switch", "--quiet", "main"]);
    write_file(root, "conflict.txt", b"main\n");
    commit_all(root, "main edit");

    let output = git_output(root, &["merge", "feature"]).expect("run conflicting merge");
    assert!(
        !output.status.success(),
        "merge should stop with a conflict"
    );

    let summary = git_path_summary(&root.join("conflict.txt"))
        .expect("read path summary")
        .expect("file is inside repository");
    assert_eq!(
        summary.operation,
        Some(crate::git::preview::GitOperationState::Merging)
    );
    assert_eq!(summary.staged_status, Some(VcsStatus::Conflicted));

    let snapshot = load_vcs_snapshot(root);
    assert!(
        snapshot
            .git_entries()
            .any(|(path, status)| path.ends_with("conflict.txt")
                && status == VcsStatus::Conflicted)
    );
}

#[test]
fn submodule_fixture_reports_dirty_submodule() {
    let temp = git_or_skip("git-submodule");
    let child = temp.join("child");
    init_repo(&child).expect("init child repository");
    write_file(&child, "child.txt", b"base\n");
    commit_all(&child, "child base");

    let parent = temp.join("parent");
    init_repo(&parent).expect("init parent repository");
    write_file(&parent, "parent.txt", b"base\n");
    commit_all(&parent, "parent base");
    let child_text = child.to_str().expect("UTF-8 child path");
    git_ok(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            child_text,
            "modules/child",
        ],
    );
    commit_all(&parent, "add submodule");

    write_file(&parent, "modules/child/child.txt", b"dirty\n");
    let snapshot = load_vcs_snapshot(&parent);
    assert!(snapshot.git_entries().any(|(path, status)| {
        path.ends_with("modules/child") && status == VcsStatus::Modified
    }));
}

#[test]
fn linked_worktree_fixture_uses_the_linked_root() {
    let temp = git_or_skip("git-worktree");
    let root = temp.join("main");
    init_repo(&root).expect("init repository");
    write_file(&root, "tracked.txt", b"base\n");
    commit_all(&root, "base");

    let linked = temp.join("linked");
    let linked_text = linked.to_str().expect("UTF-8 worktree path");
    git_ok(
        &root,
        &["worktree", "add", "--quiet", "-b", "linked", linked_text],
    );
    write_file(&linked, "tracked.txt", b"changed\n");

    let summary = git_path_summary(&linked.join("tracked.txt"))
        .expect("read linked worktree summary")
        .expect("linked worktree is inside repository");
    assert_eq!(summary.branch.as_deref(), Some("linked"));
    assert_eq!(summary.unstaged_status, Some(VcsStatus::Modified));
    let linked_root = dunce::canonicalize(&linked).expect("canonical worktree root");
    assert_eq!(summary.repository_root, linked_root);
}

#[test]
fn lfs_pointer_is_tracked_as_a_regular_file() {
    let temp = git_or_skip("git-lfs-pointer");
    let root = temp.path();
    init_repo(root).expect("init repository");
    let pointer = concat!(
        "version https://git-lfs.github.com/spec/v1\n",
        "oid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        "size 12\n"
    );
    write_file(root, "asset.bin", pointer.as_bytes());
    commit_all(root, "add pointer");
    write_file(
        root,
        "asset.bin",
        pointer.replace("size 12", "size 13").as_bytes(),
    );

    let summary = git_path_summary(&root.join("asset.bin"))
        .expect("read pointer summary")
        .expect("pointer is inside repository");
    assert_eq!(summary.unstaged_status, Some(VcsStatus::Modified));
    assert_eq!(summary.lines_added, 1);
    assert_eq!(summary.lines_removed, 1);
}

#[cfg(unix)]
#[test]
fn non_utf8_filename_can_be_created_and_reported() {
    use std::os::unix::ffi::OsStringExt;

    let temp = git_or_skip("git-non-utf8");
    let root = temp.path();
    init_repo(root).expect("init repository");
    let name = OsString::from_vec(b"invalid-\xff.txt".to_vec());
    let path = root.join(PathBuf::from(name));
    if let Err(error) = fs::write(&path, b"bytes\n") {
        // APFS rejects some non-UTF-8 names before Git can observe them.
        eprintln!("skipping non-UTF-8 fixture on this filesystem: {error}");
        return;
    }

    let snapshot = load_vcs_snapshot(root);
    let statuses = snapshot.git_entries().collect::<Vec<_>>();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].1, VcsStatus::Untracked);
}

#[cfg(windows)]
#[test]
fn windows_long_path_can_be_tracked_and_modified() {
    let temp = git_or_skip("git-windows-long-path");
    let root = temp.path();
    init_repo(root).expect("init repository");
    git_ok(root, &["config", "core.longpaths", "true"]);

    let mut relative = PathBuf::new();
    for index in 0..12 {
        relative.push(format!("segment-{index:02}-0123456789"));
    }
    relative.push("tracked.txt");
    let full = root.join(&relative);
    assert!(full.to_string_lossy().chars().count() > 260);
    let extended = PathBuf::from(format!(r"\\?\{}", full.display()));
    if let Some(parent) = extended.parent() {
        fs::create_dir_all(parent).expect("create long path parent");
    }
    fs::write(&extended, b"base\n").expect("write long path fixture");
    commit_all(root, "long path base");
    fs::write(&extended, b"changed\n").expect("modify long path fixture");

    let snapshot = load_vcs_snapshot(root);
    assert!(
        snapshot.git_entries().any(|(path, status)| {
            path.ends_with("tracked.txt") && status == VcsStatus::Modified
        })
    );
}

#[test]
fn local_bare_remote_reports_non_fast_forward_rejection() {
    let temp = git_or_skip("git-push-rejected");
    let remote = temp.join("remote.git");
    init_bare_repo(&remote).expect("init bare remote");

    let seed = temp.join("seed");
    init_repo(&seed).expect("init seed repository");
    write_file(&seed, "tracked.txt", b"base\n");
    commit_all(&seed, "base");
    let remote_text = remote.to_str().expect("UTF-8 remote path");
    git_ok(&seed, &["remote", "add", "origin", remote_text]);
    git_ok(&seed, &["push", "--quiet", "-u", "origin", "main"]);

    let clone_a = temp.join("clone-a");
    let clone_b = temp.join("clone-b");
    let clone_a_text = clone_a.to_str().expect("UTF-8 clone path");
    let clone_b_text = clone_b.to_str().expect("UTF-8 clone path");
    git_ok(
        temp.path(),
        &[
            "clone",
            "--quiet",
            "--branch",
            "main",
            remote_text,
            clone_a_text,
        ],
    );
    git_ok(
        temp.path(),
        &[
            "clone",
            "--quiet",
            "--branch",
            "main",
            remote_text,
            clone_b_text,
        ],
    );
    configure_repo(&clone_a);
    configure_repo(&clone_b);

    write_file(&clone_a, "tracked.txt", b"from a\n");
    commit_all(&clone_a, "clone a");
    git_ok(&clone_a, &["push", "--quiet"]);

    write_file(&clone_b, "tracked.txt", b"from b\n");
    commit_all(&clone_b, "clone b");
    let error = GitService::shared()
        .run_operation(
            &clone_b,
            GitOperation::Push,
            &CancellationToken::default(),
            &NoopGitEventSink,
            None,
        )
        .expect_err("non-fast-forward push must fail");
    assert_eq!(error.kind, GitErrorKind::Rejected);
}

#[test]
fn local_http_401_reports_authentication_failure() {
    let temp = git_or_skip("git-auth-failure");
    let root = temp.path();
    init_repo(root).expect("init repository");
    write_file(root, "tracked.txt", b"base\n");
    commit_all(root, "base");

    let server = UnauthorizedHttpServer::start().expect("start local HTTP server");
    let remote_url = server.url("/repository.git");
    git_ok(root, &["remote", "add", "origin", &remote_url]);
    git_ok(root, &["config", "branch.main.remote", "origin"]);
    git_ok(root, &["config", "branch.main.merge", "refs/heads/main"]);

    let error = GitService::shared()
        .run_operation(
            root,
            GitOperation::Push,
            &CancellationToken::default(),
            &NoopGitEventSink,
            None,
        )
        .expect_err("push against 401 endpoint must fail");
    assert_eq!(error.kind, GitErrorKind::Authentication, "{error:?}");
}

#[test]
fn repeated_file_commits_cover_log_first_screen() {
    let temp = git_or_skip("git-log-history");
    let root = temp.path();
    init_repo(root).expect("init repository");
    write_file(root, "tracked.txt", b"base\n");
    commit_all(root, "base");
    for index in 0..99 {
        write_file(
            root,
            "tracked.txt",
            format!("history line {index:02}\n").as_bytes(),
        );
        commit_all(root, &format!("history {index:02}"));
    }

    let log = run_vcs_query(VcsTool::Git, VcsQuery::Log, root).expect("read log first screen");
    assert_eq!(log.lines().count(), 100);
}
