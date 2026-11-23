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

## 客户端窗口框架

Git 客户端使用独立窗口。每个仓库最多打开一个窗口；再次打开同一仓库时激活已有窗口。

### 入口

- 分栏中的仓库目录或项目右键菜单：`Git > 打开 Git 客户端`。
- 主工具栏 Git 按钮；macOS 同时提供可点击的状态栏 Git 标记。
- 预览中的 Git 区块：`Git 客户端`。
- 应用菜单：`Git > 打开 Git 客户端…`。
- Windows 快捷键 `Ctrl+Shift+G`；macOS `Command+Shift+G`。

### 布局

- 左侧：变更分组和提交信息编辑区。
- 中间：提交 Log。
- 右侧：提交详情；提交文件清单由后续日志视图补充。
- 底部：可折叠 Git 控制台和后台任务面板。
- 顶部：当前分支、上游、领先 / 落后计数，以及 获取、拉取、推送、提交、贮藏。
- 状态横幅：rebase、merge、cherry-pick、revert；按适用性提供继续、跳过和中止。正在 bisect 时显示结束并重置。

### 窗口状态

`GitClientWindowSessionDto` 随主会话写入 `session.json`，按仓库根目录去重，记录：

- 窗口宽高。
- 变更栏和详情栏比例。
- 控制台高度和展开状态。
- 最后打开的视图。

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
| 变更 / 日志 / 详情 | Alt+1 / 2 / 3 | Option+1 / 2 / 3 |
| 上一个 / 下一个提交 | Alt+Up / Down | Option+Up / Down |
| 快捷键表 | F1 | F1 |
| 关闭提示或浮层 | Esc | Esc |

macOS 的 Git 窗口不使用 `Command+Shift+F` 获取，避免与主窗口全局搜索冲突。

## 未覆盖

- hunk 级和行级暂存：#119。
- 并排差异和语法高亮：#121。
- 提交图和完整日志：#122。
- blame：#128。
- 预览打开时延的前后对比：#129。
