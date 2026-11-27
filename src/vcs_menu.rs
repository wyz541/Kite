//! Fast, typed VCS state used by the canonical context-menu Composer.
//!
//! The badge-oriented [`crate::vcs::VcsSnapshot`] intentionally stays small.
//! Menu decisions need a richer, repository-scoped view, but opening a menu
//! must remain cheap: one Git porcelain-v2 status per repository, or one SVN
//! `info --xml` plus one `status --xml` per working copy.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

use roxmltree::Document;
use sha2::{Digest, Sha256};

use crate::git::{
    GitErrorKind, GitHead, GitOperationAction, GitOperationState, GitRepository, GitService,
    operation_state, run_git_operation_control,
};
use crate::git_changes::{self, GitChangeFile, GitChangeGroup, GitRepositoryChanges};
use crate::vcs::configured_command;

pub const VCS_MENU_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VcsMenuTool {
    Git,
    Svn,
}

impl VcsMenuTool {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Svn => "svn",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::Svn => "SVN",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VcsHeadKind {
    #[default]
    None,
    Branch,
    Detached,
    Unborn,
}

impl VcsHeadKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Branch => "branch",
            Self::Detached => "detached",
            Self::Unborn => "unborn",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VcsHeadState {
    pub kind: VcsHeadKind,
    pub name: Option<String>,
    pub oid: Option<String>,
    pub short_oid: Option<String>,
}

impl From<Option<GitHead>> for VcsHeadState {
    fn from(head: Option<GitHead>) -> Self {
        match head {
            Some(GitHead::Branch { name, oid }) => Self {
                kind: VcsHeadKind::Branch,
                short_oid: oid.as_deref().map(short_oid),
                name: Some(name),
                oid,
            },
            Some(GitHead::Detached { oid }) => Self {
                kind: VcsHeadKind::Detached,
                name: None,
                short_oid: Some(short_oid(&oid)),
                oid: Some(oid),
            },
            Some(GitHead::Unborn { name }) => Self {
                kind: VcsHeadKind::Unborn,
                name: Some(name),
                oid: None,
                short_oid: None,
            },
            None => Self::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VcsChangeSummary {
    /// `None` means the VCS has no index concept (for example SVN). Callers
    /// must not imply staged content when this is absent.
    pub staged: Option<u32>,
    pub unstaged: u32,
    pub untracked: u32,
    pub ignored: u32,
    pub conflicted: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum VcsOperationKind {
    #[default]
    Busy,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    CleanupRequired,
}

impl VcsOperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Bisect => "bisect",
            Self::CleanupRequired => "cleanup-required",
            Self::Busy => "busy",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Merge => "合并",
            Self::Rebase => "变基",
            Self::CherryPick => "拣选",
            Self::Revert => "还原提交",
            Self::Bisect => "二分查找",
            Self::CleanupRequired => "需要清理",
            Self::Busy => "仓库忙",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VcsOperationMenuState {
    pub kind: VcsOperationKind,
    pub label: String,
    pub continue_available: bool,
    pub skip_available: bool,
    pub abort_available: bool,
    pub resolve_available: bool,
    pub cleanup_available: bool,
    /// True when Kite has its own continuation state in
    /// `.git/kite-rebase-state.json`; dispatch must use `git_rebase_*` first.
    pub custom_rebase: bool,
}

impl VcsOperationMenuState {
    pub fn from_git(operation: Option<GitOperationState>, git_dir: &Path) -> Option<Self> {
        let custom_rebase = git_dir.join("kite-rebase-state.json").is_file();
        match operation {
            Some(GitOperationState::Merging) => Some(Self {
                kind: VcsOperationKind::Merge,
                label: "正在合并".to_string(),
                continue_available: true,
                abort_available: true,
                resolve_available: true,
                ..Self::default()
            }),
            Some(GitOperationState::Rebasing) => Some(Self {
                kind: VcsOperationKind::Rebase,
                label: if custom_rebase {
                    "Kite 变基已暂停".to_string()
                } else {
                    "正在变基".to_string()
                },
                continue_available: true,
                skip_available: true,
                abort_available: true,
                resolve_available: true,
                custom_rebase,
                ..Self::default()
            }),
            Some(GitOperationState::CherryPicking) => Some(Self {
                kind: VcsOperationKind::CherryPick,
                label: "正在拣选".to_string(),
                continue_available: true,
                skip_available: true,
                abort_available: true,
                resolve_available: true,
                ..Self::default()
            }),
            Some(GitOperationState::Reverting) => Some(Self {
                kind: VcsOperationKind::Revert,
                label: "正在还原提交".to_string(),
                continue_available: true,
                skip_available: true,
                abort_available: true,
                resolve_available: true,
                ..Self::default()
            }),
            Some(GitOperationState::Bisecting) => Some(Self {
                kind: VcsOperationKind::Bisect,
                label: "正在二分查找".to_string(),
                abort_available: true,
                ..Self::default()
            }),
            None => None,
        }
    }

    pub fn cleanup_required() -> Self {
        Self {
            kind: VcsOperationKind::CleanupRequired,
            label: "Subversion 工作副本需要清理".to_string(),
            cleanup_available: true,
            ..Self::default()
        }
    }

    pub fn busy() -> Self {
        Self {
            kind: VcsOperationKind::Busy,
            label: "版本控制仓库正忙".to_string(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SvnWorkingCopyMenuState {
    pub wc_root: String,
    pub url: Option<String>,
    pub revision: Option<u64>,
    pub switched: bool,
    pub property_modified: bool,
    pub missing: u32,
    pub conflicted: u32,
    pub unversioned: u32,
    pub cleanup_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsMenuRepository {
    pub id: String,
    pub tool: VcsMenuTool,
    pub repository_root: String,
    pub worktree_root: String,
    pub metadata_dir: Option<String>,
    pub common_dir: Option<String>,
    pub display_name: String,
    pub head: VcsHeadState,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: VcsChangeSummary,
    pub conflicts: Vec<String>,
    pub operation: Option<VcsOperationMenuState>,
    pub svn: Option<SvnWorkingCopyMenuState>,
    pub state_revision: String,
}

impl VcsMenuRepository {
    pub fn is_busy(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|operation| operation.kind == VcsOperationKind::Busy)
    }

    pub fn has_upstream(&self) -> bool {
        self.upstream
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    }

    pub fn staged_count(&self) -> Option<u32> {
        self.changes.staged
    }

    pub fn has_staged_changes(&self) -> bool {
        self.staged_count().is_some_and(|count| count > 0)
    }

    pub fn has_unstaged_changes(&self) -> bool {
        self.changes.unstaged > 0 || self.changes.untracked > 0
    }

    pub fn has_conflicts(&self) -> bool {
        self.changes.conflicted > 0 || !self.conflicts.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VcsTargetRelation {
    Outside,
    Root,
    InsideDirectory,
    Tracked,
    Untracked,
    Ignored,
    Submodule,
    SvnVersioned,
    SvnUnversioned,
}

impl VcsTargetRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outside => "outside",
            Self::Root => "root",
            Self::InsideDirectory => "inside-directory",
            Self::Tracked => "tracked",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
            Self::Submodule => "submodule",
            Self::SvnVersioned => "svn-versioned",
            Self::SvnUnversioned => "svn-unversioned",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsTargetLink {
    pub repository_id: String,
    pub tool: VcsMenuTool,
    pub relative_path: Option<String>,
    pub relation: VcsTargetRelation,
    pub staged: Option<bool>,
    pub unstaged: bool,
    pub conflicted: bool,
    pub switched: bool,
    pub property_modified: bool,
    pub missing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VcsTargetMenuState {
    pub path: String,
    pub links: Vec<VcsTargetLink>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VcsMenuSnapshot {
    pub schema_version: u32,
    pub observed_at_unix_ms: i64,
    pub git_available: bool,
    pub svn_available: bool,
    pub repositories: Vec<VcsMenuRepository>,
    pub targets: Vec<VcsTargetMenuState>,
    /// Composite revision. Individual repository revisions remain available for
    /// precise dispatch and stale-write checks.
    pub state_revision: String,
}

impl VcsMenuSnapshot {
    pub fn repository(&self, id: &str) -> Option<&VcsMenuRepository> {
        self.repositories
            .iter()
            .find(|repository| repository.id == id)
    }

    pub fn repository_for_target(
        &self,
        target_path: &str,
        tool: VcsMenuTool,
    ) -> Option<&VcsMenuRepository> {
        self.targets
            .iter()
            .find(|target| target.path == target_path)
            .and_then(|target| target.links.iter().find(|link| link.tool == tool))
            .and_then(|link| self.repository(&link.repository_id))
    }

    pub fn target(&self, path: &str) -> Option<&VcsTargetMenuState> {
        self.targets.iter().find(|target| target.path == path)
    }

    pub fn repositories_for_tool(&self, tool: VcsMenuTool) -> Vec<&VcsMenuRepository> {
        self.repositories
            .iter()
            .filter(|repository| repository.tool == tool)
            .collect()
    }

    pub fn single_repository_for_tool(&self, tool: VcsMenuTool) -> Option<&VcsMenuRepository> {
        let repositories = self.repositories_for_tool(tool);
        (repositories.len() == 1).then(|| repositories[0])
    }

    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }
}

/// Loads a menu snapshot for a target set. Paths are de-duplicated and
/// canonicalized once; one status snapshot is reused for every target sharing a
/// repository/worktree.
pub fn load_vcs_menu_snapshot(
    target_paths: &[PathBuf],
    working_directory: &Path,
) -> VcsMenuSnapshot {
    let paths = normalized_targets(target_paths, working_directory);
    let mut built = Vec::new();
    let mut git_available = false;
    let mut seen_git = HashSet::new();

    for path in &paths {
        match GitService::shared().discover_repository(path) {
            Ok(repository) => {
                git_available = true;
                let key = path_key(&repository.work_tree);
                if seen_git.insert(key) {
                    built.push(load_git_repository(&repository));
                }
            }
            Err(error) => {
                if error.kind != GitErrorKind::NotInstalled {
                    git_available = true;
                }
            }
        }
    }

    let (svn_available, svn_repositories) = load_svn_repositories(&paths);
    built.extend(svn_repositories);
    built.sort_by(|left, right| {
        left.repository
            .tool
            .cmp(&right.repository.tool)
            .then_with(|| {
                left.repository
                    .repository_root
                    .cmp(&right.repository.repository_root)
            })
    });

    let targets = paths
        .iter()
        .map(|path| build_target_state(path, &built, git_available, svn_available))
        .collect::<Vec<_>>();
    let mut revision_parts = built
        .iter()
        .map(|entry| {
            format!(
                "{}:{}",
                entry.repository.id, entry.repository.state_revision
            )
        })
        .collect::<Vec<_>>();
    revision_parts.sort();
    let state_revision = hash_text(&revision_parts.join("\n"));
    VcsMenuSnapshot {
        schema_version: VCS_MENU_SCHEMA_VERSION,
        observed_at_unix_ms: unix_millis(),
        git_available,
        svn_available,
        repositories: built.into_iter().map(|entry| entry.repository).collect(),
        targets,
        state_revision,
    }
}

/// Revalidates a write against the snapshot revision that produced the menu.
pub fn revalidate_vcs_revision(
    path: &Path,
    tool: VcsMenuTool,
    expected_revision: &str,
) -> Result<(), String> {
    let snapshot = load_vcs_menu_snapshot(&[path.to_path_buf()], path);
    let Some(repository) = snapshot
        .repositories
        .iter()
        .find(|repository| repository.tool == tool)
    else {
        return Err("版本控制状态已变化，请刷新菜单后重试。".to_string());
    };
    if repository.state_revision == expected_revision {
        Ok(())
    } else {
        Err("版本控制状态已变化，请刷新菜单后重试。".to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcsOperationControl {
    Continue,
    Skip,
    Abort,
}

/// Dispatches Git recovery controls with Kite's custom rebase state taking
/// priority over generic `git rebase --continue` handling.
pub fn run_git_menu_control(path: &Path, control: VcsOperationControl) -> Result<String, String> {
    let repository = GitService::shared()
        .discover_repository(path)
        .map_err(|error| error.message)?;
    if operation_state(&repository.git_dir) == Some(GitOperationState::Rebasing) {
        let state = match control {
            VcsOperationControl::Continue => crate::git_rebase::git_rebase_continue(path),
            VcsOperationControl::Skip => crate::git_rebase::git_rebase_skip(path),
            VcsOperationControl::Abort => crate::git_rebase::git_rebase_abort(path),
        }?;
        return Ok(state.message);
    }
    let action = match control {
        VcsOperationControl::Continue => GitOperationAction::Continue,
        VcsOperationControl::Skip => GitOperationAction::Skip,
        VcsOperationControl::Abort => GitOperationAction::Abort,
    };
    run_git_operation_control(path, action)
}

fn normalized_targets(target_paths: &[PathBuf], working_directory: &Path) -> Vec<PathBuf> {
    let mut paths = if target_paths.is_empty() {
        vec![working_directory.to_path_buf()]
    } else {
        target_paths.to_vec()
    };
    paths.retain(|path| !path.as_os_str().is_empty());
    paths = paths
        .into_iter()
        .map(|path| canonicalize_loose(&path))
        .collect();
    paths.sort_by_key(|path| path_key(path));
    paths.dedup_by_key(|path| path_key(path));
    if paths.is_empty() {
        paths.push(canonicalize_loose(working_directory));
    }
    paths
}

#[derive(Clone, Debug)]
struct BuiltRepository {
    repository: VcsMenuRepository,
    git_files: Vec<GitChangeFile>,
    svn_entries: Vec<SvnStatusEntry>,
}

fn load_git_repository(repository: &GitRepository) -> BuiltRepository {
    let (changes, raw_status, load_failed) =
        match git_changes::repository_changes_for_menu(repository, true) {
            Ok((changes, raw)) => (changes, raw, false),
            Err(error) => (empty_git_changes(repository), error.into_bytes(), true),
        };
    let operation = VcsOperationMenuState::from_git(
        if load_failed { None } else { changes.operation },
        &repository.git_dir,
    )
    .or_else(|| {
        (load_failed || repository.git_dir.join("index.lock").is_file())
            .then(VcsOperationMenuState::busy)
    });
    let mut state_material = Vec::new();
    state_material.extend(repository.work_tree.to_string_lossy().as_bytes());
    state_material.push(0);
    state_material.extend(repository.git_dir.to_string_lossy().as_bytes());
    state_material.push(0);
    state_material.extend(repository.common_dir.to_string_lossy().as_bytes());
    state_material.push(0);
    if let Some(operation) = &operation {
        state_material.extend(operation.kind.as_str().as_bytes());
        state_material.push(0);
    }
    state_material.extend(&raw_status);
    for file in &changes.files {
        state_material.extend(file.path.as_bytes());
        state_material.push(0);
        append_path_identity(&mut state_material, &repository.work_tree.join(&file.path));
    }
    let display_name = repository
        .work_tree
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository.work_tree.display().to_string());
    let id = format!("git:{}", path_key(&repository.work_tree));
    let git_files = changes.files.clone();
    BuiltRepository {
        repository: VcsMenuRepository {
            id,
            tool: VcsMenuTool::Git,
            repository_root: repository.work_tree.display().to_string(),
            worktree_root: repository.work_tree.display().to_string(),
            metadata_dir: Some(repository.git_dir.display().to_string()),
            common_dir: Some(repository.common_dir.display().to_string()),
            display_name,
            head: VcsHeadState::from(changes.head.clone()),
            upstream: changes.upstream.clone(),
            ahead: changes.ahead,
            behind: changes.behind,
            changes: change_summary(&changes),
            conflicts: conflict_paths(&changes),
            operation,
            svn: None,
            state_revision: hash_bytes(&state_material),
        },
        git_files,
        svn_entries: Vec::new(),
    }
}

fn empty_git_changes(repository: &GitRepository) -> GitRepositoryChanges {
    GitRepositoryChanges {
        repository_root: repository.work_tree.clone(),
        branch: None,
        head_short_hash: None,
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        operation: operation_state(&repository.git_dir),
        files: Vec::new(),
        crlf_warning: None,
        includes_ignored: true,
    }
}

fn change_summary(changes: &GitRepositoryChanges) -> VcsChangeSummary {
    VcsChangeSummary {
        staged: Some(count_u32(changes, GitChangeGroup::Staged)),
        unstaged: count_u32(changes, GitChangeGroup::Unstaged),
        untracked: count_u32(changes, GitChangeGroup::Untracked),
        ignored: count_u32(changes, GitChangeGroup::Ignored),
        conflicted: count_u32(changes, GitChangeGroup::Conflicted),
    }
}

fn count_u32(changes: &GitRepositoryChanges, group: GitChangeGroup) -> u32 {
    changes.count(group).try_into().unwrap_or(u32::MAX)
}

fn conflict_paths(changes: &GitRepositoryChanges) -> Vec<String> {
    changes
        .files
        .iter()
        .filter(|file| file.group == GitChangeGroup::Conflicted)
        .map(|file| file.path.clone())
        .collect()
}

fn build_target_state(
    path: &Path,
    repositories: &[BuiltRepository],
    git_available: bool,
    svn_available: bool,
) -> VcsTargetMenuState {
    let target = canonicalize_loose(path);
    let mut links = Vec::new();
    for built in repositories {
        match built.repository.tool {
            VcsMenuTool::Git => {
                if let Some(link) = git_target_link(&target, built) {
                    links.push(link);
                }
            }
            VcsMenuTool::Svn => {
                if let Some(link) = svn_target_link(&target, built) {
                    links.push(link);
                }
            }
        }
    }
    if git_available && !links.iter().any(|link| link.tool == VcsMenuTool::Git) {
        links.push(outside_link(VcsMenuTool::Git));
    }
    if svn_available && !links.iter().any(|link| link.tool == VcsMenuTool::Svn) {
        links.push(outside_link(VcsMenuTool::Svn));
    }
    VcsTargetMenuState {
        path: target.display().to_string(),
        links,
    }
}

fn outside_link(tool: VcsMenuTool) -> VcsTargetLink {
    VcsTargetLink {
        repository_id: String::new(),
        tool,
        relative_path: None,
        relation: VcsTargetRelation::Outside,
        staged: None,
        unstaged: false,
        conflicted: false,
        switched: false,
        property_modified: false,
        missing: false,
    }
}

fn git_target_link(target: &Path, built: &BuiltRepository) -> Option<VcsTargetLink> {
    let repository = &built.repository;
    let root = Path::new(&repository.worktree_root);
    let relative = target.strip_prefix(root).ok()?;
    let relative_path = relative.to_string_lossy().replace('\\', "/");
    let is_root = relative_path.is_empty();
    let is_dir = target.is_dir();
    let is_submodule =
        !is_root && is_dir && (target.join(".git").is_file() || target.join(".git").is_dir());
    let matching_files = git_files_for_target(&relative_path, is_dir, &built.git_files);
    let staged = matching_files
        .iter()
        .any(|file| file.group == GitChangeGroup::Staged);
    let unstaged = matching_files.iter().any(|file| {
        matches!(
            file.group,
            GitChangeGroup::Unstaged | GitChangeGroup::Untracked
        )
    });
    let conflicted = matching_files
        .iter()
        .any(|file| file.group == GitChangeGroup::Conflicted);
    let relation = if is_root {
        VcsTargetRelation::Root
    } else if is_submodule {
        VcsTargetRelation::Submodule
    } else if !is_dir {
        if matching_files
            .iter()
            .any(|file| file.group == GitChangeGroup::Untracked)
        {
            VcsTargetRelation::Untracked
        } else if matching_files
            .iter()
            .any(|file| file.group == GitChangeGroup::Ignored)
        {
            VcsTargetRelation::Ignored
        } else {
            VcsTargetRelation::Tracked
        }
    } else {
        VcsTargetRelation::InsideDirectory
    };
    Some(VcsTargetLink {
        repository_id: repository.id.clone(),
        tool: VcsMenuTool::Git,
        relative_path: Some(relative_path),
        relation,
        staged: Some(staged),
        unstaged,
        conflicted,
        switched: false,
        property_modified: false,
        missing: false,
    })
}

fn git_files_for_target<'a>(
    relative_path: &str,
    is_dir: bool,
    files: &'a [GitChangeFile],
) -> Vec<&'a GitChangeFile> {
    let relative_path = normalize_relative(relative_path);
    files
        .iter()
        .filter(|file| {
            let path = normalize_relative(&file.path);
            if relative_path.is_empty() || relative_path == "." {
                true
            } else if is_dir {
                path == relative_path || path.starts_with(&format!("{relative_path}/"))
            } else {
                path == relative_path
            }
        })
        .collect()
}

fn svn_target_link(target: &Path, built: &BuiltRepository) -> Option<VcsTargetLink> {
    let repository = &built.repository;
    let root = Path::new(&repository.repository_root);
    let relative = target.strip_prefix(root).ok()?;
    let relative_path = relative.to_string_lossy().replace('\\', "/");
    let target_is_dir = target.is_dir();
    let target_key = normalize_relative(&relative_path);
    let matching = svn_entries_for_target(&target_key, target_is_dir, &built.svn_entries);
    let svn = repository.svn.as_ref()?;
    let relation = if target_key.is_empty() || target_key == "." {
        VcsTargetRelation::Root
    } else if matching.iter().any(|entry| entry.unversioned) {
        VcsTargetRelation::SvnUnversioned
    } else {
        VcsTargetRelation::SvnVersioned
    };
    Some(VcsTargetLink {
        repository_id: repository.id.clone(),
        tool: VcsMenuTool::Svn,
        relative_path: Some(relative_path),
        relation,
        staged: None,
        unstaged: matching
            .iter()
            .any(|entry| entry.modified || entry.unversioned),
        conflicted: matching.iter().any(|entry| entry.conflicted),
        switched: svn.switched || matching.iter().any(|entry| entry.switched),
        property_modified: svn.property_modified
            || matching.iter().any(|entry| entry.property_modified),
        missing: matching.iter().any(|entry| entry.missing),
    })
}

fn svn_entries_for_target<'a>(
    relative_path: &str,
    is_dir: bool,
    entries: &'a [SvnStatusEntry],
) -> Vec<&'a SvnStatusEntry> {
    entries
        .iter()
        .filter(|entry| {
            let path = normalize_relative(&entry.path);
            if relative_path.is_empty() || relative_path == "." {
                true
            } else if is_dir {
                path == relative_path || path.starts_with(&format!("{relative_path}/"))
            } else {
                path == relative_path
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
struct SvnStatusEntry {
    path: String,
    relation: VcsTargetRelation,
    unversioned: bool,
    modified: bool,
    conflicted: bool,
    missing: bool,
    switched: bool,
    property_modified: bool,
    incomplete: bool,
}

fn normalize_relative(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

fn load_svn_repositories(paths: &[PathBuf]) -> (bool, Vec<BuiltRepository>) {
    if !svn_available() {
        return (false, Vec::new());
    }
    let mut repositories = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let Some(info) = read_svn_info(path) else {
            continue;
        };
        let key = path_key(Path::new(&info.wc_root));
        if !seen.insert(key) {
            continue;
        }
        let (entries, cleanup_required) =
            read_svn_status(Path::new(&info.wc_root)).unwrap_or_default();
        let cleanup_required = cleanup_required || entries.iter().any(|entry| entry.incomplete);
        let mut state = Vec::new();
        state.extend(info.wc_root.as_bytes());
        state.push(0);
        state.extend(info.url.as_deref().unwrap_or("").as_bytes());
        state.push(0);
        state.extend(info.revision.unwrap_or_default().to_string().as_bytes());
        state.push(0);
        state.extend(format!("{cleanup_required}").as_bytes());
        state.push(0);
        for entry in &entries {
            state.extend(entry.path.as_bytes());
            state.push(0);
            state.extend(entry.relation.as_str().as_bytes());
            state.push(0);
            append_path_identity(&mut state, &Path::new(&info.wc_root).join(&entry.path));
        }
        let switched = entries.iter().any(|entry| entry.switched);
        let property_modified = entries.iter().any(|entry| entry.property_modified);
        let missing = entries.iter().filter(|entry| entry.missing).count() as u32;
        let conflicted = entries.iter().filter(|entry| entry.conflicted).count() as u32;
        let unversioned = entries.iter().filter(|entry| entry.unversioned).count() as u32;
        let operation = cleanup_required.then(VcsOperationMenuState::cleanup_required);
        let wc_root = info.wc_root;
        let repository = VcsMenuRepository {
            id: format!("svn:{}", path_key(Path::new(&wc_root))),
            tool: VcsMenuTool::Svn,
            repository_root: wc_root.clone(),
            worktree_root: wc_root.clone(),
            metadata_dir: None,
            common_dir: None,
            display_name: Path::new(&wc_root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| wc_root.clone()),
            head: VcsHeadState::default(),
            upstream: None,
            ahead: 0,
            behind: 0,
            changes: VcsChangeSummary {
                staged: None,
                unstaged: entries.iter().filter(|entry| entry.modified).count() as u32,
                untracked: unversioned,
                ignored: 0,
                conflicted,
            },
            conflicts: entries
                .iter()
                .filter(|entry| entry.conflicted)
                .map(|entry| entry.path.clone())
                .collect(),
            operation,
            svn: Some(SvnWorkingCopyMenuState {
                wc_root: wc_root.clone(),
                url: info.url,
                revision: info.revision,
                switched,
                property_modified,
                missing,
                conflicted,
                unversioned,
                cleanup_required,
            }),
            state_revision: hash_bytes(&state),
        };
        repositories.push(BuiltRepository {
            repository,
            git_files: Vec::new(),
            svn_entries: entries,
        });
    }
    (true, repositories)
}

#[derive(Clone, Debug)]
struct SvnInfo {
    wc_root: String,
    url: Option<String>,
    revision: Option<u64>,
}

fn read_svn_info(path: &Path) -> Option<SvnInfo> {
    let cwd = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let output = run_svn(cwd, &["info", "--xml", "--non-interactive"]).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_svn_info(&String::from_utf8_lossy(&output.stdout)).ok()
}

fn parse_svn_info(xml: &str) -> Result<SvnInfo, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let entry = document
        .descendants()
        .find(|node| node.tag_name().name() == "entry")
        .ok_or_else(|| "SVN info 缺少 entry".to_string())?;
    let wc_root = entry
        .descendants()
        .find(|node| node.tag_name().name() == "wcroot-abspath")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "SVN info 缺少工作副本根目录".to_string())?;
    let url = entry
        .descendants()
        .find(|node| node.tag_name().name() == "url")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let revision = entry
        .attribute("revision")
        .and_then(|value| value.parse().ok());
    Ok(SvnInfo {
        wc_root: canonicalize_loose(Path::new(wc_root)).display().to_string(),
        url,
        revision,
    })
}

fn read_svn_status(root: &Path) -> Option<(Vec<SvnStatusEntry>, bool)> {
    let output = run_svn(
        root,
        &["status", "--xml", "--ignore-externals", "--non-interactive"],
    )
    .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let cleanup_required = !output.status.success()
        || String::from_utf8_lossy(&output.stderr)
            .to_ascii_lowercase()
            .contains("cleanup");
    let entries = parse_svn_status(&text, root).unwrap_or_default();
    Some((entries, cleanup_required))
}

fn parse_svn_status(xml: &str, root: &Path) -> Result<Vec<SvnStatusEntry>, String> {
    let document = Document::parse(xml).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for entry in document
        .descendants()
        .filter(|node| node.tag_name().name() == "entry")
    {
        let Some(path) = entry.attribute("path") else {
            continue;
        };
        let absolute = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            root.join(path)
        };
        let relative = absolute
            .strip_prefix(root)
            .map(|value| normalize_relative(&value.to_string_lossy()))
            .unwrap_or_else(|_| normalize_relative(path));
        let Some(status) = entry
            .descendants()
            .find(|node| node.tag_name().name() == "wc-status")
        else {
            continue;
        };
        let item = status.attribute("item").unwrap_or("normal");
        let props = status.attribute("props").unwrap_or("none");
        let switched = status
            .attribute("switched")
            .is_some_and(|value| value == "true");
        let unversioned = matches!(item, "unversioned" | "ignored" | "external");
        let conflicted = item == "conflicted"
            || status
                .attribute("tree-conflicted")
                .is_some_and(|value| value == "true");
        let missing = matches!(item, "missing" | "incomplete");
        let incomplete = item == "incomplete";
        let modified = matches!(
            item,
            "modified" | "added" | "deleted" | "replaced" | "merged" | "obstructed"
        );
        let property_modified = matches!(props, "modified" | "conflicted");
        entries.push(SvnStatusEntry {
            path: relative,
            relation: if unversioned {
                VcsTargetRelation::SvnUnversioned
            } else {
                VcsTargetRelation::SvnVersioned
            },
            unversioned,
            modified,
            conflicted,
            missing,
            switched,
            property_modified,
            incomplete,
        });
    }
    Ok(entries)
}

fn svn_available() -> bool {
    configured_command("svn", Path::new("."))
        .args(["--version", "--quiet"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_svn(cwd: &Path, args: &[&str]) -> Result<Output, std::io::Error> {
    configured_command("svn", cwd).args(args).output()
}

fn canonicalize_loose(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        value.replace('/', "\\").to_ascii_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        value.into_owned()
    }
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(7).collect()
}

fn hash_text(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(value);
    format!("{:x}", digest.finalize())
}

fn append_path_identity(target: &mut Vec<u8>, path: &Path) {
    if let Ok(metadata) = std::fs::metadata(path) {
        target.extend(metadata.len().to_string().as_bytes());
        target.push(0);
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
        {
            target.extend(duration.as_nanos().to_string().as_bytes());
        }
    }
    target.push(0);
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn run_git(root: &Path, args: &[&str]) {
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

    fn run_git_output(root: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run git")
    }

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kite-vcs-menu-{label}-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        fs::create_dir_all(&root).expect("create repo");
        run_git(&root, &["init", "--quiet"]);
        run_git(&root, &["config", "user.name", "Kite Test"]);
        run_git(&root, &["config", "user.email", "kite@example.test"]);
        root
    }

    #[test]
    fn parses_svn_info_and_status_fixtures_without_running_svn() {
        let info = parse_svn_info(
            r#"<?xml version="1.0"?><info><entry kind="dir" path="." revision="42"><url>https://example.test/repo/trunk</url><wc-info><wcroot-abspath>/tmp/wc</wcroot-abspath></wc-info></entry></info>"#,
        )
        .expect("info");
        assert_eq!(info.url.as_deref(), Some("https://example.test/repo/trunk"));
        assert_eq!(info.revision, Some(42));

        let entries = parse_svn_status(
            r#"<?xml version="1.0"?><status><target path="."><entry path="changed.txt"><wc-status item="modified" props="none" revision="42"/></entry><entry path="new.txt"><wc-status item="unversioned" props="none"/></entry><entry path="conflict.txt"><wc-status item="conflicted" props="conflicted" tree-conflicted="true"/></entry></target></status>"#,
            Path::new("/tmp/wc"),
        )
        .expect("status");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].relation, VcsTargetRelation::SvnVersioned);
        assert!(entries[0].modified);
        assert!(entries[1].unversioned);
        assert!(entries[2].conflicted);
        assert!(entries[2].property_modified);
    }

    #[test]
    fn kite_rebase_state_is_marked_for_custom_continue_dispatch() {
        let root = std::env::temp_dir().join(format!(
            "kite-vcs-menu-rebase-state-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        fs::create_dir_all(&root).expect("state directory");
        fs::write(root.join("kite-rebase-state.json"), "{}").expect("state file");
        let operation = VcsOperationMenuState::from_git(Some(GitOperationState::Rebasing), &root)
            .expect("rebase state");
        assert_eq!(operation.kind, VcsOperationKind::Rebase);
        assert!(operation.custom_rebase);
        assert!(operation.continue_available);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn git_menu_snapshot_distinguishes_staged_untracked_and_upstream_states() {
        if !git_available() {
            return;
        }
        let root = temp_repo("states");
        fs::write(root.join("tracked.txt"), "one\n").expect("write");
        run_git(&root, &["add", "tracked.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "initial"]);
        fs::write(root.join("tracked.txt"), "two\n").expect("modify");
        fs::write(root.join("new.txt"), "new\n").expect("untracked");
        run_git(&root, &["add", "tracked.txt"]);

        let snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let repository = snapshot
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("repo");
        assert_eq!(repository.changes.staged, Some(1));
        assert_eq!(repository.changes.unstaged, 0);
        assert_eq!(repository.changes.untracked, 1);
        assert!(repository.upstream.is_none());
        assert!(!repository.has_upstream());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn revalidation_rejects_a_changed_repository() {
        if !git_available() {
            return;
        }
        let root = temp_repo("revalidate");
        fs::write(root.join("a.txt"), "one\n").expect("write");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "initial"]);
        let first = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let revision = first
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("repo")
            .state_revision
            .clone();
        // Keep the same status byte and file size; the path identity material
        // must still make the revision stale.
        fs::write(root.join("a.txt"), "two\n").expect("same-size change");
        assert!(revalidate_vcs_revision(&root, VcsMenuTool::Git, &revision).is_err());
        let second = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let current = second
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("repo")
            .state_revision
            .clone();
        assert!(revalidate_vcs_revision(&root, VcsMenuTool::Git, &current).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn target_mapping_marks_root_tracked_untracked_and_ignored() {
        if !git_available() {
            return;
        }
        let root = temp_repo("targets");
        fs::write(root.join(".gitignore"), "ignored.log\n").expect("ignore");
        fs::write(root.join("tracked.txt"), "one\n").expect("tracked");
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "--quiet", "-m", "initial"]);
        fs::write(root.join("tracked.txt"), "two\n").expect("modify");
        fs::write(root.join("untracked.txt"), "new\n").expect("untracked");
        fs::write(root.join("ignored.log"), "ignored\n").expect("ignored");
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).expect("outside directory");
        let tracked = root.join("tracked.txt");
        let untracked = root.join("untracked.txt");
        let ignored = root.join("ignored.log");
        let snapshot = load_vcs_menu_snapshot(
            &[
                root.clone(),
                tracked.clone(),
                untracked.clone(),
                ignored.clone(),
                outside.clone(),
            ],
            &root,
        );
        let root_key = canonicalize_loose(&root).display().to_string();
        let tracked_key = canonicalize_loose(&tracked).display().to_string();
        let untracked_key = canonicalize_loose(&untracked).display().to_string();
        let ignored_key = canonicalize_loose(&ignored).display().to_string();
        assert_eq!(
            snapshot.target(&root_key).expect("root").links[0].relation,
            VcsTargetRelation::Root
        );
        assert_eq!(
            snapshot.target(&tracked_key).expect("tracked").links[0].relation,
            VcsTargetRelation::Tracked
        );
        assert_eq!(
            snapshot.target(&untracked_key).expect("untracked").links[0].relation,
            VcsTargetRelation::Untracked
        );
        assert_eq!(
            snapshot.target(&ignored_key).expect("ignored").links[0].relation,
            VcsTargetRelation::Ignored
        );
        assert_eq!(
            snapshot
                .target(&canonicalize_loose(&outside).display().to_string())
                .expect("outside")
                .links[0]
                .relation,
            VcsTargetRelation::Outside
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn git_menu_snapshot_tracks_detached_unborn_and_ahead_behind() {
        if !git_available() {
            return;
        }
        let root = temp_repo("head-and-sync");
        fs::write(root.join("a.txt"), "one\n").expect("write");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "base"]);
        let branch =
            String::from_utf8_lossy(&run_git_output(&root, &["branch", "--show-current"]).stdout)
                .trim()
                .to_string();

        let remote = root.with_extension("bare.git");
        run_git(
            &root,
            &[
                "init",
                "--bare",
                "--quiet",
                remote.to_str().expect("remote path"),
            ],
        );
        run_git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote url"),
            ],
        );
        run_git(&root, &["push", "--quiet", "-u", "origin", &branch]);
        fs::write(root.join("a.txt"), "local\n").expect("local change");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "local"]);

        let other = root.with_extension("other");
        run_git(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_str().expect("remote url"),
                other.to_str().expect("other path"),
            ],
        );
        run_git(&other, &["config", "user.name", "Kite Test"]);
        run_git(&other, &["config", "user.email", "kite@example.test"]);
        fs::write(other.join("remote.txt"), "remote\n").expect("remote change");
        run_git(&other, &["add", "remote.txt"]);
        run_git(&other, &["commit", "--quiet", "-m", "remote"]);
        run_git(&other, &["push", "--quiet", "origin", "HEAD"]);
        run_git(&root, &["fetch", "--quiet", "origin"]);

        let snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let repository = snapshot
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("repo");
        assert!(repository.has_upstream());
        assert_eq!(repository.ahead, 1);
        assert_eq!(repository.behind, 1);
        assert_eq!(repository.head.kind, VcsHeadKind::Branch);

        run_git(&root, &["checkout", "--quiet", "--detach", "HEAD"]);
        let detached = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        assert_eq!(
            detached
                .single_repository_for_tool(VcsMenuTool::Git)
                .expect("detached")
                .head
                .kind,
            VcsHeadKind::Detached
        );

        let unborn = temp_repo("unborn");
        let unborn_snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&unborn), &unborn);
        assert_eq!(
            unborn_snapshot
                .single_repository_for_tool(VcsMenuTool::Git)
                .expect("unborn")
                .head
                .kind,
            VcsHeadKind::Unborn
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(remote);
        let _ = fs::remove_dir_all(other);
        let _ = fs::remove_dir_all(unborn);
    }

    #[test]
    fn git_menu_snapshot_tracks_merge_and_rebase_recovery_controls() {
        if !git_available() {
            return;
        }
        let root = temp_repo("merge-conflict");
        fs::write(root.join("a.txt"), "base\n").expect("write");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "base"]);
        let branch =
            String::from_utf8_lossy(&run_git_output(&root, &["branch", "--show-current"]).stdout)
                .trim()
                .to_string();
        run_git(&root, &["checkout", "--quiet", "-b", "feature"]);
        fs::write(root.join("a.txt"), "feature\n").expect("feature");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "feature"]);
        run_git(&root, &["checkout", "--quiet", &branch]);
        fs::write(root.join("a.txt"), "main\n").expect("main");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "main"]);
        let merge = run_git_output(&root, &["merge", "--no-edit", "feature"]);
        assert!(!merge.status.success(), "merge should conflict");

        let snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let repository = snapshot
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("merge repo");
        let operation = repository.operation.as_ref().expect("merge operation");
        assert_eq!(operation.kind, VcsOperationKind::Merge);
        assert!(operation.resolve_available);
        assert!(operation.continue_available);
        assert!(operation.abort_available);
        assert!(repository.has_conflicts());
        run_git(&root, &["merge", "--abort"]);

        run_git(&root, &["checkout", "--quiet", "-b", "rebase-feature"]);
        fs::write(root.join("a.txt"), "rebase feature\n").expect("rebase feature");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "rebase feature"]);
        run_git(&root, &["checkout", "--quiet", &branch]);
        fs::write(root.join("a.txt"), "rebase main\n").expect("rebase main");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "rebase main"]);
        let rebase = run_git_output(&root, &["rebase", "rebase-feature"]);
        assert!(!rebase.status.success(), "rebase should conflict");
        let snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let repository = snapshot
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("rebase repo");
        let operation = repository.operation.as_ref().expect("rebase operation");
        assert_eq!(operation.kind, VcsOperationKind::Rebase);
        assert!(operation.skip_available);
        assert!(operation.abort_available);
        let _ = run_git_output(&root, &["rebase", "--abort"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn git_menu_snapshot_reports_index_lock_as_busy_without_running_log_queries() {
        if !git_available() {
            return;
        }
        let root = temp_repo("busy");
        fs::write(root.join("a.txt"), "one\n").expect("write");
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "initial"]);
        let lock = root.join(".git/index.lock");
        fs::write(&lock, "").expect("lock");
        let snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&root), &root);
        let repository = snapshot
            .single_repository_for_tool(VcsMenuTool::Git)
            .expect("busy repo");
        assert_eq!(
            repository
                .operation
                .as_ref()
                .map(|operation| operation.kind),
            Some(VcsOperationKind::Busy)
        );
        let _ = fs::remove_file(lock);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn live_svn_snapshot_reports_modified_and_unversioned_paths_when_available() {
        if !svn_available() {
            eprintln!("skipping live SVN fixture: svn is unavailable");
            return;
        }
        let svnadmin = Command::new("svnadmin")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !svnadmin {
            eprintln!("skipping live SVN fixture: svnadmin is unavailable");
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "kite-vcs-menu-svn-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let repo = root.join("repo");
        let wc = root.join("wc");
        fs::create_dir_all(&root).expect("svn root");
        let ok = Command::new("svnadmin")
            .args(["create"])
            .arg(&repo)
            .status()
            .is_ok_and(|status| status.success());
        if !ok {
            eprintln!("skipping live SVN fixture: svnadmin create failed");
            return;
        }
        let url = format!("file://{}", repo.display());
        let ok = Command::new("svn")
            .args(["checkout", "--quiet", &url])
            .arg(&wc)
            .status()
            .is_ok_and(|status| status.success());
        if !ok {
            eprintln!("skipping live SVN fixture: checkout failed");
            return;
        }
        fs::write(wc.join("tracked.txt"), "one\n").expect("svn tracked");
        let _ = Command::new("svn")
            .current_dir(&wc)
            .args(["add", "tracked.txt"])
            .status();
        let _ = Command::new("svn")
            .current_dir(&wc)
            .args(["commit", "--quiet", "-m", "initial"])
            .status();
        fs::write(wc.join("tracked.txt"), "two\n").expect("svn modified");
        fs::write(wc.join("new.txt"), "new\n").expect("svn unversioned");
        let snapshot = load_vcs_menu_snapshot(std::slice::from_ref(&wc), &wc);
        let repository = snapshot
            .single_repository_for_tool(VcsMenuTool::Svn)
            .expect("svn repo");
        assert_eq!(repository.changes.staged, None);
        assert_eq!(repository.changes.unstaged, 1);
        assert_eq!(repository.changes.untracked, 1);
        assert!(repository.svn.as_ref().is_some_and(|svn| svn.url.is_some()));
        let _ = fs::remove_dir_all(root);
    }
}
