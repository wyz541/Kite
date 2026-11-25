# Agent 配置模型

- 状态：已实现（#150）
- 范围：平台无关的 Agent Profile、持久化、校验和 UniFFI 边界
- 非范围：Agent 进程、ACP/JSON-RPC 会话、设置页和菜单

## 文件位置与格式

默认配置文件：

- Windows：`%APPDATA%\Kite\agent.json`
- macOS：`~/Library/Application Support/Kite/agent.json`

可通过 `KITE_AGENT_PROFILE_PATH` 覆盖路径。文件和目录由 Rust Core 原子写入。
文件大小上限为 256 KiB，最多保存 64 个 Profile。

```json
{
  "schema_version": 1,
  "default_profile_id": "agent-example",
  "profiles": [
    {
      "id": "agent-example",
      "name": "Local Agent",
      "transport": "acp_stdio",
      "executable": "agent",
      "endpoint": null,
      "arguments": ["--read-only"],
      "environment": [
        { "name": "LANG", "value": "en_US.UTF-8" }
      ],
      "model": null,
      "working_directory": "selected_item",
      "permission": "read_only",
      "timeout_seconds": 600,
      "output_limit_bytes": 8388608,
      "credential_ref": "keychain:kite/agent/local",
      "credential_env_var": null,
      "capabilities": {
        "read_selection": true,
        "read_workspace": false,
        "write_files": false,
        "execute_commands": false,
        "network": false,
        "permission_requests": true
      }
    }
  ]
}
```

`schema_version` 是必填的当前版本字段。加载器也接受旧版无版本 Profile 数组，并迁移为
schema 1。高于当前版本的配置不会被覆盖；调用方会收到 `UnknownSchemaVersion`。

## 语义

| 字段 | 语义 |
| --- | --- |
| `id` | 稳定 Profile 标识；创建时为空则由 Core 生成 |
| `name` | 用户可见名称；忽略大小写唯一 |
| `transport` | `acp_stdio`、`acp_http` 或 `cli` |
| `executable` | 进程型传输的启动命令或路径 |
| `endpoint` | HTTP 传输的绝对 `http`/`https` 端点 |
| `arguments` | 明确配置的启动参数，不经过 shell 拼接 |
| `environment` | 非敏感环境变量；敏感值必须使用 `credential_ref` |
| `model` | 可选模型名；留空表示沿用 Agent 自身配置 |
| `working_directory` | `selected_item`、`file_parent`、`repository_root`、`fixed` 或 `ask` |
| `permission` | `read_only`、`confirm_writes` 或 `agent_managed` |
| `timeout_seconds` | 单次任务超时，范围 1 到 86400 秒 |
| `output_limit_bytes` | 单次输出上限，范围 1 KiB 到 256 MiB |
| `credential_ref` | 不透明凭据引用，不包含明文凭据 |
| `credential_env_var` | 可选；启动进程时注入已保存凭据的环境变量名 |
| `capabilities` | Agent 声明支持的读取、写入、命令、网络和权限请求能力 |

`default_profile_id` 只指向可启动会话的 Profile（当前仅有 `acp_stdio`）。创建第一个可运行
Profile 会将其设为默认值；删除默认 Profile 后优先选择剩余的可运行 Profile，若没有则为
`None`。旧版本留下的 `acp_http` 或 `cli` Profile 会保留以支持诊断，但不能保存为可运行配置、
不能设为默认，也不会进入右键菜单或 Agent 栏的可用状态。

## 校验与凭据边界

Core 在保存前统一校验并规范化字段：

- Profile 名称去除首尾空白，长度不超过 80 字节。
- 进程型传输必须提供 `executable`，HTTP 传输必须提供有效 `endpoint`；二者不能混用。
- 当前仅 `acp_stdio` 可保存为可运行 Profile；`acp_http` 和 `cli` 只允许旧配置加载与连接测试。
- 环境变量名必须匹配 `[A-Za-z_][A-Za-z0-9_]*`，且忽略大小写后不能重复。
- 参数、环境变量、模型、端点和凭据引用都会检查常见明文密钥形式。
- 带内嵌用户名或密码的 URL 会被拒绝。

`credential_ref` 只接受以下不透明 scheme：

- `keychain:`：平台钥匙串或凭据管理器条目。
- `credential:`：由后续平台凭据适配层解析的通用引用。
- `env:`：明确引用进程环境变量，不在配置中复制其值。

例如 `keychain:kite/agent/local` 是有效引用；裸 token、密码或
`https://user:password@host` 都不是有效凭据引用。敏感环境变量名，如
`OPENAI_API_KEY`、`GITHUB_TOKEN` 或 `AWS_SECRET_ACCESS_KEY`，会被拒绝，提示调用方改用
`credential_ref`。

这层校验用于阻止配置模型成为明文密钥存储；它不替代后续设置页对系统钥匙串的读写，也不保证
外部 Agent 自身不会记录日志。

## 加载、恢复与并发

- 正常加载返回 `AgentProfileSnapshot`，包含当前 schema、Profile 列表、默认 Profile 和可选警告。
- `snapshot()` 不会因损坏、过大或未来版本文件而阻止 Kite 启动；它返回空快照和类型化警告。
- CRUD 方法使用严格加载；损坏或未来版本配置会返回 `AgentConfigError`，不会静默覆盖。
- `reset()` 是显式的恢复入口，只应在用户确认后调用。
- 所有写入使用同目录临时文件加原子替换。
- 进程内配置 I/O 由全局互斥锁串行化，避免并发 CRUD 的读改写丢失。
- 主文件损坏或超限时回退为空快照；不要自动删除原文件，以便后续诊断或用户恢复。

## UniFFI API

`AgentProfileStore` 通过 UniFFI 导出：

- `snapshot()`
- `listProfiles()`
- `getProfile(profileId:)`
- `defaultProfile()`
- `createProfile(profile:)`
- `updateProfile(profile:)`
- `deleteProfile(profileId:)`
- `deleteProfileWithReplacement(profileId:replacementDefaultId:)`
- `duplicateProfile(profileId:)`
- `saveProfileWithCredential(profile:credential:clearCredential:)`
- `setDefaultProfile(profileId:)`
- `reset()`

`KiteCore` additionally exposes `agentTransportCapabilities()`,
`agentDefaultProfile()` and `agentProfileCanStartSession(profile:)` so
native UIs use the same transport matrix and default-profile semantics as the
runtime.

`KiteCore.agentProfileStore()` 返回共享 Store 实例。Windows 直接调用 Rust Core；macOS 通过
生成的 Swift 绑定调用同一套记录、方法和错误类型。Swift 冒烟测试位于
`apps/macos/Tests/AgentProfileStoreSmoke.swift`，运行脚本为
`apps/macos/Tests/run-agent-profile-store-smoke.sh`。该测试由 `Agent FFI smoke` GitHub Actions
workflow 在相关 Core 或 Swift 绑定改动时执行。
