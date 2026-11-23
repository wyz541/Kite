# Kite Git 客户端架构

## 结论

Kite 的 Git 能力分两层交付：

- 预览层：文件预览显示所选文件的 Git 状态、最近提交、增删行数和工作区差异。
- 客户端层：独立 Git 客户端窗口，能力参照 DetachHead/rebased。

本文件记录当前已实现的 Core Git 服务、FFI 事件边界、仓库模型、预览层、提交历史视图和后续客户端拆分。客户端层的其余拆分见 #115 的子 issue #116–#129。

## 跨平台边界

Git 逻辑只在 Rust Core 实现一次。

- Core：`src/git/` 提供命令执行器、仓库发现、状态、操作、凭据桥、控制台和仓库监听；`src/git/log.rs` 负责分页日志、提交图、提交详情、文件树和文件历史。`src/vcs.rs` 只保留 SVN 实现及共享的 VCS 枚举。
- FFI：`src/ffi.rs` 导出 `GitService`、同步操作、后台 `GitOperationHandle`、`GitRepositoryWatchHandle`、提交历史 API，以及进度、凭据、错误和仓库状态事件回调。
- Windows：GPUI 的 Git 菜单写入和查询委托给 `kite_core::git::GitService`，提交历史窗口直接调用 `kite_core::git::log`。SVN 继续走 `vcs.rs`。
- macOS：SwiftUI `KiteGitPreviewSection` 与 `KiteGitLogWindowView` 通过 `KiteModel` 调用同一组 FFI。

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

初次 release smoke 对比见 [Git 后端对比](../performance/git-backends-v0.6.88.md)。单台 macOS 两次反向顺序结果中，gix 的 status 稳定更慢；log、blame 和 diff 的 p50 更快，但 p95 方向不一致，尚未满足稳定的 20% 门槛。混合后端仍不足以抵消配置、错误分类和测试复杂度，因此当前四类读路径继续使用 Git CLI。引入 gix 前必须满足：

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
- 提交历史左栏按本地分支、远程分支和标签分组显示同一棵引用树；点击任意引用只显示该引用的历史。

## 验收测试

Git Core 测试共享 `src/test_support.rs` 的临时目录和本地仓库工具。每个测试创建独立仓库，并在测试结束时删除目录。

| 场景 | 夹具 | 当前断言 |
| --- | --- | --- |
| 合并历史 | `main` 与 `feature` 分支执行 `--no-ff` 合并 | `run_vcs_query(Log)` 返回合并提交和功能提交 |
| 冲突 | 两端修改同一文件后执行 merge | 路径摘要显示 `Merging`，文件状态为冲突 |
| 重命名 | `git add -A` 后读取 porcelain 状态 | 新路径状态为 `Renamed` |
| submodule | 子仓库加入父仓库的 `modules/child` | 修改子仓库后父仓库报告 submodule 变更 |
| worktree | `git worktree add -b linked` | 从 linked worktree 读取正确的根和分支 |
| LFS 指针 | 提交并修改 LFS pointer 文本 | 作为普通文件报告修改和增删行 |
| 非 UTF-8 文件名 | Unix `OsStringExt` 创建原始字节文件名 | 支持该文件名的文件系统上报告未跟踪状态 |
| Windows 超长路径 | `core.longpaths=true` 和超过 260 字符的路径 | Windows CI 报告修改状态 |
| 推送被拒绝 | 本地 bare remote 加两个 clone | 非 fast-forward push 返回失败 |
| 认证失败 | 回环 HTTP 服务返回 `401` | push 在无交互模式下返回失败 |
| log 第一屏 | 同一路径连续 100 次真实修改 | `git log -n 100` 返回 100 行 |

非 UTF-8 文件名测试在 APFS 等拒绝原始字节名称的文件系统上跳过。Windows 超长路径测试只在 Windows 编译和运行。

其余 Core 测试覆盖：

- 临时仓库中 add、commit、unstage、revert 和 diff/log。
- 嵌套仓库与 linked worktree 发现。
- merge 标记识别。
- `.git` 变化监听和防抖。
- askpass 提示分类、认证失败分类、进度行分帧和取消。
- 现有 SVN 状态解析和 VCS 聚合测试。

## 两端对照

当前 Core 已提供状态、预览、diff、log 文本和基础写操作。完整 Git 客户端仍未完成，因此下表是当前事实基线，也是后续子 issue 的验收输入。

### 快捷键

| 动作 | Windows | macOS | 结论 |
| --- | --- | --- | --- |
| 刷新 | `Ctrl+R` | `Cmd+R` | 一致 |
| 聚焦路径 | `Ctrl+L` | `Cmd+L` | 一致 |
| 当前分栏搜索 | `Ctrl+F` | 输入字符或分栏搜索按钮 | macOS 不使用 Git 快捷键 |
| 下一个页签 | `Ctrl+Tab` | `Ctrl+Tab` | 一致 |
| 上一个页签 | `Ctrl+Shift+Tab` | `Ctrl+Shift+Tab` | 一致 |
| Git 专用快捷键 | 无 | 无 | 尚无，客户端窗口落地后补齐 |

### 文件右键菜单

| 动作 | Windows | macOS | 结论 |
| --- | --- | --- | --- |
| 添加 / 暂存 | Git > 添加 | 预览区“暂存” | 入口不一致 |
| 取消暂存 | Git > 取消暂存 | 预览区“取消暂存” | 入口不一致 |
| 提交 | Git > 提交… | 暂无 | macOS 待补 |
| 拉取 | Git > 拉取 | 暂无 | macOS 待补 |
| 推送 | Git > 推送 | 暂无 | macOS 待补 |
| 获取 | Git > 获取 | 暂无 | macOS 待补 |
| 显示差异 | Git > 显示差异 | 预览区“查看差异” | 入口不一致 |
| 显示日志 | Git > 显示日志 | 预览区“历史” | 入口不一致 |
| 还原 | Git > 还原… | 暂无 | macOS 待补 |
| 清理工作区 | Git > 清理工作区… | 暂无 | macOS 待补 |

在完整客户端 PR 中，两端应使用同一动作名、同一确认规则和同一快捷键。预览区快捷操作可以保留，但不能替代完整菜单验收。

## 截图与发布验收

以下清单覆盖 #129 要求的五个 UI 面。截图附在对应 PR 的 Visual verification 区；发布证据需要长期保存时再进入 `docs/design/previews/`。

| 界面 | 来源 issue | 必需状态 | 当前状态 |
| --- | --- | --- | --- |
| 客户端主窗口 | #118、#122 | 亮色、深色；空仓库和有历史仓库 | 待实现 |
| 推送对话框 | #124 | 预览、成功、被拒绝、认证失败 | 待实现 |
| 交互式 rebase | #126 | 正常 todo、冲突、取消 | 待实现 |
| 三方合并 | #127 | 无冲突、冲突、解决后完成 | 待实现 |
| 预览中的 Git 区块 | #117 | 干净、修改、目录计数、非仓库；亮色和深色 | 已有实现，证据待补 |

截图记录必须包含日期、设备、系统版本、页面、仓库状态和 commit。不得提交真实用户名、私有路径、凭据或未脱敏仓库数据。

## 发布门槛

问题 #129 的最终关闭条件：

- [ ] 所有 `parity:required` PR 已合并，且发布脚本不再发现打开的 `parity:required` PR。
- [ ] README 同时包含“预览 Git 状态”和“Git 客户端”两行，两端均不是“开发中”。
- [ ] `.github/scripts/check-feature-matrix.sh` 的 `required_features` 包含这两个能力。
- [ ] `kite-perf` 的 Git status、log、blame、diff 场景在 Windows 和 macOS 生成结果。
- [ ] `gix` 与 Git CLI 的对比结果写入 `docs/performance/`。
- [ ] 五个 UI 面对照完成，缺少适用界面的条目必须在对应 issue 中说明原因。

当前分支只落地 Core 夹具和性能对比脚手架，不应勾选上述门槛。

## 后续拆分

- 并排差异和语法高亮：#121。
- blame：#128。
- 预览打开时延和 gix/CLI 对比：#129。

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

## 合并与冲突解决

### 合并

`src/git/merge.rs` 在共享 `GitService` 上实现普通、`--no-ff`、`--ff-only` 和 `--squash` 合并。目标分支、标签或提交先通过 `rev-parse --verify <target>^{commit}` 校验；所有参数以独立 argv 元素传递，拒绝以 `-` 开头的目标。

普通合并和保留合并提交可传入自定义说明。压缩合并只有填写说明时才创建提交；说明留空时只暂存结果。

### 冲突模型

`GitService::conflicts(path)` 使用 `git ls-files -u -z` 返回 stage、OID、路径和冲突类型：

- 双方修改、双方新增、自己删除、对方删除、重命名和其他冲突。
- 每个冲突的基线、自己、对方路径及是否存在。
- 二进制标记；检测到 NUL 或非 UTF-8 内容时只允许整文件选择。
- 进行中的 merge、rebase、cherry-pick、revert 或 bisect 状态。
- 是否可继续或中止。

重命名冲突通过 stage 1/2/3 的独立路径分组；状态输出只用于恢复重命名组顺序，普通冲突记录不会干扰分组。

### 三方合并

`three_way_merge(path, conflict)` 将三方内容写入临时文件并调用 `git merge-file --diff3`。结果包含：

- 带冲突标记的完整合并文本。
- 按行标记的上下文、自己、基线、对方和标记行。
- 每个冲突块的基线与双方版本。

UI 可逐块选择自己、对方或两边，也可切换到手动编辑。`apply_conflict_choices` 生成最终文本；`resolve_conflict` 拒绝仍含冲突标记的内容，写回后执行 `git add -A -- <path>`。删除型选择会移除文件并暂存删除。

### 继续、中止与外部工具

- `continue_operation` 对 merge、rebase、cherry-pick 和 revert 使用非交互编辑器设置继续。
- `abort_operation` 支持 merge、rebase、cherry-pick、revert 和 bisect。
- `open_merge_tool` 只启动用户配置的 `merge.tool`，未配置时返回可操作错误。

### 界面

- Windows：`src/git_merge_window.rs`，从 Git 客户端“合并…”或“解决冲突…”进入。
- macOS：`apps/macos/Sources/KiteGitMergeWindow.swift`，从 Git 菜单、文件/目录右键菜单或 Git 客户端进入。
- 冲突文件列表使用红色状态，文本冲突显示自己、对方和合并结果三列；当前冲突块高亮并可前后跳转。
- 二进制和重命名冲突提供整文件选择；文本冲突支持逐块选择与手动编辑。
