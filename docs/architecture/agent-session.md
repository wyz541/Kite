# Agent 会话与 ACP 传输

- 状态：已实现 Core #151，并对接 #150 Profile 模型
- 范围：外部 Agent 进程生命周期、ACP/JSON-RPC over stdio、流式事件、权限请求、取消、超时、输出限制和 UniFFI 接口
- 不包含：Agent 设置页、右键菜单、Agent 栏、供应商预设、认证界面和通用 CLI 适配器

## 结论

Kite Core 拥有外部 Agent 会话，但仍在用户选择的外部进程中执行 Agent。

```text
Windows GPUI / macOS SwiftUI
        │ UniFFI
        ▼
AgentSession (Rust Core)
        │
        ├── AgentTransport trait
        │       └── AcpStdioTransport
        │
        ├── stdout/stderr reader threads
        ├── event dispatcher
        └── process tree guard
                ├── Windows Job Object
                └── POSIX process group
```

UI 不直接管理子进程、stdout/stderr、JSON-RPC 请求 ID 或进程组。UI 只订阅结构化事件，并调用 `start`、`send_prompt`、`respond_permission`、`cancel` 和 `shutdown`。

## 模块边界

```text
src/agent/
  profile.rs    #150 Profile 模型、校验和 AgentSessionConfig 桥接
  store.rs      #150 agent.json 持久化
  model.rs      UniFFI 记录、事件、错误和状态机类型
  config.rs     启动配置校验、环境变量白名单和超时
  events.rs     多订阅者事件分发
  process.rs    进程树句柄和平台清理
  transport.rs  AgentTransport 接口和暂未启用的 CLI 占位
  acp.rs        ACP/JSON-RPC over stdio 适配器
  session.rs    AgentSession 状态管理、后台工作线程和 UniFFI 对象
```

`AgentSession` 不依赖 GPUI、SwiftUI 或 AppKit。

## 状态机

```text
Idle -> Starting -> Ready -> Running -> Ready
                       │        │
                       │        └-> Cancelling -> Ready
                       │                          └-> Failed
                       └-> Failed

任意可关闭状态 -> Closed
```

- `Idle`：对象已创建，进程尚未启动。
- `Starting`：正在启动进程、完成握手并创建 ACP 会话。
- `Ready`：可以发送新的提示词。
- `Running`：当前提示词正在执行。
- `Cancelling`：用户已请求取消，等待 Agent 返回 `cancelled`。
- `Failed`：当前进程或协议不可继续使用。
- `Closed`：用户关闭窗口或显式释放会话。

`AgentSessionState::can_transition_to` 集中定义合法转换，避免平台 UI 自行猜测状态语义。

## ACP 流程

1. 启动已配置的可执行文件，工作目录固定为配置中的绝对路径。
2. 发送 `initialize`：
   - `protocolVersion = 1`
   - `clientInfo = Kite / <version>`
   - `clientCapabilities.fs.readTextFile = false`
   - `clientCapabilities.fs.writeTextFile = false`
   - `clientCapabilities.terminal = false`
3. 校验 Agent 返回的 `protocolVersion == 1`。
4. 读取 `agentCapabilities`，并向 UI 发送 `ProtocolReady`。
5. 发送 `session/new`，只传工作目录、附加目录和空 `mcpServers`。
6. 保存 ACP `sessionId`，发送 `SessionCreated`。
7. 发送 `session/prompt`：
   - 文本使用 `text` content block。
   - 上下文路径使用 `resource_link` content block。
8. 将 `session/update` 转换为结构化事件：
   - `agent_message_chunk`
   - `agent_thought_chunk`
   - `tool_call` / `tool_call_update`
   - `plan`
9. `session/request_permission` 不自动接受，而是发送 `PermissionRequested` 事件。
10. UI 通过 `respond_permission(request_id, option_id)` 选择选项；`option_id = None` 表示取消。
11. 用户取消时发送 `session/cancel`，并把所有未决权限请求回复为 `cancelled`。
12. `session/prompt` 返回后发送 `TurnCompleted` 或 `TurnCancelled`。

## 超时

| 超时 | 默认值 | 行为 |
| --- | ---: | --- |
| 启动超时 | 10 秒 | 覆盖进程启动、`initialize` 和 `session/new` |
| 单轮总超时 | 5 分钟 | 从发送提示词开始计算 |
| 空闲超时 | 60 秒 | 没有协议输出或用户权限响应时取消 |
| 关闭宽限 | 2 秒 | 等待取消确认，超时后终止进程树 |

- `idle_timeout_ms = 0` 显式关闭空闲超时。
- 等待用户处理权限请求时不触发空闲超时。
- 取消过程中暂停空闲超时，但保留关闭宽限。
- 超时返回结构化 `AgentErrorKind::Timeout`，并清理进程。

## 进程生命周期

### Windows

- 使用 `CREATE_NO_WINDOW` 启动 Agent，避免弹出控制台窗口。
- 启动后立即把进程分配给 Job Object。
- Job Object 设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`。
- 取消或关闭时调用 `TerminateJobObject`，完整清理子进程树。
- 如果当前系统不允许嵌套 Job Object，保留 `taskkill /T /F` 作为回退路径。
- 不等待 stdout/stderr 读取线程结束；进程树终止后由管道关闭回收线程。

### macOS / Linux

- 子进程在独立进程组中启动。
- 取消时先向整个进程组发送 `SIGTERM`，再发送 `SIGKILL`。
- 父进程退出不会让 UI 线程等待管道读取。

### 共同规则

- 任务在后台线程执行，主 UI 线程不等待 Agent 输出。
- 关闭后不保留 Kite 能控制的子进程。
- 不承诺把任意 Agent 进程沙箱化。

## 环境变量与敏感信息

子进程不继承 Kite 的完整环境。

允许继承的最小集合：

- Windows：`PATH`、`PATHEXT`、`SystemRoot`、`WINDIR`、`COMSPEC`、临时目录、用户目录和 AppData。
- macOS / Linux：`PATH`、`HOME`、临时目录、语言、终端、用户和 `SSH_AUTH_SOCK`。

Profile 中显式配置的环境变量会覆盖白名单值。

`AgentSessionConfig.environment` 是运行时解析后的映射，不用于持久化。Profile 存储只应保存 `credential_ref`，由 #150/#152 在启动会话前从系统钥匙串解析为环境变量。

`AgentSessionConfig::from_profile` 负责把已校验的 `AgentProfile`、解析后的工作目录和凭据环境映射到运行时启动参数。

日志和错误详情会遮罩：

- 名称包含 token、secret、password、credential、authorization 或 private key 的环境变量值。
- URL 中的用户名和密码。
- Bearer token。
- JSON 或 `key=value` 形式的 token、password、secret、API key 和 authorization。

## 输出限制

- 每个 stdout/stderr 帧有独立上限，默认 256 KiB。
- 整个会话有总输出上限，默认 16 MiB。
- 单轮开始时重置总输出计数。
- 超过限制时：
  - 立即停止读取。
  - 终止进程树。
  - 发送 `AgentErrorKind::OutputLimit`。
- stdout 必须是有效的换行分隔 JSON-RPC。非法 JSON 返回 `AgentErrorKind::Protocol`。

## UniFFI 接口

`AgentSession` 是 UniFFI 对象：

```text
new(AgentSessionConfig) -> AgentSession
start()
send_prompt(text, context_paths)
respond_permission(request_id, option_id?)
cancel()
shutdown()
state()
pid()
subscribe(AgentEventCallback) -> AgentEventSubscription
```

事件包括：

- 状态变化
- 进程启动
- 协议握手完成
- ACP 会话创建
- 消息增量
- 思考增量
- 工具调用更新
- 计划更新
- 权限请求
- stderr 诊断
- 单轮完成 / 取消
- 进程退出
- 结构化错误

回调由 Rust 后台线程触发。平台 UI 必须把界面更新调度回自己的主线程。

## 测试夹具

`cargo test --features agent-fixture --test agent_session` 会构建脚本化 ACP Agent。

覆盖场景：

| 场景 | 验证点 |
| --- | --- |
| `normal` | 启动、握手、流式输出、完成 |
| `permission` | 权限请求不会被静默接受 |
| `cancel` | `session/cancel`、取消事件、会话复用 |
| `spawn-tree` | 取消后子进程树被清理 |
| `idle` | 空闲超时 |
| `slow-drip` | 单轮总超时 |
| `startup-hang` | 启动超时 |
| `oversized` | 单帧/总输出限制 |
| `error` | JSON-RPC 错误和敏感值遮罩 |
| `auth` | 认证错误分类 |
| `old-version` | 协议版本不兼容 |
| `malformed` | 非法 JSON-RPC 输出 |

Linux CI 验证 POSIX 进程组路径；Windows CI 验证 Job Object 和 `CREATE_NO_WINDOW` 路径。

## 当前限制

- 通用 CLI 适配器只有接口占位，不启动。
- 尚未提供 Agent 认证 UI；认证错误会传给上层设置/会话界面。
- 不实现 `session/load`、历史回放和跨应用重启恢复；这些属于 P1。
- 不实现 Agent 发起的文件系统或终端客户端方法；P0 初始化时明确声明不支持。
- 不支持模型推理后端或供应商专用协议。
