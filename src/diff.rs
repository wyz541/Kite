//! Structured Git diff data shared by the Windows and macOS diff viewers.
//!
//! The Git CLI resolves repositories, revisions, index state and binary files.
//! Parsing and word-level highlighting live here so both native frontends see
//! the exact same hunks, rows and change navigation indices.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use similar::{ChangeTag, TextDiff};

use crate::vcs::configured_command;

const NORMAL_MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const NORMAL_MAX_DIFF_LINES: usize = 20_000;
const LARGE_MAX_DIFF_BYTES: usize = 96 * 1024;
const LARGE_MAX_DIFF_LINES: usize = 160;
const LARGE_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;
const MAX_NUMSTAT_BYTES: usize = 256 * 1024;
const MAX_WORD_HIGHLIGHT_BYTES: usize = 64 * 1024;
const MAX_SYNTAX_HIGHLIGHT_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffEndpoint {
    Revision(String),
    Index,
    WorkingTree,
}

impl DiffEndpoint {
    pub fn label(&self) -> String {
        match self {
            Self::Revision(value) => {
                let value = value.trim();
                if value.is_empty() {
                    "提交或分支".to_string()
                } else {
                    value.to_string()
                }
            }
            Self::Index => "暂存区".to_string(),
            Self::WorkingTree => "工作区".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffRequest {
    pub path: PathBuf,
    pub old: DiffEndpoint,
    pub new: DiffEndpoint,
    pub ignore_whitespace: bool,
}

impl GitDiffRequest {
    pub fn working_tree(path: PathBuf) -> Self {
        Self {
            path,
            old: DiffEndpoint::Revision("HEAD".to_string()),
            new: DiffEndpoint::WorkingTree,
            ignore_whitespace: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffContentKind {
    Text,
    Binary,
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    File,
    Hunk,
    Context,
    Deletion,
    Addition,
    Modification,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffWordRange {
    /// UTF-16 code-unit offsets, matching Swift's `NSRange` and easy to adapt
    /// to GPUI's UTF-8 string slicing in the Windows renderer.
    pub start_utf16: u32,
    pub end_utf16: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSyntaxKind {
    Keyword,
    String,
    Number,
    Comment,
    TypeName,
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSyntaxSpan {
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub kind: DiffSyntaxKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    pub highlighted: Vec<DiffWordRange>,
    pub syntax: Vec<DiffSyntaxSpan>,
    pub missing_newline: bool,
}

impl DiffLine {
    fn new(old_line: Option<u32>, new_line: Option<u32>, text: String) -> Self {
        Self {
            old_line,
            new_line,
            text,
            highlighted: Vec::new(),
            syntax: Vec::new(),
            missing_newline: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    pub text: String,
    pub left: Option<DiffLine>,
    pub right: Option<DiffLine>,
    pub change_index: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffStats {
    pub files: u32,
    pub additions: u32,
    pub deletions: u32,
    pub hunks: u32,
    pub old_bytes: Option<u64>,
    pub new_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub first_row: u32,
    pub row_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffDocument {
    pub repository_root: PathBuf,
    pub relative_path: String,
    pub old_label: String,
    pub new_label: String,
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<DiffHunk>,
    /// Row indices containing an actual change, in display order.
    pub change_rows: Vec<u32>,
    pub stats: DiffStats,
    pub content_kind: DiffContentKind,
    pub large_file: bool,
    pub truncated: bool,
    pub message: Option<String>,
    pub old_image: Option<Vec<u8>>,
    pub new_image: Option<Vec<u8>>,
}

/// Produces a structured diff between two Git states for one path.
pub fn git_diff(request: &GitDiffRequest) -> Result<GitDiffDocument, String> {
    validate_request(request)?;
    let repo = RepoContext::resolve(&request.path)?;
    let args = build_diff_args(
        &request.old,
        &request.new,
        &repo.relative_path,
        request.ignore_whitespace,
        3,
    )?;

    let old_size = endpoint_size(&repo.root, &repo.relative_path, &request.old);
    let new_size = endpoint_size(&repo.root, &repo.relative_path, &request.new);
    let large_file = old_size.is_some_and(|size| size > LARGE_FILE_BYTES)
        || new_size.is_some_and(|size| size > LARGE_FILE_BYTES);
    let git_stats = if large_file {
        GitStats {
            files: 1,
            additions: 0,
            deletions: 0,
            binary: is_image_path(&repo.relative_path),
        }
    } else {
        read_stats(&repo.root, &args, request)?
    };
    let mut bounded_truncated = false;
    let mut output_was_empty = false;
    let mut parsed = if large_file {
        large_file_preview(&repo.root, &repo.relative_path, &request.new)?
    } else {
        let output = run_git_bounded(
            &repo.root,
            &args,
            NORMAL_MAX_DIFF_BYTES,
            NORMAL_MAX_DIFF_LINES,
        )?;
        if !output.status.success() && !output.truncated {
            let status = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim().replace(['\r', '\n'], " ");
            return Err(if detail.is_empty() {
                format!("git diff 失败，退出码 {status}")
            } else {
                format!(
                    "git diff 失败：{}",
                    detail.chars().take(300).collect::<String>()
                )
            });
        }
        bounded_truncated = output.truncated;
        output_was_empty = output.stdout.is_empty();
        parse_unified_diff(&String::from_utf8_lossy(&output.stdout))
    };
    let synthetic = !large_file
        && output_was_empty
        && git_stats.additions == 0
        && git_stats.deletions == 0
        && request.new == DiffEndpoint::WorkingTree
        && path_missing_at_endpoint(&repo.root, &repo.relative_path, &request.old);
    if synthetic {
        parsed = synthetic_addition_diff(
            &repo.root,
            &repo.relative_path,
            NORMAL_MAX_DIFF_BYTES,
            NORMAL_MAX_DIFF_LINES,
        )?;
    }
    apply_syntax_highlighting(&mut parsed.rows, &repo.relative_path);

    let is_binary = parsed.binary || git_stats.binary;
    let image = is_binary && is_image_path(&repo.relative_path);
    let content_kind = if image {
        DiffContentKind::Image
    } else if is_binary {
        DiffContentKind::Binary
    } else {
        DiffContentKind::Text
    };

    let mut stats = if synthetic {
        parsed.stats.clone()
    } else {
        git_stats.as_diff_stats(0)
    };
    stats.hunks = parsed.hunks.len() as u32;
    stats.old_bytes = old_size;
    stats.new_bytes = new_size;
    if stats.files == 0 && !parsed.rows.is_empty() {
        stats.files = 1;
    }
    if let Some(last) = parsed.hunks.last_mut() {
        last.row_count = parsed.rows.len().saturating_sub(last.first_row as usize) as u32;
    } else {
        // All hunk metadata is finalized by the parser.
    }

    let (old_image, new_image) = if image {
        (
            read_endpoint_bytes(
                &repo.root,
                &repo.relative_path,
                &request.old,
                MAX_IMAGE_BYTES,
            ),
            read_endpoint_bytes(
                &repo.root,
                &repo.relative_path,
                &request.new,
                MAX_IMAGE_BYTES,
            ),
        )
    } else {
        (None, None)
    };

    let truncated = large_file || bounded_truncated || parsed.truncated;
    let message = if large_file {
        Some(format!(
            "文件较大，仅显示统计和最多 {LARGE_MAX_DIFF_LINES} 行差异。"
        ))
    } else if truncated {
        Some("差异过大，已截断显示。".to_string())
    } else if content_kind == DiffContentKind::Binary {
        Some("二进制文件已变化，无法显示文本差异。".to_string())
    } else {
        None
    };

    Ok(GitDiffDocument {
        repository_root: repo.root,
        relative_path: repo.relative_path,
        old_label: request.old.label(),
        new_label: request.new.label(),
        rows: parsed.rows,
        hunks: parsed.hunks,
        change_rows: parsed.change_rows,
        stats,
        content_kind,
        large_file,
        truncated,
        message,
        old_image,
        new_image,
    })
}

fn validate_request(request: &GitDiffRequest) -> Result<(), String> {
    validate_endpoint(&request.old)?;
    validate_endpoint(&request.new)?;
    if request.old == request.new {
        return Err("两个比较端点相同。".to_string());
    }
    Ok(())
}

fn validate_endpoint(endpoint: &DiffEndpoint) -> Result<(), String> {
    let DiffEndpoint::Revision(value) = endpoint else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() {
        return Err("请输入用于比较的提交或分支。".to_string());
    }
    if value.starts_with('-') {
        return Err("提交或分支名称不能以 - 开头。".to_string());
    }
    if value.contains('\0') {
        return Err("提交或分支名称包含无效字符。".to_string());
    }
    Ok(())
}

struct RepoContext {
    root: PathBuf,
    relative_path: String,
}

impl RepoContext {
    fn resolve(path: &Path) -> Result<Self, String> {
        let is_directory = path.is_dir();
        let start = if is_directory {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let mut command = git_command(start);
        let output = command
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|_| "未找到 Git。请确认 Git 已安装并位于 PATH 中。".to_string())?;
        if !output.status.success() {
            return Err("所选项目不在 Git 工作区中".to_string());
        }
        let root_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let root = dunce::canonicalize(&root_text).unwrap_or_else(|_| PathBuf::from(root_text));
        let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let relative = target
            .strip_prefix(&root)
            .or_else(|_| path.strip_prefix(&root))
            .map_err(|_| "所选项目不在 Git 工作区中".to_string())?;
        let relative_path = if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative.to_string_lossy().replace('\\', "/")
        };
        Ok(Self {
            root,
            relative_path,
        })
    }
}

fn build_diff_args(
    old: &DiffEndpoint,
    new: &DiffEndpoint,
    path: &str,
    ignore_whitespace: bool,
    context_lines: u32,
) -> Result<Vec<String>, String> {
    let mut args = vec![
        "diff".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        "--no-color".to_string(),
        format!("--unified={context_lines}"),
    ];
    if ignore_whitespace {
        args.push("-w".to_string());
    }

    let mut cached = false;
    let mut reverse = false;
    match (old, new) {
        (DiffEndpoint::Revision(old), DiffEndpoint::Revision(new)) => {
            args.push(old.clone());
            args.push(new.clone());
        }
        (DiffEndpoint::Revision(old), DiffEndpoint::WorkingTree) => args.push(old.clone()),
        (DiffEndpoint::Revision(old), DiffEndpoint::Index) => {
            cached = true;
            args.push(old.clone());
        }
        (DiffEndpoint::Index, DiffEndpoint::Revision(new)) => {
            cached = true;
            reverse = true;
            args.push(new.clone());
        }
        (DiffEndpoint::WorkingTree, DiffEndpoint::Revision(new)) => {
            reverse = true;
            args.push(new.clone());
        }
        (DiffEndpoint::Index, DiffEndpoint::WorkingTree) => {}
        (DiffEndpoint::WorkingTree, DiffEndpoint::Index) => reverse = true,
        (DiffEndpoint::Index, DiffEndpoint::Index)
        | (DiffEndpoint::WorkingTree, DiffEndpoint::WorkingTree) => {
            return Err("两个比较端点相同。".to_string());
        }
    }
    if reverse {
        args.push("-R".to_string());
    }
    if cached {
        args.push("--cached".to_string());
    }
    args.push("--".to_string());
    args.push(path.to_string());
    Ok(args)
}
#[derive(Default)]
struct GitStats {
    files: u32,
    additions: u32,
    deletions: u32,
    binary: bool,
}

impl GitStats {
    fn as_diff_stats(&self, hunks: u32) -> DiffStats {
        DiffStats {
            files: self.files,
            additions: self.additions,
            deletions: self.deletions,
            hunks,
            old_bytes: None,
            new_bytes: None,
        }
    }
}

fn read_stats(
    root: &Path,
    diff_args: &[String],
    request: &GitDiffRequest,
) -> Result<GitStats, String> {
    let mut args = diff_args.to_vec();
    args.insert(1, "--numstat".to_string());
    let output = run_git_bounded(root, &args, MAX_NUMSTAT_BYTES, NORMAL_MAX_DIFF_LINES)?;
    if !output.status.success() && !output.truncated {
        return Ok(GitStats::default());
    }
    let stats = parse_numstat(&String::from_utf8_lossy(&output.stdout));
    if stats.files == 0 && request.old == request.new {
        return Ok(GitStats::default());
    }
    Ok(stats)
}

fn parse_numstat(text: &str) -> GitStats {
    let mut stats = GitStats {
        files: 0,
        additions: 0,
        deletions: 0,
        binary: false,
    };
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(added) = fields.next() else {
            continue;
        };
        let Some(removed) = fields.next() else {
            continue;
        };
        if fields.next().is_none() {
            continue;
        }
        stats.files += 1;
        if added == "-" || removed == "-" {
            stats.binary = true;
            continue;
        }
        stats.additions += added.parse::<u32>().unwrap_or(0);
        stats.deletions += removed.parse::<u32>().unwrap_or(0);
    }
    stats
}

#[derive(Default)]
struct ParsedDiff {
    rows: Vec<DiffRow>,
    hunks: Vec<DiffHunk>,
    change_rows: Vec<u32>,
    stats: DiffStats,
    binary: bool,
    truncated: bool,
}

struct DiffParser {
    rows: Vec<DiffRow>,
    hunks: Vec<DiffHunk>,
    change_rows: Vec<u32>,
    removed: Vec<DiffLine>,
    added: Vec<DiffLine>,
    old_line: u32,
    new_line: u32,
    current_hunk: Option<usize>,
    binary: bool,
}

impl DiffParser {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            hunks: Vec::new(),
            change_rows: Vec::new(),
            removed: Vec::new(),
            added: Vec::new(),
            old_line: 0,
            new_line: 0,
            current_hunk: None,
            binary: false,
        }
    }

    fn push_preamble(&mut self, line: &str) {
        if line.starts_with("diff --git ") {
            self.flush_pending();
            self.finish_hunk();
            let label = line
                .split_whitespace()
                .last()
                .and_then(|value| value.strip_prefix("b/"))
                .unwrap_or_else(|| line.strip_prefix("diff --git ").unwrap_or(line));
            self.rows.push(DiffRow {
                kind: DiffRowKind::File,
                text: label.to_string(),
                left: None,
                right: None,
                change_index: None,
            });
        } else if line.starts_with("Binary files ")
            || line == "GIT binary patch"
            || line.starts_with("Binary files differ")
        {
            self.binary = true;
            self.flush_pending();
            self.finish_hunk();
            self.rows.push(DiffRow {
                kind: DiffRowKind::Hunk,
                text: "二进制文件已变化".to_string(),
                left: None,
                right: None,
                change_index: None,
            });
        }
    }

    fn push_line(&mut self, line: &str) {
        if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(line) {
            self.flush_pending();
            self.finish_hunk();
            let first_row = self.rows.len() as u32;
            self.rows.push(DiffRow {
                kind: DiffRowKind::Hunk,
                text: line.to_string(),
                left: None,
                right: None,
                change_index: None,
            });
            self.hunks.push(DiffHunk {
                header: line.to_string(),
                old_start,
                old_count,
                new_start,
                new_count,
                first_row,
                row_count: 1,
            });
            self.current_hunk = Some(self.hunks.len() - 1);
            self.old_line = old_start;
            self.new_line = new_start;
            return;
        }
        if self.current_hunk.is_none() {
            self.push_preamble(line);
            return;
        }

        let first = line.as_bytes().first().copied();
        match first {
            Some(b' ') => {
                self.flush_pending();
                let body = line[1..].to_string();
                self.rows.push(DiffRow {
                    kind: DiffRowKind::Context,
                    text: body.clone(),
                    left: Some(DiffLine::new(
                        Some(self.old_line),
                        Some(self.new_line),
                        body.clone(),
                    )),
                    right: Some(DiffLine::new(
                        Some(self.old_line),
                        Some(self.new_line),
                        body,
                    )),
                    change_index: None,
                });
                self.old_line += 1;
                self.new_line += 1;
            }
            Some(b'-') => {
                self.removed.push(DiffLine::new(
                    Some(self.old_line),
                    None,
                    line[1..].to_string(),
                ));
                self.old_line += 1;
            }
            Some(b'+') => {
                self.added.push(DiffLine::new(
                    None,
                    Some(self.new_line),
                    line[1..].to_string(),
                ));
                self.new_line += 1;
            }
            Some(b'\\') => {
                if let Some(line) = self.removed.last_mut() {
                    line.missing_newline = true;
                } else if let Some(line) = self.added.last_mut() {
                    line.missing_newline = true;
                } else if let Some(line) = self
                    .rows
                    .last_mut()
                    .and_then(|row| row.right.as_mut().or(row.left.as_mut()))
                {
                    line.missing_newline = true;
                }
            }
            _ => self.push_preamble(line),
        }
    }

    fn flush_pending(&mut self) {
        let paired = self.removed.len().min(self.added.len());
        for index in 0..paired {
            let mut left = self.removed[index].clone();
            let mut right = self.added[index].clone();
            let (left_ranges, right_ranges) = word_ranges(&left.text, &right.text);
            left.highlighted = left_ranges;
            right.highlighted = right_ranges;
            self.push_change_row(DiffRow {
                kind: DiffRowKind::Modification,
                text: right.text.clone(),
                left: Some(left),
                right: Some(right),
                change_index: None,
            });
        }
        let removed = std::mem::take(&mut self.removed);
        let added = std::mem::take(&mut self.added);
        for line in removed.into_iter().skip(paired) {
            self.push_change_row(DiffRow {
                kind: DiffRowKind::Deletion,
                text: line.text.clone(),
                left: Some(line),
                right: None,
                change_index: None,
            });
        }
        for line in added.into_iter().skip(paired) {
            self.push_change_row(DiffRow {
                kind: DiffRowKind::Addition,
                text: line.text.clone(),
                left: None,
                right: Some(line),
                change_index: None,
            });
        }
    }

    fn push_change_row(&mut self, mut row: DiffRow) {
        let row_index = self.rows.len() as u32;
        let change_index = self.change_rows.len() as u32;
        row.change_index = Some(change_index);
        self.rows.push(row);
        self.change_rows.push(row_index);
    }

    fn finish_hunk(&mut self) {
        if let Some(last) = self.hunks.last_mut() {
            last.row_count = self.rows.len().saturating_sub(last.first_row as usize) as u32;
        }
        self.current_hunk = None;
    }

    fn finish(mut self) -> ParsedDiff {
        self.flush_pending();
        self.finish_hunk();
        let change_count = self.change_rows.len() as u32;
        ParsedDiff {
            stats: DiffStats {
                files: if self.rows.is_empty() { 0 } else { 1 },
                additions: 0,
                deletions: 0,
                hunks: self.hunks.len() as u32,
                old_bytes: None,
                new_bytes: None,
            },
            rows: self.rows,
            hunks: self.hunks,
            change_rows: self.change_rows,
            binary: self.binary,
            truncated: false,
        }
        .with_change_count(change_count)
    }
}

impl ParsedDiff {
    fn with_change_count(mut self, change_count: u32) -> Self {
        self.stats.hunks = self.hunks.len() as u32;
        let _ = change_count;
        self
    }
}

fn parse_unified_diff(text: &str) -> ParsedDiff {
    let mut parser = DiffParser::new();
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        parser.push_line(line);
    }
    parser.finish()
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_spec, rest) = rest.split_once(" +")?;
    let (new_spec, _) = rest.split_once(" @@").or_else(|| rest.split_once(' '))?;
    let (old_start, old_count) = parse_hunk_range(old_spec)?;
    let (new_start, new_count) = parse_hunk_range(new_spec)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_hunk_range(value: &str) -> Option<(u32, u32)> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn word_ranges(old: &str, new: &str) -> (Vec<DiffWordRange>, Vec<DiffWordRange>) {
    if old.len() > MAX_WORD_HIGHLIGHT_BYTES || new.len() > MAX_WORD_HIGHLIGHT_BYTES {
        return (
            vec![DiffWordRange {
                start_utf16: 0,
                end_utf16: old.encode_utf16().count() as u32,
            }],
            vec![DiffWordRange {
                start_utf16: 0,
                end_utf16: new.encode_utf16().count() as u32,
            }],
        );
    }
    let diff = TextDiff::from_words(old, new);
    let mut left_offset = 0_u32;
    let mut right_offset = 0_u32;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for change in diff.iter_all_changes() {
        let length = change.value().encode_utf16().count() as u32;
        match change.tag() {
            ChangeTag::Equal => {
                left_offset = left_offset.saturating_add(length);
                right_offset = right_offset.saturating_add(length);
            }
            ChangeTag::Delete => {
                push_merged_range(
                    &mut left,
                    DiffWordRange {
                        start_utf16: left_offset,
                        end_utf16: left_offset.saturating_add(length),
                    },
                );
                left_offset = left_offset.saturating_add(length);
            }
            ChangeTag::Insert => {
                push_merged_range(
                    &mut right,
                    DiffWordRange {
                        start_utf16: right_offset,
                        end_utf16: right_offset.saturating_add(length),
                    },
                );
                right_offset = right_offset.saturating_add(length);
            }
        }
    }
    (left, right)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyntaxLanguage {
    Rust,
    JavaScript,
    Python,
    Go,
    Swift,
    Java,
    C,
    Shell,
    Json,
    Yaml,
    Toml,
    Html,
    Css,
    Sql,
    Markdown,
    Unknown,
}

fn detect_syntax_language(path: &str) -> SyntaxLanguage {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => SyntaxLanguage::Rust,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => SyntaxLanguage::JavaScript,
        "py" | "pyw" => SyntaxLanguage::Python,
        "go" => SyntaxLanguage::Go,
        "swift" => SyntaxLanguage::Swift,
        "java" | "kt" | "kts" => SyntaxLanguage::Java,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "m" | "mm" | "cs" => SyntaxLanguage::C,
        "sh" | "bash" | "zsh" | "fish" => SyntaxLanguage::Shell,
        "json" | "jsonc" => SyntaxLanguage::Json,
        "yaml" | "yml" => SyntaxLanguage::Yaml,
        "toml" => SyntaxLanguage::Toml,
        "html" | "htm" | "xml" | "svg" => SyntaxLanguage::Html,
        "css" | "scss" | "sass" | "less" => SyntaxLanguage::Css,
        "sql" => SyntaxLanguage::Sql,
        "md" | "markdown" => SyntaxLanguage::Markdown,
        _ => SyntaxLanguage::Unknown,
    }
}

fn apply_syntax_highlighting(rows: &mut [DiffRow], path: &str) {
    let language = detect_syntax_language(path);
    if language == SyntaxLanguage::Unknown || language == SyntaxLanguage::Markdown {
        return;
    }
    for row in rows {
        for line in [row.left.as_mut(), row.right.as_mut()]
            .into_iter()
            .flatten()
        {
            line.syntax = syntax_spans(&line.text, language);
        }
    }
}

fn syntax_spans(text: &str, language: SyntaxLanguage) -> Vec<DiffSyntaxSpan> {
    if text.len() > MAX_SYNTAX_HIGHLIGHT_BYTES {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let utf16_offsets = utf16_byte_offsets(text);
    let mut spans = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if language == SyntaxLanguage::Html && bytes[index] == b'<' {
            if bytes[index..].starts_with(b"<!--") {
                let end = bytes[index + 4..]
                    .windows(3)
                    .position(|window| window == b"-->")
                    .map(|offset| index + 4 + offset + 3)
                    .unwrap_or(bytes.len());
                push_syntax_span(
                    &mut spans,
                    &utf16_offsets,
                    index,
                    end,
                    DiffSyntaxKind::Comment,
                );
                index = end;
                continue;
            }
            let mut name_start = index + 1;
            if bytes.get(name_start) == Some(&b'/') {
                name_start += 1;
            }
            let mut name_end = name_start;
            while name_end < bytes.len()
                && (bytes[name_end].is_ascii_alphanumeric()
                    || matches!(bytes[name_end], b'_' | b'-' | b':'))
            {
                name_end += 1;
            }
            if name_end > name_start {
                push_syntax_span(
                    &mut spans,
                    &utf16_offsets,
                    name_start,
                    name_end,
                    DiffSyntaxKind::Keyword,
                );
                index = name_end;
                continue;
            }
        }

        let comment = match language {
            SyntaxLanguage::Rust
            | SyntaxLanguage::JavaScript
            | SyntaxLanguage::Go
            | SyntaxLanguage::Swift
            | SyntaxLanguage::Java
            | SyntaxLanguage::C => block_or_line_comment(bytes, index, b"//"),
            SyntaxLanguage::Python
            | SyntaxLanguage::Shell
            | SyntaxLanguage::Yaml
            | SyntaxLanguage::Toml => bytes[index..]
                .first()
                .is_some_and(|byte| *byte == b'#')
                .then_some(bytes.len()),
            SyntaxLanguage::Sql => block_or_line_comment(bytes, index, b"--"),
            _ => None,
        };
        if let Some(end) = comment {
            push_syntax_span(
                &mut spans,
                &utf16_offsets,
                index,
                end,
                DiffSyntaxKind::Comment,
            );
            index = end;
            continue;
        }

        if matches!(bytes[index], b'"' | b'\'' | b'`') {
            let end = quoted_string_end(bytes, index);
            push_syntax_span(
                &mut spans,
                &utf16_offsets,
                index,
                end,
                DiffSyntaxKind::String,
            );
            index = end;
            continue;
        }

        if bytes[index].is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || matches!(bytes[index], b'.' | b'_' | b'+' | b'-'))
            {
                index += 1;
            }
            push_syntax_span(
                &mut spans,
                &utf16_offsets,
                start,
                index,
                DiffSyntaxKind::Number,
            );
            continue;
        }

        if is_identifier_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            let word = &text[start..index];
            let next = bytes[index..]
                .iter()
                .position(|byte| !byte.is_ascii_whitespace())
                .map(|offset| bytes[index + offset]);
            let kind = if is_syntax_keyword(language, word) {
                Some(DiffSyntaxKind::Keyword)
            } else if next == Some(b'(') {
                Some(DiffSyntaxKind::Function)
            } else if word
                .chars()
                .next()
                .is_some_and(|character| character.is_uppercase())
            {
                Some(DiffSyntaxKind::TypeName)
            } else {
                None
            };
            if let Some(kind) = kind {
                push_syntax_span(&mut spans, &utf16_offsets, start, index, kind);
            }
            continue;
        }

        index += text[index..].chars().next().map_or(1, char::len_utf8);
    }
    spans
}

fn block_or_line_comment(bytes: &[u8], index: usize, line_marker: &[u8]) -> Option<usize> {
    if bytes[index..].starts_with(b"/*") {
        return Some(
            bytes[index + 2..]
                .windows(2)
                .position(|window| window == b"*/")
                .map(|offset| index + 2 + offset + 2)
                .unwrap_or(bytes.len()),
        );
    }
    bytes[index..]
        .starts_with(line_marker)
        .then_some(bytes.len())
}

fn quoted_string_end(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
            continue;
        }
        if bytes[index] == quote {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn is_syntax_keyword(language: SyntaxLanguage, word: &str) -> bool {
    match language {
        SyntaxLanguage::Rust => matches!(
            word,
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
        ),
        SyntaxLanguage::JavaScript => matches!(
            word,
            "async"
                | "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "export"
                | "extends"
                | "false"
                | "finally"
                | "for"
                | "from"
                | "function"
                | "get"
                | "if"
                | "import"
                | "in"
                | "instanceof"
                | "let"
                | "new"
                | "null"
                | "of"
                | "return"
                | "set"
                | "static"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typeof"
                | "undefined"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
        ),
        SyntaxLanguage::Python => matches!(
            word,
            "False"
                | "None"
                | "True"
                | "and"
                | "as"
                | "assert"
                | "async"
                | "await"
                | "break"
                | "class"
                | "continue"
                | "def"
                | "del"
                | "elif"
                | "else"
                | "except"
                | "finally"
                | "for"
                | "from"
                | "global"
                | "if"
                | "import"
                | "in"
                | "is"
                | "lambda"
                | "nonlocal"
                | "not"
                | "or"
                | "pass"
                | "raise"
                | "return"
                | "try"
                | "while"
                | "with"
                | "yield"
        ),
        SyntaxLanguage::Go => matches!(
            word,
            "break"
                | "case"
                | "chan"
                | "const"
                | "continue"
                | "default"
                | "defer"
                | "else"
                | "fallthrough"
                | "false"
                | "for"
                | "func"
                | "go"
                | "goto"
                | "if"
                | "import"
                | "interface"
                | "map"
                | "nil"
                | "package"
                | "range"
                | "return"
                | "select"
                | "struct"
                | "switch"
                | "true"
                | "type"
                | "var"
        ),
        SyntaxLanguage::Swift => matches!(
            word,
            "as" | "associatedtype"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "continue"
                | "default"
                | "defer"
                | "do"
                | "else"
                | "enum"
                | "extension"
                | "fallthrough"
                | "false"
                | "fileprivate"
                | "for"
                | "func"
                | "guard"
                | "if"
                | "import"
                | "in"
                | "init"
                | "inout"
                | "internal"
                | "is"
                | "let"
                | "nil"
                | "open"
                | "operator"
                | "private"
                | "protocol"
                | "public"
                | "repeat"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "subscript"
                | "super"
                | "switch"
                | "throw"
                | "throws"
                | "true"
                | "try"
                | "typealias"
                | "var"
                | "where"
                | "while"
        ),
        SyntaxLanguage::Java => matches!(
            word,
            "abstract"
                | "assert"
                | "boolean"
                | "break"
                | "byte"
                | "case"
                | "catch"
                | "char"
                | "class"
                | "const"
                | "continue"
                | "default"
                | "do"
                | "double"
                | "else"
                | "enum"
                | "extends"
                | "false"
                | "final"
                | "finally"
                | "float"
                | "for"
                | "if"
                | "implements"
                | "import"
                | "instanceof"
                | "int"
                | "interface"
                | "long"
                | "native"
                | "new"
                | "null"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "short"
                | "static"
                | "strictfp"
                | "super"
                | "switch"
                | "synchronized"
                | "this"
                | "throw"
                | "throws"
                | "transient"
                | "true"
                | "try"
                | "void"
                | "volatile"
                | "while"
        ),
        SyntaxLanguage::C => matches!(
            word,
            "auto"
                | "bool"
                | "break"
                | "case"
                | "catch"
                | "char"
                | "class"
                | "const"
                | "constexpr"
                | "continue"
                | "default"
                | "delete"
                | "do"
                | "double"
                | "else"
                | "enum"
                | "explicit"
                | "extern"
                | "false"
                | "float"
                | "for"
                | "friend"
                | "goto"
                | "if"
                | "inline"
                | "int"
                | "long"
                | "namespace"
                | "new"
                | "nullptr"
                | "operator"
                | "private"
                | "protected"
                | "public"
                | "register"
                | "return"
                | "short"
                | "signed"
                | "sizeof"
                | "static"
                | "struct"
                | "switch"
                | "template"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typedef"
                | "typename"
                | "union"
                | "unsigned"
                | "using"
                | "virtual"
                | "void"
                | "volatile"
                | "while"
        ),
        SyntaxLanguage::Shell => matches!(
            word,
            "case"
                | "do"
                | "done"
                | "elif"
                | "else"
                | "esac"
                | "fi"
                | "for"
                | "function"
                | "if"
                | "in"
                | "local"
                | "return"
                | "select"
                | "then"
                | "until"
                | "while"
        ),
        SyntaxLanguage::Json => matches!(word, "false" | "null" | "true"),
        SyntaxLanguage::Yaml | SyntaxLanguage::Toml => matches!(word, "false" | "true"),
        SyntaxLanguage::Html | SyntaxLanguage::Css | SyntaxLanguage::Sql => false,
        SyntaxLanguage::Markdown | SyntaxLanguage::Unknown => false,
    }
}

fn utf16_byte_offsets(text: &str) -> Vec<u32> {
    let mut offsets = vec![0_u32; text.len() + 1];
    let mut utf16 = 0_u32;
    for (byte, character) in text.char_indices() {
        offsets[byte] = utf16;
        utf16 = utf16.saturating_add(character.len_utf16() as u32);
        offsets[byte + 1..=byte + character.len_utf8()].fill(utf16);
    }
    offsets
}

fn push_syntax_span(
    spans: &mut Vec<DiffSyntaxSpan>,
    offsets: &[u32],
    start: usize,
    end: usize,
    kind: DiffSyntaxKind,
) {
    if start >= end {
        return;
    }
    let start_utf16 = offsets[start];
    let end_utf16 = offsets[end];
    if let Some(last) = spans.last_mut()
        && last.kind == kind
        && last.end_utf16 == start_utf16
    {
        last.end_utf16 = end_utf16;
        return;
    }
    spans.push(DiffSyntaxSpan {
        start_utf16,
        end_utf16,
        kind,
    });
}

fn push_merged_range(ranges: &mut Vec<DiffWordRange>, range: DiffWordRange) {
    if range.start_utf16 == range.end_utf16 {
        return;
    }
    if let Some(last) = ranges.last_mut()
        && last.end_utf16 == range.start_utf16
    {
        last.end_utf16 = range.end_utf16;
        return;
    }
    ranges.push(range);
}

fn large_file_preview(
    root: &Path,
    relative_path: &str,
    endpoint: &DiffEndpoint,
) -> Result<ParsedDiff, String> {
    let mut parsed = ParsedDiff {
        truncated: true,
        ..Default::default()
    };
    parsed.rows.push(DiffRow {
        kind: DiffRowKind::File,
        text: relative_path.to_string(),
        left: None,
        right: None,
        change_index: None,
    });
    let Some(bytes) =
        read_endpoint_bytes(root, relative_path, endpoint, LARGE_MAX_DIFF_BYTES as u64)
    else {
        parsed.binary = is_image_path(relative_path);
        parsed.rows.push(DiffRow {
            kind: DiffRowKind::Hunk,
            text: "文件较大，仅显示统计。".to_string(),
            left: None,
            right: None,
            change_index: None,
        });
        return Ok(parsed);
    };
    if is_image_path(relative_path) {
        parsed.binary = true;
    }
    if bytes.contains(&0) || String::from_utf8(bytes.clone()).is_err() {
        parsed.binary = true;
        parsed.rows.push(DiffRow {
            kind: DiffRowKind::Hunk,
            text: "二进制文件已变化".to_string(),
            left: None,
            right: None,
            change_index: None,
        });
        return Ok(parsed);
    }

    parsed.rows.push(DiffRow {
        kind: DiffRowKind::Hunk,
        text: format!("大文件预览：仅显示前 {LARGE_MAX_DIFF_LINES} 行"),
        left: None,
        right: None,
        change_index: None,
    });
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    parsed.truncated = lines.len() > LARGE_MAX_DIFF_LINES || bytes.len() >= LARGE_MAX_DIFF_BYTES;
    for (index, line) in lines.iter().take(LARGE_MAX_DIFF_LINES).enumerate() {
        let body = line.strip_suffix('\r').unwrap_or(line).to_string();
        let number = index as u32 + 1;
        parsed.rows.push(DiffRow {
            kind: DiffRowKind::Context,
            text: body.clone(),
            left: Some(DiffLine::new(Some(number), Some(number), body.clone())),
            right: Some(DiffLine::new(Some(number), Some(number), body)),
            change_index: None,
        });
    }
    Ok(parsed)
}

fn synthetic_addition_diff(
    root: &Path,
    relative_path: &str,
    max_bytes: usize,
    max_lines: usize,
) -> Result<ParsedDiff, String> {
    let path = root.join(relative_path);
    let metadata = std::fs::metadata(&path).map_err(|error| format!("无法读取文件：{error}"))?;
    if !metadata.is_file() {
        return Ok(ParsedDiff::default());
    }
    let bytes = read_file_limited(&path, max_bytes as u64 * 2)?;
    if bytes.contains(&0) || String::from_utf8(bytes.clone()).is_err() {
        let parsed = ParsedDiff {
            binary: true,
            stats: DiffStats {
                files: 1,
                ..DiffStats::default()
            },
            ..ParsedDiff::default()
        };
        return Ok(parsed);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let shown = lines.len().min(max_lines);
    let mut diff = format!("diff --git a/{relative_path} b/{relative_path}\n");
    diff.push_str("new file mode 100644\n--- /dev/null\n");
    diff.push_str(&format!("+++ b/{relative_path}\n"));
    diff.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in &lines[..shown] {
        diff.push('+');
        diff.push_str(line.strip_suffix('\r').unwrap_or(line));
        diff.push('\n');
    }
    let truncated = shown < lines.len() || bytes.len() > max_bytes;
    let mut parsed = parse_unified_diff(&diff);
    parsed.truncated = truncated;
    parsed.stats = DiffStats {
        files: 1,
        additions: lines.len() as u32,
        deletions: 0,
        hunks: parsed.hunks.len() as u32,
        old_bytes: None,
        new_bytes: Some(lines.len() as u64),
    };
    Ok(parsed)
}

fn read_file_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("无法读取文件：{error}"))?;
    let mut reader = BufReader::new(file).take(max_bytes.saturating_add(1));
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取文件：{error}"))?;
    if bytes.len() as u64 > max_bytes {
        bytes.truncate(max_bytes as usize);
    }
    Ok(bytes)
}
fn endpoint_size(root: &Path, relative_path: &str, endpoint: &DiffEndpoint) -> Option<u64> {
    match endpoint {
        DiffEndpoint::WorkingTree => std::fs::metadata(root.join(relative_path))
            .ok()
            .map(|metadata| metadata.len()),
        DiffEndpoint::Index => revision_object_size(root, &format!(":{relative_path}")),
        DiffEndpoint::Revision(revision) => {
            revision_object_size(root, &format!("{}:{relative_path}", revision.trim()))
        }
    }
}

fn revision_object_size(root: &Path, object: &str) -> Option<u64> {
    let mut command = git_command(root);
    let output = command.args(["cat-file", "-s", object]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn path_missing_at_endpoint(root: &Path, relative_path: &str, endpoint: &DiffEndpoint) -> bool {
    match endpoint {
        DiffEndpoint::WorkingTree => !root.join(relative_path).exists(),
        DiffEndpoint::Index => !git_object_exists(root, &format!(":{relative_path}")),
        DiffEndpoint::Revision(revision) => {
            !git_object_exists(root, &format!("{}:{relative_path}", revision.trim()))
        }
    }
}

fn git_object_exists(root: &Path, object: &str) -> bool {
    let mut command = git_command(root);
    command
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git_command(current_dir: &Path) -> Command {
    let mut command = configured_command("git", current_dir);
    command.env("LC_ALL", "C");
    command
}

fn read_endpoint_bytes(
    root: &Path,
    relative_path: &str,
    endpoint: &DiffEndpoint,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    match endpoint {
        DiffEndpoint::WorkingTree => read_file_limited(&root.join(relative_path), max_bytes).ok(),
        DiffEndpoint::Index => {
            let path = format!(":{relative_path}");
            let output = run_git_bounded(
                root,
                &["show".to_string(), path],
                max_bytes as usize,
                usize::MAX,
            )
            .ok()?;
            output.status.success().then_some(output.stdout)
        }
        DiffEndpoint::Revision(revision) => {
            let object = format!("{}:{relative_path}", revision.trim());
            let output = run_git_bounded(
                root,
                &["show".to_string(), object],
                max_bytes as usize,
                usize::MAX,
            )
            .ok()?;
            output.status.success().then_some(output.stdout)
        }
    }
}

fn is_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico"
            )
        })
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

fn run_git_bounded(
    root: &Path,
    args: &[String],
    max_bytes: usize,
    max_lines: usize,
) -> Result<BoundedOutput, String> {
    let mut command = git_command(root);
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法运行 git：{error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 git 输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 git 错误输出".to_string())?;
    let stderr_thread = thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });

    let mut reader = BufReader::new(stdout);
    let mut output = Vec::new();
    let mut line_count = 0_usize;
    let mut truncated = false;
    loop {
        let buffer = reader
            .fill_buf()
            .map_err(|error| format!("读取 git 输出失败：{error}"))?;
        if buffer.is_empty() {
            break;
        }
        if line_count >= max_lines {
            truncated = true;
            break;
        }
        let remaining = max_bytes.saturating_sub(output.len());
        if remaining == 0 {
            truncated = true;
            break;
        }

        let buffer_len = buffer.len();
        let mut length = buffer_len.min(remaining);
        let newline_limit = max_lines.saturating_sub(line_count);
        let mut newline_count = 0_usize;
        let mut stopped_at_line_limit = false;
        for (position, byte) in buffer[..length].iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            newline_count += 1;
            if newline_count == newline_limit {
                length = position + 1;
                stopped_at_line_limit = true;
                break;
            }
        }

        output.extend_from_slice(&buffer[..length]);
        let consumed_all = length == buffer_len;
        reader.consume(length);
        line_count = line_count.saturating_add(newline_count);

        if !consumed_all && !stopped_at_line_limit {
            truncated = true;
            break;
        }
    }
    if truncated {
        terminate_child(&mut child);
    }
    let status = child
        .wait()
        .map_err(|error| format!("等待 git 结束失败：{error}"))?;
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(BoundedOutput {
        status,
        stdout: output,
        stderr,
        truncated,
    })
}
fn terminate_child(child: &mut std::process::Child) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::SystemTime;

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
            "kite-diff-{label}-{}-{}",
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
        root
    }

    #[test]
    fn parses_hunks_and_word_ranges_deterministically() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n context\n-old value here\n+new value here\n tail\n";
        let first = parse_unified_diff(patch);
        let second = parse_unified_diff(patch);
        assert_eq!(first.rows, second.rows);
        assert_eq!(first.hunks, second.hunks);
        assert_eq!(first.change_rows, second.change_rows);
        assert_eq!(first.hunks.len(), 1);
        assert_eq!(first.change_rows, vec![3]);

        let modification = first
            .rows
            .iter()
            .find(|row| row.kind == DiffRowKind::Modification)
            .expect("modification row");
        let left_ranges = &modification.left.as_ref().unwrap().highlighted;
        let right_ranges = &modification.right.as_ref().unwrap().highlighted;
        assert_eq!(
            left_ranges.first(),
            Some(&DiffWordRange {
                start_utf16: 0,
                end_utf16: 3
            })
        );
        assert_eq!(
            right_ranges.first(),
            Some(&DiffWordRange {
                start_utf16: 0,
                end_utf16: 3
            })
        );
    }

    #[test]
    fn pairs_deletions_and_additions_for_side_by_side_rows() {
        let patch = "@@ -1,2 +1,3 @@\n-old a\n-old b\n+new a\n+new b\n+extra\n";
        let parsed = parse_unified_diff(patch);
        let changes: Vec<_> = parsed
            .rows
            .iter()
            .filter(|row| row.change_index.is_some())
            .collect();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].kind, DiffRowKind::Modification);
        assert_eq!(changes[1].kind, DiffRowKind::Modification);
        assert_eq!(changes[2].kind, DiffRowKind::Addition);
        assert!(changes[2].left.is_none());
    }

    #[test]
    fn recognizes_binary_diff_markers() {
        let patch = "diff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n";
        let parsed = parse_unified_diff(patch);
        assert!(parsed.binary);
        assert!(parsed.rows.iter().any(|row| row.text.contains("二进制")));
    }

    #[test]
    fn parses_numstat_and_binary_rows() {
        let stats = parse_numstat("4\t2\tsrc/a.rs\n-\t-\tasset.png\n");
        assert_eq!(stats.files, 2);
        assert_eq!(stats.additions, 4);
        assert_eq!(stats.deletions, 2);
        assert!(stats.binary);
    }

    #[test]
    fn rejects_option_like_revisions() {
        let error = git_diff(&GitDiffRequest {
            path: PathBuf::from("."),
            old: DiffEndpoint::Revision("--output=owned".to_string()),
            new: DiffEndpoint::WorkingTree,
            ignore_whitespace: false,
        })
        .expect_err("option-like revision");
        assert!(error.contains("不能以 - 开头"));
    }

    #[test]
    fn highlights_syntax_and_merges_adjacent_spans() {
        let line = "fn main() { let value = 42; println!(\"hi\"); } // done";
        let start = |needle: &str| {
            line[..line.find(needle).expect("needle")]
                .encode_utf16()
                .count() as u32
        };
        let spans = syntax_spans(line, SyntaxLanguage::Rust);
        assert!(spans.iter().any(|span| {
            span.kind == DiffSyntaxKind::Keyword
                && span.start_utf16 == start("fn")
                && span.end_utf16 == start("fn") + 2
        }));
        assert!(spans.iter().any(|span| {
            span.kind == DiffSyntaxKind::Function && span.start_utf16 == start("main")
        }));
        assert!(spans.iter().any(|span| {
            span.kind == DiffSyntaxKind::Number && span.start_utf16 == start("42")
        }));
        assert!(spans.iter().any(|span| {
            span.kind == DiffSyntaxKind::String && span.start_utf16 == start("\"hi\"")
        }));
        assert!(spans.iter().any(|span| {
            span.kind == DiffSyntaxKind::Comment && span.start_utf16 == start("// done")
        }));
        assert!(
            spans
                .windows(2)
                .all(|window| window[0].end_utf16 <= window[1].start_utf16)
        );
    }

    #[test]
    fn skips_word_diff_for_very_long_lines() {
        let old = "a".repeat(MAX_WORD_HIGHLIGHT_BYTES + 1);
        let new = "b".repeat(MAX_WORD_HIGHLIGHT_BYTES + 1);
        let (left, right) = word_ranges(&old, &new);
        assert_eq!(left[0].end_utf16, old.len() as u32);
        assert_eq!(right[0].end_utf16, new.len() as u32);
    }

    #[test]
    fn compares_working_tree_index_and_revisions() {
        if !git_available() {
            return;
        }
        let root = temp_repo("endpoints");
        let file = root.join("a.txt");
        std::fs::write(&file, "one\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        git_ok(&root, &["commit", "--quiet", "-m", "one"]);

        std::fs::write(&file, "two\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        std::fs::write(&file, "three\n").unwrap();

        let index_vs_head = git_diff(&GitDiffRequest {
            path: file.clone(),
            old: DiffEndpoint::Revision("HEAD".to_string()),
            new: DiffEndpoint::Index,
            ignore_whitespace: false,
        })
        .expect("HEAD to index");
        assert!(
            index_vs_head
                .rows
                .iter()
                .any(|row| row.right.as_ref().is_some_and(|line| line.text == "two"))
        );

        let worktree_vs_index = git_diff(&GitDiffRequest {
            path: file.clone(),
            old: DiffEndpoint::Index,
            new: DiffEndpoint::WorkingTree,
            ignore_whitespace: false,
        })
        .expect("index to work tree");
        assert!(
            worktree_vs_index
                .rows
                .iter()
                .any(|row| row.right.as_ref().is_some_and(|line| line.text == "three"))
        );

        let index_to_head = git_diff(&GitDiffRequest {
            path: file.clone(),
            old: DiffEndpoint::Index,
            new: DiffEndpoint::Revision("HEAD".to_string()),
            ignore_whitespace: false,
        })
        .expect("index to HEAD");
        assert!(
            index_to_head
                .rows
                .iter()
                .any(|row| row.left.as_ref().is_some_and(|line| line.text == "two"))
        );
        assert!(
            index_to_head
                .rows
                .iter()
                .any(|row| row.right.as_ref().is_some_and(|line| line.text == "one"))
        );

        let worktree_to_head = git_diff(&GitDiffRequest {
            path: file.clone(),
            old: DiffEndpoint::WorkingTree,
            new: DiffEndpoint::Revision("HEAD".to_string()),
            ignore_whitespace: false,
        })
        .expect("work tree to HEAD");
        assert!(
            worktree_to_head
                .rows
                .iter()
                .any(|row| row.left.as_ref().is_some_and(|line| line.text == "three"))
        );

        git_ok(&root, &["commit", "--quiet", "-m", "two"]);
        let revisions = git_diff(&GitDiffRequest {
            path: file,
            old: DiffEndpoint::Revision("HEAD~1".to_string()),
            new: DiffEndpoint::Revision("HEAD".to_string()),
            ignore_whitespace: false,
        })
        .expect("two revisions");
        assert!(revisions.stats.additions > 0 || revisions.stats.deletions > 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ignore_whitespace_removes_whitespace_only_changes() {
        if !git_available() {
            return;
        }
        let root = temp_repo("whitespace");
        let file = root.join("a.txt");
        std::fs::write(&file, "a b\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        git_ok(&root, &["commit", "--quiet", "-m", "one"]);
        std::fs::write(&file, "a  b\n").unwrap();

        let normal = git_diff(&GitDiffRequest::working_tree(file.clone())).expect("normal diff");
        assert!(!normal.rows.is_empty());
        let ignored = git_diff(&GitDiffRequest {
            path: file,
            old: DiffEndpoint::Revision("HEAD".to_string()),
            new: DiffEndpoint::WorkingTree,
            ignore_whitespace: true,
        })
        .expect("ignored whitespace");
        assert_eq!(ignored.stats.additions, 0);
        assert_eq!(ignored.stats.deletions, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shows_an_untracked_file_as_an_addition() {
        if !git_available() {
            return;
        }
        let root = temp_repo("untracked");
        std::fs::write(root.join("tracked.txt"), "tracked\n").unwrap();
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        let file = root.join("new.txt");
        std::fs::write(&file, "alpha\nbeta\n").unwrap();
        let diff = git_diff(&GitDiffRequest::working_tree(file)).expect("untracked diff");
        assert_eq!(diff.stats.additions, 2);
        assert!(
            diff.rows
                .iter()
                .any(|row| row.right.as_ref().is_some_and(|line| line.text == "alpha"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bounds_git_output_without_waiting_for_a_long_line() {
        if !git_available() {
            return;
        }
        let root = temp_repo("bounded-output");
        std::fs::write(root.join("one-line.txt"), "x".repeat(2 * 1024 * 1024)).unwrap();
        git_ok(&root, &["add", "one-line.txt"]);
        git_ok(&root, &["commit", "--quiet", "-m", "large line"]);
        let args = ["show", "HEAD:one-line.txt"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let output = run_git_bounded(&root, &args, 4096, 1).expect("bounded output");
        assert!(output.truncated);
        assert_eq!(output.stdout.len(), 4096);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bounds_ten_megabyte_changes() {
        if !git_available() {
            return;
        }
        let root = temp_repo("large");
        let file = root.join("large.txt");
        let content = (0..400_000)
            .map(|index| format!("line {index:06} 0123456789abcdef\n"))
            .collect::<String>();
        assert!(content.len() >= 10 * 1024 * 1024);
        std::fs::write(&file, content).unwrap();
        let diff = git_diff(&GitDiffRequest::working_tree(file)).expect("large diff");
        assert!(diff.large_file);
        assert!(diff.truncated);
        assert!(
            diff.stats
                .new_bytes
                .is_some_and(|bytes| bytes >= 10 * 1024 * 1024)
        );
        assert!(diff.rows.len() <= LARGE_MAX_DIFF_LINES + 4);
        let _ = std::fs::remove_dir_all(root);
    }
}
