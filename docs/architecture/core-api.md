# Kite Core API 与多端外壳

## 结论

Kite 的业务规则、状态机和副作用必须收敛到 UI 无关的 Core API。GUI、CLI 和 MCP 只负责输入适配、调用编排、输出渲染和协议错误映射。

```text
业务核心
  Agent / Files / Search / Git / Settings / ...
              │
              ▼
Core API
  ExecutionContext / typed errors / idempotency / state changes
              │
     ┌────────┼────────┐
     ▼        ▼        ▼
  UI shell  kite-cli  kite-mcp
  GPUI/Swift CLI      JSON-RPC
```

任何核心状态变更都必须经过 Core API。UI、CLI 和 MCP 不得绕过分发层直接推进 Agent session、写业务文件或维护第二套状态机。

## 分层

### 1. 业务核心

现有领域模块是业务核心：Agent runtime、profile store、session restore、文件读取、搜索、Git、设置等。

职责：

- 执行具体业务规则、状态机、持久化和外部依赖调用。
- 不包含 CLI 参数解析、MCP 消息、GPUI/SwiftUI 视图状态。
- 出错时返回可分类的领域错误，不渲染终端或界面文案。

### 2. Core API

入口：`CoreApi::invoke(tool, arguments, ExecutionContext)`。

职责：

- 校验工具是否存在、执行上下文是否允许调用。
- 在只读模式下拒绝一切状态变更工具。
- 对标记为 destructive 的工具要求显式确认。
- 合并任务 deadline，限制长耗时调用。
- 识别 idempotency key，重复请求返回原结果而不重复副作用。
- 调用业务核心，生成统一 `CoreResult`。
- 将领域错误映射为 `CoreErrorKind`、稳定错误码、恢复建议和可重试标记。

兼容入口 `CoreApi::execute()` 仅用于受信任的进程内调用，会自动设置 `confirmed=true`。

UI 和同进程客户端优先使用强类型入口：

```rust
let result = core_api.invoke_request(
    CoreRequest::AgentSessionPrompt {
        session_id,
        prompt,
        context_paths: None,
        wait_ready_ms: None,
    },
    context,
)?;
```

### 3. CLI shell

入口：`kite-cli`。

职责：

- 解析子命令、长/短选项、位置参数和全局选项。
- 读取环境变量、配置文件与当前目录。
- 合并配置优先级并构造 `ExecutionContext`。
- 调用 Core API；不直接调用 AgentRuntime 或修改业务状态。
- 正常结果写 stdout，错误和诊断写 stderr。
- 将错误类型映射为稳定退出码：

| 错误类型 | 退出码 |
|---|---:|
| 成功 | 0 |
| Internal | 1 |
| InvalidInput | 2 |
| Unauthorized / Forbidden | 3 |
| NotFound | 4 |
| Conflict / PreconditionFailed | 5 |
| External | 6 |
| Timeout | 7 |
| Cancelled | 130 |

`kite-cli batch` 在同一进程中保持 Core API 与 Agent session 状态，适合非交互脚本。CLI 不跨进程保存隐式 session。

### 4. MCP shell

入口：`kite-mcp`。

职责：

- 暴露 Tools、Resources、Prompts。
- 将 MCP 参数与 `_meta` 转换为 `ExecutionContext`。
- Tools：可执行动作与查询能力。
- Resources：只读的 profile、诊断和 app state 快照。
- Prompts：标准化多步 Agent 调试与验收入口。
- 返回 `content` 和 `structuredContent`；工具业务错误使用 `isError: true`。
- 对超大文本裁剪或分页，不倾倒原始长堆栈。
- `--read-only` 是服务级强制模式，客户端不能通过 `_meta` 关闭。

当前 Resources：

- `kite://agent/profiles`
- `kite://agent/diagnostics`
- `kite://app/state`

当前 Prompts：

- `agent_debug`
- `agent_acceptance`

### 5. UI shell

Windows GPUI、macOS SwiftUI 和未来 Web/移动端只负责：

- 点击、键盘、表单、拖拽和焦点。
- 主题、布局、动画和无障碍。
- 加载态、空状态、错误提示和确认弹窗。
- 将用户操作转换为 Core API 请求，并展示 CoreResult。

UI 不得：

- 复制 Core API 的业务校验或状态机。
- 将长期业务状态只保存在组件内存。
- 通过直接写 store/runtime 绕过 Core API。
- 在渲染层硬编码领域规则分支。

当前 Windows Agent 面板已通过 `CoreClient` 与 `CoreRequest` 调用 Core API，组件只保留展示状态和交互状态；不再直接持有 `AgentRuntime`。下一阶段按同样方式迁移文件、Git、搜索和设置页面：

```rust
let result = core_client.invoke_request(request)?;
```

macOS 已通过 UniFFI/Core FFI 形成独立客户端边界。后续只允许扩展该边界，不再把业务逻辑复制进 SwiftUI。

## 执行上下文

```rust
pub struct ExecutionContext {
    pub actor: Option<String>,
    pub trace_id: String,
    pub deadline_unix_ms: Option<u64>,
    pub idempotency_key: Option<String>,
    pub read_only: bool,
    pub confirmed: bool,
}
```

- `actor`：调用方身份，供权限判断和审计使用。
- `trace_id`：每个请求必须带全链路追踪标识。
- `deadline_unix_ms`：Core API 将剩余时间传给 Agent wait/prompt。
- `idempotency_key`：同一 key 只能对应一个工具；重复调用返回原结果。
- `read_only`：服务级或请求级只读模式。
- `confirmed`：高危操作的显式确认。

环境变量默认值：

- `KITE_ACTOR`
- `KITE_TRACE_ID`
- `KITE_READ_ONLY`
- `KITE_CONFIRM`

命令行优先于环境变量，环境变量优先于内置默认值。

## 结果契约

```rust
pub struct CoreResult {
    pub trace_id: String,
    pub data: Value,
    pub state: Option<Value>,
    pub changed: bool,
    pub idempotent_replay: bool,
    pub recovery_point: Option<String>,
}
```

- `data`：领域结果，不包含 UI 文案。
- `state`：最新业务状态（如 Agent runtime snapshot）。
- `changed`：本次调用是否改变业务状态。
- `idempotent_replay`：是否来自幂等缓存。
- `recovery_point`：部分生效或长任务可恢复的位置，例如 Agent session id。

## 错误契约

```rust
pub struct CoreError {
    pub kind: CoreErrorKind,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub recovery: Option<String>,
    pub details: Option<Value>,
}
```

- 参数错误、业务前置条件、权限、外部依赖、超时和取消必须可区分。
- `recovery` 提供下一步动作，而不是原始堆栈。
- CLI 将 `kind` 映射为退出码。
- MCP 将错误放入 `structuredContent.error` 和 `isError: true`。
- UI 将 `kind/code` 映射为字段错误、确认框、重试入口或不可恢复提示。

## 已迁移的 Agent 闭环

当前 Core API 已注册：

- Profile：`agent_profiles_list`、`agent_connection_test`。
- Session：`agent_session_start`、`agent_session_prompt`、`agent_session_status`、`agent_session_events`、`agent_session_wait`、`agent_session_cancel`、`agent_session_permission`、`agent_session_close`。
- One-shot：`agent_run`。
- Diagnostics：`agent_diagnostics`。
- Data：`files_list`、`files_stat`、`files_read_text`、`app_state`。

所有入口使用同一个命令注册表和同一组 Schema。

## CLI 示例

```bash
# 只读查询
kite-cli --read-only files stat Cargo.toml

# 带追踪与幂等 key 的查询
kite-cli --trace-id debug-42 --idempotency-key read-cargo-1 \
  call files_stat --args '{"path":"Cargo.toml"}'

# 真实 Agent 一次闭环
kite-cli --trace-id agent-42 \
  agent run \
  --profile claude-acp-local \
  --cwd C:/KiteAgentAcceptance/workspace \
  --context C:/KiteAgentAcceptance/workspace/hello.md \
  --prompt "Summarize hello.md. Do not modify files." \
  --timeout-ms 120000

# 高危操作必须显式确认
kite-cli --confirm call agent_session_close \
  --args '{"session_id":"..."}'
```

JSONL batch：

```json
{"tool":"agent_session_start","arguments":{}}
{"tool":"agent_session_prompt","arguments":{"session_id":"...","prompt":"inspect README"}}
{"tool":"agent_session_wait","arguments":{"session_id":"...","timeout_ms":120000}}
{"tool":"agent_session_close","arguments":{"session_id":"..."},"context":{"confirmed":true}}
```

## MCP 示例

```json
{
  "mcpServers": {
    "kite": {
      "command": "D:/path/to/kite-mcp.exe",
      "args": ["--read-only"],
      "env": {
        "KITE_AGENT_PROFILE_PATH": "C:/Users/<name>/AppData/Roaming/Kite/agent.json"
      }
    }
  }
}
```

Agents 可通过 `tools/list`、`resources/list`、`prompts/list` 发现能力；高危工具调用需在 `_meta` 中提供：

```json
{
  "name": "agent_session_close",
  "arguments": { "session_id": "..." },
  "_meta": { "confirmed": true, "traceId": "agent-42" }
}
```

## 迁移规则

1. 新业务能力先进入领域模块和 Core API。
2. 在 `tool_specs()` 声明工具、Schema、只读与 destructive 属性。
3. 在 CLI/MCP 自动暴露，不写第二套业务逻辑。
4. UI shell 接入同一个 Core API；UI 仅保留展示状态。
5. 对长耗时能力提供状态查询、事件、取消和恢复点。
6. 对写操作提供幂等标识、确认语义和结构化错误。

## 当前边界

本切片已经把 Agent 调试闭环、Agent Windows UI 调用和只读数据读取迁移到 Core API。文件修改、Git、搜索、设置和窗口状态等能力仍通过现有领域模块存在，尚未全部注册为 Core API 工具或 UI 客户端。

后续新增跨端能力必须遵守本文件的 seam，不允许在 CLI、MCP 或 UI 中复制 Agent 状态机或文件业务规则。


## 验收

```bash
cargo test --locked --lib core_api::tests
cargo test --locked --test core_api_cli --test core_api_mcp --test core_api_agent
cargo test --locked --features windows-ui --bin kite-explorer
cargo clippy --locked --all-targets --features windows-ui -- -D warnings
cargo fmt --all -- --check
```
