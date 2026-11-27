# macOS Mindmap 格式编辑切片

- 状态：Rust Core 与 macOS UI 已接通，XMind 27.01 视觉复核与 Windows GPUI 对等仍待完成
- Issues：#213、#214、#215、#216、#217
- UI：`apps/macos/Sources/KiteMindmapWindow.swift`
- FFI：`src/mindmap_ffi.rs`
- Core：`src/mindmap/graph_style.rs`、`topic_markers.rs`、`topic_todo.rs`、`topic_style.rs`、`date.rs`

## 已接入能力

macOS 编辑器现在通过 UniFFI 调用 Rust Core，不直接改写 `content.json`：

- 读取与更新文档级图结构：思维导图、左/右逻辑图、组织架构图、树形图。
- 读取与更新连线：曲线、直线、直角折线、圆角折线及实验性圆角半径。
- 读取 marker header，支持优先级、任务进度和通用标记增删。
- TODO 使用 canonical `task-*` marker；完成态显示勾选、删除线、低透明节点和低透明连线。
- 将静态日期插入标题或 `notes.plain.content`，支持 ISO、中文、English US 与系统 locale pattern。
- 读取与更新本地文字/节点样式：字体族、字号、字重、斜体、下划线、删除线、文字颜色、背景、边框和圆角。

每个格式命令均先在 Rust package 层验证并原子保存；成功后由 Swift 重新读取文档并重建画布。撤销/重做仍保存完整 `content.json` 快照并走无损 writer。

## 画布渲染

- 左右逻辑图与树形图使用横向层级布局。
- 思维导图在根主题两侧交替布置一级分支。
- 组织架构图使用向下层级布局，切换结构后自动适应画布。
- 连线根据 typed connector kind 绘制曲线、直线、直角或圆角折线。
- marker 位于主题头部固定区域；TODO checkbox 位于文本前。
- 完成态不只改变图标，还会降低节点与连线的视觉权重。

## 无损边界

- UI 只调用 known-field 格式 API。
- 未知 marker 字段、未知 style property、未知 topic 字段和未知 ZIP 条目继续保留。
- `GraphStructure::Other` 和 `ConnectorKind::Other` 不在 UI 中归一化；未知值只读取，不被静默覆盖。
- XMind theme 继承仍未由 Kite 完整解析；当前画布优先展示 topic-local style，缺失值使用工业风 fallback。

## 验证

已执行：

```text
cargo test mindmap_ffi --lib
cargo test --test mindmap_ffi_format
KITE_BUILD_PROFILE=debug bash apps/macos/build.sh
```

真机 UI 已验证：

- Toggle TODO 后显示完成勾选、删除线、muted node/connector。
- 圆角折线在画布可见并持久化。
- 组织架构图切换后可见并持久化。
- 文字 bold/italic/underline 应用后立即重绘。
- P2 与 `flag-red` marker 显示在主题头部。
- 日期插入后标题、outline 和画布同步更新。

## 仍未完成

- XMind for macOS `27.01.05089` 对 marker、TODO、`line-corner`、`shape-corner` 和字体属性的写入回读/视觉复核。
- XMind theme/style 继承解析与完全一致的字号、换行和富文本布局。
- 多选样式、复制/粘贴样式、主题内 marker picker 的原生图标集。
- Windows GPUI 对等实现；当前明确没有 Windows 代码。
