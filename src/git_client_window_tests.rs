use kite_core::git::GitOperationState;

use crate::git_client_window::{
    GIT_CLIENT_SHORTCUTS, GitNoticeAction, GitNoticeKind, notice_for_error, operation_banner_copy,
};

#[test]
fn shortcut_table_covers_the_git_client_commands() {
    let expected = [
        "打开 Git 客户端",
        "刷新仓库",
        "获取",
        "拉取",
        "推送",
        "提交",
        "贮藏",
        "显示或隐藏控制台",
        "快捷键",
        "关闭提示或浮层",
    ];
    let labels = GIT_CLIENT_SHORTCUTS
        .iter()
        .map(|(label, _)| *label)
        .collect::<Vec<_>>();
    assert_eq!(labels, expected);
    assert!(
        GIT_CLIENT_SHORTCUTS
            .iter()
            .all(|(_, shortcut)| !shortcut.trim().is_empty())
    );
}

#[test]
fn operation_banners_cover_rebase_merge_cherry_pick_and_revert() {
    assert_eq!(
        operation_banner_copy(GitOperationState::Rebasing).0,
        "正在变基"
    );
    assert_eq!(
        operation_banner_copy(GitOperationState::Merging).0,
        "正在合并"
    );
    assert_eq!(
        operation_banner_copy(GitOperationState::CherryPicking).0,
        "正在拣选提交"
    );
    assert_eq!(
        operation_banner_copy(GitOperationState::Reverting).0,
        "正在还原提交"
    );
    for operation in [
        GitOperationState::Rebasing,
        GitOperationState::Merging,
        GitOperationState::CherryPicking,
        GitOperationState::Reverting,
    ] {
        assert!(!operation_banner_copy(operation).1.is_empty());
    }
}

#[test]
fn notices_route_rejected_and_conflict_errors_to_next_actions() {
    let rejected = notice_for_error(
        "推送失败",
        "! [rejected] main -> main (non-fast-forward)".to_string(),
    );
    assert_eq!(rejected.kind, GitNoticeKind::Conflict);
    assert_eq!(rejected.title, "推送被拒绝");
    assert_eq!(rejected.action, Some(GitNoticeAction::Pull));
    assert_eq!(rejected.action_label, Some("拉取"));

    let conflict = notice_for_error("操作失败", "CONFLICT (content): merge conflict".into());
    assert_eq!(conflict.kind, GitNoticeKind::Conflict);
    assert_eq!(conflict.title, "需要解决冲突");
    assert_eq!(conflict.action, Some(GitNoticeAction::ShowChanges));
    assert_eq!(conflict.action_label, Some("查看变更"));
}
