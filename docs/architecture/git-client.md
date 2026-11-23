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

## 未覆盖

- 并排差异和语法高亮：#121。
- 提交图和完整日志：#122。
- blame：#128。
- 预览打开时延的前后对比：#129。

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
