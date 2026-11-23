# Kite Git 客户端架构

## 结论

Kite 的 Git 能力分两层交付：

- 预览层：文件预览显示所选文件的 Git 状态、最近提交、增删行数和工作区差异。
- 客户端层：独立 Git 客户端窗口，能力参照 DetachHead/rebased。

本文件记录当前已实现的预览层和 Core 服务边界。
客户端层的拆分见 #115 的子 issue #116–#129。

## 跨平台边界

Git 逻辑只在 Rust Core 实现一次。

- Core：`src/git.rs` 读取单个路径的 Git 摘要、差异、分支、冲突和三方合并数据。`src/vcs.rs` 继续负责状态徽标与通用 VCS 操作。
- FFI：`src/ffi.rs` 通过 UniFFI 导出预览查询、合并、冲突解决、继续和中止操作。
- Windows：GPUI 预览与合并窗口直接调用 `kite_core::git`。
- macOS：SwiftUI 预览与合并窗口通过 `KiteModel` 调用 FFI。

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

## 合并与冲突解决

### 合并策略

`git_merge(path, spec)` 使用 git CLI 执行普通、`--no-ff`、`--ff-only` 或 `--squash` 合并。目标分支名作为独立参数传递，拒绝以 `-` 开头的输入；合并前用 `rev-parse --verify <target>^{commit}` 校验目标。

普通合并和保留合并提交可传入自定义提交说明。压缩合并只有在用户提供说明时才创建提交；说明留空时只暂存结果，避免静默提交。

### 冲突模型

`git_conflicts(path)` 通过 `git ls-files -u -z` 读取 stage 1/2/3，并返回：

- 双方修改、双方新增、自己删除、对方删除、重命名和其他冲突。
- 每个冲突的基线、自己、对方路径及是否存在。
- 二进制标记；检测到 NUL 或非 UTF-8 内容时不进入文本合并。
- 进行中的 merge、rebase、cherry-pick、revert 或 bisect 状态。
- 是否可以继续或中止。

重命名冲突由“stage 1 原路径 + stage 2 自己的新路径 + stage 3 对方的新路径”组成。选择一边会删除另外两个路径并暂存选中的路径；选择两边会暂存两份新文件并删除原路径。

### 三方合并

`git_three_way_merge(path, conflict)` 将 stage 2、stage 1、stage 3 写入临时文件，调用 `git merge-file -p --diff3`。Core 解析出的结果包括：

- 带冲突标记的完整合并文本。
- 按行标记的上下文、自己、基线、对方和标记行。
- 每个冲突块的两个版本和基线版本。

用户逐个冲突块选择自己、对方或两边后，`apply_conflict_choices` 生成最终文本。`git_resolve_conflict` 拒绝仍含完整冲突标记的文本，写回工作区后执行 `git add -A -- <path>`。删除型解决方式移除文件后同样暂存删除。

### 外部工具

`git_open_merge_tool` 只启动用户配置的 `merge.tool`，使用 `git mergetool --no-prompt --tool <tool>`。未配置该键时返回可操作错误，不回退到终端型编辑器。

### 界面

Windows `GitMergeWindow` 与 macOS `KiteGitMergeWindowView` 使用同一 Core 数据：

- 顶部选择目标分支、合并策略和可选提交说明。
- 左侧冲突文件列表；二进制冲突和重命名冲突使用整文件选择。
- 文本冲突显示自己、对方、合并结果三列，冲突行按来源着色，当前冲突块高亮。
- 可以逐块选择自己、对方或两边，也可以切换手动编辑。
- 解决后可继续原操作；中止操作会先要求确认。

## 未覆盖

- hunk 级和行级暂存：#119。
- 并排差异和语法高亮：#121。
- 提交图和完整日志：#122。
- blame：#128。
- 预览打开时延的前后对比：#129。
