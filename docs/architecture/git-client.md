# Kite Git 客户端架构

## 结论

Kite 的 Git 能力分两层交付：

- 预览层：文件预览显示所选文件的 Git 状态、最近提交、增删行数和工作区差异。
- 客户端层：独立 Git 客户端窗口，能力参照 DetachHead/rebased。

本文件记录当前已实现的预览层和 Core 服务边界。
客户端层的拆分见 #115 的子 issue #116–#129。

## 跨平台边界

Git 逻辑只在 Rust Core 实现一次。

- Core：`src/git.rs` 读取单个路径的 Git 摘要和差异。`src/vcs.rs` 继续负责状态徽标和写操作。
- FFI：`src/ffi.rs` 通过 UniFFI 导出 `git_path_summary`、`git_file_diff`、`run_vcs_operation` 和 `run_vcs_query`。
- Windows：GPUI 分栏预览直接调用 `kite_core::git`。
- macOS：SwiftUI `KiteGitPreviewSection` 通过 `KiteModel` 调用 FFI。

macOS 此前没有 Git 写操作入口。导出 `run_vcs_operation` 和 `run_vcs_query` 补齐这个缺口。

## 后端选型

Core 调用 git CLI，不链接 libgit2 或 gix。

- git CLI 遵守用户的 git 配置、凭据助手、hook 和签名设置。
- 现有 `src/vcs.rs` 已使用 git CLI，行为保持一致。
- Rebased 和 git4idea 同样调用 git CLI。

读路径（status、log、blame、diff）是否改用 gix，由 #129 的性能基线决定。

所有命令设置 `GIT_OPTIONAL_LOCKS=0`、`GIT_TERMINAL_PROMPT=0`、`GCM_INTERACTIVE=Never` 和 `LC_ALL=C`。
Windows 使用 `CREATE_NO_WINDOW`，不弹出控制台窗口。

## 预览层

### 数据

`git_path_summary(path)` 最多运行四个 git 进程：

1. `rev-parse --show-toplevel --absolute-git-dir`：确认路径在工作区内。失败时返回 `None`，不再运行其他命令。
2. `status --porcelain=v2 --branch`：分支、上游、领先和落后计数、暂存与工作区状态、目录下的变更计数。
3. `log -1`：最近一次修改该路径的提交。
4. `diff HEAD --numstat`：相对 HEAD 的增删行数。HEAD 不存在时跳过。

进行中的操作（变基、合并、拣选、还原提交、二分查找）通过 `.git` 目录中的标记文件判断，不运行 git 进程。

`git_file_diff(path)` 返回相对 HEAD 的统一差异。HEAD 不存在时返回暂存区差异。
差异超过 256 KiB 时返回错误，超过 400 行时截断。

### 加载顺序

预览内容先加载，Git 区块后加载。

- Windows：`start_column_preview` 启动内容加载后，再调用 `start_column_git_preview`。两者共用 `preview_generation`，切换选择时旧结果被丢弃。
- macOS：`KiteGitPreviewSection` 使用 `.task(id: entry.path)`，切换选择时 SwiftUI 取消旧任务。

### 跳过条件

所选项目没有 Git 状态，且所在分栏不是 Git 工作区时，不启动 git 进程。
干净的已跟踪文件仍然显示 Git 区块，因为分栏的 `git_available` 为真。

### 界面

两端显示相同字段：

- 分支或分离 HEAD 的短哈希，领先和落后计数。
- 进行中的操作。
- 文件：暂存状态、工作区状态、增删行数。目录：变更计数。
- 最近提交：短哈希、作者、时间、标题。
- 快捷操作：查看差异、暂存、取消暂存、历史。

状态颜色沿用分栏徽标的配色：冲突、删除为红色；新增、重命名为绿色；修改为橙色；未跟踪为青色。

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

## 未覆盖

- hunk 级和行级暂存：#119。
- 并排差异和语法高亮：#121。
- 提交图和完整日志：#122。
- blame：#128。
- 预览打开时延的前后对比：#129。
