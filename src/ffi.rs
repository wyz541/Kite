use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::UNIX_EPOCH;

use crate::agent::AgentProfileStore;
use crate::archive::{self, ArchiveFormat, ArchivePhase, ArchiveProgress};
use crate::content_search::{self, ContentFileMatch, ContentLineMatch, ContentSearchSnapshot};
use crate::devices::{self, PortableDevice, StorageVolume, VolumeKind};
use crate::diff::{
    self, DiffContentKind, DiffEndpoint, DiffHunk, DiffLine, DiffRow, DiffRowKind, DiffStats,
    DiffSyntaxKind, DiffSyntaxSpan, DiffWordRange, GitDiffDocument, GitDiffRequest,
};
use crate::favorites::{self as favorites_store, FavoriteEntryDto};
use crate::fs::{EntryKind, FileEntry, SortSpec, VcsStatus, load_directory, transfer_entry};
use crate::git::GitEventSink as _;
use crate::git::log::{
    self as git_log, GitChangedFile as GitLogChangedFile, GitCommitDetails, GitGraphRow,
    GitLogCommit, GitLogFilter, GitLogPage, GitRef, GitRefKind as GitLogRefKind, GitSignature,
    GitTreeEntry,
};
use crate::git::remote::{
    self as git_remote, GitCloneOptions, GitCredentials, GitFailure, GitFailureKind,
    GitFetchOptions, GitNetworkOperation, GitProgressPhase, GitPullOptions, GitPullStrategy,
    GitPushOptions,
};
use crate::git::{
    self, GitChangedFile, GitCommitOptions, GitCommitResult, GitCommitState, GitDiffLine,
    GitDiffLineKind, GitOperationAction, GitOperationState, GitPathSummary, GitRepositorySnapshot,
    GitSigningMode,
};
use crate::git_branches::{
    self, GitBranch, GitBranchSnapshot, GitCompareCommit, GitCompareFile, GitCompareResult,
    GitDirtyPolicy, GitHeadSummary, GitRefKind, GitTag, GitWorktree,
};
use crate::git_changes::{self, GitChangeGroup, GitDiffScope, GitHunkLineKind};
use crate::git_ops::{
    self, GitCommandOutcome, GitRepositoryStatus, GitResetMode, GitStashEntry, GitStashSaveOptions,
};
use crate::git_rebase::{
    self, GitRebaseAction, GitRebaseCommit, GitRebaseOptions, GitRebasePlan, GitRebaseSpec,
    GitRebaseState, GitRebaseStatus, GitRebaseTodo,
};
use crate::platform::{HostPlatform, default_root_path, host_platform};
use crate::session::{self as session_store, GitClientWindowSessionDto, SessionStateDto};
use crate::text_editor::{self, TextDocument, TextLineEnding};
use crate::updater::{self, UpdateInfo};
use crate::vcs::{
    VcsCancellation, VcsOperation, VcsQuery, VcsTool, apply_statuses, load_vcs_snapshot,
    run_vcs_operation, run_vcs_operation_cancellable, run_vcs_query,
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

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
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

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitChangedFileDto {
    pub path: String,
    pub staged_status: Option<String>,
    pub staged_code: Option<String>,
    pub unstaged_status: Option<String>,
    pub unstaged_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitOperationKindDto {
    Rebasing,
    Merging,
    CherryPicking,
    Reverting,
    Bisecting,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRepositorySnapshotDto {
    pub repository_root: String,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<GitOperationKindDto>,
    pub changes: GitChangeCountsDto,
    pub changed_files: Vec<GitChangedFileDto>,
    pub recent_commits: Vec<GitCommitSummaryDto>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitOperationControlDto {
    ContinueOperation,
    Skip,
    Abort,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum GitSigningModeDto {
    #[default]
    GitConfig,
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCommitOptionsDto {
    pub repository_path: String,
    pub message: String,
    pub amend: bool,
    pub signoff: bool,
    pub signing: GitSigningModeDto,
    pub skip_hooks: bool,
    pub author: Option<String>,
    pub push: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCommitStateDto {
    pub repository_root: String,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub identity_configured: bool,
    pub staged_files: Vec<String>,
    pub staged_lines_added: u32,
    pub staged_lines_removed: u32,
    pub amend_files: Vec<String>,
    pub amend_lines_added: u32,
    pub amend_lines_removed: u32,
    pub unresolved_conflicts: Vec<String>,
    pub commit_template: Option<String>,
    pub previous_message: Option<String>,
    pub recent_messages: Vec<String>,
    pub signing_default: bool,
    pub signing_format: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCommitResultDto {
    pub hash: String,
    pub short_hash: String,
    pub subject: String,
    pub amended: bool,
    pub signed: bool,
    pub pushed: bool,
    pub command_output: Option<String>,
    pub push_error: Option<String>,
    pub push_output: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitStashEntryDto {
    pub selector: String,
    pub commit_hash: String,
    pub short_hash: String,
    pub date: String,
    pub message: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRepositoryStatusDto {
    pub repository_root: String,
    pub branch: Option<String>,
    pub operation: Option<String>,
    pub has_conflicts: bool,
    pub stash_count: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitCommandOutcomeDto {
    pub message: String,
    pub operation: Option<String>,
    pub has_conflicts: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitResetModeDto {
    Soft,
    Mixed,
    Hard,
    Keep,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRemoteDto {
    pub name: String,
    pub fetch_url: String,
    pub push_url: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRepositoryDto {
    pub root: String,
    pub git_dir: String,
    pub branch: Option<String>,
    pub head_oid: Option<String>,
    pub upstream: Option<String>,
    pub remotes: Vec<GitRemoteDto>,
    pub dirty: bool,
    pub operation: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitFileChangeDto {
    pub status: String,
    pub path: String,
    pub original_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum GitProgressPhaseDto {
    #[default]
    Idle,
    Preparing,
    Transferring,
    Resolving,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default, PartialEq, uniffi::Record)]
pub struct GitOperationProgressDto {
    pub operation: Option<String>,
    pub phase: GitProgressPhaseDto,
    pub message: String,
    pub detail: String,
    pub fraction: Option<f64>,
    pub active: bool,
    pub cancelled: bool,
}

#[derive(Debug, uniffi::Object)]
pub struct GitProgressMonitor {
    inner: Arc<git_remote::GitOperationMonitor>,
}

#[uniffi::export]
impl GitProgressMonitor {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: git_remote::GitOperationMonitor::new(),
        })
    }

    pub fn snapshot(&self) -> GitOperationProgressDto {
        git_progress_dto(self.inner.snapshot())
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn reset(&self) {
        self.inner.reset();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitCredentialsDto {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct GitFetchOptionsDto {
    pub remote: Option<String>,
    pub all_remotes: bool,
    pub prune: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum GitPullStrategyDto {
    #[default]
    Merge,
    Rebase,
    FastForwardOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitPullOptionsDto {
    pub strategy: GitPullStrategyDto,
    pub auto_stash: bool,
    pub remote: Option<String>,
    pub branch: Option<String>,
}

impl Default for GitPullOptionsDto {
    fn default() -> Self {
        Self {
            strategy: GitPullStrategyDto::Merge,
            auto_stash: true,
            remote: None,
            branch: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct GitPushOptionsDto {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub include_tags: bool,
    pub force_with_lease: bool,
    pub up_to_commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitPushPreviewDto {
    pub remote: String,
    pub branch: String,
    pub source_oid: Option<String>,
    pub commits: Vec<GitCommitSummaryDto>,
    pub tags: Vec<String>,
    pub protected_branch: bool,
    pub force_with_lease: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct GitCloneOptionsDto {
    pub shallow_depth: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitCloneResultDto {
    pub path: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitRefKindDto {
    LocalBranch,
    RemoteBranch,
    Tag,
    Commit,
    Other,
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
pub struct GitLogChangedFileDto {
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
    pub changed_files: Vec<GitLogChangedFileDto>,
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
pub enum GitChangeGroupDto {
    Staged,
    Unstaged,
    Untracked,
    Conflicted,
    Ignored,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitChangeFileDto {
    pub path: String,
    pub old_path: Option<String>,
    pub group: GitChangeGroupDto,
    pub status: String,
    pub status_code: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRepositoryChangesDto {
    pub repository_root: String,
    pub branch: Option<String>,
    pub head_short_hash: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub operation: Option<String>,
    pub files: Vec<GitChangeFileDto>,
    pub staged_count: u32,
    pub unstaged_count: u32,
    pub untracked_count: u32,
    pub conflicted_count: u32,
    pub ignored_count: u32,
    pub crlf_warning: Option<String>,
    pub includes_ignored: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitDiffScopeDto {
    Unstaged,
    Staged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitHunkLineKindDto {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitHunkLineDto {
    pub kind: GitHunkLineKindDto,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitDiffHunkDto {
    pub index: u32,
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<GitHunkLineDto>,
    pub selections: Vec<GitLineSelectionDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitFileChangesDiffDto {
    pub path: String,
    pub old_path: Option<String>,
    pub scope: GitDiffScopeDto,
    pub hunks: Vec<GitDiffHunkDto>,
    pub binary: bool,
    pub truncated: bool,
    pub line_ending_warning: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitLineSelectionDto {
    pub hunk_index: u32,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitChangeOperationResultDto {
    pub message: String,
    pub changes: GitRepositoryChangesDto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitRebaseActionDto {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRebaseTodoDto {
    pub commit: String,
    pub action: GitRebaseActionDto,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct GitRebaseOptionsDto {
    pub update_refs: bool,
    pub autosquash: bool,
    pub preserve_merges: bool,
    pub allow_rewrite_pushed: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRebaseSpecDto {
    pub upstream: String,
    pub onto: String,
    pub options: GitRebaseOptionsDto,
    pub todo: Vec<GitRebaseTodoDto>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRebaseCommitDto {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub unix_seconds: i64,
    pub subject: String,
    pub message: String,
    pub parents: Vec<String>,
    pub pushed: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRebasePlanDto {
    pub repository_root: String,
    pub branch: Option<String>,
    pub head: String,
    pub upstream: String,
    pub onto: String,
    pub operation: Option<String>,
    pub protected: bool,
    pub has_merges: bool,
    pub pushed_commits: Vec<String>,
    pub commits: Vec<GitRebaseCommitDto>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitRebaseStatusDto {
    InProgress,
    Stopped,
    Completed,
    Aborted,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitRebaseStateDto {
    pub status: GitRebaseStatusDto,
    pub head: String,
    pub message: String,
    pub remaining: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitDiffEndpointDto {
    Revision { revision: String },
    Index,
    WorkingTree,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GitDiffRequestDto {
    pub path: String,
    pub old: GitDiffEndpointDto,
    pub new: GitDiffEndpointDto,
    pub ignore_whitespace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DiffContentKindDto {
    Text,
    Binary,
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DiffRowKindDto {
    File,
    Hunk,
    Context,
    Deletion,
    Addition,
    Modification,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DiffWordRangeDto {
    pub start_utf16: u32,
    pub end_utf16: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DiffSyntaxKindDto {
    Keyword,
    String,
    Number,
    Comment,
    TypeName,
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DiffSyntaxSpanDto {
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub kind: DiffSyntaxKindDto,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DiffLineDto {
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    pub highlighted: Vec<DiffWordRangeDto>,
    pub syntax: Vec<DiffSyntaxSpanDto>,
    pub missing_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DiffRowDto {
    pub kind: DiffRowKindDto,
    pub text: String,
    pub left: Option<DiffLineDto>,
    pub right: Option<DiffLineDto>,
    pub change_index: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct DiffStatsDto {
    pub files: u32,
    pub additions: u32,
    pub deletions: u32,
    pub hunks: u32,
    pub old_bytes: Option<u64>,
    pub new_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DiffHunkDto {
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub first_row: u32,
    pub row_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitDiffDocumentDto {
    pub repository_root: String,
    pub relative_path: String,
    pub old_label: String,
    pub new_label: String,
    pub rows: Vec<DiffRowDto>,
    pub hunks: Vec<DiffHunkDto>,
    pub change_rows: Vec<u32>,
    pub stats: DiffStatsDto,
    pub content_kind: DiffContentKindDto,
    pub large_file: bool,
    pub truncated: bool,
    pub message: Option<String>,
    pub old_image: Option<Vec<u8>>,
    pub new_image: Option<Vec<u8>>,
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
    Stash,
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
    #[error("{message}")]
    GitAuthenticationRequired { message: String },
    #[error("{message}")]
    GitPushRejected { message: String },
    #[error("{message}")]
    GitProtectedBranch { message: String },
    #[error("{message}")]
    GitWorkingTreeDirty { message: String },
    #[error("{message}")]
    GitConflict { message: String },
    #[error("{message}")]
    GitCancelled {
        message: String,
        partial_clone_path: Option<String>,
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
    agent_profile_store: Arc<AgentProfileStore>,
}

#[uniffi::export]
impl KiteCore {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            search_cancel: Mutex::new(None),
            indexed_search_cancel: Mutex::new(None),
            agent_profile_store: AgentProfileStore::new(),
        })
    }

    pub fn agent_profile_store(&self) -> Arc<AgentProfileStore> {
        Arc::clone(&self.agent_profile_store)
    }

    pub fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    pub fn agent_config(&self) -> crate::agent::AgentProfileSnapshot {
        self.agent_profile_store.snapshot()
    }

    pub fn save_agent_profile(
        &self,
        profile: crate::agent::AgentProfile,
        credential: Option<String>,
        clear_credential: bool,
    ) -> Result<crate::agent::AgentProfileSnapshot, KiteError> {
        self.agent_profile_store
            .save_profile_with_credential(profile, credential, clear_credential)
            .map_err(|error| KiteError::operation(error.to_string()))
    }

    pub fn delete_agent_profile(
        &self,
        profile_id: String,
        replacement_default_id: Option<String>,
    ) -> Result<crate::agent::AgentProfileSnapshot, KiteError> {
        self.agent_profile_store
            .delete_profile_with_replacement(profile_id, replacement_default_id)
            .map_err(|error| KiteError::operation(error.to_string()))
    }

    pub fn set_default_agent_profile(
        &self,
        profile_id: String,
    ) -> Result<crate::agent::AgentProfileSnapshot, KiteError> {
        self.agent_profile_store
            .set_default_profile(profile_id)
            .map_err(|error| KiteError::operation(error.to_string()))
    }

    pub fn duplicate_agent_profile(
        &self,
        profile_id: String,
    ) -> Result<crate::agent::AgentProfileSnapshot, KiteError> {
        self.agent_profile_store
            .duplicate_profile(profile_id)
            .map_err(|error| KiteError::operation(error.to_string()))
    }

    pub fn test_agent_connection(
        &self,
        profile: crate::agent::AgentProfile,
        credential: Option<String>,
    ) -> crate::agent::AgentConnectionResult {
        crate::agent::test_agent_connection(profile, credential)
    }

    pub fn new_agent_profile_id(&self) -> String {
        crate::agent::new_profile_id()
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

    pub fn load_git_client_window_state(
        &self,
        repository_root: String,
    ) -> Option<GitClientWindowSessionDto> {
        session_store::load_git_client_window_state(&PathBuf::from(repository_root))
    }

    pub fn save_git_client_window_state(
        &self,
        state: GitClientWindowSessionDto,
    ) -> Result<(), KiteError> {
        session_store::save_git_client_window_state(&state).map_err(KiteError::operation)
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

    pub fn git_commit_state(&self, path: String) -> Result<GitCommitStateDto, KiteError> {
        git::git_commit_state(&PathBuf::from(path))
            .map(git_commit_state_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_commit_subject_warning(&self, message: String) -> Option<String> {
        git::commit_subject_warning(&message)
    }

    pub fn git_commit(
        &self,
        options: GitCommitOptionsDto,
    ) -> Result<GitCommitResultDto, KiteError> {
        git::commit(git_commit_options(options))
            .map(git_commit_result_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_repository_root(&self, path: String) -> Result<Option<String>, KiteError> {
        git::git_repository_root(&PathBuf::from(path))
            .map(|root| root.map(|root| root.to_string_lossy().into_owned()))
            .map_err(KiteError::operation)
    }

    pub fn git_repository_snapshot(
        &self,
        path: String,
    ) -> Result<Option<GitRepositorySnapshotDto>, KiteError> {
        git::git_repository_snapshot(&PathBuf::from(path))
            .map(|snapshot| snapshot.map(git_repository_snapshot_dto))
            .map_err(KiteError::operation)
    }

    pub fn git_file_diff(&self, path: String) -> Result<Vec<GitDiffLineDto>, KiteError> {
        git::git_file_diff(&PathBuf::from(path))
            .map(|lines| lines.into_iter().map(git_diff_line_dto).collect())
            .map_err(KiteError::operation)
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

    pub fn git_operation_status(&self, path: String) -> Result<GitRepositoryStatusDto, KiteError> {
        git_ops::git_operation_status(&PathBuf::from(path))
            .map(git_repository_status_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_stash_list(&self, path: String) -> Result<Vec<GitStashEntryDto>, KiteError> {
        git_ops::git_stash_list(&PathBuf::from(path))
            .map(|entries| entries.into_iter().map(git_stash_entry_dto).collect())
            .map_err(KiteError::operation)
    }

    pub fn git_stash_diff(
        &self,
        path: String,
        index: u32,
    ) -> Result<Vec<GitDiffLineDto>, KiteError> {
        git_ops::git_stash_diff(&PathBuf::from(path), index)
            .map(|lines| lines.into_iter().map(git_diff_line_dto).collect())
            .map_err(KiteError::operation)
    }

    pub fn git_stash_save(
        &self,
        path: String,
        message: Option<String>,
        include_untracked: bool,
        keep_index: bool,
        paths: Vec<String>,
    ) -> Result<String, KiteError> {
        git_ops::git_stash_save(
            &PathBuf::from(path),
            GitStashSaveOptions {
                message,
                include_untracked,
                keep_index,
                paths: paths.into_iter().map(PathBuf::from).collect(),
            },
        )
        .map_err(KiteError::operation)
    }

    pub fn git_stash_apply(
        &self,
        path: String,
        index: u32,
    ) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_stash_apply(&PathBuf::from(path), index)
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_stash_pop(
        &self,
        path: String,
        index: u32,
    ) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_stash_pop(&PathBuf::from(path), index)
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_stash_drop(
        &self,
        path: String,
        index: u32,
        confirmed: bool,
    ) -> Result<String, KiteError> {
        git_ops::git_stash_drop(&PathBuf::from(path), index, confirmed)
            .map_err(KiteError::operation)
    }

    pub fn git_stash_clear(&self, path: String, confirmed: bool) -> Result<String, KiteError> {
        git_ops::git_stash_clear(&PathBuf::from(path), confirmed).map_err(KiteError::operation)
    }

    pub fn git_stash_branch(
        &self,
        path: String,
        index: u32,
        branch: String,
    ) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_stash_branch(&PathBuf::from(path), index, &branch)
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_reset(
        &self,
        path: String,
        mode: GitResetModeDto,
        target: String,
        confirmed: bool,
    ) -> Result<String, KiteError> {
        git_ops::git_reset(
            &PathBuf::from(path),
            git_reset_mode(mode),
            &target,
            confirmed,
        )
        .map_err(KiteError::operation)
    }

    pub fn git_revert(
        &self,
        path: String,
        commits: Vec<String>,
    ) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_revert(&PathBuf::from(path), &commits)
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_revert_continue(&self, path: String) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_revert_continue(&PathBuf::from(path))
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_revert_abort(&self, path: String) -> Result<String, KiteError> {
        git_ops::git_revert_abort(&PathBuf::from(path)).map_err(KiteError::operation)
    }

    pub fn git_cherry_pick(
        &self,
        path: String,
        commits: Vec<String>,
    ) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_cherry_pick(&PathBuf::from(path), &commits)
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_cherry_pick_continue(
        &self,
        path: String,
    ) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_cherry_pick_continue(&PathBuf::from(path))
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_cherry_pick_skip(&self, path: String) -> Result<GitCommandOutcomeDto, KiteError> {
        git_ops::git_cherry_pick_skip(&PathBuf::from(path))
            .map(git_command_outcome_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_cherry_pick_abort(&self, path: String) -> Result<String, KiteError> {
        git_ops::git_cherry_pick_abort(&PathBuf::from(path)).map_err(KiteError::operation)
    }

    /// Flat repository change list used by the Git client window.
    pub fn git_repository_changes(
        &self,
        path: String,
        include_ignored: bool,
    ) -> Result<GitRepositoryChangesDto, KiteError> {
        git_changes::repository_changes(&PathBuf::from(path), include_ignored)
            .map(git_repository_changes_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_changes_diff(
        &self,
        repository_root: String,
        path: String,
        scope: GitDiffScopeDto,
    ) -> Result<GitFileChangesDiffDto, KiteError> {
        git_changes::file_changes_diff(
            &PathBuf::from(repository_root),
            &path,
            git_diff_scope(scope),
        )
        .map(git_file_changes_diff_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_stage_lines(
        &self,
        repository_root: String,
        path: String,
        selections: Vec<GitLineSelectionDto>,
    ) -> Result<GitChangeOperationResultDto, KiteError> {
        git_changes::stage_lines(
            &PathBuf::from(repository_root),
            &path,
            &git_line_selections(selections),
        )
        .map(git_change_operation_result_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_unstage_lines(
        &self,
        repository_root: String,
        path: String,
        selections: Vec<GitLineSelectionDto>,
    ) -> Result<GitChangeOperationResultDto, KiteError> {
        git_changes::unstage_lines(
            &PathBuf::from(repository_root),
            &path,
            &git_line_selections(selections),
        )
        .map(git_change_operation_result_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_discard_lines(
        &self,
        repository_root: String,
        path: String,
        selections: Vec<GitLineSelectionDto>,
    ) -> Result<GitChangeOperationResultDto, KiteError> {
        git_changes::discard_lines(
            &PathBuf::from(repository_root),
            &path,
            &git_line_selections(selections),
        )
        .map(git_change_operation_result_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_stage_paths(
        &self,
        repository_root: String,
        paths: Vec<String>,
    ) -> Result<GitChangeOperationResultDto, KiteError> {
        git_changes::stage_paths(&PathBuf::from(repository_root), &paths)
            .map(git_change_operation_result_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_unstage_paths(
        &self,
        repository_root: String,
        paths: Vec<String>,
    ) -> Result<GitChangeOperationResultDto, KiteError> {
        git_changes::unstage_paths(&PathBuf::from(repository_root), &paths)
            .map(git_change_operation_result_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_discard_paths(
        &self,
        repository_root: String,
        paths: Vec<String>,
    ) -> Result<GitChangeOperationResultDto, KiteError> {
        git_changes::discard_paths(&PathBuf::from(repository_root), &paths)
            .map(git_change_operation_result_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_add_to_gitignore(
        &self,
        repository_root: String,
        path: String,
        pattern: Option<String>,
        local_exclude: bool,
    ) -> Result<String, KiteError> {
        git_changes::add_to_gitignore(
            &PathBuf::from(repository_root),
            &path,
            pattern.as_deref(),
            local_exclude,
        )
        .map_err(KiteError::operation)
    }

    /// Structured two-endpoint diff for the shared native diff viewers.
    pub fn git_diff(&self, request: GitDiffRequestDto) -> Result<GitDiffDocumentDto, KiteError> {
        diff::git_diff(&git_diff_request(request))
            .map(git_diff_document_dto)
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

    pub fn run_vcs_operation_cancellable(
        &self,
        tool: VcsToolDto,
        operation: VcsOperationDto,
        path: String,
        cancellation: Arc<VcsCancellation>,
    ) -> Result<String, KiteError> {
        run_vcs_operation_cancellable(
            vcs_tool(tool),
            vcs_operation(operation),
            &PathBuf::from(path),
            &cancellation,
        )
        .map_err(KiteError::operation)
    }

    pub fn run_git_operation_control(
        &self,
        path: String,
        control: GitOperationControlDto,
    ) -> Result<String, KiteError> {
        git::run_git_operation_control(&PathBuf::from(path), git_operation_action(control))
            .map_err(KiteError::operation)
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

    pub fn git_rebase_plan(&self, path: String) -> Result<GitRebasePlanDto, KiteError> {
        git_rebase::git_rebase_plan(&PathBuf::from(path))
            .map(git_rebase_plan_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_rebase_status(&self, path: String) -> Result<Option<GitRebaseStateDto>, KiteError> {
        git_rebase::git_rebase_status(&PathBuf::from(path))
            .map(|state| state.map(git_rebase_state_dto))
            .map_err(KiteError::operation)
    }

    pub fn git_rebase_start(
        &self,
        path: String,
        spec: GitRebaseSpecDto,
    ) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase(&PathBuf::from(path), &git_rebase_spec(spec))
            .map(git_rebase_state_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_rebase_continue(&self, path: String) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase_continue(&PathBuf::from(path))
            .map(git_rebase_state_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_rebase_skip(&self, path: String) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase_skip(&PathBuf::from(path))
            .map(git_rebase_state_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_rebase_abort(&self, path: String) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase_abort(&PathBuf::from(path))
            .map(git_rebase_state_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_rebase_reword(
        &self,
        path: String,
        commit: String,
        message: String,
        allow_rewrite_pushed: bool,
    ) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase_reword(
            &PathBuf::from(path),
            &commit,
            &message,
            allow_rewrite_pushed,
        )
        .map(git_rebase_state_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_rebase_squash(
        &self,
        path: String,
        commits: Vec<String>,
        fixup: bool,
        message: Option<String>,
        allow_rewrite_pushed: bool,
    ) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase_squash(
            &PathBuf::from(path),
            &commits,
            fixup,
            message,
            allow_rewrite_pushed,
        )
        .map(git_rebase_state_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_rebase_fixup_worktree(
        &self,
        path: String,
        commit: String,
        allow_rewrite_pushed: bool,
    ) -> Result<GitRebaseStateDto, KiteError> {
        git_rebase::git_rebase_fixup_worktree(&PathBuf::from(path), &commit, allow_rewrite_pushed)
            .map(git_rebase_state_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_repository(&self, path: String) -> Result<GitRepositoryDto, KiteError> {
        git_remote::repository(&PathBuf::from(path))
            .map(git_repository_dto)
            .map_err(git_remote_error)
    }

    pub fn git_list_remotes(&self, path: String) -> Result<Vec<GitRemoteDto>, KiteError> {
        git_remote::list_remotes(&PathBuf::from(path))
            .map(|remotes| remotes.into_iter().map(git_remote_dto).collect())
            .map_err(git_remote_error)
    }

    pub fn git_add_remote(&self, path: String, name: String, url: String) -> Result<(), KiteError> {
        git_remote::add_remote(&PathBuf::from(path), &name, &url).map_err(git_remote_error)
    }

    pub fn git_rename_remote(
        &self,
        path: String,
        old_name: String,
        new_name: String,
    ) -> Result<(), KiteError> {
        git_remote::rename_remote(&PathBuf::from(path), &old_name, &new_name)
            .map_err(git_remote_error)
    }

    pub fn git_remove_remote(&self, path: String, name: String) -> Result<(), KiteError> {
        git_remote::remove_remote(&PathBuf::from(path), &name).map_err(git_remote_error)
    }

    pub fn git_set_remote_url(
        &self,
        path: String,
        name: String,
        fetch_url: String,
        push_url: Option<String>,
    ) -> Result<(), KiteError> {
        git_remote::set_remote_url(&PathBuf::from(path), &name, &fetch_url, push_url.as_deref())
            .map_err(git_remote_error)
    }

    pub fn git_fetch(
        &self,
        path: String,
        options: GitFetchOptionsDto,
        credentials: Option<GitCredentialsDto>,
        monitor: Arc<GitProgressMonitor>,
    ) -> Result<String, KiteError> {
        let credentials = credentials.map(git_credentials);
        git_remote::fetch(
            &PathBuf::from(path),
            &GitFetchOptions {
                remote: options.remote,
                all_remotes: options.all_remotes,
                prune: options.prune,
            },
            credentials.as_ref(),
            &monitor.inner,
        )
        .map_err(git_remote_error)
    }

    pub fn git_pull(
        &self,
        path: String,
        options: GitPullOptionsDto,
        credentials: Option<GitCredentialsDto>,
        monitor: Arc<GitProgressMonitor>,
    ) -> Result<String, KiteError> {
        let credentials = credentials.map(git_credentials);
        git_remote::pull(
            &PathBuf::from(path),
            &GitPullOptions {
                strategy: git_pull_strategy(options.strategy),
                auto_stash: options.auto_stash,
                remote: options.remote,
                branch: options.branch,
            },
            credentials.as_ref(),
            &monitor.inner,
        )
        .map_err(git_remote_error)
    }

    pub fn git_push_preview(
        &self,
        path: String,
        options: GitPushOptionsDto,
    ) -> Result<GitPushPreviewDto, KiteError> {
        git_remote::push_preview(&PathBuf::from(path), &git_push_options(options))
            .map(git_push_preview_dto)
            .map_err(git_remote_error)
    }

    pub fn git_commit_files(
        &self,
        path: String,
        oid: String,
    ) -> Result<Vec<GitFileChangeDto>, KiteError> {
        git_remote::commit_files(&PathBuf::from(path), &oid)
            .map(|changes| {
                changes
                    .into_iter()
                    .map(|change| GitFileChangeDto {
                        status: change.status,
                        path: change.path,
                        original_path: change.original_path,
                    })
                    .collect()
            })
            .map_err(git_remote_error)
    }

    pub fn git_push(
        &self,
        path: String,
        options: GitPushOptionsDto,
        credentials: Option<GitCredentialsDto>,
        monitor: Arc<GitProgressMonitor>,
    ) -> Result<String, KiteError> {
        let credentials = credentials.map(git_credentials);
        git_remote::push(
            &PathBuf::from(path),
            &git_push_options(options),
            credentials.as_ref(),
            &monitor.inner,
        )
        .map_err(git_remote_error)
    }

    pub fn git_clone(
        &self,
        url: String,
        target: String,
        options: GitCloneOptionsDto,
        credentials: Option<GitCredentialsDto>,
        monitor: Arc<GitProgressMonitor>,
    ) -> Result<GitCloneResultDto, KiteError> {
        let credentials = credentials.map(git_credentials);
        git_remote::clone_repository(
            &url,
            &PathBuf::from(target),
            &GitCloneOptions {
                shallow_depth: options.shallow_depth,
            },
            credentials.as_ref(),
            &monitor.inner,
        )
        .map(|result| GitCloneResultDto {
            path: result.path.to_string_lossy().into_owned(),
        })
        .map_err(git_remote_error)
    }

    pub fn git_init(&self, path: String) -> Result<String, KiteError> {
        git_remote::init_repository(&PathBuf::from(path))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(git_remote_error)
    }

    pub fn git_delete_partial_clone(&self, path: String) -> Result<(), KiteError> {
        git_remote::delete_partial_clone(&PathBuf::from(path)).map_err(git_remote_error)
    }

    pub fn git_adopt_partial_clone(
        &self,
        partial_path: String,
        target: String,
    ) -> Result<(), KiteError> {
        git_remote::adopt_partial_clone(&PathBuf::from(partial_path), &PathBuf::from(target))
            .map_err(git_remote_error)
    }
}

fn git_rebase_action(action: GitRebaseActionDto) -> GitRebaseAction {
    match action {
        GitRebaseActionDto::Pick => GitRebaseAction::Pick,
        GitRebaseActionDto::Reword => GitRebaseAction::Reword,
        GitRebaseActionDto::Edit => GitRebaseAction::Edit,
        GitRebaseActionDto::Squash => GitRebaseAction::Squash,
        GitRebaseActionDto::Fixup => GitRebaseAction::Fixup,
        GitRebaseActionDto::Drop => GitRebaseAction::Drop,
    }
}

fn git_rebase_options(options: GitRebaseOptionsDto) -> GitRebaseOptions {
    GitRebaseOptions {
        update_refs: options.update_refs,
        autosquash: options.autosquash,
        preserve_merges: options.preserve_merges,
        allow_rewrite_pushed: options.allow_rewrite_pushed,
    }
}

fn git_rebase_spec(spec: GitRebaseSpecDto) -> GitRebaseSpec {
    GitRebaseSpec {
        upstream: spec.upstream,
        onto: spec.onto,
        options: git_rebase_options(spec.options),
        todo: spec
            .todo
            .into_iter()
            .map(|item| GitRebaseTodo {
                commit: item.commit,
                action: git_rebase_action(item.action),
                message: item.message,
            })
            .collect(),
    }
}

fn git_rebase_plan_dto(plan: GitRebasePlan) -> GitRebasePlanDto {
    GitRebasePlanDto {
        repository_root: plan.repository_root.to_string_lossy().into_owned(),
        branch: plan.branch,
        head: plan.head,
        upstream: plan.upstream,
        onto: plan.onto,
        operation: plan.operation,
        protected: plan.protected,
        has_merges: plan.has_merges,
        pushed_commits: plan.pushed_commits,
        commits: plan
            .commits
            .into_iter()
            .map(git_rebase_commit_dto)
            .collect(),
    }
}

fn git_rebase_commit_dto(commit: GitRebaseCommit) -> GitRebaseCommitDto {
    GitRebaseCommitDto {
        hash: commit.hash,
        short_hash: commit.short_hash,
        author: commit.author,
        unix_seconds: commit.unix_seconds,
        subject: commit.subject,
        message: commit.message,
        parents: commit.parents,
        pushed: commit.pushed,
    }
}

fn git_rebase_state_dto(state: GitRebaseState) -> GitRebaseStateDto {
    GitRebaseStateDto {
        status: match state.status {
            GitRebaseStatus::InProgress => GitRebaseStatusDto::InProgress,
            GitRebaseStatus::Stopped => GitRebaseStatusDto::Stopped,
            GitRebaseStatus::Completed => GitRebaseStatusDto::Completed,
            GitRebaseStatus::Aborted => GitRebaseStatusDto::Aborted,
        },
        head: state.head,
        message: state.message,
        remaining: state.remaining,
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

fn vcs_tool(tool: VcsToolDto) -> VcsTool {
    match tool {
        VcsToolDto::Git => VcsTool::Git,
        VcsToolDto::Svn => VcsTool::Svn,
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
        VcsOperationDto::Stash => VcsOperation::Stash,
        VcsOperationDto::Revert => VcsOperation::Revert,
        VcsOperationDto::Clean => VcsOperation::Clean,
        VcsOperationDto::Update => VcsOperation::Update,
        VcsOperationDto::Lock => VcsOperation::Lock,
        VcsOperationDto::Unlock => VcsOperation::Unlock,
        VcsOperationDto::Cleanup => VcsOperation::Cleanup,
    }
}

fn git_operation_action(control: GitOperationControlDto) -> GitOperationAction {
    match control {
        GitOperationControlDto::ContinueOperation => GitOperationAction::Continue,
        GitOperationControlDto::Skip => GitOperationAction::Skip,
        GitOperationControlDto::Abort => GitOperationAction::Abort,
    }
}

fn git_operation_kind(operation: GitOperationState) -> GitOperationKindDto {
    match operation {
        GitOperationState::Rebasing => GitOperationKindDto::Rebasing,
        GitOperationState::Merging => GitOperationKindDto::Merging,
        GitOperationState::CherryPicking => GitOperationKindDto::CherryPicking,
        GitOperationState::Reverting => GitOperationKindDto::Reverting,
        GitOperationState::Bisecting => GitOperationKindDto::Bisecting,
    }
}

fn git_repository_snapshot_dto(snapshot: GitRepositorySnapshot) -> GitRepositorySnapshotDto {
    GitRepositorySnapshotDto {
        repository_root: snapshot.repository_root.to_string_lossy().into_owned(),
        branch: snapshot.branch,
        head_short_hash: snapshot.head_short_hash,
        upstream: snapshot.upstream,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        operation: snapshot.operation.map(git_operation_kind),
        changes: GitChangeCountsDto {
            modified: snapshot.changes.modified,
            added: snapshot.changes.added,
            deleted: snapshot.changes.deleted,
            renamed: snapshot.changes.renamed,
            untracked: snapshot.changes.untracked,
            conflicted: snapshot.changes.conflicted,
        },
        changed_files: snapshot
            .changed_files
            .into_iter()
            .map(git_changed_file_dto)
            .collect(),
        recent_commits: snapshot
            .recent_commits
            .into_iter()
            .map(git_commit_summary_dto)
            .collect(),
    }
}

fn git_changed_file_dto(file: GitChangedFile) -> GitChangedFileDto {
    GitChangedFileDto {
        path: file.path,
        staged_status: file.staged_status.map(|status| status.label().to_string()),
        staged_code: file.staged_status.map(|status| status.code().to_string()),
        unstaged_status: file
            .unstaged_status
            .map(|status| status.label().to_string()),
        unstaged_code: file.unstaged_status.map(|status| status.code().to_string()),
    }
}

fn git_commit_summary_dto(commit: git::GitCommitSummary) -> GitCommitSummaryDto {
    GitCommitSummaryDto {
        hash: commit.hash,
        short_hash: commit.short_hash,
        author: commit.author,
        unix_seconds: commit.unix_seconds,
        subject: commit.subject,
    }
}

fn git_credentials(credentials: GitCredentialsDto) -> GitCredentials {
    GitCredentials {
        username: credentials.username,
        password: credentials.password,
    }
}

fn git_pull_strategy(strategy: GitPullStrategyDto) -> GitPullStrategy {
    match strategy {
        GitPullStrategyDto::Merge => GitPullStrategy::Merge,
        GitPullStrategyDto::Rebase => GitPullStrategy::Rebase,
        GitPullStrategyDto::FastForwardOnly => GitPullStrategy::FastForwardOnly,
    }
}

fn git_push_options(options: GitPushOptionsDto) -> GitPushOptions {
    GitPushOptions {
        remote: options.remote,
        branch: options.branch,
        include_tags: options.include_tags,
        force_with_lease: options.force_with_lease,
        up_to_commit: options.up_to_commit,
    }
}

fn git_remote_dto(remote: git_remote::GitRemote) -> GitRemoteDto {
    GitRemoteDto {
        name: remote.name,
        fetch_url: remote.fetch_url,
        push_url: remote.push_url,
    }
}

fn git_repository_dto(repository: git_remote::GitRepository) -> GitRepositoryDto {
    GitRepositoryDto {
        root: repository.root.to_string_lossy().into_owned(),
        git_dir: repository.git_dir.to_string_lossy().into_owned(),
        branch: repository.branch,
        head_oid: repository.head_oid,
        upstream: repository.upstream,
        remotes: repository.remotes.into_iter().map(git_remote_dto).collect(),
        dirty: repository.dirty,
        operation: repository.operation,
    }
}

fn git_push_preview_dto(preview: git_remote::GitPushPreview) -> GitPushPreviewDto {
    GitPushPreviewDto {
        remote: preview.remote,
        branch: preview.branch,
        source_oid: preview.source_oid,
        commits: preview
            .commits
            .into_iter()
            .map(|commit| GitCommitSummaryDto {
                hash: commit.oid,
                short_hash: commit.short_oid,
                author: commit.author,
                unix_seconds: commit.unix_seconds,
                subject: commit.subject,
            })
            .collect(),
        tags: preview.tags,
        protected_branch: preview.protected_branch,
        force_with_lease: preview.force_with_lease,
    }
}

fn git_progress_dto(progress: git_remote::GitOperationProgress) -> GitOperationProgressDto {
    GitOperationProgressDto {
        operation: progress.operation.map(|operation| {
            match operation {
                GitNetworkOperation::Fetch => "fetch",
                GitNetworkOperation::Pull => "pull",
                GitNetworkOperation::Push => "push",
                GitNetworkOperation::Clone => "clone",
            }
            .to_string()
        }),
        phase: match progress.phase {
            GitProgressPhase::Idle => GitProgressPhaseDto::Idle,
            GitProgressPhase::Preparing => GitProgressPhaseDto::Preparing,
            GitProgressPhase::Transferring => GitProgressPhaseDto::Transferring,
            GitProgressPhase::Resolving => GitProgressPhaseDto::Resolving,
            GitProgressPhase::Finalizing => GitProgressPhaseDto::Finalizing,
            GitProgressPhase::Completed => GitProgressPhaseDto::Completed,
            GitProgressPhase::Failed => GitProgressPhaseDto::Failed,
            GitProgressPhase::Cancelled => GitProgressPhaseDto::Cancelled,
        },
        message: progress.message,
        detail: progress.detail,
        fraction: progress.fraction,
        active: progress.active,
        cancelled: progress.cancelled,
    }
}

fn git_remote_error(error: GitFailure) -> KiteError {
    let message = error.message;
    match error.kind {
        GitFailureKind::AuthenticationRequired => KiteError::GitAuthenticationRequired { message },
        GitFailureKind::PushRejected => KiteError::GitPushRejected { message },
        GitFailureKind::ProtectedBranch => KiteError::GitProtectedBranch { message },
        GitFailureKind::WorkingTreeDirty => KiteError::GitWorkingTreeDirty { message },
        GitFailureKind::Conflict => KiteError::GitConflict { message },
        GitFailureKind::Cancelled => KiteError::GitCancelled {
            message,
            partial_clone_path: error
                .partial_clone_path
                .map(|path| path.to_string_lossy().into_owned()),
        },
        GitFailureKind::NotRepository
        | GitFailureKind::CommandFailed
        | GitFailureKind::IdentityMissing
        | GitFailureKind::RepositoryLocked
        | GitFailureKind::GitTooOld => KiteError::OperationFailed { message },
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
        last_commit: summary.last_commit.map(git_commit_summary_dto),
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

fn git_commit_options(options: GitCommitOptionsDto) -> GitCommitOptions {
    GitCommitOptions {
        repository_path: PathBuf::from(options.repository_path),
        message: options.message,
        amend: options.amend,
        signoff: options.signoff,
        signing: match options.signing {
            GitSigningModeDto::GitConfig => GitSigningMode::GitConfig,
            GitSigningModeDto::Enabled => GitSigningMode::Enabled,
            GitSigningModeDto::Disabled => GitSigningMode::Disabled,
        },
        skip_hooks: options.skip_hooks,
        author: options.author,
        push: options.push,
    }
}

fn git_commit_state_dto(state: GitCommitState) -> GitCommitStateDto {
    GitCommitStateDto {
        repository_root: state.repository_root.to_string_lossy().into_owned(),
        branch: state.branch,
        head_short_hash: state.head_short_hash,
        upstream: state.upstream,
        user_name: state.user_name,
        user_email: state.user_email,
        identity_configured: state.identity_configured,
        staged_files: state.staged_files,
        staged_lines_added: state.staged_lines_added,
        staged_lines_removed: state.staged_lines_removed,
        amend_files: state.amend_files,
        amend_lines_added: state.amend_lines_added,
        amend_lines_removed: state.amend_lines_removed,
        unresolved_conflicts: state.unresolved_conflicts,
        commit_template: state.commit_template,
        previous_message: state.previous_message,
        recent_messages: state.recent_messages,
        signing_default: state.signing_default,
        signing_format: state.signing_format,
    }
}

fn git_commit_result_dto(result: GitCommitResult) -> GitCommitResultDto {
    GitCommitResultDto {
        hash: result.hash,
        short_hash: result.short_hash,
        subject: result.subject,
        amended: result.amended,
        signed: result.signed,
        pushed: result.pushed,
        command_output: result.command_output,
        push_error: result.push_error,
        push_output: result.push_output,
    }
}

fn git_repository_changes_dto(
    changes: git_changes::GitRepositoryChanges,
) -> GitRepositoryChangesDto {
    let staged_count = changes.count(GitChangeGroup::Staged) as u32;
    let unstaged_count = changes.count(GitChangeGroup::Unstaged) as u32;
    let untracked_count = changes.count(GitChangeGroup::Untracked) as u32;
    let conflicted_count = changes.count(GitChangeGroup::Conflicted) as u32;
    let ignored_count = changes.count(GitChangeGroup::Ignored) as u32;
    GitRepositoryChangesDto {
        repository_root: changes.repository_root.to_string_lossy().into_owned(),
        branch: changes.branch,
        head_short_hash: changes.head_short_hash,
        upstream: changes.upstream,
        ahead: changes.ahead,
        behind: changes.behind,
        operation: changes.operation.map(|state| state.label().to_string()),
        files: changes
            .files
            .into_iter()
            .map(|file| GitChangeFileDto {
                path: file.path,
                old_path: file.old_path,
                group: git_change_group_dto(file.group),
                status: file.status.label().to_string(),
                status_code: file.status.code().to_string(),
            })
            .collect(),
        staged_count,
        unstaged_count,
        untracked_count,
        conflicted_count,
        ignored_count,
        crlf_warning: changes.crlf_warning,
        includes_ignored: changes.includes_ignored,
    }
}

fn git_file_changes_diff_dto(diff: git_changes::GitFileChangesDiff) -> GitFileChangesDiffDto {
    let hunk_selections = diff
        .hunks
        .iter()
        .map(|hunk| {
            diff.selection_units_for_hunk(hunk.index)
                .into_iter()
                .map(git_line_selection_dto)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    GitFileChangesDiffDto {
        path: diff.path,
        old_path: diff.old_path,
        scope: match diff.scope {
            GitDiffScope::Unstaged => GitDiffScopeDto::Unstaged,
            GitDiffScope::Staged => GitDiffScopeDto::Staged,
        },
        hunks: diff
            .hunks
            .into_iter()
            .zip(hunk_selections)
            .map(|(hunk, selections)| GitDiffHunkDto {
                index: hunk.index,
                header: hunk.header,
                old_start: hunk.old_start,
                old_count: hunk.old_count,
                new_start: hunk.new_start,
                new_count: hunk.new_count,
                lines: hunk
                    .lines
                    .into_iter()
                    .map(|line| GitHunkLineDto {
                        kind: match line.kind {
                            GitHunkLineKind::Context => GitHunkLineKindDto::Context,
                            GitHunkLineKind::Added => GitHunkLineKindDto::Added,
                            GitHunkLineKind::Removed => GitHunkLineKindDto::Removed,
                        },
                        text: line.text,
                        old_line: line.old_line,
                        new_line: line.new_line,
                    })
                    .collect(),
                selections,
            })
            .collect(),
        binary: diff.binary,
        truncated: diff.truncated,
        line_ending_warning: diff.line_ending_warning,
    }
}

fn git_line_selection_dto(selection: git_changes::GitLineSelection) -> GitLineSelectionDto {
    GitLineSelectionDto {
        hunk_index: selection.hunk_index,
        old_line: selection.old_line,
        new_line: selection.new_line,
    }
}

fn git_change_operation_result_dto(
    result: git_changes::GitChangeOperationResult,
) -> GitChangeOperationResultDto {
    GitChangeOperationResultDto {
        message: result.message,
        changes: git_repository_changes_dto(result.changes),
    }
}

fn git_change_group_dto(group: GitChangeGroup) -> GitChangeGroupDto {
    match group {
        GitChangeGroup::Staged => GitChangeGroupDto::Staged,
        GitChangeGroup::Unstaged => GitChangeGroupDto::Unstaged,
        GitChangeGroup::Untracked => GitChangeGroupDto::Untracked,
        GitChangeGroup::Conflicted => GitChangeGroupDto::Conflicted,
        GitChangeGroup::Ignored => GitChangeGroupDto::Ignored,
    }
}

fn git_diff_scope(scope: GitDiffScopeDto) -> GitDiffScope {
    match scope {
        GitDiffScopeDto::Unstaged => GitDiffScope::Unstaged,
        GitDiffScopeDto::Staged => GitDiffScope::Staged,
    }
}

fn git_line_selections(selections: Vec<GitLineSelectionDto>) -> Vec<git_changes::GitLineSelection> {
    selections
        .into_iter()
        .map(|selection| git_changes::GitLineSelection {
            hunk_index: selection.hunk_index,
            old_line: selection.old_line,
            new_line: selection.new_line,
        })
        .collect()
}

fn git_stash_entry_dto(entry: GitStashEntry) -> GitStashEntryDto {
    GitStashEntryDto {
        selector: entry.selector,
        commit_hash: entry.commit_hash,
        short_hash: entry.short_hash,
        date: entry.date,
        message: entry.message,
    }
}

fn git_repository_status_dto(status: GitRepositoryStatus) -> GitRepositoryStatusDto {
    GitRepositoryStatusDto {
        repository_root: status.repository_root.to_string_lossy().into_owned(),
        branch: status.branch,
        operation: status.operation.map(|state| state.label().to_string()),
        has_conflicts: status.has_conflicts,
        stash_count: status.stash_count,
    }
}

fn git_command_outcome_dto(outcome: GitCommandOutcome) -> GitCommandOutcomeDto {
    GitCommandOutcomeDto {
        message: outcome.message,
        operation: outcome.operation.map(|state| state.label().to_string()),
        has_conflicts: outcome.has_conflicts,
    }
}

fn git_reset_mode(mode: GitResetModeDto) -> GitResetMode {
    match mode {
        GitResetModeDto::Soft => GitResetMode::Soft,
        GitResetModeDto::Mixed => GitResetMode::Mixed,
        GitResetModeDto::Hard => GitResetMode::Hard,
        GitResetModeDto::Keep => GitResetMode::Keep,
    }
}

fn git_diff_request(request: GitDiffRequestDto) -> GitDiffRequest {
    GitDiffRequest {
        path: PathBuf::from(request.path),
        old: git_diff_endpoint(request.old),
        new: git_diff_endpoint(request.new),
        ignore_whitespace: request.ignore_whitespace,
    }
}

fn git_diff_endpoint(endpoint: GitDiffEndpointDto) -> DiffEndpoint {
    match endpoint {
        GitDiffEndpointDto::Revision { revision } => DiffEndpoint::Revision(revision),
        GitDiffEndpointDto::Index => DiffEndpoint::Index,
        GitDiffEndpointDto::WorkingTree => DiffEndpoint::WorkingTree,
    }
}

fn git_diff_document_dto(document: GitDiffDocument) -> GitDiffDocumentDto {
    GitDiffDocumentDto {
        repository_root: document.repository_root.to_string_lossy().into_owned(),
        relative_path: document.relative_path,
        old_label: document.old_label,
        new_label: document.new_label,
        rows: document.rows.into_iter().map(diff_row_dto).collect(),
        hunks: document.hunks.into_iter().map(diff_hunk_dto).collect(),
        change_rows: document.change_rows,
        stats: diff_stats_dto(document.stats),
        content_kind: match document.content_kind {
            DiffContentKind::Text => DiffContentKindDto::Text,
            DiffContentKind::Binary => DiffContentKindDto::Binary,
            DiffContentKind::Image => DiffContentKindDto::Image,
        },
        large_file: document.large_file,
        truncated: document.truncated,
        message: document.message,
        old_image: document.old_image,
        new_image: document.new_image,
    }
}

fn diff_row_dto(row: DiffRow) -> DiffRowDto {
    DiffRowDto {
        kind: match row.kind {
            DiffRowKind::File => DiffRowKindDto::File,
            DiffRowKind::Hunk => DiffRowKindDto::Hunk,
            DiffRowKind::Context => DiffRowKindDto::Context,
            DiffRowKind::Deletion => DiffRowKindDto::Deletion,
            DiffRowKind::Addition => DiffRowKindDto::Addition,
            DiffRowKind::Modification => DiffRowKindDto::Modification,
        },
        text: row.text,
        left: row.left.map(diff_line_dto),
        right: row.right.map(diff_line_dto),
        change_index: row.change_index,
    }
}

fn diff_line_dto(line: DiffLine) -> DiffLineDto {
    DiffLineDto {
        old_line: line.old_line,
        new_line: line.new_line,
        text: line.text,
        highlighted: line
            .highlighted
            .into_iter()
            .map(diff_word_range_dto)
            .collect(),
        syntax: line.syntax.into_iter().map(diff_syntax_span_dto).collect(),
        missing_newline: line.missing_newline,
    }
}

fn diff_syntax_span_dto(span: DiffSyntaxSpan) -> DiffSyntaxSpanDto {
    DiffSyntaxSpanDto {
        start_utf16: span.start_utf16,
        end_utf16: span.end_utf16,
        kind: match span.kind {
            DiffSyntaxKind::Keyword => DiffSyntaxKindDto::Keyword,
            DiffSyntaxKind::String => DiffSyntaxKindDto::String,
            DiffSyntaxKind::Number => DiffSyntaxKindDto::Number,
            DiffSyntaxKind::Comment => DiffSyntaxKindDto::Comment,
            DiffSyntaxKind::TypeName => DiffSyntaxKindDto::TypeName,
            DiffSyntaxKind::Function => DiffSyntaxKindDto::Function,
        },
    }
}

fn diff_word_range_dto(range: DiffWordRange) -> DiffWordRangeDto {
    DiffWordRangeDto {
        start_utf16: range.start_utf16,
        end_utf16: range.end_utf16,
    }
}

fn diff_stats_dto(stats: DiffStats) -> DiffStatsDto {
    DiffStatsDto {
        files: stats.files,
        additions: stats.additions,
        deletions: stats.deletions,
        hunks: stats.hunks,
        old_bytes: stats.old_bytes,
        new_bytes: stats.new_bytes,
    }
}

fn diff_hunk_dto(hunk: DiffHunk) -> DiffHunkDto {
    DiffHunkDto {
        header: hunk.header,
        old_start: hunk.old_start,
        old_count: hunk.old_count,
        new_start: hunk.new_start,
        new_count: hunk.new_count,
        first_row: hunk.first_row,
        row_count: hunk.row_count,
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

fn git_log_changed_file_dto(file: GitLogChangedFile) -> GitLogChangedFileDto {
    GitLogChangedFileDto {
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
            .map(git_log_changed_file_dto)
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
