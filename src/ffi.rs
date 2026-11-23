use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::UNIX_EPOCH;

use crate::archive::{self, ArchiveFormat, ArchivePhase, ArchiveProgress};
use crate::content_search::{self, ContentFileMatch, ContentLineMatch, ContentSearchSnapshot};
use crate::devices::{self, PortableDevice, StorageVolume, VolumeKind};
use crate::favorites::{self as favorites_store, FavoriteEntryDto};
use crate::fs::{EntryKind, FileEntry, SortSpec, VcsStatus, load_directory, transfer_entry};
use crate::git::GitEventSink as _;
use crate::git::log::{
    self as git_log, GitChangedFile, GitCommitDetails, GitGraphRow, GitLogCommit, GitLogFilter,
    GitLogPage, GitRef, GitRefKind as GitLogRefKind, GitSignature, GitTreeEntry,
};
use crate::git::{self, GitDiffLine, GitDiffLineKind, GitPathSummary};
use crate::git_branches::{
    self, GitBranch, GitBranchSnapshot, GitCompareCommit, GitCompareFile, GitCompareResult,
    GitDirtyPolicy, GitHeadSummary, GitRefKind, GitTag, GitWorktree,
};
use crate::platform::{HostPlatform, default_root_path, host_platform};
use crate::session::{self as session_store, SessionStateDto};
use crate::text_editor::{self, TextDocument, TextLineEnding};
use crate::updater::{self, UpdateInfo};
use crate::vcs::{
    VcsOperation, VcsQuery, VcsTool, apply_statuses, load_vcs_snapshot, run_vcs_operation,
    run_vcs_query,
};

#[cfg(target_os = "macos")]
#[uniffi::export]
pub fn render_live_markdown(source: String) -> String {
    crate::preview::render_markdown_preview_html(&source)
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct DirectoryEntryDto {
    pub path: String,
    pub name: String,
    pub is_directory: bool,
    pub size: u64,
    pub modified_unix_ms: Option<i64>,
    pub kind_label: String,
    pub git_status: Option<String>,
    pub svn_status: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct DirectoryListingDto {
    pub path: String,
    pub entries: Vec<DirectoryEntryDto>,
    pub skipped: u64,
    pub git_available: bool,
    pub svn_available: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct DirectorySearchDto {
    pub path: String,
    pub query: String,
    pub entries: Vec<DirectoryEntryDto>,
    pub truncated: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GlobalSearchResultDto {
    pub roots: Vec<String>,
    pub query: String,
    pub entries: Vec<DirectoryEntryDto>,
    pub truncated: bool,
    pub partial: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VolumeDto {
    pub path: String,
    pub name: Option<String>,
    pub device_name: Option<String>,
    pub kind: String,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PortableDeviceDto {
    pub name: String,
    pub shell_path: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct DeviceSnapshotDto {
    pub volumes: Vec<VolumeDto>,
    pub portable_devices: Vec<PortableDeviceDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct UpdateInfoDto {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_name: String,
    pub release_url: String,
    pub release_notes: String,
    pub published_at: Option<String>,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_sha256: String,
    pub asset_size: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct UpdateDownloadProgressDto {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ArchiveProgressPhaseDto {
    #[default]
    Preparing,
    Checking,
    Working,
    Finishing,
}

#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct ArchiveProgressDto {
    pub phase: ArchiveProgressPhaseDto,
    pub destination: Option<String>,
    pub processed_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, uniffi::Object)]
pub struct ArchiveProgressMonitor {
    progress: Mutex<ArchiveProgressDto>,
}

#[uniffi::export]
impl ArchiveProgressMonitor {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            progress: Mutex::default(),
        })
    }

    pub fn snapshot(&self) -> ArchiveProgressDto {
        self.progress.lock().unwrap().clone()
    }
}

impl ArchiveProgressMonitor {
    fn record(&self, event: ArchiveProgress<'_>) {
        let mut progress = self.progress.lock().unwrap();
        match event {
            ArchiveProgress::Destination(path) => {
                progress.destination = Some(path.to_string_lossy().into_owned());
            }
            ArchiveProgress::Bytes {
                phase,
                processed,
                total,
            } => {
                progress.phase = match phase {
                    ArchivePhase::Checking => ArchiveProgressPhaseDto::Checking,
                    ArchivePhase::Working => ArchiveProgressPhaseDto::Working,
                };
                progress.processed_bytes = Some(processed);
                progress.total_bytes = total;
            }
            ArchiveProgress::Finishing => progress.phase = ArchiveProgressPhaseDto::Finishing,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TextDocumentDto {
    pub path: String,
    pub text: String,
    pub language: String,
    pub line_ending: String,
    pub has_bom: bool,
    pub content_sha256: String,
    pub byte_len: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContentLineMatchDto {
    pub line_number: u64,
    pub column_start: Option<u64>,
    pub column_end: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContentFileMatchDto {
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub name: String,
    pub kind_label: String,
    pub size: u64,
    pub modified_unix_ms: Option<i64>,
    pub score: f64,
    pub match_count: u64,
    pub matches: Vec<ContentLineMatchDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContentSearchResultDto {
    pub query: String,
    pub roots: Vec<String>,
    pub files: Vec<ContentFileMatchDto>,
    pub index_state: String,
    pub partial: bool,
    pub truncated: bool,
    pub scanned_documents: u64,
    pub total_matches: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ContentIndexStatusDto {
    pub state: String,
    pub indexed_files: u64,
    pub message: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCommitSummaryDto {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitChangeCountsDto {
    pub modified: u32,
    pub added: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitPathSummaryDto {
    pub repository_root: String,
    pub relative_path: String,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<String>,
    pub staged_status: Option<String>,
    pub staged_code: Option<String>,
    pub unstaged_status: Option<String>,
    pub unstaged_code: Option<String>,
    pub ignored: bool,
    pub last_commit: Option<GitCommitSummaryDto>,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub changes: GitChangeCountsDto,
    pub is_directory: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitDiffLineKindDto {
    Context,
    Added,
    Removed,
    Hunk,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitDiffLineDto {
    pub kind: GitDiffLineKindDto,
    pub text: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitHeadSummaryDto {
    pub repository_root: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitRefKindDto {
    LocalBranch,
    RemoteBranch,
    Tag,
    Commit,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum GitDirtyPolicyDto {
    #[default]
    Cancel,
    Stash,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitBranchDto {
    pub name: String,
    pub full_name: String,
    pub remote: Option<String>,
    pub commit_hash: String,
    pub short_hash: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub is_head: bool,
    pub is_merged: bool,
    pub last_commit_unix_seconds: Option<i64>,
    pub subject: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitTagDto {
    pub name: String,
    pub target_hash: String,
    pub short_hash: String,
    pub is_annotated: bool,
    pub message: Option<String>,
    pub created_unix_seconds: Option<i64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitWorktreeDto {
    pub path: String,
    pub head_hash: Option<String>,
    pub branch: Option<String>,
    pub is_current: bool,
    pub is_bare: bool,
    pub is_detached: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitBranchSnapshotDto {
    pub repository_root: String,
    pub head_branch: Option<String>,
    pub detached: bool,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
    pub local_branches: Vec<GitBranchDto>,
    pub remote_branches: Vec<GitBranchDto>,
    pub tags: Vec<GitTagDto>,
    pub worktrees: Vec<GitWorktreeDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCompareCommitDto {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCompareFileDto {
    pub status: String,
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCompareResultDto {
    pub base: String,
    pub head: String,
    pub ahead: u32,
    pub behind: u32,
    pub commits: Vec<GitCompareCommitDto>,
    pub files: Vec<GitCompareFileDto>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitErrorKindDto {
    NotInstalled,
    NotRepository,
    Authentication,
    Conflict,
    Rejected,
    MissingIdentity,
    RepositoryLocked,
    UnsupportedVersion,
    Cancelled,
    InvalidOperation,
    Io,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitCredentialKindDto {
    Https,
    SshPassphrase,
    GpgPinentry,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitCredentialRequestDto {
    pub kind: GitCredentialKindDto,
    pub prompt: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitOutputStreamDto {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitEventDto {
    Started {
        label: String,
        repository: String,
        command: String,
    },
    Progress {
        text: String,
    },
    Output {
        stream: GitOutputStreamDto,
        text: String,
    },
    CredentialRequested {
        kind: GitCredentialKindDto,
        prompt: String,
    },
    RepositoryChanged {
        repository: String,
        branch: Option<String>,
        head_oid: Option<String>,
        operation: Option<String>,
    },
    Finished {
        duration_ms: u64,
        exit_code: Option<i32>,
    },
    Failed {
        kind: GitErrorKindDto,
        message: String,
        duration_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitHeadDto {
    Branch { name: String, oid: Option<String> },
    Detached { oid: String },
    Unborn { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitRepositoryStateDto {
    pub repository_root: String,
    pub git_dir: String,
    pub common_dir: String,
    pub head: GitHeadDto,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitVersionInfoDto {
    pub raw: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub minimum: String,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitConsoleEntryDto {
    pub recorded_unix_ms: i64,
    pub label: String,
    pub command: String,
    pub repository: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitOperationDto {
    Add,
    Unstage,
    Commit { message: String },
    Pull,
    Push,
    Fetch,
    Revert,
    Clean,
    Clone { url: String, destination: String },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitOperationResultDto {
    pub message: String,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[uniffi::export(callback_interface)]
pub trait GitEventCallback: Send + Sync {
    fn on_event(&self, event: GitEventDto);
}

#[uniffi::export(callback_interface)]
pub trait GitCredentialCallback: Send + Sync {
    fn request_credential(&self, request: GitCredentialRequestDto) -> Option<String>;
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRefDto {
    pub name: String,
    pub full_name: String,
    pub kind: GitRefKindDto,
    pub is_head: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitLogContextDto {
    pub repository_root: String,
    pub current_branch: Option<String>,
    pub head_hash: Option<String>,
    pub head_short_hash: Option<String>,
    pub refs: Vec<GitRefDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitLogFilterDto {
    pub branch: Option<String>,
    pub author: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub path: Option<String>,
    pub text: Option<String>,
    pub regex: bool,
    pub hash: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitGraphEdgeDto {
    pub from_lane: u32,
    pub to_lane: u32,
    pub color_index: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitGraphRowDto {
    pub lane: u32,
    pub lane_count: u32,
    pub color_index: u32,
    pub edges: Vec<GitGraphEdgeDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitLogCommitDto {
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
    pub refs: Vec<GitRefDto>,
    pub graph: GitGraphRowDto,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitLogPageDto {
    pub repository_root: String,
    pub commits: Vec<GitLogCommitDto>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
    pub loaded_count: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitSignatureDto {
    pub status_code: String,
    pub status_label: String,
    pub signer: Option<String>,
    pub key: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitChangedFileDto {
    pub status: String,
    pub path: String,
    pub old_path: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCommitDetailsDto {
    pub commit: GitLogCommitDto,
    pub body: String,
    pub signature: GitSignatureDto,
    pub branches_containing: Vec<String>,
    pub changed_files: Vec<GitChangedFileDto>,
    pub cherry_picked: Option<bool>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitTreeEntryDto {
    pub path: String,
    pub name: String,
    pub mode: String,
    pub object_hash: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsToolDto {
    Git,
    Svn,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsOperationDto {
    Add,
    Unstage,
    Commit { message: String },
    Pull,
    Push,
    Fetch,
    Revert,
    Clean,
    Update,
    Lock,
    Unlock,
    Cleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VcsQueryDto {
    Diff,
    Log,
}

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum KiteError {
    #[error("{message}")]
    OperationFailed { message: String },
    #[error("{message}")]
    Git {
        kind: GitErrorKindDto,
        message: String,
        detail: Option<String>,
        exit_code: Option<i32>,
        command: Option<String>,
        repository: Option<String>,
    },
}

impl KiteError {
    fn operation(message: impl Into<String>) -> Self {
        Self::OperationFailed {
            message: message.into(),
        }
    }

    fn from_git(error: git::GitError) -> Self {
        Self::Git {
            kind: git_error_kind_dto(error.kind),
            message: error.message,
            detail: error.detail,
            exit_code: error.exit_code,
            command: error.command,
            repository: error
                .repository
                .map(|path| path.to_string_lossy().into_owned()),
        }
    }
}

#[derive(Debug, uniffi::Object)]
pub struct KiteCore {
    search_cancel: Mutex<Option<Arc<AtomicBool>>>,
    indexed_search_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

#[uniffi::export]
impl KiteCore {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            search_cancel: Mutex::new(None),
            indexed_search_cancel: Mutex::new(None),
        })
    }

    pub fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    pub fn check_for_update(&self) -> Result<UpdateInfoDto, KiteError> {
        updater::check_for_update()
            .map(update_info_dto)
            .map_err(KiteError::operation)
    }

    pub fn download_progress(&self) -> UpdateDownloadProgressDto {
        update_download_progress_dto(updater::download_progress())
    }

    pub fn download_update(&self, update: UpdateInfoDto) -> Result<String, KiteError> {
        if !update.update_available {
            return Err(KiteError::operation("当前版本已是最新版本"));
        }
        updater::download_update(&update_info_from_dto(update))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(KiteError::operation)
    }

    pub fn platform(&self) -> String {
        match host_platform() {
            HostPlatform::Windows => "windows",
            HostPlatform::MacOS => "macos",
            HostPlatform::Other => "other",
        }
        .to_string()
    }

    pub fn default_directory(&self) -> String {
        dirs::home_dir()
            .unwrap_or_else(default_root_path)
            .to_string_lossy()
            .into_owned()
    }

    pub fn list_directory(&self, path: String) -> Result<DirectoryListingDto, KiteError> {
        let snapshot = load_directory(&PathBuf::from(&path), SortSpec::default())
            .map_err(KiteError::operation)?;
        let vcs = load_vcs_snapshot(&snapshot.path);
        let mut entries = snapshot.entries;
        apply_statuses(&mut entries, &snapshot.path, &vcs);
        Ok(DirectoryListingDto {
            path: snapshot.path.to_string_lossy().into_owned(),
            entries: entries.iter().map(entry_dto).collect(),
            skipped: snapshot.skipped as u64,
            git_available: vcs.git_available(),
            svn_available: vcs.svn_available(),
        })
    }

    pub fn search_directory(
        &self,
        path: String,
        query: String,
    ) -> Result<Option<DirectorySearchDto>, KiteError> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = self
                .search_cancel
                .lock()
                .map_err(|_| KiteError::operation("搜索状态锁不可用"))?;
            *active = Some(cancel.clone());
        }

        let result = crate::fs::search_directory(
            &PathBuf::from(&path),
            &query,
            SortSpec::default(),
            cancel.as_ref(),
        );

        if let Ok(mut active) = self.search_cancel.lock()
            && active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancel))
        {
            *active = None;
        }

        let Some(snapshot) = result.map_err(KiteError::operation)? else {
            return Ok(None);
        };
        let vcs = load_vcs_snapshot(&snapshot.root);
        let mut entries = snapshot.entries;
        apply_statuses(&mut entries, &snapshot.root, &vcs);
        Ok(Some(DirectorySearchDto {
            path: snapshot.root.to_string_lossy().into_owned(),
            query: snapshot.query,
            entries: entries.iter().map(entry_dto).collect(),
            truncated: snapshot.truncated,
        }))
    }

    pub fn search_directories(
        &self,
        paths: Vec<String>,
        query: String,
        filters: crate::search_filters::SearchFilters,
    ) -> Result<Option<GlobalSearchResultDto>, KiteError> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = self
                .search_cancel
                .lock()
                .map_err(|_| KiteError::operation("搜索状态锁不可用"))?;
            *active = Some(cancel.clone());
        }

        let roots = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        let result =
            crate::global_name_search::search_names(&roots, &query, filters, cancel.as_ref());

        if let Ok(mut active) = self.search_cancel.lock()
            && active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancel))
        {
            *active = None;
        }

        let Some(snapshot) = result.map_err(KiteError::operation)? else {
            return Ok(None);
        };
        Ok(Some(GlobalSearchResultDto {
            roots: snapshot
                .roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect(),
            query: snapshot.query,
            entries: snapshot.entries.iter().map(entry_dto).collect(),
            truncated: snapshot.truncated,
            partial: snapshot.partial,
        }))
    }

    pub fn search_indexed_names(
        &self,
        paths: Vec<String>,
        query: String,
        filters: crate::search_filters::SearchFilters,
    ) -> Result<Option<GlobalSearchResultDto>, KiteError> {
        let roots = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = self
                .indexed_search_cancel
                .lock()
                .map_err(|_| KiteError::operation("索引搜索状态锁不可用"))?;
            *active = Some(Arc::clone(&cancel));
        }
        let result =
            crate::indexed_search::search_indexed_names(&roots, &query, filters, cancel.as_ref());
        if let Ok(mut active) = self.indexed_search_cancel.lock()
            && active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancel))
        {
            *active = None;
        }
        let Some(snapshot) = result.map_err(KiteError::operation)? else {
            return Ok(None);
        };
        Ok(Some(GlobalSearchResultDto {
            roots: snapshot
                .roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect(),
            query: snapshot.query,
            entries: snapshot.entries.iter().map(entry_dto).collect(),
            truncated: snapshot.truncated,
            partial: snapshot.partial,
        }))
    }

    pub fn cancel_directory_search(&self) {
        for slot in [&self.search_cancel, &self.indexed_search_cancel] {
            if let Ok(active) = slot.lock()
                && let Some(cancel) = active.as_ref()
            {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn prepare_content_search(&self, roots: Vec<String>) -> Result<(), KiteError> {
        let service = content_search::shared_service().map_err(KiteError::operation)?;
        let roots = roots.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        service
            .ensure_roots(&roots)
            .map(|_| ())
            .map_err(KiteError::operation)
    }

    pub fn content_search_status(&self) -> ContentIndexStatusDto {
        match content_search::shared_service() {
            Ok(service) => {
                let status = service.status();
                ContentIndexStatusDto {
                    state: status.state.as_str().to_string(),
                    indexed_files: status.indexed_files,
                    message: status.message,
                }
            }
            Err(message) => ContentIndexStatusDto {
                state: "error".to_string(),
                indexed_files: 0,
                message: Some(message),
            },
        }
    }

    pub fn note_content_search_interaction(&self) {
        if let Ok(service) = content_search::shared_service() {
            service.note_interaction();
        }
    }

    pub fn set_content_search_paused(&self, paused: bool) {
        if let Ok(service) = content_search::shared_service() {
            service.set_indexing_paused(paused);
        }
    }

    pub fn cancel_content_search(&self) {
        if let Ok(service) = content_search::shared_service() {
            service.cancel_search();
        }
    }

    pub fn clear_content_search_index(&self) -> Result<(), KiteError> {
        content_search::shared_service()
            .map_err(KiteError::operation)?
            .clear()
            .map_err(KiteError::operation)
    }

    pub fn search_content(
        &self,
        roots: Vec<String>,
        query: String,
        regex: bool,
        max_files: u32,
        filters: crate::search_filters::SearchFilters,
    ) -> Result<Option<ContentSearchResultDto>, KiteError> {
        let service = content_search::shared_service().map_err(KiteError::operation)?;
        let roots = roots.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        service
            .search_with_filters(&roots, &query, regex, max_files.max(1) as usize, filters)
            .map(|snapshot| snapshot.map(content_search_result_dto))
            .map_err(KiteError::operation)
    }

    pub fn devices(&self) -> DeviceSnapshotDto {
        let snapshot = devices::snapshot();
        DeviceSnapshotDto {
            volumes: snapshot.volumes.iter().map(volume_dto).collect(),
            portable_devices: snapshot.portable_devices.iter().map(device_dto).collect(),
        }
    }

    pub fn favorites(&self) -> Vec<FavoriteEntryDto> {
        favorites_store::load_favorites()
    }

    pub fn add_favorite(
        &self,
        path: String,
        name: String,
        is_directory: bool,
    ) -> Result<bool, KiteError> {
        favorites_store::add_favorite(&PathBuf::from(path), &name, is_directory)
            .map_err(KiteError::operation)
    }

    pub fn remove_favorite(&self, path: String) -> Result<bool, KiteError> {
        favorites_store::remove_favorite(&PathBuf::from(path)).map_err(KiteError::operation)
    }

    pub fn load_session(&self) -> Option<SessionStateDto> {
        session_store::load_session()
    }

    pub fn save_session(&self, state: SessionStateDto) -> Result<(), KiteError> {
        session_store::save_session(&state).map_err(KiteError::operation)
    }

    pub fn open_text_document(&self, path: String) -> Result<TextDocumentDto, KiteError> {
        text_editor::TextDocument::open(&PathBuf::from(path))
            .map(text_document_dto)
            .map_err(KiteError::operation)
    }

    pub fn copy_path_to(
        &self,
        source: String,
        destination_directory: String,
    ) -> Result<String, KiteError> {
        transfer_entry(
            &PathBuf::from(source),
            &PathBuf::from(destination_directory),
            false,
        )
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(KiteError::operation)
    }

    pub fn move_path_to(
        &self,
        source: String,
        destination_directory: String,
    ) -> Result<String, KiteError> {
        transfer_entry(
            &PathBuf::from(source),
            &PathBuf::from(destination_directory),
            true,
        )
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(KiteError::operation)
    }

    pub fn compress_path(&self, path: String, format: String) -> Result<String, KiteError> {
        let format = ArchiveFormat::parse(&format)
            .ok_or_else(|| KiteError::operation("不支持的压缩格式。当前支持 ZIP 和 TAR.GZ。"))?;
        archive::compress(&PathBuf::from(path), format)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(KiteError::operation)
    }

    pub fn compress_path_with_progress(
        &self,
        path: String,
        format: String,
        monitor: Arc<ArchiveProgressMonitor>,
    ) -> Result<String, KiteError> {
        let format = ArchiveFormat::parse(&format)
            .ok_or_else(|| KiteError::operation("不支持的压缩格式。当前支持 ZIP 和 TAR.GZ。"))?;
        archive::compress_with_progress(&PathBuf::from(path), format, |event| monitor.record(event))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(KiteError::operation)
    }

    pub fn extract_archive(
        &self,
        path: String,
        destination_directory: String,
    ) -> Result<String, KiteError> {
        archive::extract(&PathBuf::from(path), &PathBuf::from(destination_directory))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(KiteError::operation)
    }

    pub fn extract_archive_here(&self, path: String) -> Result<String, KiteError> {
        archive::extract_here(&PathBuf::from(path))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(KiteError::operation)
    }

    pub fn extract_archive_with_progress(
        &self,
        path: String,
        destination_directory: Option<String>,
        monitor: Arc<ArchiveProgressMonitor>,
    ) -> Result<String, KiteError> {
        let path = PathBuf::from(path);
        let result = if let Some(destination) = destination_directory {
            archive::extract_with_progress(&path, &PathBuf::from(destination), |event| {
                monitor.record(event)
            })
        } else {
            archive::extract_here_with_progress(&path, |event| monitor.record(event))
        };
        result
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(KiteError::operation)
    }

    pub fn save_text_document(
        &self,
        path: String,
        text: String,
        line_ending: String,
        has_bom: bool,
        expected_sha256: String,
    ) -> Result<TextDocumentDto, KiteError> {
        let line_ending = TextLineEnding::parse(&line_ending)
            .ok_or_else(|| KiteError::operation("不支持的换行格式"))?;
        text_editor::save_text_document(
            &PathBuf::from(path),
            &text,
            line_ending,
            has_bom,
            &expected_sha256,
        )
        .map(text_document_dto)
        .map_err(KiteError::operation)
    }

    /// Git summary for the preview pane. `None` outside a Git work tree.
    pub fn git_path_summary(&self, path: String) -> Result<Option<GitPathSummaryDto>, KiteError> {
        git::git_path_summary(&PathBuf::from(path))
            .map(|summary| summary.map(git_path_summary_dto))
            .map_err(KiteError::operation)
    }

    pub fn git_file_diff(&self, path: String) -> Result<Vec<GitDiffLineDto>, KiteError> {
        git::git_file_diff(&PathBuf::from(path))
            .map(|lines| lines.into_iter().map(git_diff_line_dto).collect())
            .map_err(KiteError::operation)
    }

    /// Reads the current branch, ahead/behind counts, and dirty state.
    pub fn git_head_summary(&self, path: String) -> Result<Option<GitHeadSummaryDto>, KiteError> {
        git_branches::git_head_summary(&PathBuf::from(path))
            .map(|summary| summary.map(git_head_summary_dto))
            .map_err(KiteError::operation)
    }

    /// Lists local branches, remote-tracking branches, tags, and worktrees.
    pub fn git_branch_snapshot(&self, path: String) -> Result<GitBranchSnapshotDto, KiteError> {
        git_branches::git_branch_snapshot(&PathBuf::from(path))
            .map(git_branch_snapshot_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_checkout(
        &self,
        path: String,
        target: String,
        kind: GitRefKindDto,
        dirty_policy: GitDirtyPolicyDto,
    ) -> Result<String, KiteError> {
        git_branches::git_checkout(
            &PathBuf::from(path),
            &target,
            git_ref_kind(kind),
            git_dirty_policy(dirty_policy),
        )
        .map_err(KiteError::operation)
    }

    pub fn git_create_branch(
        &self,
        path: String,
        name: String,
        start_point: String,
    ) -> Result<String, KiteError> {
        git_branches::create_branch(&PathBuf::from(path), &name, &start_point)
            .map_err(KiteError::operation)
    }

    pub fn git_rename_branch(
        &self,
        path: String,
        old_name: String,
        new_name: String,
    ) -> Result<String, KiteError> {
        git_branches::rename_branch(&PathBuf::from(path), &old_name, &new_name)
            .map_err(KiteError::operation)
    }

    pub fn git_delete_local_branch(
        &self,
        path: String,
        name: String,
        force: bool,
    ) -> Result<String, KiteError> {
        git_branches::delete_local_branch(&PathBuf::from(path), &name, force)
            .map_err(KiteError::operation)
    }

    pub fn git_delete_local_branches(
        &self,
        path: String,
        names: Vec<String>,
        force: bool,
    ) -> Result<String, KiteError> {
        git_branches::delete_local_branches(&PathBuf::from(path), &names, force)
            .map_err(KiteError::operation)
    }

    pub fn git_delete_remote_branch(
        &self,
        path: String,
        remote: String,
        branch: String,
    ) -> Result<String, KiteError> {
        git_branches::delete_remote_branch(&PathBuf::from(path), &remote, &branch)
            .map_err(KiteError::operation)
    }

    pub fn git_set_branch_upstream(
        &self,
        path: String,
        branch: String,
        upstream: Option<String>,
    ) -> Result<String, KiteError> {
        git_branches::set_branch_upstream(&PathBuf::from(path), &branch, upstream.as_deref())
            .map_err(KiteError::operation)
    }

    pub fn git_compare_branches(
        &self,
        path: String,
        base: String,
        head: String,
    ) -> Result<GitCompareResultDto, KiteError> {
        git_branches::compare_branches(&PathBuf::from(path), &base, &head)
            .map(git_compare_result_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_create_tag(
        &self,
        path: String,
        name: String,
        target: String,
        annotated: bool,
        message: Option<String>,
    ) -> Result<String, KiteError> {
        git_branches::create_tag(
            &PathBuf::from(path),
            &name,
            &target,
            annotated,
            message.as_deref(),
        )
        .map_err(KiteError::operation)
    }

    pub fn git_delete_tag(&self, path: String, name: String) -> Result<String, KiteError> {
        git_branches::delete_tag(&PathBuf::from(path), &name).map_err(KiteError::operation)
    }

    pub fn git_push_tag(
        &self,
        path: String,
        remote: String,
        name: String,
    ) -> Result<String, KiteError> {
        git_branches::push_tag(&PathBuf::from(path), &remote, &name).map_err(KiteError::operation)
    }

    pub fn git_delete_remote_tag(
        &self,
        path: String,
        remote: String,
        name: String,
    ) -> Result<String, KiteError> {
        git_branches::delete_remote_tag(&PathBuf::from(path), &remote, &name)
            .map_err(KiteError::operation)
    }

    pub fn git_create_worktree(
        &self,
        path: String,
        destination: String,
        branch: String,
    ) -> Result<String, KiteError> {
        git_branches::create_worktree(&PathBuf::from(path), &PathBuf::from(destination), &branch)
            .map_err(KiteError::operation)
    }
    pub fn run_vcs_operation(
        &self,
        tool: VcsToolDto,
        operation: VcsOperationDto,
        path: String,
    ) -> Result<String, KiteError> {
        let operation = vcs_operation(operation);
        let path = PathBuf::from(path);
        match tool {
            VcsToolDto::Git => crate::git::GitService::shared()
                .run_legacy_operation(&path, &operation)
                .map_err(KiteError::from_git),
            VcsToolDto::Svn => {
                run_vcs_operation(VcsTool::Svn, operation, &path).map_err(KiteError::operation)
            }
        }
    }

    pub fn run_vcs_query(
        &self,
        tool: VcsToolDto,
        query: VcsQueryDto,
        path: String,
    ) -> Result<String, KiteError> {
        let query = match query {
            VcsQueryDto::Diff => VcsQuery::Diff,
            VcsQueryDto::Log => VcsQuery::Log,
        };
        let path = PathBuf::from(path);
        match tool {
            VcsToolDto::Git => crate::git::GitService::shared()
                .run_legacy_query(&path, query)
                .map_err(KiteError::from_git),
            VcsToolDto::Svn => {
                run_vcs_query(VcsTool::Svn, query, &path).map_err(KiteError::operation)
            }
        }
    }

    pub fn git_log_context(&self, path: String) -> Result<GitLogContextDto, KiteError> {
        git_log::git_log_context(&PathBuf::from(path))
            .map(git_log_context_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_log_page(
        &self,
        path: String,
        filter: GitLogFilterDto,
        cursor: Option<String>,
        page_size: u32,
    ) -> Result<GitLogPageDto, KiteError> {
        git_log::git_log_page(
            &PathBuf::from(path),
            git_log_filter(filter),
            cursor.as_deref(),
            page_size,
        )
        .map(git_log_page_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_commit_details(
        &self,
        path: String,
        hash: String,
    ) -> Result<GitCommitDetailsDto, KiteError> {
        git_log::git_commit_details(&PathBuf::from(path), &hash)
            .map(git_commit_details_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_commit_tree(
        &self,
        path: String,
        hash: String,
    ) -> Result<Vec<GitTreeEntryDto>, KiteError> {
        git_log::git_commit_tree(&PathBuf::from(path), &hash)
            .map(|entries| entries.into_iter().map(git_tree_entry_dto).collect())
            .map_err(KiteError::operation)
    }

    pub fn git_file_history(
        &self,
        path: String,
        relative_path: String,
        cursor: Option<String>,
        page_size: u32,
    ) -> Result<GitLogPageDto, KiteError> {
        git_log::git_file_history(
            &PathBuf::from(path),
            &relative_path,
            cursor.as_deref(),
            page_size,
        )
        .map(git_log_page_dto)
        .map_err(KiteError::operation)
    }
}

struct CallbackEventSink(Box<dyn GitEventCallback>);

impl git::GitEventSink for CallbackEventSink {
    fn emit(&self, event: git::GitEvent) {
        self.0.on_event(git_event_dto(event));
    }
}

struct CallbackCredentialProvider(Box<dyn GitCredentialCallback>);

impl git::GitCredentialProvider for CallbackCredentialProvider {
    fn request_credential(&self, request: &git::GitCredentialRequest) -> Option<String> {
        self.0.request_credential(GitCredentialRequestDto {
            kind: git_credential_kind_dto(request.kind),
            prompt: request.prompt.clone(),
        })
    }
}

#[derive(uniffi::Object)]
pub struct GitService {
    inner: git::GitService,
}

#[uniffi::export]
impl GitService {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: git::GitService::shared().clone(),
        })
    }

    pub fn version(&self) -> Result<GitVersionInfoDto, KiteError> {
        self.inner
            .git_version()
            .map(git_version_dto)
            .map_err(KiteError::from_git)
    }

    pub fn repository_state(&self, path: String) -> Result<GitRepositoryStateDto, KiteError> {
        self.inner
            .repository_state(&PathBuf::from(path))
            .map(git_repository_state_dto)
            .map_err(KiteError::from_git)
    }

    pub fn console_entries(&self) -> Vec<GitConsoleEntryDto> {
        self.inner
            .console_entries()
            .into_iter()
            .map(git_console_entry_dto)
            .collect()
    }

    pub fn clear_console(&self) {
        self.inner.clear_console();
    }

    pub fn run_operation(
        &self,
        path: String,
        operation: GitOperationDto,
        events: Box<dyn GitEventCallback>,
        credentials: Box<dyn GitCredentialCallback>,
    ) -> Result<GitOperationResultDto, KiteError> {
        let events = CallbackEventSink(events);
        let credentials = CallbackCredentialProvider(credentials);
        self.inner
            .run_operation(
                &PathBuf::from(path),
                git_operation(operation),
                &git::CancellationToken::default(),
                &events,
                Some(&credentials),
            )
            .map(git_operation_result_dto)
            .map_err(KiteError::from_git)
    }

    pub fn start_operation(
        &self,
        path: String,
        operation: GitOperationDto,
        events: Box<dyn GitEventCallback>,
        credentials: Box<dyn GitCredentialCallback>,
    ) -> Arc<GitOperationHandle> {
        let events: git::SharedEventSink = Arc::new(CallbackEventSink(events));
        let credentials: git::SharedCredentialProvider =
            Arc::new(CallbackCredentialProvider(credentials));
        Arc::new(GitOperationHandle {
            inner: self.inner.start_operation(
                PathBuf::from(path),
                git_operation(operation),
                events,
                Some(credentials),
            ),
        })
    }

    pub fn watch_repository(
        &self,
        path: String,
        events: Box<dyn GitEventCallback>,
    ) -> Result<Arc<GitRepositoryWatchHandle>, KiteError> {
        let events = Arc::new(CallbackEventSink(events));
        let watcher = self
            .inner
            .watch_repository(&PathBuf::from(path), move |event| match event {
                git::GitRepositoryEvent::Changed(state) => {
                    events.emit(git::GitEvent::RepositoryChanged {
                        repository: state.repository.work_tree.clone(),
                        branch: state.branch().map(str::to_string),
                        head_oid: state.head_oid().map(str::to_string),
                        operation: state.operation,
                    });
                }
                git::GitRepositoryEvent::Error(error) => {
                    events.emit(git::GitEvent::Failed {
                        kind: error.kind,
                        message: error.message,
                        duration_ms: 0,
                    });
                }
            })
            .map_err(KiteError::from_git)?;
        Ok(Arc::new(GitRepositoryWatchHandle {
            watcher: Mutex::new(Some(watcher)),
        }))
    }
}

#[derive(uniffi::Object)]
pub struct GitOperationHandle {
    inner: Arc<git::GitOperationTask>,
}

#[uniffi::export]
impl GitOperationHandle {
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    pub fn wait(&self) -> Result<GitOperationResultDto, KiteError> {
        self.inner
            .wait()
            .map(git_operation_result_dto)
            .map_err(KiteError::from_git)
    }
}

#[derive(uniffi::Object)]
pub struct GitRepositoryWatchHandle {
    watcher: Mutex<Option<git::GitRepositoryWatcher>>,
}

#[uniffi::export]
impl GitRepositoryWatchHandle {
    pub fn stop(&self) {
        if let Ok(mut watcher) = self.watcher.lock()
            && let Some(mut watcher) = watcher.take()
        {
            watcher.stop();
        }
    }
}

fn git_operation(operation: GitOperationDto) -> git::GitOperation {
    match operation {
        GitOperationDto::Add => git::GitOperation::Add,
        GitOperationDto::Unstage => git::GitOperation::Unstage,
        GitOperationDto::Commit { message } => git::GitOperation::Commit(message),
        GitOperationDto::Pull => git::GitOperation::Pull,
        GitOperationDto::Push => git::GitOperation::Push,
        GitOperationDto::Fetch => git::GitOperation::Fetch,
        GitOperationDto::Revert => git::GitOperation::Revert,
        GitOperationDto::Clean => git::GitOperation::Clean,
        GitOperationDto::Clone { url, destination } => git::GitOperation::Clone {
            url,
            destination: PathBuf::from(destination),
        },
    }
}

fn git_operation_result_dto(result: git::GitOperationResult) -> GitOperationResultDto {
    GitOperationResultDto {
        message: result.message,
        duration_ms: result.command.duration_ms,
        stdout: result.command.stdout,
        stderr: result.command.stderr,
        exit_code: result.command.exit_code,
    }
}

fn git_repository_state_dto(state: git::GitRepositoryState) -> GitRepositoryStateDto {
    GitRepositoryStateDto {
        repository_root: state.repository.work_tree.to_string_lossy().into_owned(),
        git_dir: state.repository.git_dir.to_string_lossy().into_owned(),
        common_dir: state.repository.common_dir.to_string_lossy().into_owned(),
        head: match state.head {
            git::GitHead::Branch { name, oid } => GitHeadDto::Branch { name, oid },
            git::GitHead::Detached { oid } => GitHeadDto::Detached { oid },
            git::GitHead::Unborn { name } => GitHeadDto::Unborn { name },
        },
        upstream: state.upstream,
        ahead: state.ahead,
        behind: state.behind,
        operation: state
            .operation
            .map(|operation| operation.label().to_string()),
    }
}

fn git_version_dto(version: git::GitVersion) -> GitVersionInfoDto {
    GitVersionInfoDto {
        supported: version.is_supported(),
        raw: version.raw,
        major: version.major,
        minor: version.minor,
        patch: version.patch,
        minimum: git::MIN_GIT_VERSION.display(),
    }
}

fn git_console_entry_dto(entry: git::GitConsoleEntry) -> GitConsoleEntryDto {
    GitConsoleEntryDto {
        recorded_unix_ms: entry.recorded_unix_ms,
        label: entry.label,
        command: entry.command,
        repository: entry.repository.to_string_lossy().into_owned(),
        duration_ms: entry.duration_ms,
        exit_code: entry.exit_code,
        stdout: entry.stdout,
        stderr: entry.stderr,
    }
}

fn git_event_dto(event: git::GitEvent) -> GitEventDto {
    match event {
        git::GitEvent::Started {
            label,
            repository,
            command,
        } => GitEventDto::Started {
            label,
            repository: repository.to_string_lossy().into_owned(),
            command,
        },
        git::GitEvent::Progress { text } => GitEventDto::Progress { text },
        git::GitEvent::Output { stream, text } => GitEventDto::Output {
            stream: match stream {
                git::GitOutputStream::Stdout => GitOutputStreamDto::Stdout,
                git::GitOutputStream::Stderr => GitOutputStreamDto::Stderr,
            },
            text,
        },
        git::GitEvent::CredentialRequested { kind, prompt } => GitEventDto::CredentialRequested {
            kind: git_credential_kind_dto(kind),
            prompt,
        },
        git::GitEvent::RepositoryChanged {
            repository,
            branch,
            head_oid,
            operation,
        } => GitEventDto::RepositoryChanged {
            repository: repository.to_string_lossy().into_owned(),
            branch,
            head_oid,
            operation: operation.map(|operation| operation.label().to_string()),
        },
        git::GitEvent::Finished {
            duration_ms,
            exit_code,
        } => GitEventDto::Finished {
            duration_ms,
            exit_code,
        },
        git::GitEvent::Failed {
            kind,
            message,
            duration_ms,
        } => GitEventDto::Failed {
            kind: git_error_kind_dto(kind),
            message,
            duration_ms,
        },
    }
}

fn git_credential_kind_dto(kind: git::GitCredentialKind) -> GitCredentialKindDto {
    match kind {
        git::GitCredentialKind::Https => GitCredentialKindDto::Https,
        git::GitCredentialKind::SshPassphrase => GitCredentialKindDto::SshPassphrase,
        git::GitCredentialKind::GpgPinentry => GitCredentialKindDto::GpgPinentry,
        git::GitCredentialKind::Unknown => GitCredentialKindDto::Unknown,
    }
}

fn git_error_kind_dto(kind: git::GitErrorKind) -> GitErrorKindDto {
    match kind {
        git::GitErrorKind::NotInstalled => GitErrorKindDto::NotInstalled,
        git::GitErrorKind::NotRepository => GitErrorKindDto::NotRepository,
        git::GitErrorKind::Authentication => GitErrorKindDto::Authentication,
        git::GitErrorKind::Conflict => GitErrorKindDto::Conflict,
        git::GitErrorKind::Rejected => GitErrorKindDto::Rejected,
        git::GitErrorKind::MissingIdentity => GitErrorKindDto::MissingIdentity,
        git::GitErrorKind::RepositoryLocked => GitErrorKindDto::RepositoryLocked,
        git::GitErrorKind::UnsupportedVersion => GitErrorKindDto::UnsupportedVersion,
        git::GitErrorKind::Cancelled => GitErrorKindDto::Cancelled,
        git::GitErrorKind::InvalidOperation => GitErrorKindDto::InvalidOperation,
        git::GitErrorKind::Io => GitErrorKindDto::Io,
        git::GitErrorKind::Other => GitErrorKindDto::Other,
    }
}

fn vcs_operation(operation: VcsOperationDto) -> VcsOperation {
    match operation {
        VcsOperationDto::Add => VcsOperation::Add,
        VcsOperationDto::Unstage => VcsOperation::Unstage,
        VcsOperationDto::Commit { message } => VcsOperation::Commit(message),
        VcsOperationDto::Pull => VcsOperation::Pull,
        VcsOperationDto::Push => VcsOperation::Push,
        VcsOperationDto::Fetch => VcsOperation::Fetch,
        VcsOperationDto::Revert => VcsOperation::Revert,
        VcsOperationDto::Clean => VcsOperation::Clean,
        VcsOperationDto::Update => VcsOperation::Update,
        VcsOperationDto::Lock => VcsOperation::Lock,
        VcsOperationDto::Unlock => VcsOperation::Unlock,
        VcsOperationDto::Cleanup => VcsOperation::Cleanup,
    }
}

fn git_path_summary_dto(summary: GitPathSummary) -> GitPathSummaryDto {
    GitPathSummaryDto {
        repository_root: summary.repository_root.to_string_lossy().into_owned(),
        relative_path: summary.relative_path,
        branch: summary.branch,
        head_short_hash: summary.head_short_hash,
        upstream: summary.upstream,
        ahead: summary.ahead,
        behind: summary.behind,
        operation: summary.operation.map(|state| state.label().to_string()),
        staged_status: summary
            .staged_status
            .map(|status| status.label().to_string()),
        staged_code: summary
            .staged_status
            .map(|status| status.code().to_string()),
        unstaged_status: summary
            .unstaged_status
            .map(|status| status.label().to_string()),
        unstaged_code: summary
            .unstaged_status
            .map(|status| status.code().to_string()),
        ignored: summary.ignored,
        last_commit: summary.last_commit.map(|commit| GitCommitSummaryDto {
            hash: commit.hash,
            short_hash: commit.short_hash,
            author: commit.author,
            unix_seconds: commit.unix_seconds,
            subject: commit.subject,
        }),
        lines_added: summary.lines_added,
        lines_removed: summary.lines_removed,
        changes: GitChangeCountsDto {
            modified: summary.changes.modified,
            added: summary.changes.added,
            deleted: summary.changes.deleted,
            renamed: summary.changes.renamed,
            untracked: summary.changes.untracked,
            conflicted: summary.changes.conflicted,
        },
        is_directory: summary.is_directory,
    }
}

fn git_diff_line_dto(line: GitDiffLine) -> GitDiffLineDto {
    GitDiffLineDto {
        kind: match line.kind {
            GitDiffLineKind::Context => GitDiffLineKindDto::Context,
            GitDiffLineKind::Added => GitDiffLineKindDto::Added,
            GitDiffLineKind::Removed => GitDiffLineKindDto::Removed,
            GitDiffLineKind::Hunk => GitDiffLineKindDto::Hunk,
        },
        text: line.text,
    }
}

fn git_ref_kind(kind: GitRefKindDto) -> GitRefKind {
    match kind {
        GitRefKindDto::LocalBranch => GitRefKind::LocalBranch,
        GitRefKindDto::RemoteBranch => GitRefKind::RemoteBranch,
        GitRefKindDto::Tag => GitRefKind::Tag,
        GitRefKindDto::Commit => GitRefKind::Commit,
        GitRefKindDto::Other => GitRefKind::Commit,
    }
}

fn git_dirty_policy(policy: GitDirtyPolicyDto) -> GitDirtyPolicy {
    match policy {
        GitDirtyPolicyDto::Cancel => GitDirtyPolicy::Cancel,
        GitDirtyPolicyDto::Stash => GitDirtyPolicy::Stash,
    }
}

fn git_head_summary_dto(summary: GitHeadSummary) -> GitHeadSummaryDto {
    GitHeadSummaryDto {
        repository_root: summary.repository_root.to_string_lossy().into_owned(),
        branch: summary.branch,
        detached: summary.detached,
        short_hash: summary.short_hash,
        upstream: summary.upstream,
        ahead: summary.ahead,
        behind: summary.behind,
        dirty: summary.dirty,
    }
}

fn git_branch_snapshot_dto(snapshot: GitBranchSnapshot) -> GitBranchSnapshotDto {
    GitBranchSnapshotDto {
        repository_root: snapshot.repository_root.to_string_lossy().into_owned(),
        head_branch: snapshot.head_branch,
        detached: snapshot.detached,
        head_short_hash: snapshot.head_short_hash,
        upstream: snapshot.upstream,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        dirty: snapshot.dirty,
        local_branches: snapshot
            .local_branches
            .into_iter()
            .map(git_branch_dto)
            .collect(),
        remote_branches: snapshot
            .remote_branches
            .into_iter()
            .map(git_branch_dto)
            .collect(),
        tags: snapshot.tags.into_iter().map(git_tag_dto).collect(),
        worktrees: snapshot
            .worktrees
            .into_iter()
            .map(git_worktree_dto)
            .collect(),
    }
}

fn git_ref_kind_dto(kind: GitLogRefKind) -> GitRefKindDto {
    match kind {
        GitLogRefKind::LocalBranch => GitRefKindDto::LocalBranch,
        GitLogRefKind::RemoteBranch => GitRefKindDto::RemoteBranch,
        GitLogRefKind::Tag => GitRefKindDto::Tag,
        GitLogRefKind::Other => GitRefKindDto::Other,
    }
}

fn git_ref_dto(reference: GitRef) -> GitRefDto {
    GitRefDto {
        name: reference.name,
        full_name: reference.full_name,
        kind: git_ref_kind_dto(reference.kind),
        is_head: reference.is_head,
    }
}

fn git_graph_row_dto(row: GitGraphRow) -> GitGraphRowDto {
    GitGraphRowDto {
        lane: row.lane,
        lane_count: row.lane_count,
        color_index: row.color_index,
        edges: row
            .edges
            .into_iter()
            .map(|edge| GitGraphEdgeDto {
                from_lane: edge.from_lane,
                to_lane: edge.to_lane,
                color_index: edge.color_index,
            })
            .collect(),
    }
}

fn git_branch_dto(branch: GitBranch) -> GitBranchDto {
    GitBranchDto {
        name: branch.name,
        full_name: branch.full_name,
        remote: branch.remote,
        commit_hash: branch.commit_hash,
        short_hash: branch.short_hash,
        upstream: branch.upstream,
        ahead: branch.ahead,
        behind: branch.behind,
        is_head: branch.is_head,
        is_merged: branch.is_merged,
        last_commit_unix_seconds: branch.last_commit_unix_seconds,
        subject: branch.subject,
    }
}

fn git_tag_dto(tag: GitTag) -> GitTagDto {
    GitTagDto {
        name: tag.name,
        target_hash: tag.target_hash,
        short_hash: tag.short_hash,
        is_annotated: tag.is_annotated,
        message: tag.message,
        created_unix_seconds: tag.created_unix_seconds,
    }
}

fn git_worktree_dto(worktree: GitWorktree) -> GitWorktreeDto {
    GitWorktreeDto {
        path: worktree.path.to_string_lossy().into_owned(),
        head_hash: worktree.head_hash,
        branch: worktree.branch,
        is_current: worktree.is_current,
        is_bare: worktree.is_bare,
        is_detached: worktree.is_detached,
    }
}

fn git_compare_result_dto(result: GitCompareResult) -> GitCompareResultDto {
    GitCompareResultDto {
        base: result.base,
        head: result.head,
        ahead: result.ahead,
        behind: result.behind,
        commits: result
            .commits
            .into_iter()
            .map(git_compare_commit_dto)
            .collect(),
        files: result.files.into_iter().map(git_compare_file_dto).collect(),
    }
}

fn git_compare_commit_dto(commit: GitCompareCommit) -> GitCompareCommitDto {
    GitCompareCommitDto {
        hash: commit.hash,
        short_hash: commit.short_hash,
        author: commit.author,
        unix_seconds: commit.unix_seconds,
        subject: commit.subject,
    }
}

fn git_compare_file_dto(file: GitCompareFile) -> GitCompareFileDto {
    GitCompareFileDto {
        status: file.status,
        path: file.path,
        additions: file.additions,
        deletions: file.deletions,
    }
}

fn git_log_commit_dto(commit: GitLogCommit) -> GitLogCommitDto {
    GitLogCommitDto {
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
        refs: commit.refs.into_iter().map(git_ref_dto).collect(),
        graph: git_graph_row_dto(commit.graph),
    }
}

fn git_log_context_dto(context: git_log::GitLogContext) -> GitLogContextDto {
    GitLogContextDto {
        repository_root: context.repository_root.to_string_lossy().into_owned(),
        current_branch: context.current_branch,
        head_hash: context.head_hash,
        head_short_hash: context.head_short_hash,
        refs: context.refs.into_iter().map(git_ref_dto).collect(),
    }
}

fn git_log_filter(filter: GitLogFilterDto) -> GitLogFilter {
    GitLogFilter {
        branch: filter.branch,
        author: filter.author,
        since: filter.since,
        until: filter.until,
        path: filter.path,
        text: filter.text,
        regex: filter.regex,
        hash: filter.hash,
    }
}

fn git_log_page_dto(page: GitLogPage) -> GitLogPageDto {
    GitLogPageDto {
        repository_root: page.repository_root.to_string_lossy().into_owned(),
        commits: page.commits.into_iter().map(git_log_commit_dto).collect(),
        has_more: page.has_more,
        next_cursor: page.next_cursor,
        loaded_count: page.loaded_count,
    }
}

fn git_signature_dto(signature: GitSignature) -> GitSignatureDto {
    GitSignatureDto {
        status_label: signature.status_label().to_string(),
        status_code: signature.status_code,
        signer: signature.signer,
        key: signature.key,
    }
}

fn git_changed_file_dto(file: GitChangedFile) -> GitChangedFileDto {
    GitChangedFileDto {
        status: file.status,
        path: file.path,
        old_path: file.old_path,
    }
}

fn git_commit_details_dto(details: GitCommitDetails) -> GitCommitDetailsDto {
    GitCommitDetailsDto {
        commit: git_log_commit_dto(details.commit),
        body: details.body,
        signature: git_signature_dto(details.signature),
        branches_containing: details.branches_containing,
        changed_files: details
            .changed_files
            .into_iter()
            .map(git_changed_file_dto)
            .collect(),
        cherry_picked: details.cherry_picked,
    }
}

fn git_tree_entry_dto(entry: GitTreeEntry) -> GitTreeEntryDto {
    GitTreeEntryDto {
        path: entry.path,
        name: entry.name,
        mode: entry.mode,
        object_hash: entry.object_hash,
        size: entry.size,
    }
}

fn text_document_dto(document: TextDocument) -> TextDocumentDto {
    TextDocumentDto {
        path: document.path.to_string_lossy().into_owned(),
        text: document.text,
        language: document.language,
        line_ending: document.line_ending.as_str().to_string(),
        has_bom: document.has_bom,
        content_sha256: document.content_sha256,
        byte_len: document.byte_len,
    }
}

fn update_info_dto(info: UpdateInfo) -> UpdateInfoDto {
    UpdateInfoDto {
        current_version: info.current_version,
        latest_version: info.latest_version,
        update_available: info.update_available,
        release_name: info.release_name,
        release_url: info.release_url,
        release_notes: info.release_notes,
        published_at: info.published_at,
        asset_name: info.asset_name,
        asset_url: info.asset_url,
        asset_sha256: info.asset_sha256,
        asset_size: info.asset_size,
    }
}

fn update_download_progress_dto(
    progress: updater::UpdateDownloadProgress,
) -> UpdateDownloadProgressDto {
    UpdateDownloadProgressDto {
        downloaded_bytes: progress.downloaded_bytes,
        total_bytes: progress.total_bytes,
        active: progress.active,
    }
}

fn update_info_from_dto(info: UpdateInfoDto) -> UpdateInfo {
    UpdateInfo {
        current_version: info.current_version,
        latest_version: info.latest_version,
        update_available: info.update_available,
        release_name: info.release_name,
        release_url: info.release_url,
        release_notes: info.release_notes,
        published_at: info.published_at,
        asset_name: info.asset_name,
        asset_url: info.asset_url,
        asset_sha256: info.asset_sha256,
        asset_size: info.asset_size,
    }
}

fn entry_dto(entry: &FileEntry) -> DirectoryEntryDto {
    DirectoryEntryDto {
        path: entry.path.to_string_lossy().into_owned(),
        name: entry.name.clone(),
        is_directory: entry.kind == EntryKind::Directory,
        size: entry.size,
        modified_unix_ms: entry
            .modified
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64),
        kind_label: entry.type_label(),
        git_status: entry.git_status.map(vcs_status_text),
        svn_status: entry.svn_status.map(vcs_status_text),
    }
}

fn content_search_result_dto(snapshot: ContentSearchSnapshot) -> ContentSearchResultDto {
    ContentSearchResultDto {
        query: snapshot.query,
        roots: snapshot
            .roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect(),
        files: snapshot.files.iter().map(content_file_match_dto).collect(),
        index_state: snapshot.index_state.as_str().to_string(),
        partial: snapshot.partial,
        truncated: snapshot.truncated,
        scanned_documents: snapshot.scanned_documents,
        total_matches: snapshot.total_matches,
        duration_ms: snapshot.duration_ms,
    }
}

fn content_file_match_dto(file: &ContentFileMatch) -> ContentFileMatchDto {
    ContentFileMatchDto {
        path: file.path.to_string_lossy().into_owned(),
        root: file.root.to_string_lossy().into_owned(),
        relative_path: file.relative_path.clone(),
        name: file.name.clone(),
        kind_label: file.kind_label.clone(),
        size: file.size,
        modified_unix_ms: file.modified_unix_ms,
        score: file.score as f64,
        match_count: file.match_count,
        matches: file.matches.iter().map(content_line_match_dto).collect(),
    }
}

fn content_line_match_dto(line: &ContentLineMatch) -> ContentLineMatchDto {
    ContentLineMatchDto {
        line_number: line.line_number,
        column_start: line.column_start,
        column_end: line.column_end,
        text: line.text.clone(),
    }
}

fn vcs_status_text(status: VcsStatus) -> String {
    format!("{} ({})", status.label(), status.code())
}

fn volume_dto(volume: &StorageVolume) -> VolumeDto {
    VolumeDto {
        path: volume.path.to_string_lossy().into_owned(),
        name: volume.volume_name.clone(),
        device_name: volume.device_name.clone(),
        kind: match volume.kind {
            VolumeKind::Usb => "usb",
            VolumeKind::Removable => "removable",
            VolumeKind::Fixed => "fixed",
            VolumeKind::Optical => "optical",
            VolumeKind::Network => "network",
            VolumeKind::Other => "other",
        }
        .to_string(),
        free_bytes: volume.free_bytes,
        total_bytes: volume.total_bytes,
    }
}

fn device_dto(device: &PortableDevice) -> PortableDeviceDto {
    PortableDeviceDto {
        name: device.name.clone(),
        shell_path: device.shell_path.clone(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_progress_binding_keeps_destinations_and_work_per_operation() {
        let root = std::env::temp_dir().join(format!(
            "kite-ffi-archive-progress-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("notes.txt");
        std::fs::write(&source, b"hello").unwrap();
        let core = KiteCore::new();
        let compression = ArchiveProgressMonitor::new();
        let archive = core
            .compress_path_with_progress(
                source.to_string_lossy().into_owned(),
                "zip".into(),
                compression.clone(),
            )
            .unwrap();
        let snapshot = compression.snapshot();
        assert_eq!(snapshot.phase, ArchiveProgressPhaseDto::Finishing);
        assert_eq!(snapshot.destination.as_deref(), Some(archive.as_str()));
        assert_eq!(
            (snapshot.processed_bytes, snapshot.total_bytes),
            (Some(5), Some(5))
        );

        let extraction = ArchiveProgressMonitor::new();
        assert_eq!(
            extraction.snapshot().phase,
            ArchiveProgressPhaseDto::Preparing
        );
        assert!(extraction.snapshot().destination.is_none());
        let extracted_here = core
            .extract_archive_with_progress(archive.clone(), None, extraction.clone())
            .unwrap();
        assert_eq!(PathBuf::from(&extracted_here), root.join("notes"));
        assert_eq!(
            extraction.snapshot().destination.as_deref(),
            Some(extracted_here.as_str())
        );
        assert_eq!(extraction.snapshot().processed_bytes, Some(5));

        let selected = root.join("selected").to_string_lossy().into_owned();
        let extracted_to = core
            .extract_archive_with_progress(
                archive,
                Some(selected.clone()),
                ArchiveProgressMonitor::new(),
            )
            .unwrap();
        assert_eq!(extracted_to, selected);
        assert_eq!(
            std::fs::read(PathBuf::from(extracted_to).join("notes.txt")).unwrap(),
            b"hello"
        );
        assert_eq!(std::fs::read(source).unwrap(), b"hello");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_record_round_trips_asset_fields() {
        let update = UpdateInfo {
            current_version: "0.6.12".to_string(),
            latest_version: "0.6.13".to_string(),
            update_available: true,
            release_name: "Kite v0.6.13".to_string(),
            release_url: "https://github.com/wyz541/kite/releases/tag/v0.6.13".to_string(),
            release_notes: "notes".to_string(),
            published_at: Some("2026-10-27T10:00:00Z".to_string()),
            asset_name: "Kite.exe".to_string(),
            asset_url: "https://github.com/wyz541/kite/releases/download/v0.6.13/Kite.exe"
                .to_string(),
            asset_sha256: "a".repeat(64),
            asset_size: 1024,
        };

        let dto = update_info_dto(update.clone());
        let restored = update_info_from_dto(dto);

        assert_eq!(restored, update);
    }

    #[test]
    fn core_searches_nested_entries() {
        let directory = std::env::temp_dir().join(format!(
            "kite-core-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let nested = directory.join("nested");
        std::fs::create_dir_all(&nested).expect("create search fixture");
        std::fs::write(nested.join("target.txt"), b"search target").expect("write search fixture");

        let core = KiteCore::new();
        let result = core
            .search_directory(
                directory.to_string_lossy().into_owned(),
                "target".to_string(),
            )
            .expect("search succeeds")
            .expect("search returns a snapshot");
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(result.query, "target");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "target.txt");
        assert!(!result.truncated);
    }

    #[test]
    fn core_searches_multiple_name_roots() {
        let directory = std::env::temp_dir().join(format!(
            "kite-core-global-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let trunk = directory.join("trunk");
        let publish = directory.join("publish_s2");
        std::fs::create_dir_all(trunk.join("PocoCheck")).expect("create trunk fixture");
        std::fs::create_dir_all(publish.join("PocoCheck")).expect("create publish fixture");

        let core = KiteCore::new();
        let result = core
            .search_directories(
                vec![
                    publish.to_string_lossy().into_owned(),
                    trunk.to_string_lossy().into_owned(),
                ],
                "PocoCheck".to_string(),
                crate::search_filters::SearchFilters::default(),
            )
            .expect("global search succeeds")
            .expect("global search returns a snapshot");
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(result.query, "PocoCheck");
        assert_eq!(result.roots.len(), 2);
        assert_eq!(result.entries.len(), 2);
        assert!(
            std::path::Path::new(&result.entries[0].path)
                .ends_with(std::path::Path::new("trunk").join("PocoCheck"))
        );
        assert!(!result.truncated);
    }

    #[test]
    fn core_applies_global_name_filters() {
        let directory = std::env::temp_dir().join(format!(
            "kite-core-global-filter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("create filter fixture");
        std::fs::write(directory.join("needle.txt"), b"text").expect("write text fixture");
        std::fs::write(directory.join("needle.md"), b"markdown").expect("write markdown fixture");
        let core = KiteCore::new();
        let result = core
            .search_directories(
                vec![directory.to_string_lossy().into_owned()],
                "needle".to_string(),
                crate::search_filters::SearchFilters {
                    kind: crate::search_filters::SearchKindFilter::Markdown,
                    modified: crate::search_filters::SearchTimeFilter::Any,
                },
            )
            .expect("filtered global search succeeds")
            .expect("filtered global search returns result");
        let _ = std::fs::remove_dir_all(directory);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "needle.md");
    }

    #[test]
    fn core_searches_file_content_and_returns_lines() {
        let directory = std::env::temp_dir().join(format!(
            "kite-core-content-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("create content search fixture");
        std::fs::write(
            directory.join("notes.txt"),
            b"first line\nsearch needle\nlast line\n",
        )
        .expect("write content search fixture");

        let core = KiteCore::new();
        let root = directory.to_string_lossy().into_owned();
        core.prepare_content_search(vec![root.clone()])
            .expect("prepare content index");
        let result = core
            .search_content(
                vec![root],
                "needle".to_string(),
                false,
                20,
                crate::search_filters::SearchFilters::default(),
            )
            .expect("content search succeeds")
            .expect("content search returns a result");
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].name, "notes.txt");
        assert_eq!(result.files[0].matches[0].line_number, 2);
        assert!(result.files[0].matches[0].text.contains("needle"));
        assert!(result.total_matches >= 1);
    }

    #[test]
    fn core_exposes_platform_and_version() {
        let core = KiteCore::new();
        assert!(!core.version().is_empty());
        assert!(matches!(
            core.platform().as_str(),
            "windows" | "macos" | "other"
        ));
    }

    #[test]
    fn core_exposes_download_progress() {
        let progress = KiteCore::new().download_progress();

        assert_eq!(progress.downloaded_bytes, 0);
        assert_eq!(progress.total_bytes, 0);
        assert!(!progress.active);
    }

    #[test]
    fn git_log_dto_preserves_shared_graph_contract() {
        let page = crate::git::log::GitLogPage {
            repository_root: PathBuf::from(r"C:\repo"),
            commits: vec![GitLogCommit {
                hash: "abcdef".to_string(),
                short_hash: "abcdef".to_string(),
                parents: vec!["123456".to_string()],
                author_name: "Author".to_string(),
                author_email: "author@example.test".to_string(),
                authored_at: 1,
                committer_name: "Committer".to_string(),
                committer_email: "committer@example.test".to_string(),
                committed_at: 2,
                subject: "subject".to_string(),
                refs: vec![GitRef {
                    name: "main".to_string(),
                    full_name: "refs/heads/main".to_string(),
                    kind: GitLogRefKind::LocalBranch,
                    is_head: true,
                }],
                graph: GitGraphRow {
                    lane: 1,
                    lane_count: 2,
                    color_index: 3,
                    edges: vec![crate::git::log::GitGraphEdge {
                        from_lane: 0,
                        to_lane: 1,
                        color_index: 2,
                    }],
                },
            }],
            has_more: true,
            next_cursor: Some("cursor".to_string()),
            loaded_count: 1,
        };
        let dto = git_log_page_dto(page);
        assert_eq!(dto.commits[0].graph.lane, 1);
        assert_eq!(dto.commits[0].graph.lane_count, 2);
        assert_eq!(dto.commits[0].graph.color_index, 3);
        assert_eq!(dto.commits[0].graph.edges[0].from_lane, 0);
        assert_eq!(dto.commits[0].graph.edges[0].to_lane, 1);
        assert_eq!(dto.commits[0].refs[0].kind, GitRefKindDto::LocalBranch);
        assert_eq!(dto.next_cursor.as_deref(), Some("cursor"));
    }
}
