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
use crate::git::{self, GitDiffLine, GitDiffLineKind, GitPathSummary};
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
pub struct GitBranchSummaryDto {
    pub name: String,
    pub short_hash: String,
    pub subject: String,
    pub is_remote: bool,
    pub is_current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitMergeStrategyDto {
    Merge,
    NoFastForward,
    FastForwardOnly,
    Squash,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitMergeRequestDto {
    pub target: String,
    pub strategy: GitMergeStrategyDto,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitConflictKindDto {
    BothModified,
    ModifyDeleteOurs,
    ModifyDeleteTheirs,
    AddAdd,
    Rename,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitConflictEntryDto {
    pub path: String,
    pub base_path: Option<String>,
    pub ours_path: Option<String>,
    pub theirs_path: Option<String>,
    pub kind: GitConflictKindDto,
    pub base_present: bool,
    pub ours_present: bool,
    pub theirs_present: bool,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitConflictStateDto {
    pub repository_root: String,
    pub operation: Option<String>,
    pub entries: Vec<GitConflictEntryDto>,
    pub can_continue: bool,
    pub can_abort: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitMergeOutcomeDto {
    pub message: String,
    pub conflicts: Option<GitConflictStateDto>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitConflictSideDto {
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitConflictSidesDto {
    pub base_text: Option<String>,
    pub ours_text: Option<String>,
    pub theirs_text: Option<String>,
    pub base_present: bool,
    pub ours_present: bool,
    pub theirs_present: bool,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitMergeLineKindDto {
    Context,
    Ours,
    Base,
    Theirs,
    Marker,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitMergeLineDto {
    pub kind: GitMergeLineKindDto,
    pub text: String,
    pub conflict_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitMergeConflictHunkDto {
    pub index: u32,
    pub base_text: String,
    pub ours_text: String,
    pub theirs_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitThreeWayMergeDto {
    pub merged_text: String,
    pub lines: Vec<GitMergeLineDto>,
    pub conflicts: Vec<GitMergeConflictHunkDto>,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GitConflictChoiceSideDto {
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GitConflictChoiceDto {
    pub index: u32,
    pub side: GitConflictChoiceSideDto,
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
}

impl KiteError {
    fn operation(message: impl Into<String>) -> Self {
        Self::OperationFailed {
            message: message.into(),
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

    pub fn git_merge_branches(&self, path: String) -> Result<Vec<GitBranchSummaryDto>, KiteError> {
        git::git_merge_branches(&PathBuf::from(path))
            .map(|branches| {
                branches
                    .into_iter()
                    .map(|branch| GitBranchSummaryDto {
                        name: branch.name,
                        short_hash: branch.short_hash,
                        subject: branch.subject,
                        is_remote: branch.is_remote,
                        is_current: branch.is_current,
                    })
                    .collect()
            })
            .map_err(KiteError::operation)
    }

    pub fn git_merge(
        &self,
        path: String,
        request: GitMergeRequestDto,
    ) -> Result<GitMergeOutcomeDto, KiteError> {
        let strategy = match request.strategy {
            GitMergeStrategyDto::Merge => git::GitMergeStrategy::Merge,
            GitMergeStrategyDto::NoFastForward => git::GitMergeStrategy::NoFastForward,
            GitMergeStrategyDto::FastForwardOnly => git::GitMergeStrategy::FastForwardOnly,
            GitMergeStrategyDto::Squash => git::GitMergeStrategy::Squash,
        };
        git::git_merge(
            &PathBuf::from(path),
            &git::GitMergeSpec {
                target: request.target,
                strategy,
                message: request.message,
            },
        )
        .map(git_merge_outcome_dto)
        .map_err(KiteError::operation)
    }

    pub fn git_conflicts(&self, path: String) -> Result<Option<GitConflictStateDto>, KiteError> {
        git::git_conflicts(&PathBuf::from(path))
            .map(|state| state.map(git_conflict_state_dto))
            .map_err(KiteError::operation)
    }

    pub fn git_conflict_sides(
        &self,
        path: String,
        conflict: GitConflictEntryDto,
    ) -> Result<GitConflictSidesDto, KiteError> {
        git::git_conflict_sides(&PathBuf::from(path), &git_conflict_entry(conflict))
            .map(git_conflict_sides_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_three_way_merge(
        &self,
        path: String,
        conflict: GitConflictEntryDto,
    ) -> Result<GitThreeWayMergeDto, KiteError> {
        git::git_three_way_merge(&PathBuf::from(path), &git_conflict_entry(conflict))
            .map(git_three_way_merge_dto)
            .map_err(KiteError::operation)
    }

    pub fn git_apply_conflict_choices(
        &self,
        merged_text: String,
        choices: Vec<GitConflictChoiceDto>,
    ) -> String {
        let choices = choices
            .into_iter()
            .map(|choice| git::GitConflictChoice {
                index: choice.index,
                side: match choice.side {
                    GitConflictChoiceSideDto::Ours => git::GitConflictChoiceSide::Ours,
                    GitConflictChoiceSideDto::Theirs => git::GitConflictChoiceSide::Theirs,
                    GitConflictChoiceSideDto::Both => git::GitConflictChoiceSide::Both,
                },
            })
            .collect::<Vec<_>>();
        git::apply_conflict_choices(&merged_text, &choices)
    }

    pub fn git_resolve_conflict(
        &self,
        path: String,
        relative_path: String,
        content: Option<String>,
    ) -> Result<(), KiteError> {
        git::git_resolve_conflict(&PathBuf::from(path), &relative_path, content.as_deref())
            .map_err(KiteError::operation)
    }

    pub fn git_choose_conflict_side(
        &self,
        path: String,
        conflict: GitConflictEntryDto,
        side: GitConflictSideDto,
    ) -> Result<(), KiteError> {
        let side = match side {
            GitConflictSideDto::Ours => git::GitConflictSide::Ours,
            GitConflictSideDto::Theirs => git::GitConflictSide::Theirs,
            GitConflictSideDto::Both => git::GitConflictSide::Both,
        };
        git::git_choose_conflict_side(&PathBuf::from(path), &git_conflict_entry(conflict), side)
            .map_err(KiteError::operation)
    }

    pub fn git_continue_operation(&self, path: String) -> Result<String, KiteError> {
        git::git_continue_operation(&PathBuf::from(path)).map_err(KiteError::operation)
    }

    pub fn git_abort_operation(&self, path: String) -> Result<String, KiteError> {
        git::git_abort_operation(&PathBuf::from(path)).map_err(KiteError::operation)
    }

    pub fn git_open_merge_tool(
        &self,
        path: String,
        conflict: GitConflictEntryDto,
    ) -> Result<String, KiteError> {
        git::git_open_merge_tool(&PathBuf::from(path), &git_conflict_entry(conflict))
            .map_err(KiteError::operation)
    }

    pub fn run_vcs_operation(
        &self,
        tool: VcsToolDto,
        operation: VcsOperationDto,
        path: String,
    ) -> Result<String, KiteError> {
        run_vcs_operation(
            vcs_tool(tool),
            vcs_operation(operation),
            &PathBuf::from(path),
        )
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
        run_vcs_query(vcs_tool(tool), query, &PathBuf::from(path)).map_err(KiteError::operation)
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
        VcsOperationDto::Revert => VcsOperation::Revert,
        VcsOperationDto::Clean => VcsOperation::Clean,
        VcsOperationDto::Update => VcsOperation::Update,
        VcsOperationDto::Lock => VcsOperation::Lock,
        VcsOperationDto::Unlock => VcsOperation::Unlock,
        VcsOperationDto::Cleanup => VcsOperation::Cleanup,
    }
}

fn git_merge_outcome_dto(outcome: git::GitMergeOutcome) -> GitMergeOutcomeDto {
    GitMergeOutcomeDto {
        message: outcome.message,
        conflicts: outcome.conflicts.map(git_conflict_state_dto),
    }
}

fn git_conflict_state_dto(state: git::GitConflictState) -> GitConflictStateDto {
    GitConflictStateDto {
        repository_root: state.repository_root.to_string_lossy().into_owned(),
        operation: state
            .operation
            .map(|operation| operation.label().to_string()),
        entries: state
            .entries
            .into_iter()
            .map(git_conflict_entry_dto)
            .collect(),
        can_continue: state.can_continue,
        can_abort: state.can_abort,
    }
}

fn git_conflict_entry_dto(entry: git::GitConflictEntry) -> GitConflictEntryDto {
    GitConflictEntryDto {
        path: entry.path,
        base_path: entry.base_path,
        ours_path: entry.ours_path,
        theirs_path: entry.theirs_path,
        kind: match entry.kind {
            git::GitConflictKind::BothModified => GitConflictKindDto::BothModified,
            git::GitConflictKind::ModifyDeleteOurs => GitConflictKindDto::ModifyDeleteOurs,
            git::GitConflictKind::ModifyDeleteTheirs => GitConflictKindDto::ModifyDeleteTheirs,
            git::GitConflictKind::AddAdd => GitConflictKindDto::AddAdd,
            git::GitConflictKind::Rename => GitConflictKindDto::Rename,
            git::GitConflictKind::Other => GitConflictKindDto::Other,
        },
        base_present: entry.base_present,
        ours_present: entry.ours_present,
        theirs_present: entry.theirs_present,
        binary: entry.binary,
    }
}

fn git_conflict_entry(entry: GitConflictEntryDto) -> git::GitConflictEntry {
    git::GitConflictEntry {
        path: entry.path,
        base_path: entry.base_path,
        ours_path: entry.ours_path,
        theirs_path: entry.theirs_path,
        kind: match entry.kind {
            GitConflictKindDto::BothModified => git::GitConflictKind::BothModified,
            GitConflictKindDto::ModifyDeleteOurs => git::GitConflictKind::ModifyDeleteOurs,
            GitConflictKindDto::ModifyDeleteTheirs => git::GitConflictKind::ModifyDeleteTheirs,
            GitConflictKindDto::AddAdd => git::GitConflictKind::AddAdd,
            GitConflictKindDto::Rename => git::GitConflictKind::Rename,
            GitConflictKindDto::Other => git::GitConflictKind::Other,
        },
        base_present: entry.base_present,
        ours_present: entry.ours_present,
        theirs_present: entry.theirs_present,
        binary: entry.binary,
    }
}

fn git_conflict_sides_dto(sides: git::GitConflictSides) -> GitConflictSidesDto {
    GitConflictSidesDto {
        base_text: sides.base_text,
        ours_text: sides.ours_text,
        theirs_text: sides.theirs_text,
        base_present: sides.base_present,
        ours_present: sides.ours_present,
        theirs_present: sides.theirs_present,
        binary: sides.binary,
    }
}

fn git_three_way_merge_dto(merge: git::GitThreeWayMerge) -> GitThreeWayMergeDto {
    GitThreeWayMergeDto {
        merged_text: merge.merged_text,
        lines: merge
            .lines
            .into_iter()
            .map(|line| GitMergeLineDto {
                kind: match line.kind {
                    git::GitMergeLineKind::Context => GitMergeLineKindDto::Context,
                    git::GitMergeLineKind::Ours => GitMergeLineKindDto::Ours,
                    git::GitMergeLineKind::Base => GitMergeLineKindDto::Base,
                    git::GitMergeLineKind::Theirs => GitMergeLineKindDto::Theirs,
                    git::GitMergeLineKind::Marker => GitMergeLineKindDto::Marker,
                },
                text: line.text,
                conflict_index: line.conflict_index,
            })
            .collect(),
        conflicts: merge
            .conflicts
            .into_iter()
            .map(|conflict| GitMergeConflictHunkDto {
                index: conflict.index,
                base_text: conflict.base_text,
                ours_text: conflict.ours_text,
                theirs_text: conflict.theirs_text,
            })
            .collect(),
        binary: merge.binary,
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
}
