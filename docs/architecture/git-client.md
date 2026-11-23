# Kite Git 客户端架构

## 结论

Kite 的 Git 能力分两层交付：

- 预览层：文件预览显示所选文件的 Git 状态、最近提交、增删行数和工作区差异。
- 客户端层：独立 Git 客户端窗口，能力参照 DetachHead/rebased。

本文件记录当前已实现的 Core Git 服务、FFI 事件边界、仓库模型、预览层、提交历史视图和后续客户端拆分。客户端层的其余拆分见 #115 的子 issue #116–#129。

## 跨平台边界

Git 逻辑只在 Rust Core 实现一次。

- Core：`src/git/` 提供命令执行器、仓库发现、状态、操作、凭据桥、控制台、仓库监听和远程同步；`src/git/log.rs` 负责分页日志、提交图、提交详情、文件树和文件历史。`src/vcs.rs` 只保留 SVN 实现及共享的 VCS 枚举。
- FFI：`src/ffi.rs` 导出 `GitService`、同步操作、后台 `GitOperationHandle`、远程同步、提交状态与执行、进度监视器、提交历史 API 和仓库事件回调。
- Windows：GPUI 的 Git 菜单写入和远程同步委托给 `kite_core::git::GitService`，提交历史窗口直接调用 `kite_core::git::log`。SVN 继续走 `vcs.rs`。
- macOS：SwiftUI 通过同一 Git 服务和 UniFFI 调用预览、远程同步、仓库操作与提交历史。

## 服务边界

`GitService` 是唯一的 Git 能力入口。

```text
UI
 ├─ 同步状态读取 ──> GitService::repository_state
 ├─ 同步写入 ──────> GitService::run_operation
 ├─ 后台长操作 ────> GitService::start_operation -> GitOperationTask
 └─ .git 变化 ─────> GitService::watch_repository
                         │
                         ├─ GitExecutor
                         ├─ GitConsole
                         ├─ AskpassBridge
                         └─ GitRepository model
```

Core 调用 git CLI，不链接 libgit2。git CLI 遵守用户的 git 配置、凭据助手、hooks、签名设置、line ending 配置和 submodule 行为。Rebased 和 git4idea 也使用 CLI 作为写操作后端。

### 命令执行器

所有命令统一经过 `GitExecutor`：

- 环境变量：`GIT_OPTIONAL_LOCKS=0`、`GIT_TERMINAL_PROMPT=0`、`GCM_INTERACTIVE=Never`、`LC_ALL=C`、`LANGUAGE=C`。
- 每个 Git 命令都带全局 `--no-optional-locks`。
- 路径以 `OsString` 传递，不使用 `display()` 或 UTF-8 转换来构造命令参数。
- Windows 使用 `CREATE_NO_WINDOW`，复用 `vcs::configured_command` 的进程配置。
- stdout/stderr 分别读取并保留控制台记录；单条记录和总输出有上限。
- 命令文本在写入控制台前遮蔽 URL 内嵌凭据。

### 长操作、进度和取消

`start_operation` 在后台线程运行命令并返回 `GitOperationTask`。UI 可以调用：

- `cancel()`：设置共享取消标记；执行器终止子进程并返回 `Cancelled` 错误。
- `is_finished()`：轮询后台任务是否结束。
- `wait()`：等待并取得结构化结果。

fetch、pull、push、clone 自动启用 `--progress`。stderr 按 `\r` 和 `\n` 分帧，生成 `GitEvent::Progress`；原始输出同时作为 `GitEvent::Output` 发送。收到取消标记后最多在一个轮询周期内终止命令。

### 凭据与交互

`AskpassBridge` 为需要交互的 Git 命令创建临时 askpass 脚本，并设置：

- `GIT_ASKPASS`
- `SSH_ASKPASS`
- `SSH_ASKPASS_REQUIRE=force`
- `PINENTRY_USER_DATA`

脚本通过临时目录中的 request/response 文件与 Core 通信。Core 把提示分类为 HTTPS、SSH passphrase、GPG pinentry 或 unknown，通过 `GitCredentialCallback` 请求 UI 输入，再把结果写回。用户取消凭据提示会取消整个 Git 操作。临时目录在命令结束后删除。

当前 GPG 交互通过 `PINENTRY_USER_DATA` 暴露提示上下文；如果用户配置的 pinentry 不读取该变量，Git 仍使用用户现有的 pinentry 配置，而不是由 Kite 改写全局 gpg-agent 设置。

### 结构化错误

错误分类不依赖 UI 解析 stderr。`GitErrorKind` 包含：

- `Authentication`
- `Conflict`
- `Rejected`
- `MissingIdentity`
- `RepositoryLocked`
- `UnsupportedVersion`
- `Cancelled`
- `NotInstalled` / `NotRepository`
- `InvalidOperation` / `Io` / `Other`

正常 UI 只显示本地化摘要；原始 stderr、退出码、命令和仓库路径保存在 `GitError.detail` 中，供 Git 控制台和诊断界面查看。

### Git 控制台

`GitConsole` 是进程内环形缓冲区，保存每次命令的：

- 开始时间
- 标签和脱敏后的命令
- 仓库路径
- 耗时
- 退出码
- stdout / stderr

Windows UI 和 FFI 都读取同一个 `GitService` 控制台快照。

### Git 版本

最低版本为 **2.30.0**。`GitService::require_supported_version` 在每次写操作前检查版本，低于最低版本时返回 `UnsupportedVersion`，而不是让旧 Git 在子命令选项中失败。版本信息也通过 FFI 暴露给设置和诊断界面。

## 仓库模型

`discover_repository` 使用 `git rev-parse` 确定：

- 工作区根目录
- 当前 worktree 的 git dir
- 共享的 common dir

因此嵌套仓库会选择最近的仓库；linked worktree 会得到独立 git dir 和共享 common dir。

`GitRepositoryState` 读取 `status --porcelain=v2 --branch`，包含：

- branch / detached / unborn HEAD
- HEAD oid
- upstream
- ahead / behind
- 进行中的 rebase、merge、cherry-pick、revert、bisect

`GitRepositoryWatcher` 使用 `notify` 递归监听 git dir 和 common dir，180 ms 防抖后重新读取仓库状态并发出 `RepositoryChanged` 事件。Windows 和 macOS 的目录自动刷新仍复用现有可见分栏机制。

## FFI 接口

### 同步读取

- `GitService.repository_state(path)`
- `GitService.version()`
- `GitService.console_entries()`
- `GitService.clear_console()`
- `git_commit_state(path)`：读取暂存文件、身份、模板、最近提交信息和签名默认值。
- `git_commit_subject_warning(message)`：返回非阻塞的提交标题提示。

### 提交历史

- `git_log_context(path)`
- `git_log_page(path, filter, cursor, page_size)`
- `git_commit_details(path, hash)`
- `git_commit_tree(path, hash)`
- `git_file_history(path, relative_path, cursor, page_size)`

这些接口由 `src/git/log.rs` 生成，底层命令统一经过 `GitService` 的共享执行器。

### 同步与后台写入

- `GitService.run_operation(path, operation, events, credentials)`
- `GitService.start_operation(...) -> GitOperationHandle`
- `GitOperationHandle.cancel()`
- `GitOperationHandle.is_finished()`
- `GitOperationHandle.wait()`
- `git_commit(options)`：创建普通或 amend 提交，支持 sign-off、签名三态、跳过 hooks、指定作者和提交后推送。

`GitOperationDto` 覆盖现有 macOS Git 写操作：Add、Unstage、Commit、Pull、Push、Fetch、Revert、Clean，并预留 Clone。

### 事件回调

`GitEventCallback.on_event` 接收：

- `Started`
- `Progress`
- `Output`
- `CredentialRequested`
- `RepositoryChanged`
- `Finished`
- `Failed`

`GitCredentialCallback.request_credential` 同步返回用户输入或 `None`（取消）。

### 仓库监听

`GitService.watch_repository(path, events)` 返回 `GitRepositoryWatchHandle`。调用 `stop()` 停止监听。

## 提交

提交工作流由 `src/git/commit.rs` 提供，Windows 和 macOS 共用同一套预检查与执行逻辑：

- `git_commit_state(path)` 读取当前分支、HEAD、上游、`user.name`、`user.email`、暂存文件和增删统计。
- amend 状态按 `HEAD^` 到索引计算最终文件集；根提交使用空树作为比较基线。
- 未解决冲突、缺少用户身份、普通提交没有暂存文件时，Core 拒绝提交；界面预检查不是安全边界。
- `commit.template` 被载入为初始说明，最近 20 条提交信息可供复用。
- 标题超过 72 个 Unicode 字符时只显示提示，不阻止提交。
- `amend`、`--signoff`、`-S`、`commit.gpgsign=false`、`--no-verify` 和 `--author` 直接映射到 Git 命令。
- “跟随 Git”不覆盖 `commit.gpgsign`；GPG、X.509 与 SSH 签名均由用户 Git 配置决定。
- 提交失败时保留 stdout/stderr，并区分 hook、签名和一般 Git 错误。
- 提交后推送失败不会丢失已创建的提交；结果同时返回提交哈希和 `push_error`。
- 提交成功显示短哈希，并可打开历史窗口。

Windows 的 Git 客户端和预览提供“高级…”提交窗口；macOS 对应 `KiteGitCommitSheet`。
Git 客户端变更列表下方仍保留快速提交区，快捷键为 Windows `Ctrl+K`、macOS `Command+K`。

## 预览层

### 数据

`git_path_summary(path)` 最多运行四个 git 进程：

1. `rev-parse --show-toplevel --absolute-git-dir`：确认路径在工作区内。失败时返回 `None`，不再运行其他命令。
2. `status --porcelain=v2 --branch`：分支、上游、领先和落后计数、暂存与工作区状态、目录下的变更计数。
3. `log -1`：最近一次修改该路径的提交。
4. `diff HEAD --numstat`：相对 HEAD 的增删行数。HEAD 不存在时跳过。

进行中的操作（变基、合并、拣选、还原提交、二分查找）通过 `.git` 目录中的标记文件判断，不运行 git 进程。

`git_file_diff(path)` 返回相对 HEAD 的统一差异。HEAD 不存在时返回暂存区差异。差异超过 256 KiB 时返回错误，超过 400 行时截断。

### 加载顺序

预览内容先加载，Git 区块后加载。

- Windows：`start_column_preview` 启动内容加载后，再调用 `start_column_git_preview`。两者共用 `preview_generation`，切换选择时旧结果被丢弃。
- macOS：`KiteGitPreviewSection` 使用 `.task(id: entry.path)`，切换选择时 SwiftUI 取消旧任务。

### 跳过条件

所选项目没有 Git 状态，且所在分栏不是 Git 工作区时，不启动 git 进程。干净的已跟踪文件仍然显示 Git 区块，因为分栏的 `git_available` 为真。

### 界面

两端显示相同字段：

- 分支或分离 HEAD 的短哈希，领先和落后计数。
- 进行中的操作。
- 文件：暂存状态、工作区状态、增删行数。目录：变更计数。
- 最近提交：短哈希、作者、时间、标题。
- 快捷操作：Diff 查看器、暂存、取消暂存、历史。

状态颜色沿用分栏徽标的配色：冲突、删除为红色；新增、重命名为绿色；修改为橙色；未跟踪为青色。

## 提交历史与提交图

### 分页

`git_log_page(path, filter, cursor, page_size)` 每页最多返回 1,000 条，UI 默认使用 200 条。游标是 Core 生成的 JSON 字符串，包含：

- 当前筛选条件；
- 已读取的提交数量；
- 活跃泳道的待处理父提交哈希；
- 泳道颜色序号。

筛选条件或仓库变化后，旧游标会被拒绝。两端都不解析游标内容，只在下一次请求时原样传回。

### 排序与首屏

查询使用 Git 默认倒序，而不是 `--topo-order`。100,000 提交夹具上，默认倒序约 134 ms，`--topo-order` 约 1,024 ms；后者必须扫描完整历史，无法满足首屏预算。提交图仍然只根据 `%P` 父提交哈希计算，因此两端使用同一份泳道与连边数据。

首屏加载第一页后，UI 立即后台预取第二页。Windows 使用 `uniform_list`，macOS 使用 `LazyVStack`，仅渲染可见行。

性能数据见 [Git 提交历史性能](../performance/git-log-v0.6.88.md)。

### 图布局

`layout_commit` 维护一组活跃泳道。每个泳道保存待处理父提交和颜色序号：

- 首个匹配泳道成为提交节点；
- 多个泳道等待同一提交时，额外泳道合并到节点并绘制连边；
- 第一父提交沿用节点泳道和颜色；
- 额外父提交复用已有泳道，或分配新泳道和颜色；
- 根提交关闭泳道；
- 所有存活泳道生成跨行竖线。

布局状态写入游标，因此跨页读取与一次读取相同提交序列得到完全相同的图。快照测试覆盖线性历史、普通合并、多父合并、多个根提交、长分支和跨页稳定性。

### 提交详情

`git_commit_details(path, hash)` 返回：

- 完整提交说明；
- 作者、提交者、父提交和引用；
- Git `%G?` 签名状态、签名者和 key；
- 包含该提交的本地和远程分支；
- 相对第一父提交的变更文件；
- 上游补丁等价检测结果。

`git_commit_tree(path, hash)` 返回该提交的扁平文件树；`git_file_history(path, relative_path, cursor, page_size)` 使用路径筛选复用日志分页。

### 界面

两端提供相同能力：

- 提交图、引用标签、标题、作者、日期、哈希；
- 分支、作者、日期、路径、文本/正则和哈希筛选；
- 提交详情、签名、父提交、包含分支、变更文件；
- 提交时文件树和单文件历史；
- 右键复制哈希和在提交时浏览文件；
- 分页懒加载与下一页预取。

引用颜色由类型决定：当前分支使用强调色，本地分支绿色，远程分支紫色，标签黄色。泳道颜色在提交之间保持稳定，并由 Core 的 `color_index` 决定。

## Diff 查看器

`diff::git_diff(GitDiffRequest)` 是两端共用的结构化差异入口。请求包含路径、旧端点、新端点和是否忽略空白；端点可以是 `HEAD`、任意提交或分支、暂存区或工作区。

Core 先计算哪些端点存在以及源文件大小，再调用带环境隔离的 git CLI：

- 文本差异解析为文件、hunk、上下文、删除、新增和修改行。
- 同一 hunk 中连续的删除与新增行按顺序配对，并在 Core 计算 UTF-16 词范围；两端只渲染同一份结果。
- Core 同时按扩展名生成关键字、字符串、数字、注释、类型和函数名的 UTF-16 语法范围，两端只负责配色。
- `-w` 由 git 处理忽略空白，避免 UI 各自实现不同的空白规则。
- `change_rows` 提供稳定的改动行索引，F7 / Shift+F7 只在这些行之间导航。
- 二进制文件只显示状态说明；图片端点小于 12 MiB 时返回原始图片数据供两端并排显示。
- 任一侧超过 8 MiB 时不运行完整文本 diff，只返回统计和最多 160 行新侧预览，避免大文件阻塞界面。

FFI 入口为 `KiteCore.gitDiff`。Windows 使用 `src/diff_viewer_window.rs`，macOS 使用 `apps/macos/Sources/KiteDiffViewerWindow.swift`。两端支持统一/并排、忽略空白、折叠未修改行、上一处/下一处改动和自定义提交或分支端点。

入口包括文件预览、变更视图、Stash、Git 客户端的“比较”和“最新提交 Diff”。自定义端点覆盖提交、分支、工作区与暂存区组合，因此后续日志、文件历史和分支比较复用同一窗口，不另写差异渲染。

Core 测试 `diff::tests::parses_hunks_and_word_ranges_deterministically` 锁定同一 patch 输入得到相同 hunk、行和词范围；`diff::tests::bounds_ten_megabyte_changes` 锁定 10 MiB 文件的降级路径。

## 客户端窗口框架

Git 客户端使用独立窗口。每个仓库最多打开一个窗口；再次打开同一仓库时激活已有窗口。

### 入口

- 分栏中的仓库目录或项目右键菜单：`Git > 打开 Git 客户端`。
- 主工具栏 Git 按钮；macOS 同时提供可点击的状态栏 Git 标记。
- 预览中的 Git 区块：`Git 客户端`。
- 应用菜单：`Git > 打开 Git 客户端…`。
- Windows 快捷键 `Ctrl+Shift+G`；macOS `Command+Shift+G`。

### 布局

- 左侧：变更分组、路径筛选和提交信息编辑区。
- 右侧：所选文件或目录的差异、Hunk 与行级操作。
- 底部：可折叠 Git 控制台和后台任务面板。
- 顶部：当前分支、上游、领先 / 落后计数，以及 获取、拉取、推送、提交、贮藏。
- 状态横幅：rebase、merge、cherry-pick、revert；按适用性提供继续、跳过和中止。正在 bisect 时显示结束并重置。

提交图和完整日志由 #122 在同一窗口内补充。

### 窗口状态

`GitClientWindowSessionDto` 随主会话写入 `session.json`，按仓库根目录去重，记录：

- 窗口宽高。
- 控制台高度和展开状态。
- 分栏比例和最后视图字段，供后续日志视图扩展使用。

### 快捷键

| 操作 | Windows | macOS |
| --- | --- | --- |
| 打开 Git 客户端 | Ctrl+Shift+G | Command+Shift+G |
| 刷新仓库 | Ctrl+Shift+R | Command+Shift+R |
| 获取 | Ctrl+Shift+F | Option+Command+F |
| 拉取 | Ctrl+Shift+P | Option+Command+P |
| 推送 | Ctrl+Shift+K | Option+Command+K |
| 提交 | Ctrl+K | Command+K |
| 贮藏 | Ctrl+Alt+S | Option+Command+S |
| 控制台 | Ctrl+J | Command+J |
| 快捷键表 | F1 | F1 |
| 关闭提示或浮层 | Esc | Esc |

macOS 的 Git 窗口不使用 `Command+Shift+F` 获取，避免与主窗口全局搜索冲突。

## 后端选型

P0 采用 **git CLI 作为唯一读写后端**。

| 维度 | git CLI | gix |
| --- | --- | --- |
| 用户配置 | 完整支持 includeIf、credential helper、hooks、签名、filter、submodule | 需要逐项映射，遗漏会导致行为不一致 |
| 写操作 | 与用户终端行为一致 | 需要重新实现大量事务和 hook 细节 |
| 凭据 | askpass、GIT_ASKPASS、SSH agent、GPG pinentry 均为原生路径 | 需要自定义凭据回调 |
| 错误信息 | LC_ALL=C 后稳定、可分类 | 错误模型不同，UI 需要第二套映射 |
| 大数据性能 | 进程启动成本固定；status/log/diff 在 Git 自身优化下表现稳定 | 理论上有进程内优势，但需要真实仓库基准 |
| 维护风险 | 语义与 Git 一致 | 版本绑定和 API 变更风险更高 |

读路径的 gix 对比测试保留在 #129 的性能门禁中。引入 gix 前必须满足：

- 同一临时仓库和真实大仓库上的 status/log/diff/blame 输出等价。
- p95 延迟至少优于 CLI 20%，且内存增量在可接受范围内。
- 不需要让用户配置、line ending、submodule 或 ignore 语义产生差异。

在此之前，Core 不为 gix 增加第二条读路径，避免出现两套行为。

## 验收测试

Core 测试覆盖：

- 临时仓库中 add、commit、unstage、revert 和 diff/log。
- 嵌套仓库与 linked worktree 发现。
- merge 标记识别。
- `.git` 变化监听和防抖。
- askpass 提示分类、认证失败分类、进度行分帧和取消。
- 现有 SVN 状态解析和 VCS 聚合测试。

## Stash、Reset、Revert 与 Cherry-pick

`src/git_ops.rs` 提供两端共用的历史操作：

- Stash：列出、保存、查看差异、应用、弹出、删除、清空，以及从 Stash 新建分支。
  保存时可填写说明、包含未跟踪文件、保留暂存区，或只保存当前选中的文件。
- Reset：soft、mixed、hard、keep。`reset --hard` 在 Core 和界面两层都要求确认。
- Revert 与 Cherry-pick：支持一个或多个提交。发生冲突时保留 Git 操作状态，
  两端显示同一状态横幅，并提供继续、跳过和中止入口。
- 删除 Stash、清空 Stash 和 `reset --hard` 必须由调用方明确传入
  `confirmed: true`；界面只在用户确认后传入该值。

Windows 的 Git 操作窗口位于 `src/git_tools_window.rs`，macOS 对应
`apps/macos/Sources/KiteGitToolsWindow.swift`。两端都从 Git 客户端的“Stash / 操作”进入，
窗口内提供 Stash、历史和操作三个标签页，并共用 Rust Core 返回的数据和错误。

## 变更视图与行级暂存

### 数据模型

`src/git_changes.rs` 用一次 `git status --porcelain=v2 --branch -z` 返回完整变更列表：

- 分组：已暂存、未暂存、未跟踪、冲突、已忽略。
- 重命名记录保留旧路径；未跟踪和忽略列表按需读取。
- 列表保持平铺，Windows 和 macOS 各自生成目录树或平铺视图，避免 Core 为 1 万个文件重复建模。

单个文件的 `file_changes_diff(repository_root, path, scope)` 返回 hunk、新旧行号和每一行的稳定选择键。
修改行按同一 hunk 内相邻的删除/新增行配对，因此勾选一条修改会同时选择删除和新增两侧。

### 补丁写入

`stage_lines`、`unstage_lines`、`discard_lines` 共用同一套 Core 逻辑：

1. 读取 index、HEAD 或工作区内容。
2. 按选择生成目标内容；取消暂存和丢弃时应用未选中的行。
3. 生成仅包含目标文件的完整 unified diff，通过 stdin 传给 `git apply --cached` 或工作区。
4. 返回操作后的新变更列表。

这样避免 UI 拼接 patch，也保证两端对新增、删除、修改和无末尾换行文件的行为一致。
重命名文件只允许整文件取消暂存；二进制文件和被截断的大差异只允许整文件操作。

### 安全边界

- 丢弃前由两端 UI 弹出确认。
- 未跟踪项目通过 `trash` 移到系统回收站，不使用 `git clean -fd`。
- 丢弃已跟踪文件只还原工作区到 index，不改变已暂存内容。
- 路径拒绝绝对路径和 `..`，所有 Git 命令限制在仓库根目录内。
- `core.autocrlf=true/input` 且工作区含 CRLF 时返回提示；补丁保留 index 的换行风格。

### 性能

1 万个修改文件的 `repository_changes` 单次测量为 257 ms，方法见
[`docs/performance/git-changes-10000-v0.6.88.md`](../performance/git-changes-10000-v0.6.88.md)。

### 界面

- Windows：`src/git_client_window.rs`，从 Git 菜单的“打开 Git 客户端”进入。
- macOS：`apps/macos/Sources/KiteGitClientWindow.swift`，从文件或目录右键 Git 菜单进入。
- 两端均可用 `Ctrl+Shift+G` / `Cmd+Shift+G` 从当前目录打开。
- 两端均支持路径筛选、目录树/平铺切换、文件/目录/Hunk/行级暂存与取消暂存、丢弃、忽略规则和 CRLF 提示。

## 交互式 Rebase 与历史改写

`src/git_rebase.rs` 为 Windows 和 macOS 共用同一套 Rebase 计划、todo 编辑和内存改写逻辑。

### 计划与 todo

`git_rebase_plan(path)` 读取当前分支、上游、onto、提交列表（从旧到新）、是否受保护、是否存在合并提交，以及哪些提交已经推送到远程。

UI 将计划映射为 todo 表。每一行包含：

- 动作：`pick`、`reword`、`edit`、`squash`、`fixup`、`drop`
- 短哈希和标题
- 正在 reword 或 squash 时使用的提交信息

todo 可以调整顺序。Windows 使用上下按钮，macOS 使用列表拖动。提交按从旧到新的顺序执行。

### 内存改写

线性历史通过 Git plumbing 在内存中重写，不切换工作区：

1. `git merge-tree --write-tree --merge-base=<旧父提交>` 把提交应用到新的父提交。
2. `git commit-tree` 使用合并后的文件树创建替换提交，并保留作者姓名、邮箱和作者时间。
3. 所有提交成功后，用 `git update-ref` 原子更新当前分支。

因此 reword、squash、fixup 和 drop 不会 checkout、reset 或改动工作区和暂存区。`--update-refs` 会额外更新指向被改写提交的本地分支和轻量标签。

“把当前改动并入历史”通过 `git stash create` 读取工作树快照，创建 `fixup! <目标标题>` 提交，再以 autosquash 内存改写。已跟踪的暂存和未暂存改动都会并入；未跟踪文件不会进入 fixup 提交。根提交暂不支持这一操作。

### 流程控制

`edit` 会在创建替换提交后暂停，并保存状态到 Git 目录的 `kite-rebase-state.json`。窗口中的“继续”“跳过”“中止”分别调用：

- `git_rebase_continue`
- `git_rebase_skip`
- `git_rebase_abort`

中止会把当前分支恢复到 Rebase 前的 HEAD。

### 安全检查

- 默认保护 `main`、`master`、`trunk`、`release/*`、`hotfix/*`。可用 `KITE_PROTECTED_BRANCHES` 覆盖逗号分隔的规则。
- 提交已经出现在远程跟踪分支时，UI 显示警告，核心在 `allow_rewrite_pushed` 为 false 时拒绝执行。
- 有合并提交时默认使用 `git rebase --rebase-merges`，此时只允许 `pick`。完整 merge/conflict 编辑属于 #127。
- 已经存在 merge、rebase、cherry-pick、revert 或 bisect 时拒绝开始新的改写。

### 已知边界

- 合并提交的 reword/squash/fixup 暂不支持；会明确报错，而不是悄悄降级。
- 冲突处理依赖 #127 的三方合并视图。当前发生冲突时不会更新分支引用。
- 未运行 Git hooks 的内存改写和 `git commit-tree` 语义一致；GPG 签名未强制开启。
## 远程同步层

### 服务边界

`src/git/remote.rs` 负责 Git 远程、同步和仓库创建命令，Windows GPUI 和 macOS SwiftUI 只调用共享 FFI：

- 仓库：`git_repository`。
- 远程：`git_list_remotes`、`git_add_remote`、`git_rename_remote`、`git_remove_remote`、`git_set_remote_url`。
- 同步：`git_fetch`、`git_pull`、`git_push_preview`、`git_commit_files`、`git_push`。
- 仓库创建：`git_clone`、`git_init`、`git_delete_partial_clone`、`git_adopt_partial_clone`。
- 进度：`GitProgressMonitor.snapshot()` 和 `GitProgressMonitor.cancel()`。

所有网络操作设置 `GIT_TERMINAL_PROMPT=0`、`GCM_INTERACTIVE=Never`、`LC_ALL=C`，并按行解析 `--progress` 输出。调用方在后台执行这些同步 FFI，并每 100–120 毫秒读取进度监视器。

### Pull 与本地更改

Pull 支持合并、变基和仅快进。工作区有未提交更改时，Core 执行：

1. `git stash push --include-untracked`。
2. 按所选策略执行 `git pull`。
3. 无论拉取成功或失败，都尝试 `git stash pop`。

恢复失败会返回冲突类错误，保留 stash，让用户可以从 Git 客户端继续处理。

### Push 安全

- 推送前由 `git_push_preview` 返回待推送提交、选中提交的文件和本地标签。
- 目标远程和分支可由用户修改；未指定时使用上游、`origin` 或当前分支。
- UI 的“只推送到所选提交”把该提交 OID 作为 refspec 源。
- 强制推送只有 `--force-with-lease=<目标 ref>`，代码中不存在 `--force`。
- `main`、`master`、`trunk`、`release/*`、`hotfix/*`，以及 `branch.<name>.kiteProtected=true` 或 `kite.protectedBranch` 配置的分支禁止强制推送。
- 推送被拒绝时，Core 返回结构化的 `GitPushRejected`，两端都提供“合并后重试”和“变基后重试”。

### 克隆取消

克隆先写入同级的隐藏 staging 目录（`.<name>.kite-clone-*`），成功后再原子改名为目标目录。取消时进程被终止：

- 返回 `GitCancelled` 和 `partial_clone_path`。
- UI 明确询问“删除”或“保留”。
- 删除操作只接受 Kite staging 目录命名，拒绝删除任意用户目录。
- 选择保留时，staging 目录会改名为用户选择的目标目录。

### 凭据与定时 Fetch

认证失败返回 `GitAuthenticationRequired`。两端弹出原生用户名/密码对话框，凭据只通过环境变量和临时 askpass helper 传给一次 Git 进程，不写入配置或日志。

定时 Fetch 默认关闭，可在设置中启用；启用后每 15 分钟对当前仓库执行只读 Fetch，不自动合并，也不显示凭据弹窗。

### 测试

`src/git/remote.rs` 的测试使用本机 bare 仓库模拟远程，覆盖：

- 推送被拒绝。
- 受保护分支阻止强制推送。
- 强制推送参数只包含 `--force-with-lease`。
- Pull 自动 stash 并恢复未提交文件。
- 远程增删改。
- 进度进程取消和未完成克隆目录的安全删除边界。
