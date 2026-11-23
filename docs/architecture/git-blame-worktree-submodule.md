# Blame、Worktree 与 Submodule

## Core 边界

`src/git/` 在共享 Git Service 之上暴露三项能力：

- `git_blame(path, start_line, line_count, ignore_whitespace)`：按页调用
  `git blame --line-porcelain`。默认页面 400 行，单页上限 2,000 行。
- `git_worktrees(path)` / `create_git_worktree` / `worktree_removal_check` /
  `remove_git_worktree`：解析 `git worktree list --porcelain -z`，创建新分支或已有分支
  Worktree，并在删除前检查未提交和未跟踪文件。
- `git_submodules(path)` / `git_init_submodules` / `git_update_submodules`：解析
  `git submodule status --recursive`，读取 `.gitmodules` 中的路径和 URL，并检查已初始化
  Submodule 的脏状态。

所有 Git 子进程都通过 `GitService` / `GitExecutor` 执行，统一继承取消、进度、凭据桥、
结构化错误、控制台记录、环境加固和 Windows 无窗口启动。非仓库路径返回 `None` 或空结果。

## Blame 行为

- 默认忽略空白变化，使用 `git blame -w`。
- 不显式传入 `--ignore-revs-file`，因此用户配置的 `blame.ignoreRevsFile` 由 Git 自动应用。
- 未提交行使用“未提交”短哈希，并在两端用红色提示。
- 第一页只读取 400 行。Windows 预览和 macOS 编辑器在后台追加剩余页面。
- 点击行会打开提交历史并定位到该提交；预览详情同时显示作者、日期、哈希和提交标题，鼠标悬停显示完整摘要和邮箱。

## Worktree 安全规则

- 列表展示主 Worktree、当前 Worktree、锁定和可清理状态。
- 不能在 Kite 中删除主 Worktree 或当前 Worktree。
- 删除前必须调用 `worktree_removal_check`。
- 有未提交或未跟踪改动时，UI 必须显示改动摘要并要求用户确认放弃，随后才用
  `force = true` 调用删除。
- 新建 Worktree 支持两种模式：
  - 新分支：`git worktree add -b <branch> <path> [start-point]`
  - 已有分支：`git worktree add <path> <branch>`

## Submodule 行为

- 分栏中的 Submodule 条目显示独立状态徽标。
- 状态包括未初始化、已初始化、与索引不一致和冲突。
- 已初始化的 Submodule 会额外检查工作区是否有未提交改动。
- “初始化”写入 `.git/config`；“更新全部”执行
  `git submodule update --init --recursive`。

## FFI

`src/ffi.rs` 导出以下 UniFFI 方法：

- `git_blame`
- `git_worktrees`
- `git_add_worktree`
- `git_worktree_removal_check`
- `git_remove_worktree`
- `git_submodules`
- `git_init_submodules`
- `git_update_submodules`

Windows GPUI 和 macOS SwiftUI 只消费这些 DTO，不各自实现 Git 解析逻辑。
