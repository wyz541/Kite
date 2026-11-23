use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::GitService;
use super::preview::run_git;

pub const DEFAULT_BLAME_PAGE_LINES: u32 = 400;
pub const MAX_BLAME_PAGE_LINES: u32 = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBlameLine {
    pub line_number: u32,
    pub commit_hash: String,
    pub short_hash: String,
    pub author: String,
    pub author_mail: Option<String>,
    pub author_time: i64,
    pub committer: String,
    pub committer_mail: Option<String>,
    pub committer_time: i64,
    pub summary: String,
    pub filename: Option<String>,
    pub previous: Option<(String, u32)>,
    pub is_uncommitted: bool,
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBlamePage {
    pub repository_root: PathBuf,
    pub relative_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub total_lines: u32,
    pub ignore_whitespace: bool,
    pub lines: Vec<GitBlameLine>,
}

/// Returns one bounded page of line attribution. Paging keeps the first screen
/// responsive on large files while preserving a single shared implementation.
pub fn git_blame(
    path: &Path,
    start_line: u32,
    line_count: u32,
    ignore_whitespace: bool,
) -> Result<GitBlamePage, String> {
    if !path.is_file() {
        return Err("目标不是文件".to_string());
    }
    let start_line = start_line.max(1);
    let line_count = line_count.clamp(1, MAX_BLAME_PAGE_LINES);
    let start = path.parent().unwrap_or(path);
    let repository = GitService::shared()
        .discover_repository(start)
        .map_err(|error| error.to_string())?;
    let relative = repository
        .relative_path(path)
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let root = repository.work_tree;
    let total_lines = file_line_count(path)?;
    if total_lines == 0 || start_line > total_lines {
        return Ok(GitBlamePage {
            repository_root: root,
            relative_path: relative,
            start_line,
            end_line: 0,
            total_lines,
            ignore_whitespace,
            lines: Vec::new(),
        });
    }
    let end_line = start_line
        .saturating_add(line_count - 1)
        .min(total_lines.max(1));
    let range = format!("{start_line},{end_line}");

    let mut args = vec!["blame", "--line-porcelain"];
    if ignore_whitespace {
        args.push("-w");
    }
    args.extend(["-L", range.as_str(), "--", relative.as_str()]);
    let output = run_git(&root, &args).map_err(|error| error.to_string())?;

    Ok(GitBlamePage {
        repository_root: root,
        relative_path: relative,
        start_line,
        end_line,
        total_lines,
        ignore_whitespace,
        lines: parse_blame_porcelain(&output.stdout),
    })
}

fn file_line_count(path: &Path) -> Result<u32, String> {
    let file = File::open(path).map_err(|error| format!("无法读取文件行数：{error}"))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut lines = 0u32;
    let mut saw_bytes = false;
    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| format!("无法读取文件行数：{error}"))?;
        if read == 0 {
            break;
        }
        saw_bytes = true;
        lines = lines.saturating_add(1);
    }
    Ok(if saw_bytes { lines } else { 0 })
}

#[derive(Default)]
struct BlameBlock {
    hash: String,
    original_line: u32,
    final_line: u32,
    author: String,
    author_mail: Option<String>,
    author_time: i64,
    committer: String,
    committer_mail: Option<String>,
    committer_time: i64,
    summary: String,
    filename: Option<String>,
    previous: Option<(String, u32)>,
}

fn parse_blame_porcelain(text: &str) -> Vec<GitBlameLine> {
    let mut output = Vec::new();
    let mut lines = text.lines();
    while let Some(header) = lines.next() {
        let mut header_parts = header.split_whitespace();
        let Some(hash) = header_parts.next() else {
            continue;
        };
        let original_line = header_parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let final_line = header_parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut block = BlameBlock {
            hash: hash.trim_start_matches('^').to_string(),
            original_line,
            final_line,
            ..Default::default()
        };

        let code = loop {
            let Some(value) = lines.next() else {
                break None;
            };
            if let Some(code) = value.strip_prefix('\t') {
                break Some(code.to_string());
            }
            let (key, value) = value.split_once(' ').unwrap_or((value, ""));
            match key {
                "author" => block.author = value.to_string(),
                "author-mail" => block.author_mail = Some(trim_angle_mail(value)),
                "author-time" => block.author_time = value.parse().unwrap_or(0),
                "committer" => block.committer = value.to_string(),
                "committer-mail" => block.committer_mail = Some(trim_angle_mail(value)),
                "committer-time" => block.committer_time = value.parse().unwrap_or(0),
                "summary" => block.summary = value.to_string(),
                "filename" => block.filename = Some(value.to_string()),
                "previous" => {
                    let mut fields = value.split_whitespace();
                    if let (Some(hash), Some(line)) = (fields.next(), fields.next())
                        && let Ok(line) = line.parse()
                    {
                        block.previous = Some((hash.to_string(), line));
                    }
                }
                _ => {}
            }
        };

        let Some(code) = code else {
            continue;
        };
        let is_uncommitted = block.hash.chars().all(|character| character == '0')
            || block.author == "Not Committed Yet";
        output.push(GitBlameLine {
            line_number: if block.final_line == 0 {
                block.original_line
            } else {
                block.final_line
            },
            short_hash: if is_uncommitted {
                "未提交".to_string()
            } else {
                block.hash.chars().take(7).collect()
            },
            commit_hash: block.hash,
            author: block.author,
            author_mail: block.author_mail,
            author_time: block.author_time,
            committer: block.committer,
            committer_mail: block.committer_mail,
            committer_time: block.committer_time,
            summary: block.summary,
            filename: block.filename,
            previous: block.previous,
            is_uncommitted,
            code,
        });
    }
    output
}

fn trim_angle_mail(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-git-blame-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo root");
        git_ok(&root, &["init", "--quiet", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "kite@example.com"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        root
    }

    fn commit_file(root: &Path, lines: &[u8], message: &str) -> String {
        std::fs::write(root.join("sample.txt"), lines).expect("write fixture");
        git_ok(root, &["add", "sample.txt"]);
        git_ok(root, &["commit", "--quiet", "-m", message]);
        let output = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("resolve HEAD");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn parses_line_porcelain_and_preserves_line_numbers() {
        let text = "\
aaaaaaaaaaaaaaaa 1 1 1
author Ada Lovelace
author-mail <ada@example.com>
author-time 1700000000
committer Ada Lovelace
committer-mail <ada@example.com>
committer-time 1700000001
summary Initial
filename sample.txt
\tfn main() {
bbbbbbbbbbbbbbbb 2 2 1
author Grace Hopper
author-time 1700000100
committer Grace Hopper
committer-time 1700000101
summary Add print
filename sample.txt
previous bbbbbbbbbbbbbbbb 1
\tprintln!(\"hi\");
";
        let parsed = parse_blame_porcelain(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].line_number, 1);
        assert_eq!(parsed[0].short_hash, "aaaaaaa");
        assert_eq!(parsed[0].author, "Ada Lovelace");
        assert_eq!(parsed[0].code, "fn main() {");
        assert_eq!(parsed[1].line_number, 2);
        assert_eq!(parsed[1].previous.as_ref().unwrap().1, 1);
    }

    #[test]
    fn pages_blame_and_ignores_whitespace_using_git_config() {
        if !git_available() {
            return;
        }
        let root = temp_repo("ignore-whitespace");
        let original = commit_file(&root, b"one\ntwo\nthree\n", "initial");
        let whitespace = commit_file(&root, b"one\n  two\nthree\n", "whitespace only");
        assert_ne!(original, whitespace);
        std::fs::write(
            root.join(".git-blame-ignore-revs"),
            format!("{whitespace}\n"),
        )
        .expect("write ignore revs");
        git_ok(&root, &["add", ".git-blame-ignore-revs"]);
        git_ok(
            &root,
            &["commit", "--quiet", "-m", "configure blame ignore"],
        );
        git_ok(
            &root,
            &["config", "blame.ignoreRevsFile", ".git-blame-ignore-revs"],
        );

        let page = git_blame(&root.join("sample.txt"), 2, 1, true).expect("blame page");
        assert_eq!(page.total_lines, 3);
        assert_eq!(page.start_line, 2);
        assert_eq!(page.end_line, 2);
        assert_eq!(page.lines.len(), 1);
        assert_eq!(page.lines[0].author, "Kite Test");
        assert!(!page.lines[0].is_uncommitted);
        assert_eq!(page.lines[0].commit_hash, original);
        assert!(page.ignore_whitespace);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reports_uncommitted_lines_and_caps_page_size() {
        if !git_available() {
            return;
        }
        let root = temp_repo("uncommitted");
        commit_file(&root, b"one\n", "initial");
        std::fs::write(root.join("sample.txt"), b"one\nlocal edit\n").expect("edit fixture");
        let page = git_blame(
            &root.join("sample.txt"),
            1,
            MAX_BLAME_PAGE_LINES + 100,
            false,
        )
        .expect("blame page");
        assert_eq!(page.lines.len(), 2);
        assert!(page.lines[1].is_uncommitted);
        assert_eq!(page.lines[1].short_hash, "未提交");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handles_empty_files_without_running_git_blame() {
        if !git_available() {
            return;
        }
        let root = temp_repo("empty");
        commit_file(&root, b"", "empty fixture");
        let page = git_blame(&root.join("sample.txt"), 1, 400, false).expect("empty page");
        assert_eq!(page.total_lines, 0);
        assert!(page.lines.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "performance baseline; run explicitly with --ignored --nocapture"]
    fn benchmark_ten_thousand_line_first_page() {
        if !git_available() {
            return;
        }
        let root = temp_repo("perf");
        let mut content = String::with_capacity(10_000 * 18);
        for index in 0..10_000 {
            content.push_str(&format!("line {index:05} value\n"));
        }
        commit_file(&root, content.as_bytes(), "large fixture");
        let start = Instant::now();
        let page = git_blame(&root.join("sample.txt"), 1, DEFAULT_BLAME_PAGE_LINES, false)
            .expect("large blame page");
        let elapsed = start.elapsed();
        println!(
            "blame_first_page lines=10000 page={} elapsed_ms={}",
            page.lines.len(),
            elapsed.as_millis()
        );
        assert_eq!(page.lines.len(), DEFAULT_BLAME_PAGE_LINES as usize);
        let _ = std::fs::remove_dir_all(root);
    }
}
