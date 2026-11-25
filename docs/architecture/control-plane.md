# Kite Headless Control Plane

## 结论

Kite 的高层能力应先进入一个无 UI 的 Control Plane，再由 GUI、CLI 和 MCP 作为 adapter 调用。

```text
Windows GPUI ─┐
macOS SwiftUI ─┼─> ControlPlane tool registry ─> Core services
kite-cli ──────┤                                  Agent / files / state
kite-mcp ──────┘
```

这条路径保证同一项能力不再分别通过点击、手工脚本和 fixture 验证。调试 Agent 时可以直接读取状态、发送提示、轮询事件、处理权限和关闭进程，不需要启动或操作文件管理器窗口。

## 当前实现

共享入口位于 `src/control.rs`：

- `ControlPlane::execute(tool, arguments)` 是唯一命令分发器。
- `tool_specs()` 是 MCP 和 CLI 共用的 JSON Schema 注册表。
- 参数校验、错误码、Agent session 生命周期和结果 JSON 都由 Control Plane 负责。
- `kite-cli` 与 `kite-mcp` 不复制业务逻辑。

当前注册的工具：

| 工具 | 用途 |
|---|---|
| `agent_profiles_list` | 读取 Agent profile 与默认 profile |
| `agent_connection_test` | 执行 ACP initialize 连接测试 |
| `agent_session_start` | 启动长期存活的 ACP session |
| `agent_session_prompt` | 等待 ready 后发送提示 |
| `agent_session_status` | 读取运行时快照 |
| `agent_session_events` | 消费增量事件 |
| `agent_session_wait` | 等待完成并返回剩余事件 |
| `agent_session_cancel` | 请求取消 |
| `agent_session_permission` | 结构化回复权限请求 |
| `agent_session_close` | 关闭并清理完整进程树 |
| `agent_run` | 一次完成启动、提示、等待和关闭 |
| `agent_diagnostics` | 读取脱敏诊断与日志 |
| `files_list` | 读取目录 |
| `files_stat` | 读取路径元数据 |
| `files_read_text` | 有上限地读取 UTF-8 文本 |
| `app_state` | 读取 Tab 与 UI preferences 数据 |

这个列表是当前可验收范围，不代表整个文件管理器已经全部迁移。后续新增 UI 功能时，默认先在 Control Plane 注册数据和操作，再由 CLI/MCP 自动暴露。

## CLI

构建：

```bash
cargo build --locked --bin kite-cli --bin kite-mcp
```

查看工具与直接调用：

```bash
kite-cli list-tools
kite-cli files stat Cargo.toml
kite-cli call agent_connection_test --args '{"profile_id":"claude-acp-local"}'
```

一次真实 Agent 调试：

```bash
kite-cli agent run \
  --profile claude-acp-local \
  --cwd C:/KiteAgentAcceptance/workspace \
  --context C:/KiteAgentAcceptance/workspace/hello.md \
  --prompt 'Summarize hello.md. Do not modify files.' \
  --timeout-ms 120000
```

长期 session 必须在一个长生命周期进程中调用。`kite-cli batch` 从 stdin 逐行读取：

```json
{"tool":"agent_session_start","arguments":{"profile_id":"fixture","working_directory":"C:/work"}}
{"tool":"agent_session_prompt","arguments":{"session_id":"...","prompt":"inspect README"}}
{"tool":"agent_session_wait","arguments":{"session_id":"...","timeout_ms":120000}}
{"tool":"agent_session_close","arguments":{"session_id":"..."}}
```

每行输出一个 `{ok,result|error}` 对象。MCP server 和 batch 进程都会在退出时关闭其中的 Agent session。

## MCP

`kite-mcp` 使用 JSON-RPC over stdio，并实现：

- `initialize`
- `notifications/initialized`
- `ping`
- `tools/list`
- `tools/call`

示例 MCP 配置：

```json
{
  "mcpServers": {
    "kite": {
      "command": "D:/path/to/kite-mcp.exe",
      "env": {
        "KITE_AGENT_PROFILE_PATH": "C:/Users/me/AppData/Roaming/Kite/agent.json"
      }
    }
  }
}
```

`tools/call` 同时返回 MCP `content` 文本和 `structuredContent`。工具业务错误以 `isError: true` 返回，协议方法错误使用 JSON-RPC error。

## 不变量

- 工具名称、输入 Schema、错误码和结果形状只在一个注册表定义。
- 密钥只能来自 profile environment 或系统凭据存储，不能出现在工具的 profile 输出中。
- 文件读取有字节上限；超限必须明确报错或显式截断。
- 会修改系统状态的工具必须标记 `destructiveHint`，并在描述中说明影响。
- `agent_session_prompt` 必须先等待 `ready`，避免把提示发给 `initializing` 状态。
- `agent_session_close` 和 `agent_run` 必须终止 Agent 完整子进程树。
- UI 不是验收前提；同一能力必须能通过 CLI 或 MCP 获取结构化结果。

## UI 迁移规则

1. 新能力先在 Core 形成可测试的数据操作。
2. 在 `tool_specs()` 注册命令和 Schema。
3. Control Plane 实现命令，CLI/MCP 自动获得入口。
4. UI adapter 调用同一命令，或调用 Control Plane 暴露的异步事件适配层。
5. UI 只负责展示、焦点、动画和平台交互，不再拥有 Agent 会话状态机。

当前 Agent 面板仍保留现有 GPUI 事件循环；本切片先建立可独立验收的 headless 路径，后续再把面板状态迁移到共享 Control Plane。

## 验收

```bash
cargo test --locked --test control_plane_mcp
cargo test --locked --test control_plane_agent
cargo clippy --locked --bins -- -D warnings
kite-cli list-tools
```

真实 Agent 验收使用真实 ACP 进程；fixture 只用于自动化和协议边界测试。
