# Agent 结果审核与写入

## 结论

Agent 结果进入 Kite 时先形成只读审核记录。

- 回答、引用文件、文件修改、权限请求和命令建议统一进入结果窗口。
- 文件修改必须展示 unified diff、目标路径、增删行数和覆盖风险。
- 只有用户点击“应用全部修改”后才写入。
- 审核后目标文件发生变化时，Kite 拒绝覆盖并要求重新生成 diff。
- 权限回复以结构化 JSON 返回给会话适配器；拒绝不会让任务进入不可恢复状态。
- Agent 建议的命令只以文本展示和复制，Kite 不执行。
- 审计日志只记录元数据，不记录提示词、回答正文、文件内容或密钥。

本功能依赖 #151 的会话事件和 #155 的对话界面。当前仓库同时提供“审阅结构化结果”入口，便于在会话 UI 合入前验证同一套审核链路。

## 结构化结果

结果入口使用 JSON，schema 版本当前为 1。

```json
{
  "schema_version": 1,
  "id": "task-result-1",
  "agent": "Example Agent",
  "model": "example-model",
  "transport": "acp",
  "working_directory": "/work/project",
  "context_paths": ["/work/project/src/lib.rs"],
  "answer": "说明文字",
  "references": [
    { "path": "src/lib.rs", "label": "lib.rs", "line": 42 }
  ],
  "file_changes": [
    {
      "path": "src/lib.rs",
      "proposed_text": "完整新内容",
      "expected_sha256": "可选，Agent 读取时的文件哈希"
    }
  ],
  "permission_requests": [
    {
      "id": "permission-1",
      "kind": "write",
      "title": "修改源文件",
      "detail": "Agent 需要写入 src/lib.rs",
      "target_paths": ["src/lib.rs"],
      "can_allow_task": true
    }
  ],
  "commands": [
    {
      "command": "cargo test",
      "rationale": "验证修改"
    }
  ]
}
```

transport 为 acp 或 cli。

- acp：Kite 可以生成“仅本次允许 / 本任务允许 / 拒绝”的结构化回复。
- cli：Kite 明确提示权限由外部 Agent 自身管理，不宣称进程已被沙箱化或强制只读。

## 写入保护

1. 工作目录必须存在且为目录。
2. 目标路径必须位于工作目录内。
3. 路径中的上级目录跳转、符号链接、目录目标和超大文件会被拒绝。
4. 已有文件先以 UTF-8 读取并记录 SHA-256。
5. review 阶段生成 diff，不写入文件。
6. 应用阶段重新校验所有目标文件哈希。
7. 任一路径冲突或预检失败时，所有修改都不写入。
8. 写入使用同目录临时文件和原子替换。
9. 多文件写入中途失败时，仅在文件仍等于 Kite 写入结果时回滚。
10. 写入完成后再次读取文件并校验内容哈希。
11. 已有文件的 UTF-8 BOM 和 LF/CRLF 换行风格会保留。

“另存为”使用独立的用户选择路径。目标已存在时必须再次确认覆盖。

## 权限回复

拒绝回执示例：

```json
{
  "request_id": "permission-1",
  "decision": "deny",
  "allowed": false,
  "scope": "deny",
  "task_id": "task-1",
  "message": "用户拒绝了该请求"
}
```

“本任务允许”会返回 task scope 回执。任务级授权由会话适配器保存在当前任务内；Kite 不把永久授权写入磁盘。

## 审计

审计日志位于系统本地数据目录：

```text
Kite/agent-audit.jsonl
```

记录字段：

- 时间。
- Agent 和模型。
- 工作目录。
- 上下文数量。
- 结果类型。
- 修改文件数量。
- 用户是否确认写入。
- 权限决定或结果动作。

不记录：

- 提示词正文。
- Agent 回答正文。
- 文件内容或 diff。
- 命令或命令理由。
- 密钥和环境变量值。

诊断文本在写入审计或 UI 错误前经过 Bearer、token 赋值、URL 用户信息等模式脱敏。

## 结果操作

- 复制回答。
- 在 Kite 分栏中定位引用文件。
- 使用默认应用打开引用文件。
- 将回答或文件建议保存为独立文件。
- 将回答物化为草稿并在 Kite 编辑器中打开。
- 复制命令建议，不执行。
- 应用全部文件修改或放弃。

## 平台实现

- Rust Core：src/agent_review.rs。
- UniFFI：src/ffi.rs 中的 Agent review DTO 和 KiteCore 方法。
- Windows：src/agent_review_window.rs；入口为 Agent 栏“审阅结果”和 Agent > 审阅结构化结果。
- macOS：apps/macos/Sources/KiteAgentReviewWindow.swift；入口为 Agent 栏“审阅结果”和 Agent > 审阅结构化结果。

Windows 和 macOS 使用同一套 Core 校验、权限回执和审计记录。
