//! Paginated Git history and deterministic commit-graph layout.
//!
//! `git log` supplies commits in display order. The layout cursor
//! keeps the active lanes between pages so the Windows and macOS clients draw
//! exactly the same graph without re-reading the history from the beginning.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::{GitCommandResult, GitService};

const MAX_LOG_PAGE_SIZE: u32 = 1_000;
const GRAPH_COLOR_COUNT: u32 = 10;
const LOG_FORMAT: &str = "%H%x00%h%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%D%x00%s%x1e";
const DETAILS_FORMAT: &str = "%H%x00%h%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%D%x00%s%x00%B%x00%G?%x00%GS%x00%GK%x1e";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitRefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRef {
    pub name: String,
    pub full_name: String,
    pub kind: GitRefKind,
    pub is_head: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitLogContext {
    pub repository_root: PathBuf,
    pub current_branch: Option<String>,
    pub head_hash: Option<String>,
    pub head_short_hash: Option<String>,
    pub refs: Vec<GitRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLogFilter {
    pub branch: Option<String>,
    pub author: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub path: Option<String>,
    pub text: Option<String>,
    pub regex: bool,
    pub hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitGraphEdge {
    pub from_lane: u32,
    pub to_lane: u32,
    pub color_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitGraphRow {
    pub lane: u32,
    pub lane_count: u32,
    pub color_index: u32,
    pub edges: Vec<GitGraphEdge>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitLogCommit {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub committed_at: i64,
    pub subject: String,
    pub refs: Vec<GitRef>,
    pub graph: GitGraphRow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitLogPage {
    pub repository_root: PathBuf,
    pub commits: Vec<GitLogCommit>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
    pub loaded_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSignature {
    pub status_code: String,
    pub signer: Option<String>,
    pub key: Option<String>,
}

impl GitSignature {
    pub fn is_signed(&self) -> bool {
        !self.status_code.is_empty() && self.status_code != "N"
    }

    pub fn status_label(&self) -> &'static str {
        match self.status_code.as_str() {
            "G" => "签名有效",
            "B" => "签名无效",
            "U" => "签名有效性未知",
            "X" => "签名已过期",
            "Y" => "签名密钥已过期",
            "R" => "签名密钥已撤销",
            "E" => "缺少签名密钥",
            "N" | "" => "未签名",
            _ => "无法验证签名",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangedFile {
    pub status: String,
    pub path: String,
    pub old_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitDetails {
    pub commit: GitLogCommit,
    pub body: String,
    pub signature: GitSignature,
    pub branches_containing: Vec<String>,
    pub changed_files: Vec<GitChangedFile>,
    pub cherry_picked: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTreeEntry {
    pub path: String,
    pub name: String,
    pub mode: String,
    pub object_hash: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitLogLoadMetrics {
    pub command_millis: u64,
    pub parse_millis: u64,
    pub layout_millis: u64,
    pub commit_count: usize,
    pub lane_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitLogPageWithMetrics {
    pub page: GitLogPage,
    pub metrics: GitLogLoadMetrics,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GraphLane {
    pending_hash: String,
    color_index: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct GraphCursor {
    repository_root: String,
    filter: GitLogFilter,
    offset: u64,
    next_color: u32,
    lanes: Vec<GraphLane>,
}

#[derive(Clone, Debug)]
struct RawLogCommit {
    hash: String,
    short_hash: String,
    parents: Vec<String>,
    author_name: String,
    author_email: String,
    authored_at: i64,
    committer_name: String,
    committer_email: String,
    committed_at: i64,
    subject: String,
    refs: Vec<GitRef>,
}

pub fn git_log_context(path: &Path) -> Result<GitLogContext, String> {
    let root = resolve_repository(path)?;
    let current_branch = git_text_optional(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let head_hash = git_text_optional(&root, &["rev-parse", "--verify", "HEAD"]);
    let head_short_hash = git_text_optional(&root, &["rev-parse", "--short=7", "HEAD"]);
    let refs = load_refs(&root, current_branch.as_deref())?;
    Ok(GitLogContext {
        repository_root: root,
        current_branch,
        head_hash,
        head_short_hash,
        refs,
    })
}

pub fn git_log_page(
    path: &Path,
    filter: GitLogFilter,
    cursor: Option<&str>,
    page_size: u32,
) -> Result<GitLogPage, String> {
    git_log_page_with_metrics(path, filter, cursor, page_size).map(|result| result.page)
}

pub fn git_log_page_with_metrics(
    path: &Path,
    filter: GitLogFilter,
    cursor: Option<&str>,
    page_size: u32,
) -> Result<GitLogPageWithMetrics, String> {
    let root = resolve_repository(path)?;
    let page_size = page_size.clamp(1, MAX_LOG_PAGE_SIZE);
    let mut graph = match cursor {
        Some(cursor) if !cursor.trim().is_empty() => {
            let parsed: GraphCursor = serde_json::from_str(cursor)
                .map_err(|_| "提交历史游标无效，请重新加载".to_string())?;
            if parsed.repository_root != root.to_string_lossy() {
                return Err("提交历史游标属于另一个仓库".to_string());
            }
            if parsed.filter != filter {
                return Err("筛选条件已变化，请重新加载提交历史".to_string());
            }
            parsed
        }
        _ => GraphCursor {
            repository_root: root.to_string_lossy().into_owned(),
            filter: filter.clone(),
            ..GraphCursor::default()
        },
    };

    let command_started = std::time::Instant::now();
    let output = run_log_command(&root, &filter, &graph, page_size)?;
    let command_millis = command_started.elapsed().as_millis() as u64;

    let parse_started = std::time::Instant::now();
    let mut commits = parse_log_output(output.stdout.as_bytes());
    let has_more = commits.len() > page_size as usize;
    if has_more {
        commits.truncate(page_size as usize);
    }
    let parse_millis = parse_started.elapsed().as_millis() as u64;

    let layout_started = std::time::Instant::now();
    let mut rows = Vec::with_capacity(commits.len());
    for commit in &commits {
        rows.push(layout_commit(&mut graph, commit));
    }
    let layout_millis = layout_started.elapsed().as_millis() as u64;
    graph.offset += commits.len() as u64;

    let commits = commits
        .into_iter()
        .zip(rows)
        .map(|(commit, graph)| GitLogCommit {
            hash: commit.hash,
            short_hash: commit.short_hash,
            parents: commit.parents,
            author_name: commit.author_name,
            author_email: commit.author_email,
            authored_at: commit.authored_at,
            committer_name: commit.committer_name,
            committer_email: commit.committer_email,
            committed_at: commit.committed_at,
            subject: commit.subject,
            refs: commit.refs,
            graph,
        })
        .collect::<Vec<_>>();

    let loaded_count = graph.offset;
    let lane_count = graph.lanes.len();
    let next_cursor = has_more.then(|| serde_json::to_string(&graph).unwrap_or_default());
    Ok(GitLogPageWithMetrics {
        page: GitLogPage {
            repository_root: root,
            commits,
            has_more,
            next_cursor,
            loaded_count,
        },
        metrics: GitLogLoadMetrics {
            command_millis,
            parse_millis,
            layout_millis,
            commit_count: loaded_count as usize,
            lane_count,
        },
    })
}

pub fn git_commit_details(path: &Path, hash: &str) -> Result<GitCommitDetails, String> {
    let root = resolve_repository(path)?;
    let revision = validate_commit(&root, hash)?;
    let output = run_git(
        &root,
        &[
            "show",
            "-s",
            &format!("--format={DETAILS_FORMAT}"),
            &revision,
        ],
    )?;
    let text = output.stdout.as_str();
    let record = text.trim_end_matches(['\r', '\n', '\x1e']);
    let mut fields = record.splitn(15, '\0');
    let hash = required_field(&mut fields, "提交哈希")?;
    let short_hash = required_field(&mut fields, "短哈希")?;
    let parents = split_parents(&required_field(&mut fields, "父提交")?);
    let author_name = required_field(&mut fields, "作者")?;
    let author_email = required_field(&mut fields, "作者邮箱")?;
    let authored_at = parse_unix(&required_field(&mut fields, "作者时间")?)?;
    let committer_name = required_field(&mut fields, "提交者")?;
    let committer_email = required_field(&mut fields, "提交者邮箱")?;
    let committed_at = parse_unix(&required_field(&mut fields, "提交时间")?)?;
    let decorations = fields.next().unwrap_or_default();
    let subject = required_field(&mut fields, "提交标题")?;
    let body = fields.next().unwrap_or_default().trim().to_string();
    let signature_code = fields.next().unwrap_or("N").trim().to_string();
    let signer = nonempty(fields.next().unwrap_or_default());
    let key = nonempty(fields.next().unwrap_or_default());
    let refs = refs_from_decorations(decorations);
    let commit = GitLogCommit {
        hash: hash.clone(),
        short_hash,
        parents,
        author_name,
        author_email,
        authored_at,
        committer_name,
        committer_email,
        committed_at,
        subject,
        refs,
        graph: GitGraphRow {
            lane: 0,
            lane_count: 1,
            color_index: 0,
            edges: Vec::new(),
        },
    };
    let branches_containing = branches_containing(&root, &revision);
    let changed_files = changed_files(&root, &revision)?;
    let cherry_picked = cherry_picked_status(&root, &revision);
    Ok(GitCommitDetails {
        commit,
        body,
        signature: GitSignature {
            status_code: signature_code,
            signer,
            key,
        },
        branches_containing,
        changed_files,
        cherry_picked,
    })
}

pub fn git_commit_tree(path: &Path, hash: &str) -> Result<Vec<GitTreeEntry>, String> {
    let root = resolve_repository(path)?;
    let revision = validate_commit(&root, hash)?;
    let output = run_git(
        &root,
        &["ls-tree", "-r", "-l", "-z", "--full-tree", &revision],
    )?;
    let text = output.stdout.as_str();
    let mut entries = Vec::new();
    for record in text.split('\0').filter(|record| !record.is_empty()) {
        let Some((metadata, path)) = record.split_once('\t') else {
            continue;
        };
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().unwrap_or_default().to_string();
        let _object_type = fields.next();
        let object_hash = fields.next().unwrap_or_default().to_string();
        let size = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        entries.push(GitTreeEntry {
            path: path.to_string(),
            name,
            mode,
            object_hash,
            size,
        });
    }
    Ok(entries)
}

pub fn git_file_history(
    path: &Path,
    relative_path: &str,
    cursor: Option<&str>,
    page_size: u32,
) -> Result<GitLogPage, String> {
    let filter = GitLogFilter {
        path: Some(relative_path.to_string()),
        ..GitLogFilter::default()
    };
    git_log_page(path, filter, cursor, page_size)
}

fn run_log_command(
    root: &Path,
    filter: &GitLogFilter,
    graph: &GraphCursor,
    page_size: u32,
) -> Result<GitCommandResult, String> {
    let max_count = page_size.saturating_add(1).to_string();
    let skip = graph.offset.to_string();
    let mut args = vec![
        "log".to_string(),
        // Topological ordering makes Git walk the entire history before it can
        // answer the first page. The default reverse-chronological order keeps
        // first paint interactive; parent hashes below still drive the graph.
        "--decorate=full".to_string(),
        format!("--max-count={max_count}"),
        format!("--skip={skip}"),
        format!("--format={LOG_FORMAT}"),
    ];

    if let Some(hash) = normalized(&filter.hash) {
        args.push(validate_commit(root, hash)?);
    } else if let Some(branch) = normalized(&filter.branch) {
        args.push(branch.to_string());
    } else {
        args.push("--all".to_string());
    }

    if let Some(author) = normalized(&filter.author) {
        args.push(format!("--author={author}"));
        args.push("--regexp-ignore-case".to_string());
    }
    if let Some(since) = normalized(&filter.since) {
        args.push(format!("--since={since}"));
    }
    if let Some(until) = normalized(&filter.until) {
        args.push(format!("--until={until}"));
    }
    if let Some(text) = normalized(&filter.text) {
        if filter.regex {
            args.push("--extended-regexp".to_string());
        } else {
            args.push("--fixed-strings".to_string());
        }
        args.push("--regexp-ignore-case".to_string());
        args.push(format!("--grep={text}"));
    }
    if let Some(pathspec) = normalized(&filter.path) {
        args.push("--".to_string());
        args.push(pathspec.to_string());
    }

    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    GitService::shared()
        .run_log_command(root, &refs)
        .map_err(|error| error.message)
}

fn layout_commit(graph: &mut GraphCursor, commit: &RawLogCommit) -> GitGraphRow {
    let old_lane_count = graph.lanes.len();
    let mut matching = graph
        .lanes
        .iter()
        .enumerate()
        .filter_map(|(index, lane)| (lane.pending_hash == commit.hash).then_some(index))
        .collect::<Vec<_>>();

    if matching.is_empty() {
        let lane = graph.lanes.len();
        let color = next_graph_color(graph);
        graph.lanes.push(GraphLane {
            pending_hash: commit.hash.clone(),
            color_index: color,
        });
        matching.push(lane);
    }

    let node_lane = matching[0];
    let node_color = graph.lanes[node_lane].color_index;
    let mut edges = Vec::new();

    // Multiple active lanes can wait for the same commit when branches merge.
    for source in matching.into_iter().skip(1).rev() {
        if source < graph.lanes.len() {
            let color = graph.lanes.remove(source).color_index;
            edges.push(GitGraphEdge {
                from_lane: source as u32,
                to_lane: node_lane as u32,
                color_index: color,
            });
        }
    }

    if let Some(first_parent) = commit.parents.first() {
        if let Some(target) = graph
            .lanes
            .iter()
            .enumerate()
            .position(|(index, lane)| index != node_lane && lane.pending_hash == *first_parent)
        {
            edges.push(GitGraphEdge {
                from_lane: node_lane as u32,
                to_lane: target as u32,
                color_index: node_color,
            });
            graph.lanes.remove(node_lane);
        } else if node_lane < graph.lanes.len() {
            graph.lanes[node_lane].pending_hash = first_parent.clone();
        }
    } else if node_lane < graph.lanes.len() {
        graph.lanes.remove(node_lane);
    }

    for parent in commit.parents.iter().skip(1) {
        if let Some(target) = graph
            .lanes
            .iter()
            .position(|lane| lane.pending_hash == *parent)
        {
            edges.push(GitGraphEdge {
                from_lane: node_lane as u32,
                to_lane: target as u32,
                color_index: node_color,
            });
        } else {
            let lane = graph.lanes.len();
            let color = next_graph_color(graph);
            graph.lanes.push(GraphLane {
                pending_hash: parent.clone(),
                color_index: color,
            });
            edges.push(GitGraphEdge {
                from_lane: node_lane as u32,
                to_lane: lane as u32,
                color_index: color,
            });
        }
    }

    // Draw every surviving lane as a continuous vertical edge.
    for (index, lane) in graph.lanes.iter().enumerate() {
        edges.push(GitGraphEdge {
            from_lane: index as u32,
            to_lane: index as u32,
            color_index: lane.color_index,
        });
    }

    deduplicate_edges(&mut edges);
    let lane_count = old_lane_count
        .max(graph.lanes.len())
        .max(node_lane.saturating_add(1))
        .max(1);
    GitGraphRow {
        lane: node_lane as u32,
        lane_count: lane_count as u32,
        color_index: node_color,
        edges,
    }
}

fn next_graph_color(graph: &mut GraphCursor) -> u32 {
    let color = graph.next_color % GRAPH_COLOR_COUNT;
    graph.next_color = graph.next_color.wrapping_add(1);
    color
}

fn deduplicate_edges(edges: &mut Vec<GitGraphEdge>) {
    let mut seen = HashSet::new();
    edges.retain(|edge| seen.insert((edge.from_lane, edge.to_lane)));
}

fn parse_log_output(output: &[u8]) -> Vec<RawLogCommit> {
    let text = String::from_utf8_lossy(output);
    text.split('\x1e')
        .filter_map(parse_log_record)
        .collect::<Vec<_>>()
}

fn parse_log_record(record: &str) -> Option<RawLogCommit> {
    let record = record.trim_matches(|character| matches!(character, '\r' | '\n' | '\x1e'));
    if record.is_empty() {
        return None;
    }
    let mut fields = record.splitn(11, '\0');
    let hash = fields.next()?.to_string();
    if hash.is_empty() {
        return None;
    }
    let short_hash = fields.next()?.to_string();
    let parents = split_parents(fields.next()?);
    let author_name = fields.next()?.to_string();
    let author_email = fields.next()?.to_string();
    let authored_at = fields.next()?.parse().ok()?;
    let committer_name = fields.next()?.to_string();
    let committer_email = fields.next()?.to_string();
    let committed_at = fields.next()?.parse().ok()?;
    let decorations = fields.next().unwrap_or_default();
    let subject = fields.next().unwrap_or_default().to_string();
    Some(RawLogCommit {
        hash,
        short_hash,
        parents,
        author_name,
        author_email,
        authored_at,
        committer_name,
        committer_email,
        committed_at,
        subject,
        refs: refs_from_decorations(decorations),
    })
}

fn split_parents(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|parent| !parent.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn refs_from_decorations(decorations: &str) -> Vec<GitRef> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for part in decorations
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if part == "HEAD" {
            continue;
        }
        if let Some(target) = part.strip_prefix("HEAD -> ") {
            let full_name = target.trim().to_string();
            let name = short_ref_name(&full_name);
            if seen.insert((name.clone(), full_name.clone())) {
                refs.push(GitRef {
                    name,
                    full_name,
                    kind: ref_kind_for(target.trim()),
                    is_head: true,
                });
            }
            continue;
        }
        let full_name = part
            .strip_prefix("tag: ")
            .unwrap_or(part)
            .trim()
            .to_string();
        if full_name.is_empty() {
            continue;
        }
        let name = short_ref_name(&full_name);
        if seen.insert((name.clone(), full_name.clone())) {
            refs.push(GitRef {
                name,
                full_name: full_name.clone(),
                kind: ref_kind_for(&full_name),
                is_head: false,
            });
        }
    }
    refs
}

fn ref_kind_for(full_name: &str) -> GitRefKind {
    if full_name.starts_with("refs/heads/") {
        GitRefKind::LocalBranch
    } else if full_name.starts_with("refs/remotes/") {
        GitRefKind::RemoteBranch
    } else if full_name.starts_with("refs/tags/") {
        GitRefKind::Tag
    } else {
        GitRefKind::Other
    }
}

fn short_ref_name(full_name: &str) -> String {
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        if let Some(name) = full_name.strip_prefix(prefix) {
            return name.to_string();
        }
    }
    full_name.to_string()
}

fn load_refs(root: &Path, current_branch: Option<&str>) -> Result<Vec<GitRef>, String> {
    let output = run_git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    let text = output.stdout.as_str();
    let mut refs = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\0');
        let full_name = fields.next().unwrap_or_default().trim();
        if full_name.is_empty() {
            continue;
        }
        let name = short_ref_name(full_name);
        let head_full_name = current_branch.map(|branch| format!("refs/heads/{branch}"));
        let is_head = head_full_name.as_deref() == Some(full_name);
        refs.push(GitRef {
            name,
            full_name: full_name.to_string(),
            kind: ref_kind_for(full_name),
            is_head,
        });
    }
    Ok(refs)
}

fn branches_containing(root: &Path, revision: &str) -> Vec<String> {
    run_git(
        root,
        &[
            "branch",
            "--all",
            "--contains",
            revision,
            "--format=%(refname:short)",
        ],
    )
    .ok()
    .map(|output| {
        output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

fn changed_files(root: &Path, revision: &str) -> Result<Vec<GitChangedFile>, String> {
    let output = run_git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "-r",
            "-m",
            "--first-parent",
            "--root",
            revision,
        ],
    )?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for line in output.stdout.lines() {
        let mut fields = line.split('\t');
        let status = fields.next().unwrap_or_default().to_string();
        if status.is_empty() {
            continue;
        }
        let Some(first_path) = fields.next() else {
            continue;
        };
        let second_path = fields.next();
        let (old_path, path) = match (status.as_bytes().first(), second_path) {
            (Some(b'R' | b'C'), Some(path)) => (Some(first_path.to_string()), path.to_string()),
            _ => (None, first_path.to_string()),
        };
        if seen.insert((status.clone(), path.clone())) {
            files.push(GitChangedFile {
                status,
                path,
                old_path,
            });
        }
    }
    Ok(files)
}

fn cherry_picked_status(root: &Path, revision: &str) -> Option<bool> {
    let upstream = git_text_optional(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    let output = run_git(
        root,
        &[
            "rev-list",
            "--cherry-mark",
            "--right-only",
            "--no-merges",
            &format!("{upstream}...HEAD"),
        ],
    )
    .ok()?;
    let text = output.stdout.as_str();
    for line in text.lines() {
        if let Some(hash) = line.strip_prefix('=') {
            if hash.trim() == revision {
                return Some(true);
            }
        } else if let Some(hash) = line.strip_prefix('+')
            && hash.trim() == revision
        {
            return Some(false);
        }
    }
    None
}

fn validate_commit(root: &Path, hash: &str) -> Result<String, String> {
    let hash = hash.trim();
    if hash.is_empty() {
        return Err("提交哈希不能为空".to_string());
    }
    let revision = format!("{hash}^{{commit}}");
    let output = run_git(root, &["rev-parse", "--verify", "--quiet", &revision])?;
    Ok(output.stdout.trim().to_string())
}

fn resolve_repository(path: &Path) -> Result<PathBuf, String> {
    let start = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let output = run_git(start, &["rev-parse", "--show-toplevel"])?;
    let root = output.stdout.trim().to_string();
    if root.is_empty() {
        return Err("不能确定 Git 工作区根目录".to_string());
    }
    Ok(dunce::canonicalize(&root).unwrap_or_else(|_| PathBuf::from(root)))
}

fn git_text_optional(root: &Path, args: &[&str]) -> Option<String> {
    let output = run_git(root, args).ok()?;
    nonempty(output.stdout)
}

fn normalized(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_field(fields: &mut std::str::SplitN<'_, char>, label: &str) -> Result<String, String> {
    fields
        .next()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("Git 提交缺少{label}"))
}

fn parse_unix(value: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| "Git 提交时间格式无效".to_string())
}

fn nonempty(value: impl AsRef<str>) -> Option<String> {
    let value = value.as_ref().trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn run_git(root: &Path, args: &[&str]) -> Result<GitCommandResult, String> {
    GitService::shared()
        .run_log_command(root, args)
        .map_err(|error| error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git_ok(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Kite Test")
            .env("GIT_AUTHOR_EMAIL", "kite@example.test")
            .env("GIT_COMMITTER_NAME", "Kite Test")
            .env("GIT_COMMITTER_EMAIL", "kite@example.test")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-git-log-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create repo");
        git_ok(&root, &["init", "--quiet", "-b", "main"]);
        git_ok(&root, &["config", "user.name", "Kite Test"]);
        git_ok(&root, &["config", "user.email", "kite@example.test"]);
        root
    }

    fn raw(hash: &str, parents: &[&str]) -> RawLogCommit {
        RawLogCommit {
            hash: hash.to_string(),
            short_hash: hash.chars().take(7).collect(),
            parents: parents.iter().map(|parent| parent.to_string()).collect(),
            author_name: "A".to_string(),
            author_email: "a@example.test".to_string(),
            authored_at: 0,
            committer_name: "C".to_string(),
            committer_email: "c@example.test".to_string(),
            committed_at: 0,
            subject: hash.to_string(),
            refs: Vec::new(),
        }
    }

    fn graph_signature(rows: &[GitGraphRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let mut edges = row
                    .edges
                    .iter()
                    .map(|edge| format!("{}>{}:{}", edge.from_lane, edge.to_lane, edge.color_index))
                    .collect::<Vec<_>>();
                edges.sort();
                format!(
                    "{}:{}:{}[{}]",
                    row.lane,
                    row.lane_count,
                    row.color_index,
                    edges.join("|")
                )
            })
            .collect()
    }

    fn layout_all(commits: &[RawLogCommit]) -> (Vec<GitGraphRow>, GraphCursor) {
        let mut cursor = GraphCursor::default();
        let rows = commits
            .iter()
            .map(|commit| layout_commit(&mut cursor, commit))
            .collect();
        (rows, cursor)
    }

    #[test]
    fn graph_snapshot_linear_history() {
        let commits = vec![raw("a", &["b"]), raw("b", &["c"]), raw("c", &[])];
        let (rows, cursor) = layout_all(&commits);
        assert_eq!(
            graph_signature(&rows),
            vec!["0:1:0[0>0:0]", "0:1:0[0>0:0]", "0:1:0[]"]
        );
        assert!(cursor.lanes.is_empty());
    }

    #[test]
    fn graph_snapshot_merge_history() {
        let commits = vec![
            raw("a", &["b", "c"]),
            raw("b", &["d"]),
            raw("c", &["d"]),
            raw("d", &[]),
        ];
        let (rows, cursor) = layout_all(&commits);
        assert_eq!(
            graph_signature(&rows),
            vec![
                "0:2:0[0>0:0|0>1:1|1>1:1]",
                "0:2:0[0>0:0|1>1:1]",
                "1:2:1[0>0:0|1>0:1]",
                "0:1:0[]",
            ]
        );
        assert!(cursor.lanes.is_empty());
    }

    #[test]
    fn graph_snapshot_octopus_merge() {
        let commits = vec![
            raw("a", &["b", "c", "d"]),
            raw("b", &["e"]),
            raw("c", &["e"]),
            raw("d", &["e"]),
            raw("e", &[]),
        ];
        let (rows, cursor) = layout_all(&commits);
        assert_eq!(rows[0].lane_count, 3);
        assert!(
            rows[0]
                .edges
                .iter()
                .any(|edge| edge.from_lane == 0 && edge.to_lane == 2)
        );
        assert!(cursor.lanes.is_empty());
    }

    #[test]
    fn graph_snapshot_multiple_roots() {
        let commits = vec![
            raw("a", &["b"]),
            raw("b", &[]),
            raw("c", &["d"]),
            raw("d", &[]),
        ];
        let (rows, cursor) = layout_all(&commits);
        assert_eq!(rows[1].lane_count, 1);
        assert_eq!(rows[2].lane, 0);
        assert!(cursor.lanes.is_empty());
    }

    #[test]
    fn graph_snapshot_long_branch() {
        let mut commits = Vec::new();
        for index in 0..64 {
            let hash = format!("{index:02x}");
            let parent = format!("{:02x}", index + 1);
            commits.push(raw(&hash, &[&parent]));
        }
        commits.push(raw("40", &[]));
        let (rows, cursor) = layout_all(&commits);
        assert_eq!(rows.len(), 65);
        assert!(rows.iter().all(|row| row.lane == 0 && row.lane_count == 1));
        assert!(cursor.lanes.is_empty());
    }

    #[test]
    fn graph_is_stable_when_split_across_pages() {
        let commits = vec![
            raw("a", &["b", "c"]),
            raw("b", &["d"]),
            raw("c", &["d"]),
            raw("d", &["e"]),
            raw("e", &[]),
        ];
        let (all_rows, _) = layout_all(&commits);
        let mut cursor = GraphCursor::default();
        let mut split_rows = Vec::new();
        for commit in &commits[..2] {
            split_rows.push(layout_commit(&mut cursor, commit));
        }
        let encoded = serde_json::to_string(&cursor).expect("serialize cursor");
        cursor = serde_json::from_str(&encoded).expect("deserialize cursor");
        for commit in &commits[2..] {
            split_rows.push(layout_commit(&mut cursor, commit));
        }
        assert_eq!(graph_signature(&all_rows), graph_signature(&split_rows));
    }

    #[test]
    fn parses_decorations_into_distinct_ref_kinds() {
        let refs = refs_from_decorations(
            "HEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1.0",
        );
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].kind, GitRefKind::LocalBranch);
        assert!(refs[0].is_head);
        assert_eq!(refs[1].kind, GitRefKind::RemoteBranch);
        assert_eq!(refs[2].kind, GitRefKind::Tag);
    }

    #[test]
    fn loads_paginated_log_and_commit_details() {
        if !git_available() {
            return;
        }
        let root = temp_repo("integration");
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        git_ok(&root, &["add", "a.txt"]);
        git_ok(&root, &["commit", "--quiet", "-m", "initial commit"]);
        std::fs::write(root.join("a.txt"), "two\n").expect("write");
        git_ok(&root, &["commit", "--quiet", "-am", "second commit"]);

        let text_filter = GitLogFilter {
            text: Some("second".to_string()),
            ..GitLogFilter::default()
        };
        let text_page = git_log_page(&root, text_filter, None, 10).expect("text filter");
        assert_eq!(text_page.commits.len(), 1);
        assert_eq!(text_page.commits[0].subject, "second commit");

        let regex_filter = GitLogFilter {
            text: Some("^sec.*commit$".to_string()),
            regex: true,
            ..GitLogFilter::default()
        };
        let regex_page = git_log_page(&root, regex_filter, None, 10).expect("regex filter");
        assert_eq!(regex_page.commits.len(), 1);

        let author_filter = GitLogFilter {
            author: Some("Kite Test".to_string()),
            ..GitLogFilter::default()
        };
        let author_page = git_log_page(&root, author_filter, None, 10).expect("author filter");
        assert_eq!(author_page.commits.len(), 2);

        let path_filter = GitLogFilter {
            path: Some("a.txt".to_string()),
            ..GitLogFilter::default()
        };
        let path_page = git_log_page(&root, path_filter, None, 10).expect("path filter");
        assert_eq!(path_page.commits.len(), 2);

        let branch_filter = GitLogFilter {
            branch: Some("main".to_string()),
            ..GitLogFilter::default()
        };
        let branch_page = git_log_page(&root, branch_filter, None, 10).expect("branch filter");
        assert_eq!(branch_page.commits.len(), 2);
        let context = git_log_context(&root).expect("context");
        assert_eq!(context.current_branch.as_deref(), Some("main"));
        assert!(
            context
                .refs
                .iter()
                .any(|item| item.name == "main" && item.is_head)
        );

        let first = git_log_page(&root, GitLogFilter::default(), None, 1).expect("first page");
        assert_eq!(first.commits.len(), 1);
        assert!(first.has_more);
        assert_eq!(first.commits[0].subject, "second commit");
        let cursor = first.next_cursor.expect("cursor");
        let second =
            git_log_page(&root, GitLogFilter::default(), Some(&cursor), 1).expect("second page");
        assert_eq!(second.commits[0].subject, "initial commit");
        assert!(!second.has_more);
        assert_eq!(first.commits[0].graph.lane_count, 1);
        assert_eq!(second.commits[0].graph.lane_count, 1);

        let details = git_commit_details(&root, &first.commits[0].hash).expect("details");
        assert_eq!(details.body, "second commit");
        assert_eq!(details.changed_files.len(), 1);
        assert_eq!(details.changed_files[0].path, "a.txt");

        let tree = git_commit_tree(&root, &first.commits[0].hash).expect("tree");
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].path, "a.txt");

        let history = git_file_history(&root, "a.txt", None, 10).expect("file history");
        assert_eq!(history.commits.len(), 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_cursor_for_another_filter() {
        if !git_available() {
            return;
        }
        let root = temp_repo("cursor");
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "one"]);
        std::fs::write(root.join("a.txt"), "two\n").expect("write");
        git_ok(&root, &["commit", "--quiet", "-am", "two"]);
        let first = git_log_page(&root, GitLogFilter::default(), None, 1).expect("first");
        let cursor = first.next_cursor.expect("cursor");
        let filter = GitLogFilter {
            author: Some("nobody".to_string()),
            ..GitLogFilter::default()
        };
        let error = git_log_page(&root, filter, Some(&cursor), 1).expect_err("filter mismatch");
        assert!(error.contains("筛选条件"));
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    #[ignore = "manual 100k-commit performance benchmark"]
    fn benchmark_large_history() {
        if !git_available() {
            return;
        }
        let root = temp_repo("benchmark-100k");
        create_linear_history_fast(&root, 100_000);

        for (label, order) in [
            ("date-default", None),
            ("topo", Some("--topo-order")),
            ("date", Some("--date-order")),
            ("author-date", Some("--author-date-order")),
        ] {
            let mut command = Command::new("git");
            command.current_dir(&root).args([
                "log",
                "--all",
                "--max-count=201",
                "--format=%H%x00%P%x1e",
            ]);
            if let Some(order) = order {
                command.arg(order);
            }
            let started = std::time::Instant::now();
            let output = command.output().expect("benchmark git log");
            println!(
                "100k order-check {}: {}ms bytes={}",
                label,
                started.elapsed().as_millis(),
                output.stdout.len()
            );
        }
        let first_started = std::time::Instant::now();
        let first = git_log_page_with_metrics(&root, GitLogFilter::default(), None, 200)
            .expect("first page");
        let first_total = first_started.elapsed();
        let first_layout = first.metrics.layout_millis;
        println!(
            "100k first-page: total={}ms git={}ms parse={}ms layout={}ms lanes={} rows={}",
            first_total.as_millis(),
            first.metrics.command_millis,
            first.metrics.parse_millis,
            first.metrics.layout_millis,
            first.metrics.lane_count,
            first.page.commits.len()
        );

        let mut cursor = first.page.next_cursor;
        let mut page_timings = Vec::new();
        let mut layout_timings = Vec::new();
        for page_index in 0..10 {
            let Some(next_cursor) = cursor else { break };
            let started = std::time::Instant::now();
            let page =
                git_log_page_with_metrics(&root, GitLogFilter::default(), Some(&next_cursor), 200)
                    .expect("next page");
            page_timings.push(started.elapsed().as_micros() as u64);
            layout_timings.push(page.metrics.layout_millis);
            cursor = page.page.next_cursor;
            println!(
                "100k scroll-page {}: total={}us layout={}ms",
                page_index + 1,
                page_timings.last().copied().unwrap_or(0),
                page.metrics.layout_millis
            );
        }
        page_timings.sort_unstable();
        layout_timings.sort_unstable();
        println!(
            "100k scroll-pages: count={} p50={}us p95={}us max={}us first-layout={}ms",
            page_timings.len(),
            percentile(&page_timings, 50),
            percentile(&page_timings, 95),
            page_timings.last().copied().unwrap_or(0),
            first_layout
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn create_linear_history_fast(root: &Path, commits: usize) {
        use std::io::Write as _;
        use std::process::Stdio;

        let mut child = Command::new("git")
            .current_dir(root)
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start git fast-import");
        let mut stdin = child.stdin.take().expect("fast-import stdin");
        let mut buffer = String::with_capacity(commits * 220);
        for index in 0..commits {
            let message = format!("benchmark commit {index}");
            let content = format!("{index}\n");
            buffer.push_str(&format!(
                "commit refs/heads/main\nmark :{}\ncommitter Kite Bench <bench@example.test> {} +0000\ndata {}\n{}\n",
                index + 1,
                1_700_000_000_u64 + index as u64,
                message.len(),
                message
            ));
            if index > 0 {
                buffer.push_str(&format!("from :{index}\n"));
            }
            buffer.push_str(&format!(
                "M 100644 inline file.txt\ndata {}\n{}\n",
                content.len(),
                content
            ));
        }
        stdin
            .write_all(buffer.as_bytes())
            .expect("write fast-import");
        drop(stdin);
        let output = child.wait_with_output().expect("finish fast-import");
        assert!(
            output.status.success(),
            "fast-import failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        git_ok(root, &["reset", "--hard", "main"]);
    }

    fn percentile(values: &[u64], percentile: usize) -> u64 {
        if values.is_empty() {
            return 0;
        }
        let index = ((values.len() - 1) * percentile / 100).min(values.len() - 1);
        values[index]
    }
}
