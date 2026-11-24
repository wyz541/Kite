//! Commit workflow shared by the Git client and commit panels.
//!
//! The exact command runs through the same configured Git executable and
//! non-interactive environment as the rest of Kite's VCS integration. Git
//! itself owns hooks, signing, templates and credential configuration.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::vcs::configured_command;

pub const COMMIT_SUBJECT_WARNING_CHARS: usize = 72;
const COMMIT_RECENT_MESSAGE_LIMIT: usize = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitSigningMode {
    /// Follow `commit.gpgsign` and the rest of the user's Git configuration.
    #[default]
    GitConfig,
    /// Force a signed commit. Git uses the configured OpenPGP or SSH key.
    Enabled,
    /// Suppress signing even when `commit.gpgsign` is enabled.
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitOptions {
    /// A path inside the repository. It may point at the root, a directory, or
    /// a file.
    pub repository_path: PathBuf,
    pub message: String,
    pub amend: bool,
    pub signoff: bool,
    pub signing: GitSigningMode,
    pub skip_hooks: bool,
    pub author: Option<String>,
    pub push: bool,
}

/// Everything the commit panel needs before the user edits a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitState {
    pub repository_root: PathBuf,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub identity_configured: bool,
    pub staged_files: Vec<String>,
    pub staged_lines_added: u32,
    pub staged_lines_removed: u32,
    /// Exact diff against the amended commit's parent. Empty on an unborn
    /// branch. This includes both the previous commit and newly staged work.
    pub amend_files: Vec<String>,
    pub amend_lines_added: u32,
    pub amend_lines_removed: u32,
    pub unresolved_conflicts: Vec<String>,
    /// Contents of `commit.template`, if one is configured.
    pub commit_template: Option<String>,
    pub previous_message: Option<String>,
    pub recent_messages: Vec<String>,
    /// Whether Git is configured to sign commits by default.
    pub signing_default: bool,
    /// `gpg.format`; absent means Git's OpenPGP default.
    pub signing_format: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitResult {
    pub hash: String,
    pub short_hash: String,
    pub subject: String,
    pub amended: bool,
    pub signed: bool,
    pub pushed: bool,
    /// Git and hook output. Successful hooks may write here too.
    pub command_output: Option<String>,
    /// A push failure does not discard the successful commit result.
    pub push_error: Option<String>,
    pub push_output: Option<String>,
}

pub fn commit_subject_warning(message: &str) -> Option<String> {
    let subject = message.lines().next().unwrap_or_default().trim_end();
    if subject.is_empty() && !message.trim().is_empty() {
        return Some("提交标题为空；建议在第一行写一句简洁的摘要。".to_string());
    }
    let length = subject.chars().count();
    (length > COMMIT_SUBJECT_WARNING_CHARS).then(|| {
        format!("提交标题为 {length} 个字符；建议不超过 {COMMIT_SUBJECT_WARNING_CHARS} 个字符。")
    })
}

/// Reads the staged changes, identity, template, recent message history, and
/// signing defaults used by the commit panel.
pub fn git_commit_state(path: &Path) -> Result<GitCommitState, String> {
    let repository_root = resolve_repository_root(path)?;
    let branch = git_text_optional(&repository_root, &["symbolic-ref", "--short", "HEAD"])?;
    let head_short_hash = git_text_optional(&repository_root, &["rev-parse", "--short", "HEAD"])?;
    let upstream = git_text_optional(
        &repository_root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;

    let user_name = git_config_value(&repository_root, "user.name", false)?;
    let user_email = git_config_value(&repository_root, "user.email", false)?;
    let identity_configured = user_name
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && user_email
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());

    let staged_files = git_nul_paths(
        &repository_root,
        &["diff", "--cached", "--name-only", "-z", "--no-ext-diff"],
    )?;
    let unresolved_conflicts = git_nul_paths(
        &repository_root,
        &[
            "diff",
            "--name-only",
            "--diff-filter=U",
            "-z",
            "--no-ext-diff",
        ],
    )?;
    let staged_numstat = git_text_optional(
        &repository_root,
        &["diff", "--cached", "--numstat", "--no-ext-diff"],
    )?
    .unwrap_or_default();
    let (staged_lines_added, staged_lines_removed) = parse_numstat(&staged_numstat);
    let (amend_files, amend_lines_added, amend_lines_removed) = amend_index_diff(&repository_root)?;

    let commit_template =
        git_config_value(&repository_root, "commit.template", true)?.and_then(|template| {
            let template = PathBuf::from(template);
            let template = if template.is_absolute() {
                template
            } else {
                repository_root.join(template)
            };
            std::fs::read_to_string(template).ok()
        });

    let previous_message = git_text_optional(&repository_root, &["log", "-1", "--format=%B"])?
        .map(|message| message.trim_end().to_string())
        .filter(|message| !message.is_empty());
    let recent_limit = COMMIT_RECENT_MESSAGE_LIMIT.to_string();
    let recent_output = git_text_optional(
        &repository_root,
        &["log", "-n", &recent_limit, "--format=%B%x00"],
    )?
    .unwrap_or_default();
    let recent_messages = parse_recent_messages(&recent_output);

    let signing_default = git_config_value(&repository_root, "commit.gpgsign", false)?
        .as_deref()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "yes" | "on" | "1"
            )
        });
    let signing_format = git_config_value(&repository_root, "gpg.format", false)?;

    Ok(GitCommitState {
        repository_root,
        branch,
        head_short_hash,
        upstream,
        user_name,
        user_email,
        identity_configured,
        staged_files,
        staged_lines_added,
        staged_lines_removed,
        amend_files,
        amend_lines_added,
        amend_lines_removed,
        unresolved_conflicts,
        commit_template,
        previous_message,
        recent_messages,
        signing_default,
        signing_format,
    })
}

/// Creates a commit (optionally amending the previous one), runs hooks, and
/// optionally pushes it. A push failure is returned inside the successful
/// commit result so the UI does not lose the created commit.
pub fn commit(options: GitCommitOptions) -> Result<GitCommitResult, String> {
    let state = git_commit_state(&options.repository_path)?;
    let message = options.message.trim();
    if message.is_empty() {
        return Err("请输入提交说明".to_string());
    }
    if !state.identity_configured {
        return Err(
            "尚未配置 Git 用户身份。请先设置 user.name 和 user.email 后再提交。".to_string(),
        );
    }
    if !state.unresolved_conflicts.is_empty() {
        return Err(format!(
            "仍有未解决的冲突，无法提交：{}",
            state.unresolved_conflicts.join("、")
        ));
    }
    if options.amend && state.head_short_hash.is_none() {
        return Err("当前分支还没有提交，无法 amend。".to_string());
    }
    if !options.amend && state.staged_files.is_empty() {
        return Err("没有已暂存的文件。请先暂存要提交的改动。".to_string());
    }

    let root = &state.repository_root;
    let output = commit_command(root, &options)
        .output()
        .map_err(|error| format!("无法运行 git commit：{error}"))?;
    if !output.status.success() {
        return Err(commit_failure_error(&output));
    }

    let hash = git_text_required(root, &["rev-parse", "HEAD"])?;
    let short_hash = git_text_required(root, &["rev-parse", "--short", "HEAD"])?;
    let subject = git_text_optional(root, &["log", "-1", "--format=%s"])?
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut result = GitCommitResult {
        hash,
        short_hash,
        subject,
        amended: options.amend,
        signed: match options.signing {
            GitSigningMode::GitConfig => state.signing_default,
            GitSigningMode::Enabled => true,
            GitSigningMode::Disabled => false,
        },
        pushed: false,
        command_output: process_output_text(&output.stdout, &output.stderr),
        push_error: None,
        push_output: None,
    };

    if options.push {
        match push_current_branch(root, state.upstream.as_deref()) {
            Ok(output) => {
                result.pushed = true;
                result.push_output = process_output_text(&output.stdout, &output.stderr);
            }
            Err(error) => result.push_error = Some(error),
        }
    }

    Ok(result)
}

fn commit_command(root: &Path, options: &GitCommitOptions) -> Command {
    let mut command = git_command(root);
    if options.signing == GitSigningMode::Disabled {
        command.args(["-c", "commit.gpgsign=false"]);
    }
    command.arg("commit");
    if options.signing == GitSigningMode::Enabled {
        command.arg("-S");
    }
    if options.amend {
        command.arg("--amend");
    }
    if options.signoff {
        command.arg("--signoff");
    }
    if options.skip_hooks {
        command.arg("--no-verify");
    }
    if let Some(author) = options
        .author
        .as_deref()
        .map(str::trim)
        .filter(|author| !author.is_empty())
    {
        command.arg("--author").arg(author);
    }
    command.arg("-m").arg(options.message.trim());
    command
}

fn push_current_branch(root: &Path, upstream: Option<&str>) -> Result<Output, String> {
    let mut command = git_command(root);
    command.arg("push");
    if upstream.is_none() {
        let remotes = git_text_optional(root, &["remote"])?.unwrap_or_default();
        let mut names = remotes
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let remote = names
            .clone()
            .find(|name| *name == "origin")
            .or_else(|| names.next())
            .map(str::to_string)
            .ok_or_else(|| "没有可用的 Git 远程仓库，无法推送。".to_string())?;
        command.args(["-u", &remote, "HEAD"]);
    }
    let output = command
        .output()
        .map_err(|error| format!("无法运行 git push：{error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "提交已创建，但推送失败：{}",
            process_output_text(&output.stdout, &output.stderr)
                .unwrap_or_else(|| format!("退出码 {}", output.status.code().unwrap_or(-1)))
        ))
    }
}

fn resolve_repository_root(path: &Path) -> Result<PathBuf, String> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let output = run_git_required(start, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Err("所选项目不在 Git 工作区中".to_string());
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Err("Git 未返回工作区根目录".to_string());
    }
    Ok(dunce::canonicalize(&root).unwrap_or_else(|_| PathBuf::from(root)))
}

fn git_nul_paths(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let output = run_git_required(root, args)?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn amend_index_diff(root: &Path) -> Result<(Vec<String>, u32, u32), String> {
    if git_text_optional(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?.is_none() {
        return Ok((Vec::new(), 0, 0));
    }

    let base = if let Some(parent) =
        git_text_optional(root, &["rev-parse", "--verify", "--quiet", "HEAD^"])?
    {
        parent
    } else {
        // An amend of the root commit compares the index with an empty tree.
        git_text_required(root, &["hash-object", "-t", "tree", "--stdin"])?
    };
    let files = git_nul_paths(
        root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-ext-diff",
            &base,
        ],
    )?;
    let numstat = git_text_optional(
        root,
        &["diff", "--cached", "--numstat", "--no-ext-diff", &base],
    )?
    .unwrap_or_default();
    let (added, removed) = parse_numstat(&numstat);
    Ok((files, added, removed))
}

fn git_config_value(root: &Path, key: &str, path: bool) -> Result<Option<String>, String> {
    let mut args = vec!["config"];
    if path {
        args.push("--path");
    }
    args.extend(["--get", key]);
    let output = run_git_required(root, &args)?;
    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok((!value.is_empty()).then_some(value))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(output_error(&output))
    }
}

fn git_text_optional(root: &Path, args: &[&str]) -> Result<Option<String>, String> {
    let output = run_git_required(root, args)?;
    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok((!value.is_empty()).then_some(value))
    } else {
        Ok(None)
    }
}

fn git_text_required(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_required(root, args)?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Err("Git 未返回预期结果".to_string())
    } else {
        Ok(value)
    }
}

fn parse_recent_messages(output: &str) -> Vec<String> {
    let mut messages = Vec::new();
    for message in output.split('\0') {
        let message = message.trim();
        if message.is_empty() || messages.iter().any(|existing| existing == message) {
            continue;
        }
        messages.push(message.to_string());
        if messages.len() == COMMIT_RECENT_MESSAGE_LIMIT {
            break;
        }
    }
    messages
}

fn commit_failure_error(output: &Output) -> String {
    let details = process_output_text(&output.stdout, &output.stderr)
        .unwrap_or_else(|| format!("退出码 {}", output.status.code().unwrap_or(-1)));
    let lower = details.to_ascii_lowercase();
    let prefix = if [
        "pre-commit",
        "commit-msg",
        "prepare-commit-msg",
        "post-commit",
        "hook",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        "Git hook 拒绝了提交"
    } else if lower.contains("gpg") || lower.contains("signing") || lower.contains("ssh") {
        "签名提交失败"
    } else {
        "Git 提交失败"
    };
    format!("{prefix}：{details}")
}

fn process_output_text(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let text = match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => return None,
        (false, true) => stdout,
        (true, false) => stderr,
        (false, false) => format!("{stdout}\n{stderr}"),
    };
    Some(text.chars().take(16_384).collect())
}

fn parse_numstat(text: &str) -> (u32, u32) {
    text.lines().fold((0, 0), |(added, removed), line| {
        let mut fields = line.split('\t');
        // Binary files report "-" for both counts.
        let line_added = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let line_removed = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (added + line_added, removed + line_removed)
    })
}

fn run_git(current_dir: &Path, args: &[&str]) -> Option<Output> {
    git_command(current_dir).args(args).output().ok()
}

fn run_git_required(current_dir: &Path, args: &[&str]) -> Result<Output, String> {
    run_git(current_dir, args)
        .ok_or_else(|| "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string())
}

fn git_command(current_dir: &Path) -> Command {
    let mut command = configured_command("git", current_dir);
    command.stdin(Stdio::null()).env("LC_ALL", "C");
    command
}

fn output_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!(
            "Git 操作失败，退出码 {}",
            output.status.code().unwrap_or(-1)
        )
    } else {
        format!(
            "Git 操作失败：{}",
            stderr
                .replace(['\r', '\n'], " ")
                .chars()
                .take(300)
                .collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = git_output(root, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(root: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git")
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-git-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo root");
        git_ok(&root, &["init", "--quiet", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "kite@example.com"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "core.autocrlf", "false"]);
        git_ok(&root, &["config", "commit.gpgsign", "false"]);
        root
    }

    fn commit_options(root: &Path, message: &str) -> GitCommitOptions {
        GitCommitOptions {
            repository_path: root.to_path_buf(),
            message: message.to_string(),
            amend: false,
            signoff: false,
            signing: GitSigningMode::GitConfig,
            skip_hooks: false,
            author: None,
            push: false,
        }
    }

    fn stage_new_file(root: &Path, name: &str, contents: &str) {
        std::fs::write(root.join(name), contents).expect("write staged file");
        git_ok(root, &["add", name]);
    }

    #[test]
    fn commit_subject_warning_is_non_blocking_and_unicode_aware() {
        assert!(commit_subject_warning("short subject").is_none());
        assert!(commit_subject_warning(&"长".repeat(COMMIT_SUBJECT_WARNING_CHARS + 1)).is_some());
        assert!(commit_subject_warning("\nbody without a subject").is_some());
    }

    #[test]
    fn commit_state_loads_template_identity_staged_files_and_recent_messages() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-state");
        let template_path = root.join("message-template.txt");
        std::fs::write(&template_path, "Template subject\n\nTemplate body\n")
            .expect("write template");
        git_ok(
            &root,
            &["config", "commit.template", "message-template.txt"],
        );
        stage_new_file(&root, "first.txt", "one\n");
        let first = commit(commit_options(&root, "Initial subject")).expect("initial commit");
        stage_new_file(&root, "second.txt", "two\n");

        let state = git_commit_state(&root).expect("commit state");
        assert_eq!(state.repository_root, dunce::canonicalize(&root).unwrap());
        assert_eq!(state.user_name.as_deref(), Some("Kite Test"));
        assert_eq!(state.user_email.as_deref(), Some("kite@example.com"));
        assert!(state.identity_configured);
        assert_eq!(state.staged_files, vec!["second.txt"]);
        assert_eq!(
            (state.staged_lines_added, state.staged_lines_removed),
            (1, 0)
        );
        assert_eq!(
            state.commit_template.as_deref(),
            Some("Template subject\n\nTemplate body\n")
        );
        assert_eq!(state.previous_message.as_deref(), Some("Initial subject"));
        assert!(
            state
                .recent_messages
                .iter()
                .any(|message| message == "Initial subject")
        );
        assert!(!first.hash.is_empty());
        assert!(first.hash.starts_with(&first.short_hash));
        assert!(first.short_hash.len() >= 7);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn commit_supports_signoff_and_author_override() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-signoff");
        stage_new_file(&root, "signed.txt", "signed\n");
        let mut options = commit_options(&root, "Signed contribution");
        options.signoff = true;
        options.author = Some("Contributor <contributor@example.com>".to_string());
        let result = commit(options).expect("commit");

        let author =
            String::from_utf8_lossy(&git_output(&root, &["log", "-1", "--format=%an"]).stdout)
                .trim()
                .to_string();
        let body =
            String::from_utf8_lossy(&git_output(&root, &["log", "-1", "--format=%B"]).stdout)
                .to_string();
        assert_eq!(author, "Contributor");
        assert!(body.contains("Signed-off-by: Kite Test <kite@example.com>"));
        assert_eq!(result.subject, "Signed contribution");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn amend_replaces_the_previous_commit_and_keeps_one_commit() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-amend");
        stage_new_file(&root, "amend.txt", "first\n");
        let initial = commit(commit_options(&root, "First message")).expect("initial commit");
        stage_new_file(&root, "amend.txt", "second\n");
        let state = git_commit_state(&root).expect("amend state");
        assert_eq!(state.amend_files, vec!["amend.txt"]);
        assert_eq!((state.amend_lines_added, state.amend_lines_removed), (1, 0));

        let mut options = commit_options(&root, "Amended message");
        options.amend = true;
        let amended = commit(options).expect("amend commit");
        let count =
            String::from_utf8_lossy(&git_output(&root, &["rev-list", "--count", "HEAD"]).stdout)
                .trim()
                .to_string();

        assert_ne!(initial.hash, amended.hash);
        assert_eq!(count, "1");
        assert_eq!(amended.subject, "Amended message");
        assert!(amended.amended);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn amend_is_rejected_before_the_first_commit() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-amend-unborn");
        stage_new_file(&root, "first.txt", "first\n");
        let mut options = commit_options(&root, "No previous commit");
        options.amend = true;

        let error = commit(options).expect_err("amend requires a commit");
        assert!(error.contains("还没有提交"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skip_hooks_bypasses_a_failing_pre_commit_hook() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-hooks");
        let hooks = root.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).expect("create hooks directory");
        let hook = hooks.join("pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'policy rejected the commit' >&2\nexit 1\n",
        )
        .expect("write hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&hook)
                .expect("hook metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&hook, permissions).expect("make hook executable");
        }
        stage_new_file(&root, "hooked.txt", "hooked\n");

        let error = commit(commit_options(&root, "Blocked by hook"))
            .expect_err("pre-commit hook must block the commit");
        assert!(error.contains("hook") || error.contains("policy rejected"));

        let mut options = commit_options(&root, "Bypass hook");
        options.skip_hooks = true;
        let result = commit(options).expect("--no-verify should bypass hook");
        assert_eq!(result.subject, "Bypass hook");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn signing_modes_build_the_expected_git_arguments() {
        let root = Path::new(".");
        let mut options = commit_options(root, "message");
        let mut args = |signing| {
            options.signing = signing;
            commit_command(root, &options)
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };

        let from_config = args(GitSigningMode::GitConfig);
        assert!(!from_config.iter().any(|arg| arg == "-S"));
        assert!(!from_config.iter().any(|arg| arg == "commit.gpgsign=false"));

        let enabled = args(GitSigningMode::Enabled);
        assert!(enabled.iter().any(|arg| arg == "-S"));

        let disabled = args(GitSigningMode::Disabled);
        assert!(
            disabled
                .windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == "commit.gpgsign=false")
        );
    }

    #[test]
    fn enabled_signing_reports_a_signing_failure() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-signing-enabled");
        git_ok(&root, &["config", "gpg.format", "openpgp"]);
        git_ok(
            &root,
            &["config", "gpg.program", "kite-missing-gpg-program"],
        );
        stage_new_file(&root, "requires-signature.txt", "signed\n");

        let mut options = commit_options(&root, "Signed commit");
        options.signing = GitSigningMode::Enabled;
        let error = commit(options).expect_err("missing signer should fail the commit");
        let lower = error.to_ascii_lowercase();
        assert!(
            error.contains("签名") || lower.contains("gpg") || lower.contains("sign"),
            "{error}"
        );
        assert!(
            !git_output(&root, &["rev-parse", "--verify", "--quiet", "HEAD"])
                .status
                .success()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn disabled_signing_overrides_commit_gpgsign_config() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-signing-disabled");
        git_ok(&root, &["config", "commit.gpgsign", "true"]);
        git_ok(
            &root,
            &["config", "gpg.program", "kite-missing-gpg-program"],
        );
        stage_new_file(&root, "unsigned.txt", "unsigned\n");

        let mut options = commit_options(&root, "Unsigned override");
        options.signing = GitSigningMode::Disabled;
        let result = commit(options).expect("disabled signing should override Git config");
        assert!(!result.signed);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn identity_and_unresolved_conflicts_block_commits() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-blockers");
        stage_new_file(&root, "identity.txt", "identity\n");
        git_ok(&root, &["config", "user.name", ""]);
        git_ok(&root, &["config", "user.email", ""]);
        let error = commit(commit_options(&root, "No identity")).expect_err("identity required");
        assert!(error.contains("身份"));

        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "user.email", "kite@example.com"]);
        std::fs::write(root.join("identity.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "identity.txt"]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "other"]);
        std::fs::write(root.join("identity.txt"), "other\n").expect("write other");
        git_ok(&root, &["commit", "--quiet", "-am", "other"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        std::fs::write(root.join("identity.txt"), "main\n").expect("write main");
        git_ok(&root, &["commit", "--quiet", "-am", "main"]);
        let merge = git_output(&root, &["merge", "other"]);
        assert!(!merge.status.success(), "merge should conflict");

        let error = commit(commit_options(&root, "Resolve conflict")).expect_err("conflicts block");
        assert!(error.contains("冲突"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn commit_and_push_updates_a_bare_remote() {
        if !git_available() {
            return;
        }
        let root = temp_repo("commit-push");
        let remote = std::env::temp_dir().join(format!(
            "kite-git-remote-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let remote_output = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&remote)
            .output()
            .expect("create bare remote");
        assert!(remote_output.status.success());
        let remote_arg = remote.to_string_lossy().to_string();
        git_ok(&root, &["remote", "add", "origin", &remote_arg]);
        stage_new_file(&root, "pushed.txt", "push me\n");

        let mut options = commit_options(&root, "Commit and push");
        options.push = true;
        let result = commit(options).expect("commit and push");
        assert!(result.pushed);
        assert!(result.push_error.is_none());
        let remote_head = Command::new("git")
            .args(["--git-dir"])
            .arg(&remote)
            .args(["rev-parse", "refs/heads/main"])
            .output()
            .expect("read remote ref");
        assert_eq!(
            String::from_utf8_lossy(&remote_head.stdout).trim(),
            result.hash
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(remote);
    }
}
