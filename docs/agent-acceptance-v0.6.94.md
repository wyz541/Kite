# Kite Agent v0.6.94 验收记录

## 状态

该记录用于 #157 的双平台产品化验收。
Core、CLI、MCP、fixture、Windows 编译与性能基线已可复现。
2026-11-25 已在 Windows 上使用真实 Claude Code ACP 完成 UI、权限拒绝和取消闭环；macOS 实机、Codex ACP、自动跟随和完整键盘焦点仍需继续验收。

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

- `cargo test --locked --lib`：346 项通过，12 项按设计忽略。
- `cargo test --locked --lib agent`：73 项通过，3 项按设计忽略，覆盖配置、预设、会话、运行时、诊断和 #156 结果审核。
- `cargo test --locked --features windows-ui --bin kite-explorer`：98 项通过，4 项按设计忽略。
- `cargo test --locked --test agent_fixture`：7 项通过，覆盖正常流、权限、取消、超时、超大输出、失败和进程树清理。
- `cargo test --locked --test agent_fixture_protocol`：6 项通过，覆盖 ACP 帧、Markdown、权限、取消、重试和脱敏输入。
- `cargo check --locked --features windows-ui --bin kite-explorer`：通过。
- `cargo clippy --locked --all-targets --features windows-ui -- -D warnings`：通过。
- `bash .github/scripts/check-feature-matrix.sh`：通过。
- `python .github/scripts/check-privacy.py`：通过。
- Windows x64 性能基线：`docs/performance/results/v0.6.94-agent-windows-x64.json`。

macOS 构建和真实 Agent 验收不能在当前 Windows 主机上替代，必须使用 Apple Silicon 实机或 macOS CI 产生证据。

## 真实 Agent 闭环

| 检查 | Windows | macOS |
|---|---|---|
| 安装并认证 Claude Code ACP | 已通过（真实 ACP） | 待实机执行 |
| 安装并认证 Codex ACP | 待实机执行 | 待实机执行 |
| 设置中测试连接 | 已执行；ACP 初始化成功，Agent 声明需原生认证 | 待实机执行 |
| 右键文件发起 Agent | 已通过 | 待实机执行 |
| 流式回答 | 已通过 | 待实机执行 |
| 停止任务 | 已通过；子进程退出 | 待实机执行 |
| 权限确认与拒绝 | 已通过（真实 Claude ACP；拒绝后未写文件） | 待实机执行 |
| 重试失败或取消的任务 | 已通过（取消后重试） | 待实机执行 |
| 自动跟随与手动滚动暂停 | 待实机执行 | 待实机执行 |
| 复制回答 | 已通过（`FINAL-OK`） | 待实机执行 |
| 关闭后无残留子进程 | 已通过 | 待实机执行 |
| 独立窗口关闭/重开并保持会话 | 已通过 | 待实机执行 |

验收使用真实 Agent，不使用 fixture 替代产品闭环。
fixture 只用于自动化边界测试。

## 截图矩阵

截图必须使用只读合成目录和脱敏配置。
文件名固定如下：

| 状态 | Windows | macOS |
|---|---|---|
| 未配置空状态 | [windows-agent-empty.png](images/agent-acceptance-v0.6.94/windows-agent-empty.png) | `macos-agent-empty.png`（待执行） |
| 已连接空闲 | [windows-agent-ready.png](images/agent-acceptance-v0.6.94/windows-agent-ready.png) | `macos-agent-ready.png`（待执行） |
| 流式回答 | [windows-agent-streaming.png](images/agent-acceptance-v0.6.94/windows-agent-streaming.png) | `macos-agent-streaming.png`（待执行） |
| 权限确认 | [windows-agent-permission.png](images/agent-acceptance-v0.6.94/windows-agent-permission.png) | `macos-agent-permission.png`（待执行） |
| 错误与恢复 | [windows-agent-error.png](images/agent-acceptance-v0.6.94/windows-agent-error.png) | `macos-agent-error.png`（待执行） |
| 会话路径修复 | [windows-agent-repair.png](images/agent-acceptance-v0.6.94/windows-agent-repair.png) | `macos-agent-repair.png`（待执行） |
| 诊断页 | [windows-agent-diagnostics.png](images/agent-acceptance-v0.6.94/windows-agent-diagnostics.png) | `macos-agent-diagnostics.png`（待执行） |
| 窄窗口 Agent 栏 | [windows-agent-narrow.png](images/agent-acceptance-v0.6.94/windows-agent-narrow.png) | `macos-agent-narrow.png`（待执行） |

Windows 额外证据：
[完成态](images/agent-acceptance-v0.6.94/windows-agent-completed.png)、
[取消态](images/agent-acceptance-v0.6.94/windows-agent-cancelled.png)、
[独立窗口](images/agent-acceptance-v0.6.94/windows-agent-popout.png)、
[Agent 设置](images/agent-acceptance-v0.6.94/windows-agent-settings.png)、
[连接测试](images/agent-acceptance-v0.6.94/windows-agent-connection-test.png)、
[权限拒绝后](images/agent-acceptance-v0.6.94/windows-agent-permission-rejected.png)。

`windows-agent-error.png` 与 `windows-agent-repair.png` 使用故意损坏的合成会话验证恢复和移除入口，不代表真实 Agent 协议失败。
`windows-agent-permission.png` 来自真实 Claude Code ACP；拒绝后 `permission-test.txt` 未创建。
截图不得包含密钥、真实用户名、真实项目路径或提示正文。

## 键盘验收

两端都必须验证：

- 从右键菜单打开 Agent。
- Agent 输入框可聚焦。
- `Enter` 发送，`Shift+Enter` 换行。
- `Escape` 关闭菜单或确认层，不清空正在运行的任务。
- 停止按钮可通过键盘聚焦和执行。
- 权限请求的操作顺序固定，并有可见焦点。
- 上下文 chip 可移除和定位。
- 关闭独立窗口后，如果策略保留任务，重新打开仍能看到同一会话。

## 平台对等

Windows 与 macOS 必须显示相同的 Agent、模型、工作目录、权限和连接状态语义。
平台可以不同：

- 字体、间距和材质。
- 菜单渲染器。
- Keychain/系统凭据入口。
- 进程树终止实现。

平台不可以不同：

- 预设元数据。
- 错误分类。
- 权限决策结果。
- 脱敏规则。
- 会话恢复缺失路径处理。

## 自动化证据

- `cargo test --locked --lib agent::`
- `cargo test --locked --test agent_fixture_protocol`
- `cargo run --release --locked --bin kite-agent-perf -- --scenario long --runs 3`
- `bash .github/scripts/check-feature-matrix.sh`
- Windows 与 macOS 的 platform parity 工作流。

性能结果和平台命令见 `docs/performance/agent-v0.6.94.md`。
