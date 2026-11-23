//! Merge, conflict inspection, and three-way resolution.
//!
//! Commands use the shared Git executor so environment hardening, structured
//! errors, and console logging stay consistent with the rest of the service.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use super::GitService;
use super::command::{CancellationToken, GitCommandOptions, GitCommandResult};
use super::error::{GitError, GitErrorKind};
use super::events::{GitOperationState, NoopGitEventSink};
use super::repository::{GitRepository, discover_repository, operation_state};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitMergeStrategy {
    Merge,
    NoFastForward,
    FastForwardOnly,
    Squash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeSpec {
    pub target: String,
    pub strategy: GitMergeStrategy,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitConflictKind {
    BothModified,
    ModifyDeleteOurs,
    ModifyDeleteTheirs,
    AddAdd,
    Rename,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitConflictEntry {
    pub path: String,
    pub base_path: Option<String>,
    pub ours_path: Option<String>,
    pub theirs_path: Option<String>,
    pub kind: GitConflictKind,
    pub base_present: bool,
    pub ours_present: bool,
    pub theirs_present: bool,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitConflictState {
    pub repository_root: PathBuf,
    pub operation: Option<GitOperationState>,
    pub entries: Vec<GitConflictEntry>,
    pub can_continue: bool,
    pub can_abort: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeOutcome {
    pub message: String,
    pub conflicts: Option<GitConflictState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitConflictSide {
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitConflictSides {
    pub base_text: Option<String>,
    pub ours_text: Option<String>,
    pub theirs_text: Option<String>,
    pub base_present: bool,
    pub ours_present: bool,
    pub theirs_present: bool,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitMergeLineKind {
    Context,
    Ours,
    Base,
    Theirs,
    Marker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeLine {
    pub kind: GitMergeLineKind,
    pub text: String,
    pub conflict_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitMergeConflictHunk {
    pub index: u32,
    pub base_text: String,
    pub ours_text: String,
    pub theirs_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitThreeWayMerge {
    pub merged_text: String,
    pub lines: Vec<GitMergeLine>,
    pub conflicts: Vec<GitMergeConflictHunk>,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitConflictChoiceSide {
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitConflictChoice {
    pub index: u32,
    pub side: GitConflictChoiceSide,
}

#[derive(Clone, Debug)]
struct GitStageEntry {
    oid: String,
    stage: u8,
    path: String,
}

impl GitService {
    pub fn merge(&self, path: &Path, spec: &GitMergeSpec) -> Result<GitMergeOutcome, GitError> {
        self.require_supported_version()?;
        let repository = self.discover_repository(path)?;
        let target = spec.target.trim();
        if target.is_empty() {
            return Err(invalid("请选择要合并的分支或提交", &repository));
        }
        if target.starts_with('-') {
            return Err(invalid("无效的合并目标", &repository));
        }
        let verify_args = vec![
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(format!("{target}^{{commit}}")),
        ];
        if run(self, &repository.work_tree, verify_args, "校验合并目标").is_err() {
            return Err(invalid(format!("找不到合并目标：{target}"), &repository));
        }

        let message = spec
            .message
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut args = vec![OsString::from("merge"), OsString::from("--no-edit")];
        match spec.strategy {
            GitMergeStrategy::Merge => {}
            GitMergeStrategy::NoFastForward => args.push(OsString::from("--no-ff")),
            GitMergeStrategy::FastForwardOnly => args.push(OsString::from("--ff-only")),
            GitMergeStrategy::Squash => args.push(OsString::from("--squash")),
        }
        if let Some(message) = message
            && spec.strategy != GitMergeStrategy::Squash
        {
            args.push(OsString::from("-m"));
            args.push(OsString::from(message));
        }
        args.push(OsString::from(target));
        let merge_result = run(self, &repository.work_tree, args, "Git 合并");
        let state = conflicts_for_repository(self, &repository)?;
        if !state.entries.is_empty() {
            return Ok(GitMergeOutcome {
                message: format!(
                    "合并产生 {} 个冲突，请在冲突解决视图中处理",
                    state.entries.len()
                ),
                conflicts: Some(state),
            });
        }
        let result = merge_result?;

        if spec.strategy == GitMergeStrategy::Squash {
            if let Some(message) = message {
                let commit = run(
                    self,
                    &repository.work_tree,
                    vec![
                        OsString::from("commit"),
                        OsString::from("-m"),
                        OsString::from(message),
                    ],
                    "提交压缩合并",
                )?;
                return Ok(GitMergeOutcome {
                    message: format!("已压缩合并并提交：{}", first_nonempty_line(&commit.stdout)),
                    conflicts: None,
                });
            }
            return Ok(GitMergeOutcome {
                message: "已压缩合并到暂存区，提交消息留空所以尚未提交".to_string(),
                conflicts: None,
            });
        }

        let summary = first_nonempty_line(&result.stdout);
        Ok(GitMergeOutcome {
            message: if summary.is_empty() {
                "合并完成".to_string()
            } else {
                summary
            },
            conflicts: None,
        })
    }

    pub fn conflicts(&self, path: &Path) -> Result<Option<GitConflictState>, GitError> {
        let repository = match discover_repository(self.executor(), path) {
            Ok(repository) => repository,
            Err(error) if error.kind == GitErrorKind::NotRepository => return Ok(None),
            Err(error) => return Err(error),
        };
        conflicts_for_repository(self, &repository).map(Some)
    }

    pub fn conflict_sides(
        &self,
        path: &Path,
        conflict: &GitConflictEntry,
    ) -> Result<GitConflictSides, GitError> {
        let repository = self.discover_repository(path)?;
        let stages = grouped_stage_entries(self, &repository)?;
        let base = stage_oid(&stages, 1, conflict.base_path.as_deref())
            .and_then(|oid| read_blob(self, &repository, &oid).ok());
        let ours = stage_oid(&stages, 2, conflict.ours_path.as_deref())
            .and_then(|oid| read_blob(self, &repository, &oid).ok());
        let theirs = stage_oid(&stages, 3, conflict.theirs_path.as_deref())
            .and_then(|oid| read_blob(self, &repository, &oid).ok());
        let binary = [base.as_deref(), ours.as_deref(), theirs.as_deref()]
            .into_iter()
            .flatten()
            .any(is_binary);
        Ok(GitConflictSides {
            base_text: if binary {
                None
            } else {
                base.as_deref().map(text_from_bytes)
            },
            ours_text: if binary {
                None
            } else {
                ours.as_deref().map(text_from_bytes)
            },
            theirs_text: if binary {
                None
            } else {
                theirs.as_deref().map(text_from_bytes)
            },
            base_present: base.is_some(),
            ours_present: ours.is_some(),
            theirs_present: theirs.is_some(),
            binary,
        })
    }

    pub fn three_way_merge(
        &self,
        path: &Path,
        conflict: &GitConflictEntry,
    ) -> Result<GitThreeWayMerge, GitError> {
        if conflict.kind == GitConflictKind::Rename {
            return Err(invalid_path("重命名冲突请选择保留自己、对方或两者", path));
        }
        let sides = self.conflict_sides(path, conflict)?;
        if sides.binary {
            return Ok(GitThreeWayMerge {
                merged_text: String::new(),
                lines: Vec::new(),
                conflicts: Vec::new(),
                binary: true,
            });
        }

        let directory = temporary_git_directory("merge-file");
        let ours_path = directory.join("ours.txt");
        let base_path = directory.join("base.txt");
        let theirs_path = directory.join("theirs.txt");
        std::fs::write(&ours_path, sides.ours_text.as_deref().unwrap_or_default())?;
        std::fs::write(&base_path, sides.base_text.as_deref().unwrap_or_default())?;
        std::fs::write(
            &theirs_path,
            sides.theirs_text.as_deref().unwrap_or_default(),
        )?;
        let args = vec![
            OsString::from("merge-file"),
            OsString::from("--diff3"),
            OsString::from("-L"),
            OsString::from("ours"),
            OsString::from("-L"),
            OsString::from("base"),
            OsString::from("-L"),
            OsString::from("theirs"),
            ours_path.clone().into_os_string(),
            base_path.into_os_string(),
            theirs_path.into_os_string(),
        ];
        let result = run(self, &directory, args, "Git 三方合并");
        let merged_text = std::fs::read_to_string(&ours_path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&directory);
        match result {
            Ok(_) => {}
            Err(error)
                if error.exit_code.is_some_and(|code| (1..128).contains(&code))
                    && !merged_text.is_empty() => {}
            Err(error) => return Err(error),
        }
        let (lines, conflicts) = parse_three_way_merge(&merged_text);
        Ok(GitThreeWayMerge {
            merged_text,
            lines,
            conflicts,
            binary: false,
        })
    }

    pub fn resolve_conflict(
        &self,
        path: &Path,
        relative_path: &str,
        content: Option<&str>,
    ) -> Result<(), GitError> {
        let repository = self.discover_repository(path)?;
        let relative = validate_relative_path(relative_path, &repository)?;
        let target = repository.work_tree.join(&relative);
        if let Some(content) = content {
            if contains_conflict_markers(content) {
                return Err(invalid(
                    "合并结果中仍有冲突标记，请先解决所有冲突块",
                    &repository,
                ));
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, content)?;
        } else if target.exists() {
            std::fs::remove_file(&target)?;
        }
        let args = vec![
            OsString::from("add"),
            OsString::from("-A"),
            OsString::from("--"),
            relative.into_os_string(),
        ];
        run(self, &repository.work_tree, args, "暂存冲突解决结果")?;
        Ok(())
    }

    pub fn choose_conflict_side(
        &self,
        path: &Path,
        conflict: &GitConflictEntry,
        side: GitConflictSide,
    ) -> Result<(), GitError> {
        let repository = self.discover_repository(path)?;
        if conflict.kind == GitConflictKind::Rename {
            let ours = conflict
                .ours_path
                .as_deref()
                .ok_or_else(|| invalid("重命名冲突缺少自己的版本", &repository))?;
            let theirs = conflict
                .theirs_path
                .as_deref()
                .ok_or_else(|| invalid("重命名冲突缺少对方的版本", &repository))?;
            let base = conflict.base_path.as_deref().unwrap_or(ours);
            match side {
                GitConflictSide::Ours => {
                    stage_delete(self, &repository, &[base, theirs])?;
                    write_stage_to_path(self, &repository, 2, ours)?;
                }
                GitConflictSide::Theirs => {
                    stage_delete(self, &repository, &[base, ours])?;
                    write_stage_to_path(self, &repository, 3, theirs)?;
                }
                GitConflictSide::Both => {
                    stage_delete(self, &repository, &[base])?;
                    write_stage_to_path(self, &repository, 2, ours)?;
                    write_stage_to_path(self, &repository, 3, theirs)?;
                }
            }
            return Ok(());
        }

        if side == GitConflictSide::Both {
            return Err(invalid(
                "同一路径不能同时保留双方，请在三方合并视图中逐块选择",
                &repository,
            ));
        }
        let (stage, target) = match side {
            GitConflictSide::Ours => (2, conflict.ours_path.as_deref()),
            GitConflictSide::Theirs => (3, conflict.theirs_path.as_deref()),
            GitConflictSide::Both => unreachable!(),
        };
        match target {
            Some(target) => write_stage_to_path(self, &repository, stage, target),
            None => {
                let target = conflict
                    .ours_path
                    .as_deref()
                    .or(conflict.theirs_path.as_deref())
                    .or(conflict.base_path.as_deref())
                    .unwrap_or(&conflict.path);
                stage_delete(self, &repository, &[target])
            }
        }
    }

    pub fn continue_operation(&self, path: &Path) -> Result<String, GitError> {
        let repository = self.discover_repository(path)?;
        let state = conflicts_for_repository(self, &repository)?;
        if !state.entries.is_empty() {
            return Err(GitError::new(
                GitErrorKind::Conflict,
                format!("仍有 {} 个冲突文件未解决", state.entries.len()),
            )
            .with_repository(repository.work_tree));
        }
        let args: Vec<OsString> = match state.operation {
            Some(GitOperationState::Merging) => {
                vec![OsString::from("commit"), OsString::from("--no-edit")]
            }
            Some(GitOperationState::Rebasing) => vec![
                OsString::from("-c"),
                OsString::from("core.editor=true"),
                OsString::from("-c"),
                OsString::from("sequence.editor=true"),
                OsString::from("rebase"),
                OsString::from("--continue"),
            ],
            Some(GitOperationState::CherryPicking) => vec![
                OsString::from("-c"),
                OsString::from("core.editor=true"),
                OsString::from("cherry-pick"),
                OsString::from("--continue"),
            ],
            Some(GitOperationState::Reverting) => vec![
                OsString::from("-c"),
                OsString::from("core.editor=true"),
                OsString::from("revert"),
                OsString::from("--continue"),
            ],
            Some(GitOperationState::Bisecting) => {
                return Err(invalid(
                    "当前正在进行 bisect，请先执行 git bisect reset",
                    &repository,
                ));
            }
            None => return Err(invalid("当前没有可继续的 Git 操作", &repository)),
        };
        let result = run(self, &repository.work_tree, args, "继续 Git 操作")?;
        let summary = first_nonempty_line(&result.stdout);
        Ok(if summary.is_empty() {
            "已继续 Git 操作".to_string()
        } else {
            summary
        })
    }

    pub fn abort_operation(&self, path: &Path) -> Result<String, GitError> {
        let repository = self.discover_repository(path)?;
        let state = conflicts_for_repository(self, &repository)?;
        let args: Vec<OsString> = match state.operation {
            Some(GitOperationState::Merging) => {
                vec![OsString::from("merge"), OsString::from("--abort")]
            }
            Some(GitOperationState::Rebasing) => {
                vec![OsString::from("rebase"), OsString::from("--abort")]
            }
            Some(GitOperationState::CherryPicking) => {
                vec![OsString::from("cherry-pick"), OsString::from("--abort")]
            }
            Some(GitOperationState::Reverting) => {
                vec![OsString::from("revert"), OsString::from("--abort")]
            }
            Some(GitOperationState::Bisecting) => {
                vec![OsString::from("bisect"), OsString::from("reset")]
            }
            None => return Err(invalid("当前没有可中止的 Git 操作", &repository)),
        };
        run(self, &repository.work_tree, args, "中止 Git 操作")?;
        Ok("已中止当前 Git 操作".to_string())
    }

    pub fn open_merge_tool(
        &self,
        path: &Path,
        conflict: &GitConflictEntry,
    ) -> Result<String, GitError> {
        let repository = self.discover_repository(path)?;
        let configured = run(
            self,
            &repository.work_tree,
            vec![
                OsString::from("config"),
                OsString::from("--get"),
                OsString::from("merge.tool"),
            ],
            "读取外部合并工具",
        )
        .map(|result| result.stdout.trim().to_string())
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            invalid(
                "未配置 merge.tool，请先在 Git 配置中选择外部合并工具",
                &repository,
            )
        })?;
        let target = conflict
            .ours_path
            .as_deref()
            .or(conflict.theirs_path.as_deref())
            .or(conflict.base_path.as_deref())
            .unwrap_or(&conflict.path);
        let relative = validate_relative_path(target, &repository)?;
        let args = vec![
            OsString::from("-c"),
            OsString::from("core.editor=true"),
            OsString::from("mergetool"),
            OsString::from("--no-prompt"),
            OsString::from("--tool"),
            OsString::from(&configured),
            OsString::from("--"),
            relative.into_os_string(),
        ];
        run(self, &repository.work_tree, args, "外部合并工具")?;
        Ok(format!("已使用 {configured} 处理 {target}"))
    }
}

pub fn apply_conflict_choices(merged_text: &str, choices: &[GitConflictChoice]) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Context,
        Ours(u32),
        Base(u32),
        Theirs(u32),
    }

    let ends_with_newline = merged_text.ends_with('\n');
    let uses_crlf = merged_text.contains("\r\n");
    let normalized = if uses_crlf {
        merged_text.replace("\r\n", "\n")
    } else {
        merged_text.to_string()
    };
    let mut state = State::Context;
    let mut output = Vec::new();
    let mut raw_block = Vec::new();
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    let mut conflict_index = 0u32;

    for line in normalized.lines() {
        match state {
            State::Context if line.starts_with("<<<<<<<") => {
                raw_block.push(line.to_string());
                state = State::Ours(conflict_index);
            }
            State::Ours(index) if line.starts_with("|||||||") => {
                raw_block.push(line.to_string());
                state = State::Base(index);
            }
            State::Ours(index) | State::Base(index) if line == "=======" => {
                raw_block.push(line.to_string());
                state = State::Theirs(index);
            }
            State::Theirs(index) if line.starts_with(">>>>>>>") => {
                raw_block.push(line.to_string());
                let choice = choices
                    .iter()
                    .rev()
                    .find(|choice| choice.index == index)
                    .map(|choice| choice.side);
                match choice {
                    Some(GitConflictChoiceSide::Ours) => output.extend(ours.iter().cloned()),
                    Some(GitConflictChoiceSide::Theirs) => output.extend(theirs.iter().cloned()),
                    Some(GitConflictChoiceSide::Both) => {
                        output.extend(ours.iter().cloned());
                        output.extend(theirs.iter().cloned());
                    }
                    None => output.extend(raw_block.iter().cloned()),
                }
                raw_block.clear();
                ours.clear();
                theirs.clear();
                conflict_index = conflict_index.saturating_add(1);
                state = State::Context;
            }
            State::Context => output.push(line.to_string()),
            State::Ours(index) => {
                ours.push(line.to_string());
                raw_block.push(line.to_string());
                state = State::Ours(index);
            }
            State::Base(index) => {
                raw_block.push(line.to_string());
                state = State::Base(index);
            }
            State::Theirs(index) => {
                theirs.push(line.to_string());
                raw_block.push(line.to_string());
                state = State::Theirs(index);
            }
        }
    }
    if !raw_block.is_empty() {
        output.extend(raw_block);
    }
    let mut result = output.join("\n");
    if ends_with_newline {
        result.push('\n');
    }
    if uses_crlf {
        result = result.replace('\n', "\r\n");
    }
    result
}

fn conflicts_for_repository(
    service: &GitService,
    repository: &GitRepository,
) -> Result<GitConflictState, GitError> {
    let grouped = grouped_stage_entries(service, repository)?;
    if grouped.is_empty() {
        return Ok(conflict_state(repository, Vec::new()));
    }

    let mut rename_paths = Vec::new();
    let mut entries = Vec::new();
    for [base, ours, theirs] in rename_conflict_groups(service, repository, &grouped)? {
        rename_paths.extend([base.clone(), ours.clone(), theirs.clone()]);
        let binary = [
            stage_oid(&grouped, 1, Some(&base)),
            stage_oid(&grouped, 2, Some(&ours)),
            stage_oid(&grouped, 3, Some(&theirs)),
        ]
        .into_iter()
        .flatten()
        .filter_map(|oid| read_blob(service, repository, &oid).ok())
        .any(|bytes| is_binary(&bytes));
        entries.push(GitConflictEntry {
            path: format!("{ours} ↔ {theirs}"),
            base_path: Some(base),
            ours_path: Some(ours),
            theirs_path: Some(theirs),
            kind: GitConflictKind::Rename,
            base_present: true,
            ours_present: true,
            theirs_present: true,
            binary,
        });
    }

    for (path, stages) in grouped {
        if rename_paths.contains(&path) {
            continue;
        }
        let base = stages.get(&1);
        let ours = stages.get(&2);
        let theirs = stages.get(&3);
        let kind = match (base.is_some(), ours.is_some(), theirs.is_some()) {
            (true, true, true) => GitConflictKind::BothModified,
            (true, true, false) => GitConflictKind::ModifyDeleteTheirs,
            (true, false, true) => GitConflictKind::ModifyDeleteOurs,
            (false, true, true) => GitConflictKind::AddAdd,
            _ => GitConflictKind::Other,
        };
        let binary = [base, ours, theirs]
            .into_iter()
            .flatten()
            .filter_map(|entry| read_blob(service, repository, &entry.oid).ok())
            .any(|bytes| is_binary(&bytes));
        entries.push(GitConflictEntry {
            base_path: base.map(|entry| entry.path.clone()),
            ours_path: ours.map(|entry| entry.path.clone()),
            theirs_path: theirs.map(|entry| entry.path.clone()),
            kind,
            base_present: base.is_some(),
            ours_present: ours.is_some(),
            theirs_present: theirs.is_some(),
            binary,
            path,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(conflict_state(repository, entries))
}

fn conflict_state(repository: &GitRepository, entries: Vec<GitConflictEntry>) -> GitConflictState {
    let operation = operation_state(&repository.git_dir);
    GitConflictState {
        repository_root: repository.work_tree.clone(),
        operation,
        entries,
        can_continue: matches!(
            operation,
            Some(
                GitOperationState::Merging
                    | GitOperationState::Rebasing
                    | GitOperationState::CherryPicking
                    | GitOperationState::Reverting
            )
        ),
        can_abort: operation.is_some(),
    }
}

fn stage_entries(
    service: &GitService,
    repository: &GitRepository,
) -> Result<Vec<GitStageEntry>, GitError> {
    let result = run(
        service,
        &repository.work_tree,
        vec![
            OsString::from("ls-files"),
            OsString::from("-u"),
            OsString::from("-z"),
        ],
        "读取 Git 冲突索引",
    )?;
    Ok(parse_stage_entries(result.stdout.as_bytes()))
}

fn grouped_stage_entries(
    service: &GitService,
    repository: &GitRepository,
) -> Result<BTreeMap<String, BTreeMap<u8, GitStageEntry>>, GitError> {
    Ok(stage_entries(service, repository)?.into_iter().fold(
        BTreeMap::new(),
        |mut grouped, entry| {
            grouped
                .entry(entry.path.clone())
                .or_default()
                .insert(entry.stage, entry);
            grouped
        },
    ))
}

fn parse_stage_entries(bytes: &[u8]) -> Vec<GitStageEntry> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            let (metadata, path) = record.split_once('\t')?;
            let fields = metadata.split_whitespace().collect::<Vec<_>>();
            let oid = fields.get(1)?.to_string();
            let stage = fields.get(2)?.parse().ok()?;
            Some(GitStageEntry {
                oid,
                stage,
                path: path.to_string(),
            })
        })
        .collect()
}

fn rename_conflict_groups(
    service: &GitService,
    repository: &GitRepository,
    grouped: &BTreeMap<String, BTreeMap<u8, GitStageEntry>>,
) -> Result<Vec<[String; 3]>, GitError> {
    let result = run(
        service,
        &repository.work_tree,
        vec![
            OsString::from("status"),
            OsString::from("--porcelain=v2"),
            OsString::from("-z"),
            OsString::from("--untracked-files=no"),
        ],
        "读取 Git 重命名冲突",
    )?;
    let paths = result
        .stdout
        .split('\0')
        .filter_map(|record| {
            record.strip_prefix("u ")?;
            record.splitn(11, ' ').nth(10).map(str::to_string)
        })
        .filter(|path| grouped.get(path).is_some_and(|stages| stages.len() == 1))
        .collect::<Vec<_>>();
    let mut groups = Vec::new();
    let mut index = 0;
    while index + 2 < paths.len() {
        let candidates = [&paths[index], &paths[index + 1], &paths[index + 2]];
        if candidates[0] != candidates[1]
            && candidates[0] != candidates[2]
            && candidates[1] != candidates[2]
        {
            let stages = candidates.map(|path| {
                grouped
                    .get(path)
                    .and_then(|stages| (stages.len() == 1).then(|| stages.keys().next().copied()))
                    .flatten()
            });
            if let [Some(first), Some(second), Some(third)] = stages
                && [first, second, third].contains(&1)
                && [first, second, third].contains(&2)
                && [first, second, third].contains(&3)
            {
                let base =
                    candidates[stages.iter().position(|stage| *stage == Some(1)).unwrap()].clone();
                let ours =
                    candidates[stages.iter().position(|stage| *stage == Some(2)).unwrap()].clone();
                let theirs =
                    candidates[stages.iter().position(|stage| *stage == Some(3)).unwrap()].clone();
                groups.push([base, ours, theirs]);
                index += 3;
                continue;
            }
        }
        index += 1;
    }
    Ok(groups)
}

fn stage_oid(
    stages: &BTreeMap<String, BTreeMap<u8, GitStageEntry>>,
    stage: u8,
    path: Option<&str>,
) -> Option<String> {
    stages
        .get(path?)?
        .get(&stage)
        .map(|entry| entry.oid.clone())
}

fn read_blob(
    _service: &GitService,
    repository: &GitRepository,
    oid: &str,
) -> Result<Vec<u8>, GitError> {
    let mut command = crate::vcs::configured_command("git", &repository.work_tree);
    command.args(["--no-optional-locks", "cat-file", "blob", oid]);
    let output = command.output().map_err(|error| {
        GitError::from(error)
            .with_command(format!("git cat-file blob {oid}"))
            .with_repository(repository.work_tree.clone())
    })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::classified(
            &String::from_utf8_lossy(&output.stderr),
            &String::from_utf8_lossy(&output.stdout),
            output.status.code(),
            format!("git cat-file blob {oid}"),
            repository.work_tree.clone(),
        ))
    }
}

fn write_stage_to_path(
    service: &GitService,
    repository: &GitRepository,
    stage: u8,
    path: &str,
) -> Result<(), GitError> {
    let grouped = grouped_stage_entries(service, repository)?;
    let oid = stage_oid(&grouped, stage, Some(path))
        .ok_or_else(|| invalid(format!("冲突文件 {path} 在索引中缺少对应版本"), repository))?;
    let bytes = read_blob(service, repository, &oid)?;
    let relative = validate_relative_path(path, repository)?;
    let target = repository.work_tree.join(&relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, bytes)?;
    run(
        service,
        &repository.work_tree,
        vec![
            OsString::from("add"),
            OsString::from("--"),
            relative.into_os_string(),
        ],
        "暂存冲突版本",
    )?;
    Ok(())
}

fn stage_delete(
    service: &GitService,
    repository: &GitRepository,
    paths: &[&str],
) -> Result<(), GitError> {
    let mut args = vec![
        OsString::from("rm"),
        OsString::from("-f"),
        OsString::from("--ignore-unmatch"),
        OsString::from("--"),
    ];
    for path in paths {
        args.push(validate_relative_path(path, repository)?.into_os_string());
    }
    run(service, &repository.work_tree, args, "删除冲突路径")?;
    Ok(())
}

fn parse_three_way_merge(text: &str) -> (Vec<GitMergeLine>, Vec<GitMergeConflictHunk>) {
    #[derive(Clone, Copy)]
    enum State {
        Context,
        Ours(u32),
        Base(u32),
        Theirs(u32),
    }

    let mut state = State::Context;
    let mut lines = Vec::new();
    let mut conflicts = Vec::new();
    let mut ours = Vec::new();
    let mut base = Vec::new();
    let mut theirs = Vec::new();
    let mut conflict_index = 0u32;
    for line in text.lines() {
        match state {
            State::Context if line.starts_with("<<<<<<<") => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(conflict_index),
                });
                state = State::Ours(conflict_index);
            }
            State::Ours(index) if line.starts_with("|||||||") => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Base(index);
            }
            State::Ours(index) | State::Base(index) if line == "=======" => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Theirs(index);
            }
            State::Theirs(index) if line.starts_with(">>>>>>>") => {
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Marker,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                conflicts.push(GitMergeConflictHunk {
                    index,
                    base_text: join_lines(&base),
                    ours_text: join_lines(&ours),
                    theirs_text: join_lines(&theirs),
                });
                ours.clear();
                base.clear();
                theirs.clear();
                conflict_index = conflict_index.saturating_add(1);
                state = State::Context;
            }
            State::Context => lines.push(GitMergeLine {
                kind: GitMergeLineKind::Context,
                text: line.to_string(),
                conflict_index: None,
            }),
            State::Ours(index) => {
                ours.push(line.to_string());
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Ours,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Ours(index);
            }
            State::Base(index) => {
                base.push(line.to_string());
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Base,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Base(index);
            }
            State::Theirs(index) => {
                theirs.push(line.to_string());
                lines.push(GitMergeLine {
                    kind: GitMergeLineKind::Theirs,
                    text: line.to_string(),
                    conflict_index: Some(index),
                });
                state = State::Theirs(index);
            }
        }
    }
    (lines, conflicts)
}

fn run(
    service: &GitService,
    directory: &Path,
    args: Vec<OsString>,
    label: &str,
) -> Result<GitCommandResult, GitError> {
    service.executor().run(
        directory,
        args,
        GitCommandOptions::new(label),
        &CancellationToken::default(),
        &NoopGitEventSink,
        None,
    )
}

fn validate_relative_path(path: &str, repository: &GitRepository) -> Result<PathBuf, GitError> {
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid("无效的 Git 路径", repository));
    }
    Ok(path)
}

fn invalid(message: impl Into<String>, repository: &GitRepository) -> GitError {
    GitError::new(GitErrorKind::InvalidOperation, message)
        .with_repository(repository.work_tree.clone())
}

fn invalid_path(message: impl Into<String>, path: &Path) -> GitError {
    GitError::new(GitErrorKind::InvalidOperation, message).with_repository(path)
}

fn join_lines(lines: &[String]) -> String {
    let mut value = lines.join("\n");
    if !value.is_empty() {
        value.push('\n');
    }
    value
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn text_from_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn contains_conflict_markers(content: &str) -> bool {
    content.lines().any(|line| {
        line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line.starts_with("|||||||")
    })
}

fn first_nonempty_line(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn temporary_git_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "kite-git-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&directory);
    directory
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Output};
    use std::time::SystemTime;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git_output(root: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git")
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = git_output(root, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-git-merge-{label}-{}-{}",
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

    fn git_status_text(root: &Path) -> String {
        String::from_utf8_lossy(&git_output(root, &["status", "--porcelain=v1"]).stdout)
            .into_owned()
    }

    #[test]
    fn parses_stage_entries_with_oids() {
        let entries = parse_stage_entries(
            b"100644 aaa 1\tbase.txt\0 100644 bbb 2\tours.txt\0 100644 ccc 3\ttheirs.txt\0",
        );
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].oid, "aaa");
        assert_eq!(entries[0].stage, 1);
        assert_eq!(entries[0].path, "base.txt");
    }

    #[test]
    fn parses_and_applies_conflict_choices() {
        let merged =
            "one\n<<<<<<< ours\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> theirs\ntwo\n";
        let (lines, conflicts) = parse_three_way_merge(merged);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].ours_text, "ours\n");
        assert_eq!(conflicts[0].theirs_text, "theirs\n");
        assert!(lines.iter().any(|line| {
            line.conflict_index == Some(0) && line.kind == GitMergeLineKind::Marker
        }));
        assert_eq!(
            apply_conflict_choices(
                merged,
                &[GitConflictChoice {
                    index: 0,
                    side: GitConflictChoiceSide::Theirs,
                }],
            ),
            "one\ntheirs\ntwo\n"
        );
        assert_eq!(
            apply_conflict_choices(
                merged,
                &[GitConflictChoice {
                    index: 0,
                    side: GitConflictChoiceSide::Both,
                }],
            ),
            "one\nours\ntheirs\ntwo\n"
        );
        assert_eq!(
            apply_conflict_choices(
                "one\r\n<<<<<<< ours\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> theirs\r\ntwo\r\n",
                &[GitConflictChoice {
                    index: 0,
                    side: GitConflictChoiceSide::Ours,
                }],
            ),
            "one\r\nours\r\ntwo\r\n"
        );
        assert!(contains_conflict_markers(
            "<<<<<<< ours\nours\n=======\ntheirs\n>>>>>>> theirs\n"
        ));
        assert!(!contains_conflict_markers(
            "Markdown divider\n=======\ntext\n"
        ));
    }

    #[test]
    fn resolves_modify_delete_binary_and_rename_conflicts() {
        if !git_available() {
            return;
        }
        let service = GitService::new();
        let root = temp_repo("conflicts");
        std::fs::write(root.join("modify-delete.txt"), "modify base\n").expect("write text base");
        std::fs::write(root.join("binary.bin"), [0, 1, b'b', b'a', b's', b'e'])
            .expect("write binary base");
        std::fs::write(root.join("original.txt"), "rename base\n").expect("write rename base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("modify-delete.txt"), "side\n").expect("modify side");
        std::fs::write(root.join("binary.bin"), [0, 2, b's', b'i', b'd', b'e'])
            .expect("write binary side");
        git_ok(&root, &["mv", "original.txt", "side-name.txt"]);
        std::fs::write(root.join("side-name.txt"), "rename base\nside\n").expect("rename side");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        std::fs::remove_file(root.join("modify-delete.txt")).expect("delete main file");
        std::fs::write(root.join("binary.bin"), [0, 3, b'm', b'a', b'i', b'n'])
            .expect("write binary main");
        git_ok(&root, &["mv", "original.txt", "main-name.txt"]);
        std::fs::write(root.join("main-name.txt"), "rename base\nmain\n").expect("rename main");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "main"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = service.conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.operation, Some(GitOperationState::Merging));
        assert_eq!(state.entries.len(), 3);
        let modify_delete = state
            .entries
            .iter()
            .find(|entry| entry.path == "modify-delete.txt")
            .expect("modify/delete conflict");
        assert_eq!(modify_delete.kind, GitConflictKind::ModifyDeleteOurs);
        service
            .choose_conflict_side(&root, modify_delete, GitConflictSide::Ours)
            .expect("delete side");
        assert!(!root.join("modify-delete.txt").exists());

        let binary = state
            .entries
            .iter()
            .find(|entry| entry.path == "binary.bin")
            .expect("binary conflict");
        assert!(binary.binary);
        service
            .choose_conflict_side(&root, binary, GitConflictSide::Theirs)
            .expect("choose binary side");
        assert_eq!(
            std::fs::read(root.join("binary.bin")).expect("read binary"),
            [0, 2, b's', b'i', b'd', b'e']
        );

        let rename = state
            .entries
            .iter()
            .find(|entry| entry.kind == GitConflictKind::Rename)
            .expect("rename conflict");
        service
            .choose_conflict_side(&root, rename, GitConflictSide::Ours)
            .expect("choose rename side");
        assert!(root.join("main-name.txt").exists());
        assert!(!root.join("side-name.txt").exists());
        assert!(
            service
                .conflicts(&root)
                .expect("conflicts")
                .expect("repo")
                .entries
                .is_empty()
        );
        assert!(!git_status_text(&root).contains("UU"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn groups_multiple_rename_conflicts_independently() {
        if !git_available() {
            return;
        }
        let service = GitService::new();
        let root = temp_repo("rename-multiple");
        for name in ["original-a.txt", "original-b.txt"] {
            std::fs::write(root.join(name), "base\n").expect("write base");
        }
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        for name in ["original-a", "original-b"] {
            git_ok(
                &root,
                &["mv", &format!("{name}.txt"), &format!("{name}-side.txt")],
            );
            std::fs::write(root.join(format!("{name}-side.txt")), "base\nside\n")
                .expect("modify side");
        }
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "side renames"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        for name in ["original-a", "original-b"] {
            git_ok(
                &root,
                &["mv", &format!("{name}.txt"), &format!("{name}-main.txt")],
            );
            std::fs::write(root.join(format!("{name}-main.txt")), "base\nmain\n")
                .expect("modify main");
        }
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "--quiet", "-m", "main renames"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());
        let state = service.conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.entries.len(), 2);
        assert!(
            state
                .entries
                .iter()
                .all(|entry| entry.kind == GitConflictKind::Rename)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn real_three_way_merge_updates_index_and_continues() {
        if !git_available() {
            return;
        }
        let service = GitService::new();
        let root = temp_repo("three-way");
        std::fs::write(root.join("f.txt"), "one\nbase\ntwo\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("f.txt"), "one\ntheirs\ntwo\n").expect("write side");
        git_ok(&root, &["commit", "--quiet", "-am", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        std::fs::write(root.join("f.txt"), "one\nours\ntwo\n").expect("write main");
        git_ok(&root, &["commit", "--quiet", "-am", "main"]);
        assert!(!git_output(&root, &["merge", "side"]).status.success());

        let state = service.conflicts(&root).expect("conflicts").expect("repo");
        let conflict = state.entries.first().expect("text conflict");
        let merge = service
            .three_way_merge(&root, conflict)
            .expect("three way merge");
        assert_eq!(merge.conflicts.len(), 1);
        let resolved = apply_conflict_choices(
            &merge.merged_text,
            &[GitConflictChoice {
                index: 0,
                side: GitConflictChoiceSide::Ours,
            }],
        );
        service
            .resolve_conflict(&root, &conflict.path, Some(&resolved))
            .expect("resolve");
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).expect("read result"),
            "one\nours\ntwo\n"
        );
        service.continue_operation(&root).expect("continue merge");
        assert_eq!(
            service
                .conflicts(&root)
                .expect("conflicts")
                .expect("repo")
                .operation,
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lists_stash_pop_conflicts_without_a_resumable_operation() {
        if !git_available() {
            return;
        }
        let service = GitService::new();
        let root = temp_repo("stash-pop");
        std::fs::write(root.join("f.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        std::fs::write(root.join("f.txt"), "stashed\n").expect("write stash");
        git_ok(&root, &["stash", "push", "--quiet", "-m", "stash fixture"]);
        std::fs::write(root.join("f.txt"), "committed\n").expect("write committed");
        git_ok(&root, &["commit", "--quiet", "-am", "committed"]);
        assert!(!git_output(&root, &["stash", "pop"]).status.success());
        let state = service.conflicts(&root).expect("conflicts").expect("repo");
        assert_eq!(state.operation, None);
        assert!(!state.can_continue);
        assert!(!state.entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clean_merge_accepts_a_custom_message() {
        if !git_available() {
            return;
        }
        let service = GitService::new();
        let root = temp_repo("merge-clean");
        std::fs::write(root.join("base.txt"), "base\n").expect("write base");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "base"]);
        git_ok(&root, &["checkout", "--quiet", "-b", "side"]);
        std::fs::write(root.join("side.txt"), "side\n").expect("write side");
        git_ok(&root, &["add", "."]);
        git_ok(&root, &["commit", "--quiet", "-m", "side"]);
        git_ok(&root, &["checkout", "--quiet", "main"]);
        let outcome = service
            .merge(
                &root,
                &GitMergeSpec {
                    target: "side".to_string(),
                    strategy: GitMergeStrategy::NoFastForward,
                    message: Some("custom merge message".to_string()),
                },
            )
            .expect("merge");
        assert!(outcome.conflicts.is_none());
        assert_eq!(
            String::from_utf8_lossy(&git_output(&root, &["log", "-1", "--format=%s"]).stdout)
                .trim(),
            "custom merge message"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
