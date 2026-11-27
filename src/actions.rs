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
    pub action_id: String,
    pub variant_key: Option<String>,
}

impl ContextMenuCommand {
    /// Stable mapping from the UI-independent ActionId vocabulary to the
    /// Windows dispatcher command. New dynamic variants must extend the
    /// variant payload, not fork action identities.
    pub(crate) fn from_action_id(action_id: &str, variant_key: Option<&str>) -> Option<Self> {
        let command = match action_id {
            "navigation.refresh" => Self::Refresh,
            "open.default" => Self::Open,
            "clipboard.copy" => Self::Copy,
            "clipboard.copy-to" => Self::CopyTo,
            "clipboard.copy-path" => Self::CopyPath,
            "clipboard.cut" => Self::Cut,
            "clipboard.paste" => Self::Paste,
            "file.new-folder" => Self::NewFolder,
            "file.new-file" => Self::NewFile,
            "favorite.add" => Self::AddFavorite,
            "favorite.remove" => Self::RemoveFavorite,
            "file.rename" => Self::Rename,
            "file.move-to-trash" | "device.delete" => Self::Recycle,
            "archive.compress.zip" => Self::CompressZip,
            "archive.compress.tar-gz" => Self::CompressTarGz,
            "archive.extract-here" => Self::ExtractHere,
            "archive.extract-to" => Self::ExtractTo,
            "system.properties" => Self::ShowProperties,
            "system.native-menu" => Self::SystemMenu,
            "agent.ask" => Self::Agent(AgentMenuCommand::Ask),
            "agent.explain-file" => Self::Agent(AgentMenuCommand::ExplainFile),
            "agent.summarize-folder" => Self::Agent(AgentMenuCommand::SummarizeFolder),
            "agent.review-changes" => Self::Agent(AgentMenuCommand::ReviewChanges),
            "agent.generate-commit-message" => Self::Agent(AgentMenuCommand::GenerateCommitMessage),
            "agent.configure" => Self::Agent(AgentMenuCommand::Configure),
            "git.open-client" => Self::Vcs(VcsMenuCommand::GitOpenClient),
            "git.add" => Self::Vcs(VcsMenuCommand::GitAdd),
            "git.unstage" => Self::Vcs(VcsMenuCommand::GitUnstage),
            "git.commit" => Self::Vcs(VcsMenuCommand::GitCommit),
            "git.pull" => Self::Vcs(VcsMenuCommand::GitPull),
            "git.push" => Self::Vcs(VcsMenuCommand::GitPush),
            "git.fetch-current" => Self::Vcs(VcsMenuCommand::GitFetch),
            "git.fetch-all" => Self::Vcs(VcsMenuCommand::GitFetchAll),
            "git.remotes.manage" => Self::Vcs(VcsMenuCommand::GitManageRemotes),
            "git.clone" => Self::Vcs(VcsMenuCommand::GitClone),
            "git.init" => Self::Vcs(VcsMenuCommand::GitInit),
            "git.diff" => Self::Vcs(VcsMenuCommand::GitDiff),
            "git.log" => Self::Vcs(VcsMenuCommand::GitLog),
            "git.rebase.interactive" => Self::Vcs(VcsMenuCommand::GitRebase),
            "git.discard" => Self::Vcs(VcsMenuCommand::GitRevert),
            "git.clean-untracked" => Self::Vcs(VcsMenuCommand::GitClean),
            "svn.update" => Self::Vcs(VcsMenuCommand::SvnUpdate),
            "svn.add" => Self::Vcs(VcsMenuCommand::SvnAdd),
            "svn.commit" => Self::Vcs(VcsMenuCommand::SvnCommit),
            "svn.lock" => Self::Vcs(VcsMenuCommand::SvnLock),
            "svn.unlock" => Self::Vcs(VcsMenuCommand::SvnUnlock),
            "svn.diff" => Self::Vcs(VcsMenuCommand::SvnDiff),
            "svn.log" => Self::Vcs(VcsMenuCommand::SvnLog),
            "svn.revert" => Self::Vcs(VcsMenuCommand::SvnRevert),
            "svn.cleanup" => Self::Vcs(VcsMenuCommand::SvnCleanup),
            _ => return None,
        };
        if variant_key.is_some()
            && !matches!(
                action_id,
                "open.default" | "open.with" | "open.in" | "compare.with" | "git.open-client"
            )
        {
            return None;
        }
        Some(command)
    }
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
