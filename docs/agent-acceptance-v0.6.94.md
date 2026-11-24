# Kite Agent v0.6.94 验收记录

## 状态

该记录用于 #157 的双平台产品化验收。
`issues/157-ui` 已集成 #150-#156 的 UI 与 Core 表面。
Core、fixture、Windows 编译与性能基线已可复现；真实 Agent、双平台截图和键盘验收仍需在 Windows 与 Apple Silicon macOS 实机完成。

## 当前自动化证据

- `cargo test --locked --lib --features windows-ui agent::`：27 项通过。
- `cargo test --locked --test agent_fixture`：7 项通过，覆盖正常流、权限、取消、超时、超大输出、失败和进程树清理。
- `cargo test --locked --test agent_fixture_protocol`：6 项通过，覆盖 ACP 帧、Markdown、权限、取消、重试和脱敏输入。
- `cargo check --locked --features windows-ui --bin kite-explorer`：通过。
- `bash .github/scripts/check-feature-matrix.sh`：通过。
- Windows x64 性能基线：`docs/performance/results/v0.6.94-agent-windows-x64.json`。

macOS 构建和真实 Agent 验收不能在当前 Windows 主机上替代，必须使用 Apple Silicon 实机或 macOS CI 产生证据。

## 真实 Agent 闭环

| 检查 | Windows | macOS |
|---|---|---|
| 安装并认证 Claude Code ACP | 待实机执行 | 待实机执行 |
| 安装并认证 Codex ACP | 待实机执行 | 待实机执行 |
| 设置中测试连接 | 待实机执行 | 待实机执行 |
| 右键文件发起 Agent | 待实机执行 | 待实机执行 |
| 流式回答 | 待实机执行 | 待实机执行 |
| 停止任务 | 待实机执行 | 待实机执行 |
| 复制回答 | 待实机执行 | 待实机执行 |
| 关闭后无残留子进程 | 待实机执行 | 待实机执行 |

验收使用真实 Agent，不使用 fixture 替代产品闭环。
fixture 只用于自动化边界测试。

## 截图矩阵

截图必须使用只读合成目录和脱敏配置。
文件名固定如下：

| 状态 | Windows | macOS |
|---|---|---|
| 未配置空状态 | `windows-agent-empty.png` | `macos-agent-empty.png` |
| 已连接空闲 | `windows-agent-ready.png` | `macos-agent-ready.png` |
| 流式回答 | `windows-agent-streaming.png` | `macos-agent-streaming.png` |
| 权限确认 | `windows-agent-permission.png` | `macos-agent-permission.png` |
| 错误与恢复 | `windows-agent-error.png` | `macos-agent-error.png` |
| 会话路径修复 | `windows-agent-repair.png` | `macos-agent-repair.png` |
| 诊断页 | `windows-agent-diagnostics.png` | `macos-agent-diagnostics.png` |
| 窄窗口 Agent 栏 | `windows-agent-narrow.png` | `macos-agent-narrow.png` |

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
