use std::path::PathBuf;

use gpui_kit::*;
use kite_core::agent::AgentMenuCommand;

actions!(
    kite,
    [
        NavigateBack,
        NavigateForward,
        NavigateUp,
        RefreshFolder,
        OpenSelection,
        CopySelection,
        CutSelection,
        PasteSelection,
        SelectAll,
        FocusAddress,
        FocusSearch,
        OpenGlobalSearch,
        OpenGitClient,
        SetGlobalSearchScopeCurrentFolder,
        SetGlobalSearchScopeFavorites,
        SetGlobalSearchScopeLocalDisks,
        SetGlobalSearchScopeNetworkVolumes,
        SetGlobalSearchKindAll,
        SetGlobalSearchKindFolders,
        SetGlobalSearchKindDocuments,
        SetGlobalSearchKindSourceCode,
        SetGlobalSearchKindMarkdown,
        SetGlobalSearchKindImages,
        SetGlobalSearchKindMedia,
        SetGlobalSearchTimeAny,
        SetGlobalSearchTimeLast24Hours,
        SetGlobalSearchTimeLast7Days,
        SetGlobalSearchTimeLast30Days,
        CreateFolder,
        RenameSelection,
        RecycleSelection,
        ClearSearchOverlay,
        ShowListView,
        ShowColumnsView,
        TogglePathBar,
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        SetDefaultExplorer,
        RestoreDefaultExplorer,
        CreateStartMenuShortcut,
        CheckForUpdates,
        OpenSettings,
        OpenWorkspaceEditor,
        OpenAgentReview,
        AgentPromptFocusNext,
        AgentPromptFocusPrev,
        GitClientRefresh,
        GitClientFetch,
        GitClientPull,
        GitClientPush,
        GitClientCommit,
        GitClientStash,
        GitClientToggleConsole,
        GitClientShowShortcuts,
        GitClientContinue,
        GitClientSkip,
        GitClientAbort,
        GitClientCancelTask,
        GitClientDismiss,
        SaveEditor,
        DiffNextChange,
        DiffPreviousChange,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextMenuCommand {
    Open,
    Agent(AgentMenuCommand),
    Copy,
    CopyTo,
    CopyPath,
    Cut,
    Paste,
    NewFolder,
    NewFile,
    AddFavorite,
    RemoveFavorite,
    Rename,
    Recycle,
    ShowProperties,
    CompressZip,
    CompressTarGz,
    ExtractHere,
    ExtractTo,
    Vcs(VcsMenuCommand),
    SystemMenu,
    Refresh,
}

#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = kite, no_json)]
pub struct ContextMenuAction {
    pub target: PathBuf,
    pub paths: Vec<PathBuf>,
    pub working_directory: PathBuf,
    pub command: ContextMenuCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnPaneCommand {
    SortNameAscending,
    SortNameDescending,
    SortModifiedNewest,
    SortModifiedOldest,
    SortKind,
    SortSizeDescending,
    FilterAll,
    FilterFolders,
    FilterFiles,
    FilterMedia,
    FilterDocuments,
    GroupNone,
    GroupKind,
    GroupExtension,
    GroupModified,
    ResetView,
    ToggleDateColumn,
    ToggleKindColumn,
    ToggleSizeColumn,
    SetCompactColumnWidth,
    SetStandardColumnWidth,
    SetWideColumnWidth,
    ResetLayout,
}

#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = kite, no_json)]
pub struct ColumnPaneAction {
    pub path: PathBuf,
    pub command: ColumnPaneCommand,
}

#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = kite, no_json)]
pub struct GitRebaseSetAction {
    pub index: usize,
    pub action: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcsMenuCommand {
    GitOpenClient,
    GitAdd,
    GitUnstage,
    GitCommit,
    GitPull,
    GitPush,
    GitFetch,
    GitFetchAll,
    GitManageRemotes,
    GitClone,
    GitInit,
    GitDiff,
    GitLog,
    GitRebase,
    GitRevert,
    GitClean,
    SvnUpdate,
    SvnAdd,
    SvnCommit,
    SvnLock,
    SvnUnlock,
    SvnDiff,
    SvnLog,
    SvnRevert,
    SvnCleanup,
}
