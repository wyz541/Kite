use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::app::KiteApp;
use crate::git_branches::{
    self, DIRTY_WORKTREE_ERROR, GitBranch, GitBranchSnapshot, GitCompareResult, GitDirtyPolicy,
    GitRefKind, GitTag, GitWorktree,
};
use crate::ui_preferences;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchFilter {
    All,
    Local,
    Remote,
    Tags,
    Favorites,
    Merged,
    Worktrees,
}

impl BranchFilter {
    const ALL: [Self; 7] = [
        Self::All,
        Self::Local,
        Self::Remote,
        Self::Tags,
        Self::Favorites,
        Self::Merged,
        Self::Worktrees,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "全部引用",
            Self::Local => "本地分支",
            Self::Remote => "远程分支",
            Self::Tags => "标签",
            Self::Favorites => "收藏",
            Self::Merged => "已合并",
            Self::Worktrees => "Worktree",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::All => IconName::Boxes,
            Self::Local => IconName::GitBranch,
            Self::Remote => IconName::Network,
            Self::Tags => IconName::Link,
            Self::Favorites => IconName::Star,
            Self::Merged => IconName::Check,
            Self::Worktrees => IconName::FolderOpen,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectedRef {
    Local(String),
    Remote { remote: String, name: String },
    Tag(String),
    Worktree(PathBuf),
}

#[derive(Clone)]
enum RefRow {
    Local(GitBranch),
    Remote(GitBranch),
    Tag(GitTag),
    Worktree(GitWorktree),
}
enum EditorKind {
    CreateBranch,
    RenameBranch { old_name: String },
    CreateTag { target: String },
    Worktree { branch: String },
    Upstream { branch: String },
}

impl EditorKind {
    fn title(&self) -> &'static str {
        match self {
            Self::CreateBranch => "新建分支",
            Self::RenameBranch { .. } => "重命名分支",
            Self::CreateTag { .. } => "新建标签",
            Self::Worktree { .. } => "在新 worktree 中检出",
            Self::Upstream { .. } => "设置上游",
        }
    }

    fn primary_placeholder(&self) -> &'static str {
        match self {
            Self::CreateBranch => "分支名称",
            Self::RenameBranch { .. } => "新分支名称",
            Self::CreateTag { .. } => "标签名称",
            Self::Worktree { .. } => "目标文件夹路径",
            Self::Upstream { .. } => "上游分支，例如 origin/main（留空清除）",
        }
    }

    fn secondary_placeholder(&self) -> Option<&'static str> {
        match self {
            Self::CreateBranch => Some("起点：分支、标签或提交（默认 HEAD）"),
            Self::CreateTag { .. } => Some("附注标签说明（轻量标签可留空）"),
            _ => None,
        }
    }
}

struct EditorState {
    kind: EditorKind,
    primary: Entity<InputState>,
    secondary: Entity<InputState>,
    annotated: bool,
}

#[derive(Clone)]
enum GitAction {
    Checkout {
        target: String,
        kind: GitRefKind,
        dirty_policy: GitDirtyPolicy,
    },
    CreateBranch {
        name: String,
        start_point: String,
    },
    RenameBranch {
        old_name: String,
        new_name: String,
    },
    DeleteLocal {
        names: Vec<String>,
        force: bool,
    },
    DeleteRemote {
        remote: String,
        branch: String,
    },
    SetUpstream {
        branch: String,
        upstream: Option<String>,
    },
    CreateTag {
        name: String,
        target: String,
        annotated: bool,
        message: Option<String>,
    },
    DeleteTag {
        name: String,
    },
    PushTag {
        remote: String,
        name: String,
    },
    DeleteRemoteTag {
        remote: String,
        name: String,
    },
    CreateWorktree {
        destination: PathBuf,
        branch: String,
    },
}

impl GitAction {
    fn execute(&self, root: &Path) -> Result<String, String> {
        match self {
            Self::Checkout {
                target,
                kind,
                dirty_policy,
            } => git_branches::git_checkout(root, target, *kind, *dirty_policy),
            Self::CreateBranch { name, start_point } => {
                git_branches::create_branch(root, name, start_point)
            }
            Self::RenameBranch { old_name, new_name } => {
                git_branches::rename_branch(root, old_name, new_name)
            }
            Self::DeleteLocal { names, force } => {
                git_branches::delete_local_branches(root, names, *force)
            }
            Self::DeleteRemote { remote, branch } => {
                git_branches::delete_remote_branch(root, remote, branch)
            }
            Self::SetUpstream { branch, upstream } => {
                git_branches::set_branch_upstream(root, branch, upstream.as_deref())
            }
            Self::CreateTag {
                name,
                target,
                annotated,
                message,
            } => git_branches::create_tag(root, name, target, *annotated, message.as_deref()),
            Self::DeleteTag { name } => git_branches::delete_tag(root, name),
            Self::PushTag { remote, name } => git_branches::push_tag(root, remote, name),
            Self::DeleteRemoteTag { remote, name } => {
                git_branches::delete_remote_tag(root, remote, name)
            }
            Self::CreateWorktree {
                destination,
                branch,
            } => git_branches::create_worktree(root, destination, branch),
        }
    }
}

pub(crate) struct GitBranchesWindow {
    main_app: Entity<KiteApp>,
    root: PathBuf,
    snapshot: Option<GitBranchSnapshot>,
    query_input: Entity<InputState>,
    filter: BranchFilter,
    selected: Option<SelectedRef>,
    favorites: BTreeSet<String>,
    protected_patterns: BTreeSet<String>,
    compare: Option<(GitCompareResult, String, String)>,
    editor: Option<EditorState>,
    loading: bool,
    status: Option<SharedString>,
    error: Option<SharedString>,
    generation: u64,
    _task: Task<()>,
    _query_subscription: Subscription,
}

pub(crate) fn open_git_branches_window(main_app: Entity<KiteApp>, root: PathBuf, cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Kite · Git 分支与标签".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(960.), px(660.)), cx)),
        window_min_size: Some(size(px(760.), px(520.))),
        ..Default::default()
    };
    if let Err(error) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| GitBranchesWindow::new(main_app.clone(), root.clone(), window, cx))
    }) {
        eprintln!("无法打开 Git 分支窗口：{error}");
    }
}

impl GitBranchesWindow {
    fn new(
        main_app: Entity<KiteApp>,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = ui_preferences::load_git_repository_preferences(&root);
        let query_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索分支、远程或标签")
                .clean_on_escape()
        });
        let _query_subscription = cx.subscribe_in(
            &query_input,
            window,
            |_this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        let mut view = Self {
            main_app,
            root,
            snapshot: None,
            query_input,
            filter: BranchFilter::All,
            selected: None,
            favorites: preferences.favorites,
            protected_patterns: preferences.protected_patterns,
            compare: None,
            editor: None,
            loading: false,
            status: None,
            error: None,
            generation: 0,
            _task: Task::ready(()),
            _query_subscription,
        };
        view.load(window, cx);
        view
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let root = self.root.clone();
        self.loading = true;
        self.error = None;
        cx.notify();
        self._task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { git_branches::git_branch_snapshot(&root) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(snapshot) => {
                        if this.selected.is_none() {
                            this.selected = snapshot
                                .head_branch
                                .as_ref()
                                .map(|name| SelectedRef::Local(name.clone()));
                        }
                        this.snapshot = Some(snapshot);
                    }
                    Err(error) => {
                        this.snapshot = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
    }

    fn request_main_refresh(&mut self, cx: &mut Context<Self>) {
        self.main_app
            .update(cx, |app, cx| app.request_git_refresh(cx));
    }

    fn run_action(&mut self, action: GitAction, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.loading = true;
        self.status = Some("正在执行 Git 操作…".into());
        self.error = None;
        cx.notify();
        self._task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { action.execute(&root) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(message) => {
                        this.error = None;
                        this.status = Some(message.into());
                        this.compare = None;
                        this.request_main_refresh(cx);
                        this.load(window, cx);
                    }
                    Err(error) => {
                        this.status = None;
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        });
    }

    fn compare_with_head(&mut self, reference: String, cx: &mut Context<Self>) {
        let Some(head) = self.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .head_branch
                .clone()
                .or_else(|| snapshot.head_short_hash.clone())
        }) else {
            self.error = Some("当前没有可比较的 HEAD".into());
            cx.notify();
            return;
        };
        let root = self.root.clone();
        let base = head.clone();
        let head_ref = reference.clone();
        let base_for_task = base.clone();
        let head_for_task = head_ref.clone();
        self.loading = true;
        self.error = None;
        self.compare = None;
        cx.notify();
        self._task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    git_branches::compare_branches(&root, &base_for_task, &head_for_task)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(result) => {
                        this.compare = Some((result, base, head_ref));
                        this.error = None;
                        this.status = Some("已加载分支比较".into());
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                        this.status = None;
                    }
                }
                cx.notify();
            });
        });
    }
}
impl GitBranchesWindow {
    fn open_editor(
        &mut self,
        kind: EditorKind,
        primary_value: &str,
        secondary_value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let primary_placeholder = kind.primary_placeholder().to_string();
        let secondary_placeholder = kind.secondary_placeholder().map(str::to_string);
        let primary = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(primary_placeholder)
                .default_value(primary_value)
                .clean_on_escape()
        });
        let secondary = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(secondary_placeholder.unwrap_or_default())
                .default_value(secondary_value)
                .clean_on_escape()
        });
        self.editor = Some(EditorState {
            kind,
            primary: primary.clone(),
            secondary,
            annotated: false,
        });
        primary.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn close_editor(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.editor = None;
        cx.notify();
    }

    fn submit_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        let primary = editor.primary.read(cx).value().trim().to_string();
        let secondary = editor.secondary.read(cx).value().trim().to_string();
        if primary.is_empty() {
            self.error = Some("请填写必填字段".into());
            cx.notify();
            return;
        }
        let annotated_selection = editor.annotated;
        let action = match editor.kind {
            EditorKind::CreateBranch => Some(GitAction::CreateBranch {
                name: primary,
                start_point: if secondary.is_empty() {
                    "HEAD".to_string()
                } else {
                    secondary
                },
            }),
            EditorKind::RenameBranch { old_name } => Some(GitAction::RenameBranch {
                old_name,
                new_name: primary,
            }),
            EditorKind::CreateTag { target } => {
                let annotated = annotated_selection || !secondary.is_empty();
                if annotated && secondary.is_empty() {
                    self.error = Some("附注标签需要填写说明".into());
                    cx.notify();
                    return;
                }
                Some(GitAction::CreateTag {
                    name: primary,
                    target,
                    annotated,
                    message: (!secondary.is_empty()).then_some(secondary),
                })
            }
            EditorKind::Worktree { branch } => Some(GitAction::CreateWorktree {
                destination: PathBuf::from(primary),
                branch,
            }),
            EditorKind::Upstream { branch } => Some(GitAction::SetUpstream {
                branch,
                upstream: (!primary.is_empty()).then_some(primary),
            }),
        };
        if let Some(action) = action {
            self.run_action(action, window, cx);
        }
    }

    fn checkout_selected(
        &mut self,
        kind: GitRefKind,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.dirty);
        if !dirty {
            self.run_action(
                GitAction::Checkout {
                    target,
                    kind,
                    dirty_policy: GitDirtyPolicy::Cancel,
                },
                window,
                cx,
            );
            return;
        }
        let message = "工作区有未提交的更改。切换分支前如何处理？";
        let detail = format!(
            "选择“Stash 并切换”会先保存更改，切换成功后自动恢复。\n{}",
            DIRTY_WORKTREE_ERROR
        );
        let answers = [
            PromptButton::ok("Stash 并切换"),
            PromptButton::cancel("取消"),
        ];
        let answer = window.prompt(PromptLevel::Warning, message, Some(&detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_action(
                    GitAction::Checkout {
                        target,
                        kind,
                        dirty_policy: GitDirtyPolicy::Stash,
                    },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn confirm_delete_local(
        &mut self,
        branch: &GitBranch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let force = !branch.is_merged;
        let (message, detail) = if force {
            (
                format!("分支“{}”尚未合并到当前 HEAD。", branch.name),
                "继续删除会丢失这个分支独有的提交。此操作不可撤销。",
            )
        } else {
            (
                format!("删除本地分支“{}”？", branch.name),
                "该分支已合并到当前 HEAD。",
            )
        };
        let answers = [PromptButton::ok("删除"), PromptButton::cancel("取消")];
        let answer = window.prompt(
            if force {
                PromptLevel::Warning
            } else {
                PromptLevel::Warning
            },
            &message,
            Some(detail),
            &answers,
            cx,
        );
        let name = branch.name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_action(
                    GitAction::DeleteLocal {
                        names: vec![name.clone()],
                        force,
                    },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn confirm_delete_remote(
        &mut self,
        remote: String,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = format!("删除远程分支“{remote}/{branch}”？");
        let detail = "这会从远程仓库删除该分支。本地分支不会改变。";
        let answers = [
            PromptButton::ok("删除远程分支"),
            PromptButton::cancel("取消"),
        ];
        let answer = window.prompt(PromptLevel::Warning, &message, Some(detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_action(GitAction::DeleteRemote { remote, branch }, window, cx);
            });
        })
        .detach();
    }

    fn confirm_delete_tag(
        &mut self,
        message: String,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answers = [PromptButton::ok("删除"), PromptButton::cancel("取消")];
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("删除标签不会删除提交。"),
            &answers,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_action(action, window, cx);
            });
        })
        .detach();
    }

    fn cleanup_merged(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let mut names = snapshot
            .local_branches
            .iter()
            .filter(|branch| !branch.is_head && branch.is_merged)
            .map(|branch| branch.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        if names.is_empty() {
            self.status = Some("没有可清理的已合并本地分支".into());
            self.error = None;
            cx.notify();
            return;
        }
        let message = format!("删除 {} 个已合并的本地分支？", names.len());
        let detail = names.join("\n");
        let answers = [PromptButton::ok("全部删除"), PromptButton::cancel("取消")];
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &answers, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice != 0 {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_action(
                    GitAction::DeleteLocal {
                        names,
                        force: false,
                    },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn toggle_favorite(&mut self, key: String, cx: &mut Context<Self>) {
        let favorite = !self.favorites.contains(&key);
        match ui_preferences::save_git_branch_favorite(&self.root, &key, favorite) {
            Ok(()) => {
                if favorite {
                    self.favorites.insert(key);
                } else {
                    self.favorites.remove(&key);
                }
                self.status = Some(
                    if favorite {
                        "已收藏该引用"
                    } else {
                        "已取消收藏"
                    }
                    .into(),
                );
                self.error = None;
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    fn toggle_protection(&mut self, branch: &str, cx: &mut Context<Self>) {
        let pattern = branch.to_string();
        let protected = !self.protected_patterns.contains(&pattern);
        match ui_preferences::save_git_protected_pattern(&self.root, &pattern, protected) {
            Ok(()) => {
                if protected {
                    self.protected_patterns.insert(pattern.clone());
                } else {
                    self.protected_patterns.remove(&pattern);
                }
                self.status = Some(
                    if protected {
                        format!("已保护 {pattern}")
                    } else {
                        format!("已取消保护 {pattern}")
                    }
                    .into(),
                );
                self.error = None;
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }
}
impl GitBranchesWindow {
    fn filtered_rows(&self, cx: &Context<Self>) -> Vec<RefRow> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let query = self.query_input.read(cx).value().trim().to_lowercase();
        let mut rows = Vec::new();
        if matches!(
            self.filter,
            BranchFilter::All
                | BranchFilter::Local
                | BranchFilter::Favorites
                | BranchFilter::Merged
        ) {
            rows.extend(
                snapshot
                    .local_branches
                    .iter()
                    .filter(|branch| {
                        self.filter == BranchFilter::All
                            || self.filter == BranchFilter::Local
                            || (self.filter == BranchFilter::Favorites
                                && self.favorites.contains(&format!("branch:{}", branch.name)))
                            || (self.filter == BranchFilter::Merged
                                && !branch.is_head
                                && branch.is_merged)
                    })
                    .filter(|branch| {
                        query.is_empty()
                            || branch.name.to_lowercase().contains(&query)
                            || branch
                                .upstream
                                .as_deref()
                                .is_some_and(|value| value.to_lowercase().contains(&query))
                            || branch
                                .subject
                                .as_deref()
                                .is_some_and(|value| value.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .map(RefRow::Local),
            );
        }
        if matches!(
            self.filter,
            BranchFilter::All | BranchFilter::Remote | BranchFilter::Favorites
        ) {
            rows.extend(
                snapshot
                    .remote_branches
                    .iter()
                    .filter(|branch| {
                        self.filter != BranchFilter::Favorites
                            || self.favorites.contains(&format!("remote:{}", branch.name))
                    })
                    .filter(|branch| {
                        query.is_empty()
                            || branch.name.to_lowercase().contains(&query)
                            || branch
                                .subject
                                .as_deref()
                                .is_some_and(|value| value.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .map(RefRow::Remote),
            );
        }
        if matches!(
            self.filter,
            BranchFilter::All | BranchFilter::Tags | BranchFilter::Favorites
        ) {
            rows.extend(
                snapshot
                    .tags
                    .iter()
                    .filter(|tag| {
                        self.filter != BranchFilter::Favorites
                            || self.favorites.contains(&format!("tag:{}", tag.name))
                    })
                    .filter(|tag| {
                        query.is_empty()
                            || tag.name.to_lowercase().contains(&query)
                            || tag
                                .message
                                .as_deref()
                                .is_some_and(|value| value.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .map(RefRow::Tag),
            );
        }
        if matches!(
            self.filter,
            BranchFilter::All | BranchFilter::Worktrees | BranchFilter::Favorites
        ) {
            rows.extend(
                snapshot
                    .worktrees
                    .iter()
                    .filter(|worktree| {
                        self.filter != BranchFilter::Favorites
                            || self
                                .favorites
                                .contains(&format!("worktree:{}", worktree.path.display()))
                    })
                    .filter(|worktree| {
                        query.is_empty()
                            || worktree
                                .path
                                .to_string_lossy()
                                .to_lowercase()
                                .contains(&query)
                            || worktree
                                .branch
                                .as_deref()
                                .is_some_and(|value| value.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .map(RefRow::Worktree),
            );
        }
        rows
    }

    fn row_key(row: &RefRow) -> SelectedRef {
        match row {
            RefRow::Local(branch) => SelectedRef::Local(branch.name.clone()),
            RefRow::Remote(branch) => SelectedRef::Remote {
                remote: branch.remote.clone().unwrap_or_default(),
                name: branch.name.clone(),
            },
            RefRow::Tag(tag) => SelectedRef::Tag(tag.name.clone()),
            RefRow::Worktree(worktree) => SelectedRef::Worktree(worktree.path.clone()),
        }
    }

    fn selected_key(selected: &SelectedRef) -> String {
        match selected {
            SelectedRef::Local(name) => format!("branch:{name}"),
            SelectedRef::Remote { name, .. } => format!("remote:{name}"),
            SelectedRef::Tag(name) => format!("tag:{name}"),
            SelectedRef::Worktree(path) => format!("worktree:{}", path.display()),
        }
    }

    fn branch_is_protected(&self, branch: &str) -> bool {
        let remote_branch = self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .remote_branches
                .iter()
                .any(|item| item.name == branch)
        });
        let local = if remote_branch {
            branch
                .split_once('/')
                .map(|(_, name)| name)
                .unwrap_or(branch)
        } else {
            branch
        };
        self.protected_patterns
            .iter()
            .any(|pattern| wildcard_match(pattern, branch) || wildcard_match(pattern, local))
    }

    fn default_remote(&self) -> Option<String> {
        self.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .remote_branches
                .iter()
                .find_map(|branch| branch.remote.clone())
        })
    }

    fn filter_count(&self, filter: BranchFilter) -> usize {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return 0;
        };
        let favorites = snapshot
            .local_branches
            .iter()
            .filter(|branch| self.favorites.contains(&format!("branch:{}", branch.name)))
            .count()
            + snapshot
                .remote_branches
                .iter()
                .filter(|branch| self.favorites.contains(&format!("remote:{}", branch.name)))
                .count()
            + snapshot
                .tags
                .iter()
                .filter(|tag| self.favorites.contains(&format!("tag:{}", tag.name)))
                .count()
            + snapshot
                .worktrees
                .iter()
                .filter(|worktree| {
                    self.favorites
                        .contains(&format!("worktree:{}", worktree.path.display()))
                })
                .count();
        match filter {
            BranchFilter::All => {
                snapshot.local_branches.len()
                    + snapshot.remote_branches.len()
                    + snapshot.tags.len()
                    + snapshot.worktrees.len()
            }
            BranchFilter::Local => snapshot.local_branches.len(),
            BranchFilter::Remote => snapshot.remote_branches.len(),
            BranchFilter::Tags => snapshot.tags.len(),
            BranchFilter::Favorites => favorites,
            BranchFilter::Merged => snapshot
                .local_branches
                .iter()
                .filter(|branch| !branch.is_head && branch.is_merged)
                .count(),
            BranchFilter::Worktrees => snapshot.worktrees.len(),
        }
    }
}
impl GitBranchesWindow {
    fn render_filter_button(
        &self,
        filter: BranchFilter,
        count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Button::new(SharedString::from(format!("git-filter-{filter:?}")))
            .ghost()
            .compact()
            .icon(filter.icon())
            .label(format!("{}  {count}", filter.label()))
            .selected(self.filter == filter)
            .w_full()
            .justify_start()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.filter = filter;
                this.compare = None;
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_row(&self, row: &RefRow, cx: &mut Context<Self>) -> AnyElement {
        let selected_ref = Self::row_key(row);
        let selected = self.selected.as_ref() == Some(&selected_ref);
        let key = Self::selected_key(&selected_ref);
        let favorite = self.favorites.contains(&key);
        let (icon, title, subtitle, badge, favoritable) = match row {
            RefRow::Local(branch) => (
                IconName::GitBranch,
                branch.name.clone(),
                format!(
                    "{}{}  ·  {}",
                    branch
                        .upstream
                        .clone()
                        .unwrap_or_else(|| "未设置上游".to_string()),
                    ahead_behind(branch.ahead, branch.behind),
                    branch.short_hash
                ),
                if branch.is_head {
                    "● HEAD".to_string()
                } else if !branch.is_merged {
                    "未合并".to_string()
                } else {
                    String::new()
                },
                true,
            ),
            RefRow::Remote(branch) => (
                IconName::Network,
                branch.name.clone(),
                format!(
                    "{}  ·  {}",
                    branch.short_hash,
                    branch.subject.clone().unwrap_or_default()
                ),
                String::new(),
                true,
            ),
            RefRow::Tag(tag) => (
                IconName::Link,
                tag.name.clone(),
                format!(
                    "{}{}{}",
                    tag.short_hash,
                    if tag.is_annotated {
                        "  ·  附注标签"
                    } else {
                        "  ·  轻量标签"
                    },
                    tag.message
                        .as_ref()
                        .map(|message| format!("  ·  {message}"))
                        .unwrap_or_default()
                ),
                String::new(),
                true,
            ),
            RefRow::Worktree(worktree) => (
                IconName::FolderOpen,
                worktree
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| worktree.path.display().to_string()),
                format!(
                    "{}  ·  {}",
                    worktree.branch.as_deref().unwrap_or("分离 HEAD"),
                    worktree.path.display()
                ),
                if worktree.is_current {
                    "● 当前".to_string()
                } else {
                    String::new()
                },
                false,
            ),
        };
        let protected = match row {
            RefRow::Local(branch) => self.branch_is_protected(&branch.name),
            RefRow::Remote(branch) => self.branch_is_protected(&branch.name),
            _ => false,
        };
        let row_for_click = selected_ref.clone();
        let key_for_favorite = key.clone();
        h_flex()
            .id(SharedString::from(format!("git-ref-row-{key}")))
            .w_full()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if selected {
                cx.theme().primary.opacity(0.55)
            } else {
                cx.theme().border
            })
            .bg(if selected {
                cx.theme().primary.opacity(0.12)
            } else {
                cx.theme().background.opacity(0.52)
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = Some(row_for_click.clone());
                this.compare = None;
                cx.notify();
            }))
            .child(
                Icon::new(icon)
                    .size_4()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(title),
                            )
                            .when(protected, |this| {
                                this.child(
                                    Icon::new(IconName::TriangleAlert)
                                        .size_3()
                                        .text_color(cx.theme().warning),
                                )
                            })
                            .when(!badge.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(badge),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    ),
            )
            .when(favoritable, |this| {
                this.child(
                    Button::new(SharedString::from(format!(
                        "git-favorite-{key_for_favorite}"
                    )))
                    .ghost()
                    .compact()
                    .icon(if favorite {
                        IconName::StarMinus
                    } else {
                        IconName::StarPlus
                    })
                    .tooltip(if favorite { "取消收藏" } else { "收藏" })
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.toggle_favorite(key_for_favorite.clone(), cx);
                        },
                    )),
                )
            })
            .into_any_element()
    }
}
impl GitBranchesWindow {
    fn render_details(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.selected.clone() {
            Some(SelectedRef::Local(name)) => self.render_local_details(name, cx),
            Some(SelectedRef::Remote { remote, name }) => {
                self.render_remote_details(remote, name, cx)
            }
            Some(SelectedRef::Tag(name)) => self.render_tag_details(name, cx),
            Some(SelectedRef::Worktree(path)) => self.render_worktree_details(path, cx),
            None => v_flex()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("选择一个分支、远程引用、标签或 worktree 以管理它。")
                .into_any_element(),
        }
    }

    fn render_local_details(&self, name: String, cx: &mut Context<Self>) -> AnyElement {
        let Some(branch) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .local_branches
                    .iter()
                    .find(|branch| branch.name == name)
            })
            .cloned()
        else {
            return div()
                .p_4()
                .child("分支已不存在，请刷新。")
                .into_any_element();
        };
        let key = format!("branch:{name}");
        let favorite = self.favorites.contains(&key);
        let protected = self.branch_is_protected(&name);
        let upstream = self.default_upstream(&name).unwrap_or_default();
        let worktree_path = default_worktree_path(&self.root, &name);
        let checkout_name = name.clone();
        let compare_name = name.clone();
        let upstream_name = name.clone();
        let worktree_branch = name.clone();
        let rename_name = name.clone();
        let protect_name = name.clone();
        let delete_name = name.clone();
        let favorite_key = key.clone();
        let primary = h_flex()
            .gap_2()
            .child(
                Button::new("git-action-checkout")
                    .primary()
                    .small()
                    .label(if branch.is_head {
                        "当前分支"
                    } else {
                        "检出"
                    })
                    .disabled(branch.is_head || self.loading)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.checkout_selected(
                            GitRefKind::LocalBranch,
                            checkout_name.clone(),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("git-action-compare")
                    .outline()
                    .small()
                    .label("与 HEAD 比较")
                    .disabled(self.loading)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.compare_with_head(compare_name.clone(), cx);
                    })),
            )
            .child(
                Button::new("git-action-upstream")
                    .outline()
                    .small()
                    .label("设置上游")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_editor(
                            EditorKind::Upstream {
                                branch: upstream_name.clone(),
                            },
                            &upstream,
                            "",
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("git-action-worktree")
                    .outline()
                    .small()
                    .label("新 worktree")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_editor(
                            EditorKind::Worktree {
                                branch: worktree_branch.clone(),
                            },
                            &worktree_path.to_string_lossy(),
                            "",
                            window,
                            cx,
                        );
                    })),
            );
        let secondary = h_flex()
            .gap_2()
            .child(
                Button::new("git-action-favorite")
                    .ghost()
                    .small()
                    .label(if favorite { "取消收藏" } else { "收藏" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_favorite(favorite_key.clone(), cx);
                    })),
            )
            .child(
                Button::new("git-action-rename")
                    .outline()
                    .small()
                    .label("重命名")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_editor(
                            EditorKind::RenameBranch {
                                old_name: rename_name.clone(),
                            },
                            "",
                            "",
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("git-action-protect")
                    .outline()
                    .small()
                    .label(if protected { "取消保护" } else { "保护" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_protection(&protect_name, cx);
                    })),
            )
            .child(
                Button::new("git-action-delete")
                    .danger()
                    .small()
                    .label(if branch.is_merged {
                        "删除"
                    } else {
                        "删除未合并分支"
                    })
                    .disabled(branch.is_head)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let branch = this
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| {
                                snapshot
                                    .local_branches
                                    .iter()
                                    .find(|branch| branch.name == delete_name)
                            })
                            .cloned();
                        if let Some(branch) = branch {
                            this.confirm_delete_local(&branch, window, cx);
                        }
                    })),
            );
        v_flex()
            .p_4()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_base().font_semibold().child(branch.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {} · 上游：{}",
                                branch.short_hash,
                                branch.commit_hash,
                                branch.upstream.as_deref().unwrap_or("无")
                            )),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(branch.subject.clone().unwrap_or_default()),
                    ),
            )
            .child(primary)
            .child(secondary)
            .into_any_element()
    }
}
impl GitBranchesWindow {
    fn render_remote_details(
        &self,
        remote: String,
        name: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(branch) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .remote_branches
                    .iter()
                    .find(|branch| branch.name == name)
            })
            .cloned()
        else {
            return div()
                .p_4()
                .child("远程分支已不存在，请刷新。")
                .into_any_element();
        };
        let key = format!("remote:{name}");
        let favorite = self.favorites.contains(&key);
        let protected = self.branch_is_protected(&name);
        let checkout_name = name.clone();
        let compare_name = name.clone();

        let favorite_key = key.clone();
        let branch_name = name
            .split_once('/')
            .map(|(_, branch)| branch.to_string())
            .unwrap_or_else(|| name.clone());
        let delete_remote = remote.clone();
        let primary = h_flex()
            .gap_2()
            .child(
                Button::new("git-action-checkout-remote")
                    .primary()
                    .small()
                    .label("检出并跟踪")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.checkout_selected(
                            GitRefKind::RemoteBranch,
                            checkout_name.clone(),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("git-action-compare-remote")
                    .outline()
                    .small()
                    .label("与 HEAD 比较")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.compare_with_head(compare_name.clone(), cx);
                    })),
            );
        let secondary = h_flex()
            .gap_2()
            .child(
                Button::new("git-action-favorite")
                    .ghost()
                    .small()
                    .label(if favorite { "取消收藏" } else { "收藏" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_favorite(favorite_key.clone(), cx);
                    })),
            )
            .child(
                Button::new("git-action-delete-remote")
                    .danger()
                    .small()
                    .label("删除远程分支")
                    .disabled(protected)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.confirm_delete_remote(
                            delete_remote.clone(),
                            branch_name.clone(),
                            window,
                            cx,
                        );
                    })),
            );
        v_flex()
            .p_4()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_base().font_semibold().child(branch.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} · {}", branch.short_hash, branch.commit_hash)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(branch.subject.clone().unwrap_or_default()),
                    ),
            )
            .child(primary)
            .child(secondary)
            .into_any_element()
    }

    fn render_tag_details(&self, name: String, cx: &mut Context<Self>) -> AnyElement {
        let Some(tag) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.tags.iter().find(|tag| tag.name == name))
            .cloned()
        else {
            return div()
                .p_4()
                .child("标签已不存在，请刷新。")
                .into_any_element();
        };
        let key = format!("tag:{name}");
        let favorite = self.favorites.contains(&key);
        let remote = self
            .default_remote()
            .unwrap_or_else(|| "origin".to_string());
        let checkout_name = name.clone();
        let compare_name = name.clone();
        let push_tag_name = name.clone();
        let delete_tag_name = name.clone();
        let delete_remote_name = name.clone();
        let favorite_key = key.clone();
        let push_remote = remote.clone();
        let delete_remote = remote.clone();
        let primary = h_flex()
            .gap_2()
            .child(
                Button::new("git-action-checkout-tag")
                    .primary()
                    .small()
                    .label("检出标签")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.checkout_selected(GitRefKind::Tag, checkout_name.clone(), window, cx);
                    })),
            )
            .child(
                Button::new("git-action-compare-tag")
                    .outline()
                    .small()
                    .label("与 HEAD 比较")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.compare_with_head(compare_name.clone(), cx);
                    })),
            )
            .child(
                Button::new("git-action-push-tag")
                    .outline()
                    .small()
                    .label("推送标签")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.run_action(
                            GitAction::PushTag {
                                remote: push_remote.clone(),
                                name: push_tag_name.clone(),
                            },
                            window,
                            cx,
                        );
                    })),
            );
        let secondary = h_flex()
            .gap_2()
            .child(
                Button::new("git-action-favorite")
                    .ghost()
                    .small()
                    .label(if favorite { "取消收藏" } else { "收藏" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_favorite(favorite_key.clone(), cx);
                    })),
            )
            .child(
                Button::new("git-action-delete-tag")
                    .danger()
                    .small()
                    .label("删除本地标签")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.confirm_delete_tag(
                            format!("删除标签“{delete_tag_name}”？"),
                            GitAction::DeleteTag {
                                name: delete_tag_name.clone(),
                            },
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("git-action-delete-remote-tag")
                    .danger()
                    .small()
                    .label("删除远程标签")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.confirm_delete_tag(
                            format!("从 {delete_remote} 删除标签“{delete_remote_name}”？"),
                            GitAction::DeleteRemoteTag {
                                remote: delete_remote.clone(),
                                name: delete_remote_name.clone(),
                            },
                            window,
                            cx,
                        );
                    })),
            );
        v_flex()
            .p_4()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_base().font_semibold().child(tag.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}{}",
                                tag.target_hash,
                                tag.short_hash,
                                if tag.is_annotated {
                                    " · 附注标签"
                                } else {
                                    " · 轻量标签"
                                }
                            )),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(tag.message.clone().unwrap_or_default()),
                    ),
            )
            .child(primary)
            .child(secondary)
            .into_any_element()
    }

    fn render_worktree_details(&self, path: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let Some(worktree) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .worktrees
                    .iter()
                    .find(|worktree| worktree.path == path)
            })
            .cloned()
        else {
            return div()
                .p_4()
                .child("worktree 已不存在，请刷新。")
                .into_any_element();
        };
        v_flex()
            .p_4()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .child(worktree.path.display().to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                worktree.branch.as_deref().unwrap_or("分离 HEAD"),
                                worktree.head_hash.as_deref().unwrap_or("未知")
                            )),
                    ),
            )
            .child(
                Button::new("git-action-open-worktree")
                    .primary()
                    .small()
                    .label("在 Kite 中打开")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let path = worktree.path.clone();
                        this.main_app
                            .update(cx, |app, cx| app.open_git_worktree(path, cx));
                    })),
            )
            .into_any_element()
    }

    fn default_upstream(&self, branch: &str) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        snapshot
            .local_branches
            .iter()
            .find(|candidate| candidate.name == branch)
            .and_then(|candidate| candidate.upstream.clone())
            .or_else(|| {
                snapshot
                    .remote_branches
                    .iter()
                    .find(|candidate| candidate.name.ends_with(&format!("/{branch}")))
                    .map(|candidate| candidate.name.clone())
            })
    }
}
impl GitBranchesWindow {
    fn render_compare(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (result, base, head) = self.compare.as_ref()?;
        let files = result
            .files
            .iter()
            .take(6)
            .map(|file| format!("{} +{} -{}", file.path, file.additions, file.deletions))
            .collect::<Vec<_>>()
            .join("\n");
        let commits = result
            .commits
            .iter()
            .take(4)
            .map(|commit| format!("{}  {}", commit.short_hash, commit.subject))
            .collect::<Vec<_>>()
            .join("\n");
        Some(
            v_flex()
                .p_3()
                .gap_1()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.35))
                .text_xs()
                .child(div().font_semibold().child(format!(
                    "{base} ↔ {head}  ·  领先 {} / 落后 {}",
                    result.ahead, result.behind
                )))
                .when(!commits.is_empty(), |this| {
                    this.child(div().text_color(cx.theme().muted_foreground).child(commits))
                })
                .when(!files.is_empty(), |this| {
                    this.child(div().text_color(cx.theme().muted_foreground).child(files))
                })
                .into_any_element(),
        )
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.editor.as_ref()?;
        let title = editor.kind.title();
        let primary = Input::new(&editor.primary)
            .aria_label(title)
            .cleanable(true)
            .w_full();
        let secondary = editor.kind.secondary_placeholder().map(|_| {
            Input::new(&editor.secondary)
                .aria_label("附加信息")
                .cleanable(true)
                .w_full()
                .into_any_element()
        });
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000088))
                .child(
                    v_flex()
                        .w(px(480.))
                        .max_w(relative(0.9))
                        .gap_3()
                        .p_5()
                        .rounded_lg()
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .shadow_xl()
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(div().text_lg().font_semibold().child(title))
                                .child(
                                    Button::new("git-editor-close")
                                        .ghost()
                                        .compact()
                                        .icon(IconName::X)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.close_editor(window, cx)
                                            },
                                        )),
                                ),
                        )
                        .child(primary)
                        .when_some(secondary, |this, input| this.child(input))
                        .when(
                            matches!(editor.kind, EditorKind::CreateTag { .. }),
                            |this| {
                                this.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            Button::new("git-editor-light-tag")
                                                .ghost()
                                                .compact()
                                                .label("轻量标签")
                                                .selected(!editor.annotated)
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _, cx| {
                                                        if let Some(editor) = this.editor.as_mut() {
                                                            editor.annotated = false;
                                                        }
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new("git-editor-annotated-tag")
                                                .ghost()
                                                .compact()
                                                .label("附注标签")
                                                .selected(editor.annotated)
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _, cx| {
                                                        if let Some(editor) = this.editor.as_mut() {
                                                            editor.annotated = true;
                                                        }
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                )
                            },
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("git-editor-cancel").label("取消").on_click(
                                    cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.close_editor(window, cx)
                                    }),
                                ))
                                .child(
                                    Button::new("git-editor-submit")
                                        .primary()
                                        .label("确定")
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.submit_editor(window, cx)
                                            },
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
impl Render for GitBranchesWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.filtered_rows(cx);
        let mut list = v_flex().w_full().gap_1p5();
        for row in &rows {
            list = list.child(self.render_row(row, cx));
        }
        let list = if rows.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if self.loading {
                    "正在读取 Git 引用…"
                } else {
                    "没有匹配的 Git 引用"
                })
                .into_any_element()
        } else {
            div()
                .id("git-ref-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .child(list)
                .into_any_element()
        };
        let sidebar = v_flex()
            .w(px(190.))
            .flex_shrink_0()
            .gap_1()
            .p_2()
            .border_r_1()
            .border_color(cx.theme().border)
            .children(BranchFilter::ALL.into_iter().map(|filter| {
                let count = self.filter_count(filter);
                self.render_filter_button(filter, count, cx)
            }));
        let status = self
            .error
            .clone()
            .or_else(|| self.status.clone())
            .unwrap_or_default();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_lg().font_semibold().child("分支与标签"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(self.root.display().to_string()),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("git-new-branch")
                                    .outline()
                                    .small()
                                    .icon(IconName::GitBranch)
                                    .label("新建分支")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        let start = this
                                            .selected
                                            .as_ref()
                                            .map(selected_reference_name)
                                            .unwrap_or_else(|| "HEAD".to_string());
                                        this.open_editor(
                                            EditorKind::CreateBranch,
                                            "",
                                            &start,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("git-new-tag")
                                    .outline()
                                    .small()
                                    .icon(IconName::Link)
                                    .label("新建标签")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        let target = this
                                            .selected
                                            .as_ref()
                                            .map(selected_reference_name)
                                            .unwrap_or_else(|| "HEAD".to_string());
                                        this.open_editor(
                                            EditorKind::CreateTag {
                                                target: target.clone(),
                                            },
                                            "",
                                            "",
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("git-cleanup-merged")
                                    .ghost()
                                    .small()
                                    .label("清理已合并")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.cleanup_merged(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("git-refresh")
                                    .ghost()
                                    .small()
                                    .icon(IconName::RotateCw)
                                    .tooltip("刷新")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.load(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("git-close-window")
                                    .ghost()
                                    .small()
                                    .icon(IconName::X)
                                    .tooltip("关闭")
                                    .on_click(|_, window, _| window.remove_window()),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().min_w(px(220.)).child(
                            Input::new(&self.query_input)
                                .aria_label("搜索 Git 引用")
                                .prefix(Icon::new(IconName::Search).size_4())
                                .cleanable(true),
                        ),
                    )
                    .when(
                        self.snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.dirty),
                        |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child("工作区有未提交更改；检出时会询问是否 stash"),
                            )
                        },
                    )
                    .when(self.loading, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("正在执行…"),
                        )
                    }),
            )
            .child(
                h_flex().flex_1().min_h_0().child(sidebar).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(list)
                        .when_some(self.render_compare(cx), |this, compare| {
                            this.child(div().px_3().pb_2().child(compare))
                        })
                        .child(self.render_details(cx)),
                ),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(if self.error.is_some() {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(status),
            )
            .when_some(self.render_editor(cx), |this, editor| this.child(editor))
    }
}

fn selected_reference_name(selected: &SelectedRef) -> String {
    match selected {
        SelectedRef::Local(name) | SelectedRef::Tag(name) => name.clone(),
        SelectedRef::Remote { name, .. } => name.clone(),
        SelectedRef::Worktree(path) => path.display().to_string(),
    }
}

fn ahead_behind(ahead: u32, behind: u32) -> String {
    if ahead == 0 && behind == 0 {
        String::new()
    } else {
        format!("  ↑{ahead} ↓{behind}")
    }
}

fn default_worktree_path(root: &Path, branch: &str) -> PathBuf {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    root.parent()
        .unwrap_or(root)
        .join(format!("{name}--{}", branch.replace('/', "-")))
}

fn wildcard_match(pattern: &str, branch: &str) -> bool {
    let pattern = pattern.as_bytes();
    let branch = branch.as_bytes();
    let (mut pattern_index, mut branch_index, mut star, mut matched) = (0, 0, None, 0);
    while branch_index < branch.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == branch[branch_index])
        {
            pattern_index += 1;
            branch_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            matched = branch_index;
            pattern_index += 1;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            matched += 1;
            branch_index = matched;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}
