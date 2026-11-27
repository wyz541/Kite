# Kite 上下文菜单架构（v1）

## 状态与权威文档

- 产品讨论：[#220](https://github.com/wyz541/Kite/discussions/220)
- 父 Feature：[#221](https://github.com/wyz541/Kite/issues/221)
- 规范：[`context-menu-spec.md`](context-menu-spec.md)
- 跨平台验收：[`../agent-acceptance-context-menu-v1.md`](../agent-acceptance-context-menu-v1.md)
- Draft PR：[#233](https://github.com/wyz541/Kite/pull/233)

`context-menu-spec.md` 是动作语义、上下文矩阵和验收不变量的权威来源。本文件只描述当前实现架构，不复制完整动作目录。

## 已采纳的边界

1. **Kite 菜单主导，系统原生菜单为辅。**
   - v1 不提供“默认改用系统右键菜单”的全局开关。
   - `system.native-menu` 始终保留为最后的根级入口。
   - 原生平面不可用时，入口仍可见，但禁用并显示 `NATIVE_PLANE_UNAVAILABLE`。
2. **打开动作按语义分离。**
   - `open.default`：执行目标的主激活行为。
   - `open.with`：为单个文件选择兼容应用。
   - `open.in`：把文件、目录、仓库或工作区交给已验证工具。
   - `compare.with`：把恰好两个文件交给 diff/merge 工具。
3. **Git/SVN 使用内建状态机和原生扩展兜底。**
   - Kite 负责高频、可验证的动作和进行中操作的恢复入口。
   - TortoiseGit、TortoiseSVN、企业扩展和未建模命令继续由系统原生平面提供。
   - 不在 Kite 菜单内复制同一套外部命令树。

## 分层架构

```text
平台上下文快照
    ↓
ActionProvider[]             只声明候选，不决定布局
    ↓
ContextMenuComposer         过滤、去重、排序、分组、子菜单提升、校验
    ↓
MenuPlan                     平台无关、可序列化、可测试
    ↓
Windows GPUI / macOS SwiftUI 投影
    ↓
冻结 TargetSet + 动作载荷
    ↓
平台动作分发与状态复核
```

### 核心模型

`src/context_menu_model.rs` 定义：

- `ContextSurface`：文件列表、分栏、搜索、背景、侧边栏、预览和设备命名空间。
- `ContextSnapshot`：平台、Surface、冻结的 `TargetSet`、容器、能力、工具目录、VCS 快照和用户策略。
- `TargetSet`：有序目标、主目标、选择关系、工作目录和冻结路径。
- `ActionCandidate`：Provider 的候选声明，可携带稳定 variant、payload 和证据优先级。
- `ResolvedAction`：规范化后的 `ActionId`、Scope、Risk、Owner、Effect、排序、目标、可用性和反馈。
- `MenuPlan`：根组和单层任务子菜单；渲染器不再自行决定语义位置。

`ActionInstanceKey` 是 `ActionId` 加可选 `variant_key`。例如所有“打开方式”应用共享 `open.with`，但每个应用有独立实例键；同一菜单中重复实例会被 Composer 拒绝。

### Composer

`ContextMenuComposer` 的固定流水线：

```text
Collect → Normalize → Filter → Deduplicate → Resolve → Group → Project → Validate
```

关键规则：

- 候选先按目标形态、Surface、能力、加载状态和用户策略过滤。
- 同一实例优先选择可用候选，再按显式证据优先级和稳定 Provider 顺序决胜。
- 根组固定为 `G00` 到 `G99`；`system.native-menu` 只允许位于 `G99` 且必须是最后根项。
- 高风险动作提升到 `G90`。
- 只有单项的任务子菜单提升为根动作，不保留空壳子菜单。
- 背景菜单禁止 Entity/Collection 泄漏；多选菜单禁止单目标动作泄漏。
- 所有可见动作必须有标签；临时禁用动作必须有稳定原因码和用户可读原因。
- 普通根级项硬上限为 16，超过时 Composer 失败并进入安全回退。
- 普通 Composer 失败时，回退菜单保留默认打开、可用的定位动作和原生入口。

### Provider

Provider 只返回候选，不直接绘制分隔线、一级位置或手写菜单树。

```text
Refresh
Core
OpenWith
Tool
Archive
Agent
VCS
System
```

Provider 注册点集中在 `default_providers()`。新增功能应扩展动作目录和 Provider，而不是在视图层复制菜单代码。

## 平台投影

### Windows / GPUI

入口位于 `src/context_menu/mod.rs`：

- 将 `ContextMenuTarget` 转换为 `ContextSnapshot`。
- 将 `MenuPlan` 投影为 GPUI `PopupMenu`。
- 把稳定 `ActionId`、variant 和 payload 转成 `ContextMenuAction`。
- 分发前复核目标存在性、目标类型和 VCS 状态版本。

Windows 原生桥位于 `src/context_menu/windows.rs`：

- 使用 `IShellItemArray` 保留多选目标。
- 通过 `IContextMenu`、`IContextMenu2`、`IContextMenu3` 转发 Shell 扩展消息。
- 使用延迟窗口消息打开原生菜单，避免在 GPUI 更新栈中同步启动嵌套消息循环。
- `Show more options` 是独立、最后、可点击的系统入口，不镜像 Shell 命令树。

### macOS / SwiftUI + AppKit

入口位于 `apps/macos/Sources/KiteContextMenu.swift`：

- 菜单创建时冻结右侧列的选择、主目标、工作目录和容器。
- 同一个 SwiftUI 计划渲染器服务文件、目录/背景、搜索、侧边栏、收藏、卷和预览溢出菜单。
- 叶子动作使用标准 `Button`，子菜单使用标准 `Menu`，保留系统键盘、Escape、首字符导航和辅助功能语义。
- 高风险动作使用 destructive role。
- 分发前复核冻结目标仍存在且类型未变化。

macOS 原生平面入口映射到 AppKit Services 菜单，而不是伪造一套“完整 Finder 菜单”。Services 不可用时该项保持可见但禁用。

## 打开与工具边界

### Open With

- 仅单个、存在、可读的普通文件或压缩包。
- 顺序为默认应用、最近应用、其余系统兼容应用、“选择其他应用…”。
- macOS 使用 LaunchServices；Windows 使用 Shell association handlers。
- 不提供“始终使用此应用打开”，不修改系统默认关联。
- 动态应用发现不得阻塞选择其他应用入口。

### Open In / Compare With

- 共享 `src/tool_registry.rs` 的工具定义、能力匹配、策略排序和 argv 模板。
- 只显示声明支持当前目标形态和角色的已验证工具。
- 启动使用参数数组，不经过 shell 字符串插值。
- 固定、最近和用户顺序只影响工具子菜单内部，不影响根级语义顺序。
- `compare.with` 在 v1 只接受恰好两个文件。
- Kite 编辑器、Mindmap 和 Git 客户端通过工具注册表进入 `open.in`，不再作为零散临时菜单命令复制。

## Git / SVN 状态

`src/vcs_menu.rs` 提供轻量菜单快照：

- Git repository/worktree/common-dir、HEAD、upstream、ahead/behind、index、冲突和进行中操作。
- SVN working-copy root、URL、revision、switched、property-only、missing、conflict 和 cleanup 状态。
- 多选只有在所有目标映射到同一仓库时才显示仓库级动作。
- 进行中操作优先显示 continue/skip/abort；普通写入被抑制，只保留安全的读取入口。
- 每次 VCS 写操作携带 `state_revision`，分发前重新校验。
- Kite 自有 rebase 恢复状态优先于通用 `git rebase --continue`。

## 交互与可访问性不变

- 右键打开菜单不改变选择、预览对象、导航历史或目录加载状态。
- 菜单动作只使用打开菜单时冻结的 `TargetSet`，不重新读取实时选择。
- 危险动作必须确认，并固定在 `G90`。
- 永久不支持的动作隐藏；临时不可用的动作显示并解释原因。
- 根级到普通叶节点最多两级；长菜单由平台菜单系统滚动。
- 所有动作和子菜单必须有非空可访问标签；禁用项必须提供可读原因。

自动化测试覆盖计划级不变量；键盘、屏幕阅读器、长菜单滚动和真实第三方扩展仍需平台人工验收。详见跨平台验收文档。

## 当前边界

- Windows 文件列表仍沿用单行选择模型；Windows 分栏和 macOS 多选已支持冻结多目标。
- macOS 原生平面是 AppKit Services，不等同于 Windows Shell 的完整 `IContextMenu`。
- Windows `IContextMenu2/3`、MTP 设备能力、UWP/打包应用关联和真实第三方工具需要在 Windows 10/11 真机验证。
- Open With、Open In 和 Compare With 的第三方启动不在自动测试中执行。
- 端到端首帧延迟、系统菜单打开/取消延迟和屏幕阅读器体验尚未形成人工基准。
