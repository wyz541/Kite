# XMind 主题标记与 TODO 语义

- 状态：Core foundation 已实现，XMind 27.01 视觉/写入复验待完成
- Issues：#214、#215
- 设计讨论：#201
- 实现：`src/mindmap/topic_markers.rs`、`src/mindmap/topic_todo.rs`
- 测试：`tests/mindmap_markers_todo.rs`

## 结论

现代 XMind 把主题标记保存在 topic 的有序 `markers` 数组中：

```json
{
  "id": "topic-id",
  "class": "topic",
  "title": "Ship Kite",
  "markers": [
    { "markerId": "priority-1" },
    { "markerId": "task-half" },
    { "markerId": "flag-red" }
  ]
}
```

Kite 的 typed model 只解释 `markerId`：

- `priority-1` 到 `priority-9`：优先级。
- `task-*`：任务进度和 TODO 状态。
- 其他 marker：通用标记，并按 `tag-*`、`flag-*`、`star-*`、`people-*`、`smiley-*`、`symbol-*` 分类。
- 对象中除 `markerId` 外的字段全部作为 opaque fields 保留。
- 非对象条目或缺少有效 `markerId` 的条目作为 opaque marker entry 保留。

Kite 不重建 topic 或 markers 数组，只修改目标 marker 的已知字段。因此未编辑 marker 的未来字段、未知 marker、topic 的其他字段和无关 topic 都保持不变。

## 优先级映射

| markerId | typed value |
|---|---|
| `priority-1` | `PriorityMarker::Priority1` |
| `priority-2` | `PriorityMarker::Priority2` |
| ... | ... |
| `priority-9` | `PriorityMarker::Priority9` |

优先级是单值槽。应用新优先级时保留原 marker 对象上的未知字段，并删除重复优先级 marker，但不改变其他 marker 的相对顺序。

## TODO / task-progress 映射

XMind 当前任务进度使用 `task-*` marker，百分比精确表示为八分之一：

| markerId | 进度 | typed value |
|---|---:|---|
| `task-start` | 0/8 | `TaskProgress::Start` |
| `task-oct` | 1/8 | `TaskProgress::Eighth` |
| `task-quarter` | 2/8 | `TaskProgress::Quarter` |
| `task-3oct` | 3/8 | `TaskProgress::ThreeEighths` |
| `task-half` | 4/8 | `TaskProgress::Half` |
| `task-5oct` | 5/8 | `TaskProgress::FiveEighths` |
| `task-3quar` | 6/8 | `TaskProgress::ThreeQuarters` |
| `task-7oct` | 7/8 | `TaskProgress::SevenEighths` |
| `task-done` | 8/8 | `TaskProgress::Done` |
| `task-pause` | 暂停 | `TaskProgress::Paused` |

每个 topic 最多规范化一个 task-progress marker。`ToggleCompletion` 的当前语义是：

- 已完成：写回 `task-start`。
- 未完成、进行中或暂停：写为 `task-done`。
- 节点不移动、不删除，其他 marker 和 topic 字段不变。

仓库此前没有观察到现代 XMind topic 上存在必须由 Kite 创建和重写的独立结构化 `task` 对象。因此 Kite 不把它作为权威持久化状态：

- TODO 权威字段是 `markers[].markerId` 的 `task-*` 值。
- 如果 topic 上已有未知 `task` 对象，Kite 原样保留，不合并、不删除、不重写。
- `本机 27.01 写入回读` 完成后，才可把该映射标为发布级一比一。

## 完成态渲染语义

Rust Core 返回平台无关的 typed view，不包含 SwiftUI/GPUI 颜色或像素值：

```rust
CompletionRendering {
    text_decoration: TextDecoration::LineThrough,
    tone: VisualTone::Muted,
    text_opacity: OpacitySemantics::Reduced,
    connector_opacity: OpacitySemantics::Reduced,
}
```

非完成状态统一返回：

```text
text_decoration = None
tone            = Normal
text_opacity    = Full
connector_opacity = Full
```

macOS 和 Windows 分别把抽象语义映射到自己的颜色、透明度和绘制实现。这样两端的完成态行为一致，但不会把平台视觉参数写进 `.xmind`。

## 尚未确认的语义

1. XMind `27.01.05089` 是否继续使用完全相同的 `task-*` marker ID 和 `markers[]` 写入位置。
2. XMind 是否会在某些任务功能中同时写入独立 task 元数据；当前只确认 marker 映射，不推测其他字段。
3. “阻塞”和“风险”目前没有可靠的独立 task-status 字段映射。Kite 不把 `flag-red`、`symbol-attention` 等通用 marker 擅自解释成阻塞状态；UI 可以通过 `TopicMarkerSet` 读取这些通用 marker，后续需要以 27.01 真实文件确认语义后再增加 typed 状态。
4. 父节点自动进度聚合尚未实现，也没有写入派生 marker，避免把 UI 聚合值误存为 XMind 源数据。
5. Undo/redo 与自动保存属于命令层，不在本 core foundation 的职责内；package API 保证每次编辑只在成功后替换内存内容。

这些问题在 macOS/Windows UI 接入前必须完成 27.01 真机往返验证，或显式降级。

## API

```rust
use kite_core::mindmap::{
    PriorityMarker, SlotUpdate, TaskProgress, TodoUpdate,
    TopicMarkerPatch, TopicTodoPatch,
};

let header = package.topic_header_markers(0, &topic_id)?;

package.apply_topic_markers(
    0,
    &topic_id,
    &TopicMarkerPatch {
        priority: SlotUpdate::Set(PriorityMarker::Priority2),
        task_progress: SlotUpdate::Set(TaskProgress::Half),
        add: vec!["flag-red".into()],
        remove: vec!["tag-red".into()],
    },
)?;

let todo = package.apply_topic_todo(
    0,
    &topic_id,
    &TopicTodoPatch {
        update: TodoUpdate::ToggleCompletion,
    },
)?;

package.save()?;
```

## 验证

已覆盖：

- priority、task progress、generic marker 的类型和顺序读取。
- marker 对象未知字段在更新后保留。
- opaque marker entry 和无关 topic 原样保留。
- priority/task 冲突规范化。
- 未知 ZIP entry、topic 上未知 `task` 对象和未知 topic 字段保留。
- TODO 完成态 rendering semantics。
- TODO 从完成态恢复为 `task-start`。
- 暂停状态和普通未完成状态区分。
- 3,019 topic capability fixture 的 header 读取、根部标记写入、子树不变和保存重开。

运行：

```bash
cargo test --test mindmap_markers_todo
```
