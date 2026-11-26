# Kite Agent v0.6.94 验收记录

## 状态

该记录用于 #157 的双平台产品化验收。**#157 保持 open，直至下列剩余项全部完成。**

Core、CLI、MCP、fixture、Windows 编译与性能基线已可复现。2026-11-26 已在 macOS Apple Silicon 上使用真实 Claude Code ACP 与真实 Codex ACP 完成主要 UI 闭环；Codex 通过 CC Switch 的现有 DeepSeek 配置运行，Kite 未复制或保存该提供方的 token。

仍需完成：

- Windows 上的 Codex ACP 实机闭环。
- Windows 自动跟随/手动滚动暂停验证。
- Windows 与 macOS 的完整键盘焦点序列，重点是权限按钮和上下文 chip。
- 上述项完成后更新本记录并关闭 #157。

## macOS 实机验收（2026-11-26）

环境：macOS Apple Silicon、Kite v0.6.96、合成工作目录 `/tmp/kite-agent-acceptance`。

- Claude Code ACP：`claude-code-acp 0.16.2`，使用已登录的本机 Claude CLI。
- Codex ACP：`codex-acp 0.16.0`，复用 CC Switch 当前 Codex 配置，模型 `deepseek-flash`、provider `custom`。
- Kite 配置只保存可执行文件、工作目录策略和权限策略；未把 CC Switch token 写入 `agent.json`。

已通过：

- 从文件右键菜单打开真实 Agent；工作目录、预设、模型和权限语义正确。
- Codex/DeepSeek 真实流式回答完成，最终回答为 `DEEPSEEK-OK` 与 `DEEPSEEK-OK-2`。
- Claude Code 真实回答完成，最终回答为 `FINAL-OK`；复制后 `pbpaste` 内容与回答完全一致。
- 长输出运行中显示“停止”；点击停止或按 `Command+.` 后状态为“Agent 已取消”。
- 取消后可重试；失败任务也可重试，重试完成后回答可见。
- 流式输出渲染、自动跟随、手动滚动后暂停以及“回到最新”恢复自动跟随。
- 真实 Claude Code 对外部目录写入发起权限请求；UI 展示 `Allow / Always Allow / Reject / 始终拒绝 / 取消任务`。拒绝后目标文件未创建，修复后显示结构化“权限请求已拒绝”；允许重试后文件内容正确。
- 关闭独立 Agent 窗口后再次打开，会话与上下文保持一致。
- 故意损坏的合成会话在恢复时显示失效路径和修复入口，不阻塞应用启动；上下文 chip 可移除。
- 窄窗口 `900 × 720` 下 Agent 栏可用，输入区、回答区和操作按钮不重叠。
- Agent 设置、连接测试和诊断页可用；诊断轮次、能力、命令和最近错误可见。
- 诊断日志路径使用 `~/Library/...` 展示，不在截图中暴露真实用户名。
- 修复 Codex ACP 连接测试因孙进程持有管道而卡在“测试中…”的问题；测试改为回收整棵进程树。
- 修复 macOS 轮询 Ready 后未发送待处理提示词的问题。
- 修复完成/取消/失败后仍显示“Agent 运行中”的状态残留。
- 完成、取消和失败后均检查 `claude-code-acp` / `codex-acp`，未残留 Agent 子进程。

键盘已通过：

- 右键菜单可用 `Escape` 关闭。
- Agent 输入框可聚焦，`Enter` 发送，`Shift+Enter` 保留换行。
- `Command+.` 可停止正在运行的 Agent。
- 设置页 Tab 顺序覆盖“通用、外观、布局、文件处理、Agent、更新、关于”，并可继续进入 Agent 配置按钮。
- 权限请求的呈现顺序固定为允许一次、始终允许、拒绝一次、始终拒绝、取消任务。

仍未通过的键盘项：

- 从 Agent 输入框完整 Tab/Shift+Tab 到权限按钮与上下文 chip 的环形焦点序列。
- 上下文 chip 的键盘移除与定位动作。
- Windows 全键盘序列。

## Windows 实机验收（2026-11-25）

环境：Windows x64、Kite v0.6.94、真实 Claude Code ACP `0.16.2`、只读合成工作目录 `C:\KiteAgentAcceptance\final\workspace`。

已通过：

- 从右键菜单打开文件 Agent 上下文。
- 流式回答与完成态；最终回答为 `FINAL-OK`。
- 复制回答后剪贴板内容为 `FINAL-OK`。
- 停止长回答后 UI 显示“Agent 已取消”，ACP 子进程退出。
- 取消后可重试；重试任务再次进入运行并可停止。
- 关闭独立 Agent 窗口后重新打开，会话与上下文保持一致。
- 窄窗口、Agent 设置、诊断页与连接初始化。
- 故意损坏的合成会话可在恢复时显示失效路径，不阻塞启动；移除入口可清理失效上下文。
- 真实 Claude Code ACP 请求写入 `permission-test.txt`；UI 展示允许/拒绝/取消入口，选择拒绝后任务结束且文件未创建。
- 修复内层 popout 的 double-lease 崩溃：窗口创建延后到 AgentPanel 当前 update 结束之后。
- 修复 session 启动后未发送 `AgentSessionPrompt` 的问题：轮询到 Ready 后再提交提示词，不阻塞 UI。
- 修复终态提示残留：完成、取消、失败和关闭都有明确状态文案。

仍未完成的 Windows 验收：Codex ACP、自动跟随/手动滚动暂停、完整键盘焦点序列。上述项目不得记为通过。

## 当前自动化证据

- `cargo test --locked --lib agent -- --test-threads=1`：通过。
- `cargo test --locked --test agent_fixture`：通过，覆盖正常流、权限、拒绝错误分类、取消、超时、超大输出、失败、重试和进程树清理。
- `cargo test --locked --test agent_fixture_protocol`：7 项通过，覆盖 ACP 帧、Markdown、权限、取消、重试、连接探针子进程回收和脱敏输入。
- `cargo test --locked --test agent_redaction`：2 项通过，覆盖短 credential 拒绝、跨 chunk secret、终态 flush、持久化与日志脱敏。
- `cargo clippy --locked --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- `bash apps/macos/build.sh`：通过。
- `bash .github/scripts/check-feature-matrix.sh`：通过。
- `python .github/scripts/check-privacy.py`：通过。
- Windows x64 性能基线：`docs/performance/results/v0.6.94-agent-windows-x64.json`。

## 真实 Agent 闭环

| 检查 | Windows | macOS |
|---|---|---|
| 安装并认证 Claude Code ACP | 已通过（真实 ACP） | 已通过（真实 ACP） |
| 安装并认证 Codex ACP | 待实机执行 | 已通过（真实 ACP；CC Switch DeepSeek） |
| 设置中测试连接 | 已执行；ACP 初始化成功 | 已通过；228 ms 内完成且无残留进程 |
| 右键文件发起 Agent | 已通过 | 已通过 |
| 流式回答 | 已通过 | 已通过 |
| 停止任务 | 已通过；子进程退出 | 已通过；鼠标停止与 `Command+.` |
| 权限确认与拒绝 | 已通过（拒绝后未写文件） | 已通过（拒绝后未写文件，允许重试成功） |
| 重试失败或取消的任务 | 已通过 | 已通过 |
| 自动跟随与手动滚动暂停 | 待实机执行 | 已通过 |
| 复制回答 | 已通过 | 已通过（`pbpaste` 校验） |
| 关闭后无残留子进程 | 已通过 | 已通过 |
| 独立窗口关闭/重开并保持会话 | 已通过 | 已通过 |

验收使用真实 Agent，不使用 fixture 替代产品闭环。fixture 只用于自动化边界测试。

## 截图矩阵

截图必须使用合成目录和脱敏配置。

| 状态 | Windows | macOS |
|---|---|---|
| 未配置空状态 | [windows-agent-empty.png](images/agent-acceptance-v0.6.94/windows-agent-empty.png) | [macos-agent-empty.png](images/agent-acceptance-v0.6.94/macos-agent-empty.png)、[未配置菜单](images/agent-acceptance-v0.6.94/macos-agent-unconfigured.png) |
| 已连接空闲 | [windows-agent-ready.png](images/agent-acceptance-v0.6.94/windows-agent-ready.png) | [macos-agent-ready.png](images/agent-acceptance-v0.6.94/macos-agent-ready.png) |
| 流式回答 | [windows-agent-streaming.png](images/agent-acceptance-v0.6.94/windows-agent-streaming.png) | [macos-agent-streaming.png](images/agent-acceptance-v0.6.94/macos-agent-streaming.png) |
| 权限确认 | [windows-agent-permission.png](images/agent-acceptance-v0.6.94/windows-agent-permission.png) | [macos-agent-permission.png](images/agent-acceptance-v0.6.94/macos-agent-permission.png) |
| 错误与恢复 | [windows-agent-error.png](images/agent-acceptance-v0.6.94/windows-agent-error.png) | [macos-agent-error.png](images/agent-acceptance-v0.6.94/macos-agent-error.png) |
| 会话路径修复 | [windows-agent-repair.png](images/agent-acceptance-v0.6.94/windows-agent-repair.png) | [macos-agent-repair.png](images/agent-acceptance-v0.6.94/macos-agent-repair.png) |
| 诊断页 | [windows-agent-diagnostics.png](images/agent-acceptance-v0.6.94/windows-agent-diagnostics.png) | [macos-agent-diagnostics.png](images/agent-acceptance-v0.6.94/macos-agent-diagnostics.png) |
| 窄窗口 Agent 栏 | [windows-agent-narrow.png](images/agent-acceptance-v0.6.94/windows-agent-narrow.png) | [macos-agent-narrow.png](images/agent-acceptance-v0.6.94/macos-agent-narrow.png) |

macOS 额外证据：
[完成态（Codex/DeepSeek）](images/agent-acceptance-v0.6.94/macos-agent-codex-completed.png)、
[取消态](images/agent-acceptance-v0.6.94/macos-agent-cancelled.png)、
[独立窗口](images/agent-acceptance-v0.6.94/macos-agent-popout.png)、
[Agent 设置](images/agent-acceptance-v0.6.94/macos-agent-settings.png)、
[连接测试](images/agent-acceptance-v0.6.94/macos-agent-connection-test.png)。

Windows 额外证据：
[完成态](images/agent-acceptance-v0.6.94/windows-agent-completed.png)、
[取消态](images/agent-acceptance-v0.6.94/windows-agent-cancelled.png)、
[独立窗口](images/agent-acceptance-v0.6.94/windows-agent-popout.png)、
[Agent 设置](images/agent-acceptance-v0.6.94/windows-agent-settings.png)、
[连接测试](images/agent-acceptance-v0.6.94/windows-agent-connection-test.png)、
[权限拒绝后](images/agent-acceptance-v0.6.94/windows-agent-permission-rejected.png)。

`windows-agent-error.png`、`windows-agent-repair.png`、`macos-agent-error.png` 与 `macos-agent-repair.png` 使用故意损坏的合成会话或合成超时验证恢复，不代表真实 Agent 协议故障。
Windows 与 macOS 权限截图均来自真实 Claude Code ACP；拒绝后目标文件未创建。

截图不得包含密钥、真实用户名、真实项目路径或提示正文。

## 平台对等

Windows 与 macOS 必须显示相同的 Agent、模型、工作目录、权限和连接状态语义。
平台可以不同：

- 字体、间距和材质。
- 菜单渲染器。
- Keychain/系统凭据入口。
- 进程树终止实现。
- 快捷键显示方式。

平台不可以不同：

- 预设元数据。
- 错误分类。
- 权限决策结果。
- 脱敏规则。
- 会话恢复缺失路径处理。

## 已知限制

- Codex ACP 会声明 ChatGPT / API key 认证方法；连接测试因此显示“需要认证”。但 CC Switch 自定义 provider 的 profile 可继续完成真实会话，Kite 只做诊断展示，不改变 Agent 原生认证。
- Claude Code ACP 在用户拒绝写入权限时可能返回上游 `Internal error`；Kite 现在将该终态归一化为“权限请求已拒绝”，原始细节仍保留在脱敏诊断日志。
- 完整键盘焦点序列尚需继续修复并复验；#157 不得提前关闭。
