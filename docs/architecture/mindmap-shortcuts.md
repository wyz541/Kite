# Mindmap 快捷键映射

- 状态：跨平台基线
- 平台：macOS 已接入，Windows GPUI 按同一动作表实现
- 目标：同一动作在两端使用同一套语义，不因本地键位差异改变操作习惯

## 修饰键约定

| 语义 | macOS | Windows |
|---|---|---|
| Primary | `Command` | `Control` |
| Insert | `Shift` | `Shift` |
| Alternate | `Option` | `Alt` |
| Control | `Control` | `Control` |

除特别标注外，表中的 `Primary` 在 macOS 映射为 `Command`，在 Windows 映射为 `Control`。

## 节点创建

| 动作 | macOS | Windows | 状态 |
|---|---|---|---|
| 新建同级主题 | `Enter` | `Enter` | 已接入 |
| 新建子主题 | `Tab` | `Tab` | 已接入 |
| 当前主题前插入同级 | `Shift+Enter` | `Shift+Enter` | 已接入 |
| 新建父主题 | `Primary+Shift+Enter` | `Ctrl+Shift+Enter` | 已接入 |
| 新建自由主题 | 空白处双击 / `Primary+Shift+L` | 空白处双击 / `Ctrl+Shift+L` | 待实现 |

## 编辑与节点操作

| 动作 | macOS | Windows | 状态 |
|---|---|---|---|
| 开始编辑标题 | `F2` / `Space` | `F2` / `Space` | 已接入 |
| 编辑中插入换行 | `Shift+Enter` | `Shift+Enter` | 已接入 |
| 提交标题编辑 | `Enter` | `Enter` | 已接入 |
| 取消标题编辑 | `Escape` | `Escape` | 已接入 |
| 删除节点 | `Delete` / `Backspace` | `Delete` / `Backspace` | 已接入 |
| 同级上移 | `Option+Up` | `Alt+Up` | 已接入 |
| 同级下移 | `Option+Down` | `Alt+Down` | 已接入 |
| 复制节点子树 | `Primary+C` | `Ctrl+C` | 已接入 |
| 剪切节点子树 | `Primary+X` | `Ctrl+X` | 已接入 |
| 粘贴节点子树 | `Primary+V` | `Ctrl+V` | 已接入 |
| 仅粘贴文本为主题 | `Primary+Shift+V` | `Ctrl+Shift+V` | 已接入 |
| 复制样式 | `Primary+Option+C` | `Ctrl+Alt+C` | 待实现 |
| 粘贴样式 | `Primary+Option+V` | `Ctrl+Alt+V` | 待实现 |

## 画布与导航

| 动作 | macOS | Windows | 状态 |
|---|---|---|---|
| 放大 | `Primary++` | `Ctrl++` | 已接入 |
| 缩小 | `Primary+-` | `Ctrl+-` | 已接入 |
| 重置到 100% | `Primary+0` | `Ctrl+0` | 已接入 |
| 适应画布 | `Primary+F5` | `Ctrl+F5` | 已接入 |
| 回到中心主题 | `Primary+Home` | `Ctrl+Home` | 已接入 |
| 展开/收起当前分支 | `/` | `/` | 已接入 |
| 展开当前分支全部下级 | `*` | `*` | 已接入 |
| 专注模式 | `F8` | `F8` | 已接入 |
| 显示/隐藏侧栏 | `Primary+Option+F` | `Ctrl+Alt+F` | 已接入 |

## 高级元素

这些动作需要先补齐 XMind 文档模型和编辑器能力。

| 动作 | macOS | Windows | 状态 |
|---|---|---|---|
| 联系线 | `Primary+L` | `Ctrl+L` | 待实现 |
| 外框 | `Primary+B` | `Ctrl+B` | 待实现 |
| 概要 | `Primary+]` | `Ctrl+]` | 待实现 |
| 备注 | `Primary+Shift+N` | `Ctrl+Shift+N` | 待实现 |
| 超链接 | `Primary+K` | `Ctrl+K` | 待实现 |

未实现的动作当前会在状态栏给出明确提示，不会静默失败。

## 文件与撤销

| 动作 | macOS | Windows | 状态 |
|---|---|---|---|
| 新建文档 | `Primary+N` | `Ctrl+N` | 待实现 |
| 打开文档 | `Primary+O` | `Ctrl+O` | 已接入 |
| 保存 | `Primary+S` | `Ctrl+S` | 已接入 |
| 撤销 | `Primary+Z` | `Ctrl+Z` | 已接入 |
| 重做 | `Primary+Shift+Z` / `Primary+Y` | `Ctrl+Y` | 已接入 |
| 搜索主题 | `Primary+F` | `Ctrl+F` | 已接入 |

## 核心键盘流

最高频链路固定为：

```text
Tab → 向下拆解
Enter → 同级发散
方向键 → 在结构中游走
F2 / Space → 修改当前节点
Option/Alt + Up/Down → 调整同级顺序
```

这五项在 macOS 和 Windows 上使用同一动作语义，只替换 Primary 修饰键。

## 实现位置

- macOS 输入分发：`apps/macos/Sources/KiteMindmapWindow.swift` 的 `installKeyboardMonitor()`。
- Rust 结构操作：`src/mindmap_ffi.rs` 的 `MindmapEditDto` 与 `edit_mindmap_document()`。
- Windows GPUI 接入时必须复用同一动作表，并通过 Rust 编辑命令执行。
