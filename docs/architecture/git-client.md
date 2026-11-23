# Kite Git 客户端架构

## 结论

Kite 的 Git 能力分两层交付：

- 预览层：文件预览显示所选文件的 Git 状态、最近提交、增删行数和工作区差异。
- 客户端层：独立 Git 客户端窗口，能力参照 DetachHead/rebased。

本文件记录当前已实现的 Core Git 服务、FFI 事件边界、仓库模型和后续客户端拆分。客户端层的其余拆分见 #115 的子 issue #116–#129。

## 跨平台边界

Git 逻辑只在 Rust Core 实现一次。

- Core：`src/git/` 提供命令执行器、仓库发现、状态、操作、凭据桥、控制台和仓库监听。`src/vcs.rs` 只保留 SVN 实现及共享的 VCS 枚举。
- FFI：`src/ffi.rs` 导出 `GitService`、同步操作、后台 `GitOperationHandle`、`GitRepositoryWatchHandle`，以及进度、凭据、错误和仓库状态事件回调。
- Windows：GPUI 的 Git 菜单写入和查询委托给 `kite_core::git::GitService`。SVN 继续走 `vcs.rs`。
- macOS：现有 `run_vcs_operation` / `run_vcs_query` 已补齐并委托给同一 Git 服务；SwiftUI 预览继续使用 `git_path_summary` 和 `git_file_diff`。

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

### 同步与后台写入

- `GitService.run_operation(path, operation, events, credentials)`
- `GitService.start_operation(...) -> GitOperationHandle`
- `GitOperationHandle.cancel()`
- `GitOperationHandle.is_finished()`
- `GitOperationHandle.wait()`

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
- 快捷操作：查看差异、暂存、取消暂存、历史。

状态颜色沿用分栏徽标的配色：冲突、删除为红色；新增、重命名为绿色；修改为橙色；未跟踪为青色。

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

## 分支与标签

`src/git_branches.rs` 提供跨平台的分支与标签服务，`src/ffi.rs` 将其导出给 macOS。

- 读取本地分支、远程跟踪分支、轻量/附注标签、HEAD、上游和领先/落后计数。
- 检出本地分支、远程跟踪分支、标签或提交；工作区不干净时默认拒绝，也可先 stash、切换后自动恢复。
- 新建、重命名、删除本地分支；删除未合并分支必须显式 force。批量清理只处理已合并分支。
- 删除远程分支前要求 UI 确认；远程删除使用 `--force-with-lease`。
- 设置/清除上游、比较分支的领先/落后、提交和文件。
- 新建轻量/附注标签、推送、删除本地标签和删除远程标签。
- 在任意目标目录创建新 worktree。
- Windows 分支窗口和 macOS 分支 sheet 共用同一组 Core 操作；收藏和受保护分支规则保存在各平台本地偏好中。
- 工具栏显示当前分支及领先/落后箭头，点击打开可搜索、可收藏的分支树。
## 验收测试

Core 测试覆盖：

- 临时仓库中 add、commit、unstage、revert 和 diff/log。
- 嵌套仓库与 linked worktree 发现。
- merge 标记识别。
- `.git` 变化监听和防抖。
- askpass 提示分类、认证失败分类、进度行分帧和取消。
- 现有 SVN 状态解析和 VCS 聚合测试。

## 后续拆分

- hunk/行级暂存：#119。
- 并排差异和语法高亮：#121。
- 提交图和完整日志：#122。
- blame：#128。
- 预览打开时延和 gix/CLI 对比：#129。
