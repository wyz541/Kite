# Kite 上下文菜单规范（v1）

- 状态：Canonical，可进入实现与验收
- 日期：2026-11-27
- 父功能：[#221 Context Menu 重构](https://github.com/wyz541/Kite/issues/221)
- 决策讨论：[#220](https://github.com/wyz541/Kite/discussions/220)
- 本文对应：[#222](https://github.com/wyz541/Kite/issues/222)
- 适用平台：Windows、macOS；其他平台的投影器应按同一语义实现或明确声明不支持
- 非目标：本文不规定 GPUI、SwiftUI、Win32、AppKit 的具体类名或渲染细节，也不修改生产代码

本规范中的“必须”“不得”“应当”分别表示强制要求、禁止项和默认推荐。平台渲染器可以不同，但动作身份、上下文判定、可见性和安全语义必须一致。

---

## 0. 已固定的产品决策

1. **Kite 菜单是主入口，系统原生菜单是辅助且完整的逃生口。**
   - v1 不提供“整菜单默认入口切换”。
   - `system.native-menu` 必须是固定、最后、可点击的入口；平台明确不提供原生平面时才允许保留禁用态。
   - 原生入口不得因为普通菜单 Composer 失败而消失。

2. **打开动作按语义分离。**
   - `open.default`：默认打开对象。
   - `open.with`：为文件选择应用。
   - `open.in`：把文件、目录、仓库交给已验证的工具。
   - `compare.with`：交给差异或合并工具。
   - 这四个动作可以使用同一组底层进程 API，但不得合并为同一个 `ActionId`。

3. **Git/SVN 使用“内建状态机 + 原生完整能力平面”。**
   - Kite 负责高频、跨平台、可验证、可恢复的动作。
   - TortoiseGit、TortoiseSVN、企业扩展和高级工具继续由 OS 原生平面提供。
   - Kite 不复制外部扩展的内部命令树。

4. **Provider 只声明动作，不决定布局。**
   - 动作的过滤、去重、状态提升、分组和排序统一由 Composer 完成。

5. **菜单是当前上下文下合法动作的投影，不是功能目录。**
   - 视觉区块可以调整，但必须先有稳定的动作图。

---

## 1. 核心本体

### 1.1 `ContextSnapshot`

`ContextSnapshot` 是右键菜单打开瞬间冻结的不可变快照。菜单渲染、键盘导航和动作分发只读取该快照，不重新读取实时选择或导航状态。

```text
ContextSnapshot
  schema_version
  snapshot_id
  opened_at
  surface
  activation_mode
  target_set
  container
  workspace
  repositories[]
  capabilities
  user_policy
  focus_token
```

#### 字段定义

| 字段 | 含义 | 规则 |
|---|---|---|
| `schema_version` | 快照结构版本 | 不兼容变更必须升主版本 |
| `snapshot_id` | 本次菜单实例的稳定标识 | 用于日志、测试与竞态检测 |
| `surface` | 触发菜单的 UI 表面 | 只影响投影和允许的目标规则，不改变动作语义 |
| `activation_mode` | 鼠标、键盘或溢出菜单触发 | 不得改变动作集合，只影响定位与焦点 |
| `target_set` | 主目标、有序目标列表和选择关系 | 背景菜单允许为空集合 |
| `container` | 当前目录/栏/设备容器 | 提供粘贴、新建、刷新等 Container 动作 |
| `workspace` | 当前工作区根和工具上下文 | 提供 Open In、初始化和工具启动语义 |
| `repositories` | 与目标相关的一个或多个仓库快照 | 多仓选择默认不提升仓库动作 |
| `capabilities` | 已验证能力与不可用原因 | 只能声明已验证能力，不能猜测 |
| `user_policy` | 隐藏、固定、最近使用、确认策略 | 不得改变系统原生入口 |
| `focus_token` | 菜单关闭后应恢复的焦点 | 用于可访问性与键盘交互 |

#### Surface

规范 Surface 集合：

```text
browser.list
browser.columns
preview
search.results
sidebar
background
device.namespace
```

辅助 Surface 可以复用同一 Composer，但必须单独声明：

```text
toolbar.path
tab.strip
vcs.log
```

#### `TargetSet`

```text
TargetSet
  primary: TargetRef?
  targets: TargetRef[]
  cardinality
  selection_relation: none | clicked_unselected | clicked_in_selection
  order: visible_order | explicit_order
  homogeneous
```

- `primary` 是菜单文案、标题和主路径来源。
- `targets` 的顺序必须固定；批量动作不得使用集合遍历顺序作为随机输入。
- 背景菜单使用 `cardinality = 0`，不得伪造当前目录为 Entity。
- 右键未选中项时，`selection_relation = clicked_unselected`，逻辑目标只包含被点击项；不得改写现有视觉选择。
- 右键已选中的任一项时，逻辑目标包含整个选择集。
- 多个目标必须保留“全部支持”或“部分支持”的判定，不得静默挑选一个目标执行。

#### `TargetRef`

```text
TargetRef
  id
  locator: file_path | namespace_uri | opaque_locator
  display_name
  kind
  exists
  readable
  writable
  container_id?
  repo_memberships[]
```

目标种类至少包括：

```text
file
directory
archive
repository_root
workspace_root
device_container
device_item
virtual_item
```

#### `ContainerSnapshot`

```text
ContainerSnapshot
  locator
  display_name
  writable
  can_create
  can_paste
  can_reveal
  loading
  refresh_generation
```

Container 是“动作发生的位置”，不等同于被右键的 Entity。例如文件菜单中的 `file.new-folder` 若被允许，其目标是父目录而不是文件；v1 默认在文件实体菜单隐藏 Container 动作，以减少作用域混用。

#### `WorkspaceSnapshot`

```text
WorkspaceSnapshot
  id
  root
  kind: local | repository | remote | device | unknown
  writable
  available_tool_ids[]
  active_tool_id?
```

`WorkspaceSnapshot` 负责工具启动和仓库发现，不替代 `TargetSet`。

#### `RepositorySnapshot`

```text
RepositorySnapshot
  repository_id
  vcs: git | svn | unknown
  repository_root
  worktree_root
  target_relations[]
  branch_or_ref
  upstream
  ahead
  behind
  index
  operation
  conflicts
  lock_state
  offline
  state_revision
  observed_at
```

Git 的 `index` 包含 staged/unstaged/untracked/conflicted 状态；SVN 没有 Git index，必须使用 working-copy/change-list 状态，不能伪造“暂存区”。

`state_revision` 用于分发前再验证。菜单快照冻结目标，但真正的危险动作在执行前仍必须检查状态版本；版本变化时取消执行并提示重新选择，不得把“删除 A”静默执行成“删除 B”。

#### `CapabilitySnapshot`

能力必须以验证结果声明，至少包括：

```text
file.read
file.write
file.rename
file.delete
file.copy
file.cut
file.paste
favorite.add
favorite.remove
open.with.available
open.in.available
compare.with.available
quick_look.available
system.reveal
system.share
system.properties
native_menu.available
vcs.git
vcs.svn
tool.<tool-id>
application.<application-id>
```

能力缺失分为：

```text
permanent_unsupported  永久不支持，隐藏动作
scope_inapplicable     当前作用域不适用，隐藏动作
temporarily_unavailable 临时不可用，显示禁用与原因
available              可用
```

#### `UserPolicySnapshot`

```text
UserPolicySnapshot
  menu_mode: kite_primary
  show_temporary_unavailable: true
  hidden_action_ids[]
  pinned_tool_ids[]
  recent_tool_ids[]
  recent_application_ids[]
  confirm_high_risk: true
  diagnostics_show_hidden: false
```

v1 的 `menu_mode` 固定为 `kite_primary`。架构保留扩展字段，但不得因此提供“默认使用系统菜单”的开关。

---

### 1.2 `ActionDescriptor`

`ActionDescriptor` 是动作注册表中的稳定定义。它描述“能做什么”，不描述“放在哪个 Provider 的第几个节点”。

```text
ActionDescriptor
  action_id
  label
  scope
  plane
  owner
  effect
  risk
  target_rule
    kinds[]
    cardinality
    surfaces[]
    all_targets_required
    container_fallback
  preconditions[]
  visibility_policy
  confirmation
  feedback
  icon_key
  shortcut?
  group
  order
  options[]
```

#### 字段定义

| 字段 | 含义 |
|---|---|
| `action_id` | 命名空间化、跨平台稳定、只增不改的 ASCII ID，例如 `file.rename` |
| `label` | 本地化 key 与参数；不把动态目标名硬编码进 ID |
| `scope` | `entity`、`collection`、`container`、`workspace`、`repository`、`system`、`extension` 之一 |
| `plane` | `kite_semantic` 或 `os_native_extension` |
| `owner` | `kite`、`os`、`tool`、`external_extension` |
| `effect` | 导航、读取、剪贴板、文件变更、进程启动、VCS 读、VCS 写、系统 UI、外部未知 |
| `risk` | `none`、`low`、`medium`、`high`、`external_unknown` |
| `target_rule` | 目标种类、数量、Surface、目标间关系；`container_fallback` 允许 Collection 动作解析为当前容器 |
| `preconditions` | 必须满足的可验证条件 |
| `visibility_policy` | 永久隐藏、作用域隐藏、临时禁用、可用 |
| `confirmation` | 无确认、普通确认、危险确认、系统自行确认 |
| `feedback` | 无提示、通知、进度、对话框、OS 负责 |
| `group/order` | Composer 使用的语义分组与稳定顺序 |
| `options` | 枚举型扩展，例如应用、工具、仓库引用 |

#### `ActionId` 规则

- 使用小写 ASCII 点分命名：`<domain>.<verb>` 或 `<domain>.<object>.<verb>`。
- `action_id` 表示语义身份，不包含平台、视图、标签或目标路径。
- 重命名文案不得改变 `action_id`。
- 改语义必须新增 ID，不能在原 ID 上静默改变风险或确认级别。
- 动态枚举项使用 `action_id + variant_key` 表示调用身份，例如：

```text
action_id = open.with
variant_key = application:com.microsoft.VSCode
```

`ActionInstanceKey = action_id + canonical_variant_key`。同一菜单层不得出现重复 `ActionInstanceKey`；核心语义层也不得注册两个同名 `action_id`。

#### `ActionInstance`

Composer 将 descriptor 和 snapshot 结合后生成 `ActionInstance`：

```text
ActionInstance
  action_instance_key
  action_id
  resolved_label
  resolved_target_ids[]
  visibility
  disabled_reason?
  risk
  confirmation?
  submenu_options[]
  provenance
```

`visibility`：

```text
available
disabled(reason_code, localized_reason)
hidden(permanent_unsupported | scope_inapplicable | user_hidden)
```

#### 结果反馈约定

- 只读/导航：成功可不提示，失败必须可见。
- 剪贴板：短暂通知，内容必须说明是文件对象还是路径文本。
- 文件写操作：显示结果；可恢复失败必须保持原文件不变。
- 长任务：使用非阻塞进度反馈；关闭进度窗口不等于取消操作。
- VCS 写操作：刷新仓库快照；失败显示可恢复原因。
- 外部应用/工具启动：Kite 负责启动失败；外部应用内部结果由该工具负责。
- 原生扩展：反馈与确认由 OS/扩展负责，Kite 只显示入口和启动错误。

---

## 2. Scope 分类

每个动作必须有且只有一个主 Scope。

| Scope | 作用对象 | 数量规则 | 示例 | 菜单规则 |
|---|---|---|---|---|
| `entity` | 单个 Entity | 恰好 1 个目标 | 重命名、显示属性 | 多选或背景菜单不得出现 |
| `collection` | 有序目标集合 | 1 个或多个；必须声明全部目标支持 | 复制、分享、批量删除、Open 多文件 | 多选必须检查所有目标；可显式声明 `container_fallback` |
| `container` | 当前目录/栏/设备容器 | 不依赖 Entity 目标 | 粘贴、新建、刷新 | 背景菜单可存在；文件菜单默认隐藏 |
| `workspace` | 工作区/根目录 | 作用于工作区而非单个文件 | Open In、初始化仓库 | 不得伪装成 Entity 动作 |
| `repository` | Git/SVN 工作副本 | 需要仓库状态 | 提交、拉取、Rebase | 文件级状态与仓库级动作必须分组 |
| `system` | OS 能力 | 由平台定义 | Finder/Explorer 定位、分享、属性 | 由平台执行 |
| `extension` | 第三方扩展 | 开放式集合 | TortoiseGit、云盘命令 | 只出现在 OS 原生平面 |

补充规则：

1. `collection` 动作可以在单目标上下文出现，但必须显式声明“单目标也安全”。
2. `entity` 动作绝不能被自动包装成“对第一个目标执行”。
3. 一个动作如果需要多个目标，不得在同一次调用中偷偷截断到主目标。
4. 多目标默认采用 `all_targets`：任一目标不满足前置条件时禁用并解释；只有 descriptor 显式声明 `supported_subset` 才允许部分执行，且 v1 默认不启用该模式。
5. 混合多仓选择时，仓库级动作隐藏；文件级动作只有在其语义不依赖仓库时保留。
6. `collection` 动作可以声明 `container_fallback = true`。背景菜单仍保持 `TargetSet.cardinality = 0`，Composer 在执行时把当前容器解析为目标；这不会把 Entity-only 动作带入背景菜单。
7. `entity` 动作不得声明 `container_fallback`。

---

## 3. Composer 流水线

每次菜单打开都必须经过同一条纯函数管线。输入是冻结快照和已验证的动作注册表，输出是可验证的菜单树。

```text
Collect
  → Normalize
  → Filter
  → Deduplicate
  → Resolve
  → Group
  → Project
  → Validate
```

### 3.1 各阶段契约

| 阶段 | 输入 | 输出 | 规则 |
|---|---|---|---|
| Collect | snapshot、注册表 | 候选 ActionDescriptor/Instance | 不读取渲染状态，不追加分隔线 |
| Normalize | 候选 | 规范化 ActionId、variant、标签参数 | 别名只能映射到已有 ID |
| Filter | 上下文与能力 | 可应用动作 | 永久不支持/作用域不适用先隐藏；临时不可用保留 |
| Deduplicate | 同层候选 | 唯一实例 | 使用 `ActionInstanceKey`；重复来源合并，保留最高优先级 provenance |
| Resolve | 目标、VCS 状态、权限 | available/disabled/hidden + reason + promotion hint | 冲突状态和操作状态在这一阶段判定 |
| Group | 可用实例 | 语义组与组内稳定顺序 | Provider 不得提供布局；promotion hint 只允许在既有组内提升 |
| Project | 语义菜单树 | GPUI/SwiftUI/AppKit 节点 | 投影器只负责渲染、焦点、滚动与键盘 |
| Validate | 投影前菜单 | 通过/失败 | 失败时回退安全菜单，保留原生入口 |

### 3.2 去重键

- 单实例动作：`ActionInstanceKey = action_id`。
- 枚举动作：`ActionInstanceKey = action_id + canonical_variant_key`。
- 同一层级不允许两个节点展开到同一个实例键。
- 同一个语义动作不得同时出现在一级菜单和子菜单中；例如“打开 Git 客户端”不能既是一级命令，又在 Git 子菜单中重复一次。
- 内容完全相同但 owner 不同的动作必须区分显示，例如 Kite 内建“提交…”与外部工具“在 TortoiseGit 中提交…”不能使用同一文案。

### 3.3 推荐的去重胜出顺序

同一动作有多个 Provider 候选时：

1. 状态更具体且已通过验证的 Provider。
2. Kite 语义平面优先于外部扩展平面。
3. 固定工具优先于最近使用工具。
4. 用户显式固定优先于自动发现。
5. 仍冲突则保留 `action_id` 固定顺序中的第一个，并记录诊断，不改变菜单语义。

---

## 4. 菜单不变量

以下规则必须进入自动测试，而不是只存在于设计文档。

1. 同一菜单层级不存在重复 `ActionInstanceKey`。
2. 背景菜单不包含 `entity` Scope 动作。
3. 多选菜单不包含仅支持单目标的动作。
4. 多选动作必须对所有目标成立，或按显式 `supported_subset` 规则显示数量；v1 默认全部成立。
5. 永久不支持的动作隐藏。
6. 当前作用域不适用的动作隐藏。
7. 临时不可用的动作显示为禁用，并提供面向用户的原因。
8. 子菜单不得为空；只有单项的子菜单要么提升动作，要么不渲染。
9. 分隔线不得出现在菜单开头、结尾或连续出现。
10. 危险动作固定在最后一个非系统动作组，并位于普通动作之后。
11. `system.native-menu` 必须是最后一个根级入口。
12. 原生入口必须独立于普通 Composer；普通动作生成失败时仍要保留。
13. 打开菜单不得改变选择、导航历史、预览对象或目录加载状态。
14. 菜单动作只作用于冻结的 `TargetSet`。
15. 所有可见动作必须定义 `ActionId`、Scope、Risk、Owner、Effect 和结果反馈。
16. 任何写入动作执行前必须复核目标存在性与状态版本。
17. 最近使用和频率只允许改变子菜单内部顺序，不得改变根级语义顺序。
18. 两个可见动作不得拥有相同路径标签却指向不同 owner/语义。

### 4.1 原生入口降级

若 OS 原生入口不可用：

- 仍然渲染 `system.native-menu`。
- 显示禁用与稳定的 `NATIVE_PLANE_UNAVAILABLE` 原因。
- 不得用 Kite 内建命令冒充完整原生平面。
- 普通 Composer 失败时，安全回退菜单至少包含：
  - `open.default`（单目标或显式支持时）
  - `system.reveal`（如果可用）
  - `system.native-menu`

---

## 5. 可见性、风险、排序与深度

### 5.1 可见性规则

| 情况 | 示例 | 处理 |
|---|---|---|
| 永久不支持 | 普通目录没有“解压”语义 | 隐藏 |
| 当前作用域不适用 | 背景菜单中的“重命名” | 隐藏 |
| 临时不可用 | 加载中、无暂存、冲突阻止提交、离线 | 显示并禁用，附原因 |
| 用户隐藏 | 用户隐藏某个工具 | 隐藏，可诊断模式显示 |
| 可选能力未安装 | 没有安装 Beyond Compare | 隐藏兼容工具；保留 Open With/Open In 入口策略 |

稳定原因代码至少包括：

```text
TARGET_LOADING
TARGET_MISSING
PERMISSION_DENIED
FILE_BUSY
NO_UNSTAGED_CHANGES
NO_STAGED_CHANGES
NO_UPSTREAM
NO_UNTRACKED_FILES
REMOTE_OFFLINE
OPERATION_IN_PROGRESS
UNRESOLVED_CONFLICTS
REBASE_IN_PROGRESS
CHERRY_PICK_IN_PROGRESS
REVERT_IN_PROGRESS
SVN_LOCKED
DEVICE_BUSY
NATIVE_PLANE_UNAVAILABLE
TOOL_INVALIDATED
```

### 5.2 风险等级

| 等级 | 定义 | 默认确认 |
|---|---|---|
| `none` | 只读、无系统状态变化 | 无 |
| `low` | 导航、剪贴板、启动工具 | 无 |
| `medium` | 可恢复写入、暂存、普通提交 | 通常无；覆盖或群操作可升级 |
| `high` | 批量删除、丢弃更改、清理未跟踪文件、Rebase 中止/跳过 | 必须确认 |
| `external_unknown` | OS/第三方扩展内部语义未知 | 由 OS 扩展负责 |

最终风险由基础风险加上下文修饰：

```text
final_risk = max(base_risk, context_modifiers)
```

例如：

- 单文件移到回收站：`medium`。
- 多选移到回收站：`high`。
- 设备命名空间删除：`high`。
- `git.discard`：`high`。
- `git.clean-untracked`：`high`。

### 5.3 稳定排序

根级语义组固定顺序：

```text
G00  Refresh
G10  Open & Launch
G20  Transfer
G30  Create & Organize
G40  Archive
G50  Agent & Tools
G60  Git / SVN
G70  System
G90  Destructive
G99  OS Native Plane
```

规则：

- 组内使用固定的 `order`，不因点击频率重排。
- 最近使用只出现在 `open.with`、`open.in`、`compare.with` 等枚举子菜单内部。
- 状态提升只能把当前恢复动作提升到 `G60` 内最前，不得把 Git 操作移到系统或打开组。
- 危险动作只能位于 `G90`。
- `system.native-menu` 始终位于 `G99` 最后。
- 不提供含义模糊的根级“更多”或“其他”；需要收纳时使用 `file-actions`、`archive`、`agent`、`git`、`svn`、`system` 等任务名。

### 5.4 菜单深度和长菜单

- 根级可见普通项软上限为 12，硬上限为 16；超过软上限时进入命名任务子菜单。
- 根级到普通叶节点最多 2 条边，即 `root → task submenu → leaf`。
- 只有 Git/SVN 的 `advanced` 区域允许第 3 层，且只能放低频、不可逆或高级操作。
- 任何子菜单在过滤后只剩 1 项时必须提升或不渲染。
- 菜单高度超过可视区域时必须滚动；键盘可到达所有项。
- 禁用项不得让键盘焦点跳过其解释文本；屏幕阅读器必须能读出原因。

---

## 6. 双平面模型

### 6.1 Plane A：Kite 语义平面

特点：

- Kite 维护稳定 `ActionId`、Scope、Risk、确认与反馈。
- 跨平台语义一致。
- 能进入自动化命令集合测试。
- Git/SVN 内建状态机属于此平面。

### 6.2 Plane B：OS 原生扩展平面

特点：

- 命令集合开放、平台相关、可能有动态子菜单。
- Windows `IContextMenu/IContextMenu2/IContextMenu3`、TortoiseGit、TortoiseSVN、云盘扩展属于此平面。
- macOS Services、LaunchServices、Finder 能力属于此平面。
- Kite 不解析、不复制、不重写其内部命令树。

### 6.3 边界规则

1. `system.native-menu` 是从 Plane A 进入 Plane B 的唯一通用入口。
2. Windows 必须用完整 Shell 菜单实现该入口，且应支持多选 `IShellItemArray`。
3. macOS 不存在与 Windows 完全等价的开放式 Shell 菜单。入口应打开可用的原生能力平面，例如 Services、LaunchServices 打开方式选择器和其他已验证的 AppKit 动作；不得宣称“完整复制 Windows Shell 菜单”。
4. 外部扩展命令不要求 Kite 定义细粒度 `ActionId`；入口本身有 `system.native-menu`，内部条目由 OS 识别。
5. 外部 Git GUI 可以作为 `open.in` 的工具选项，因为 Kite 只负责“把仓库交给已配置工具”；但其内部提交、拉取等命令不得复制进 `git` 子菜单。
6. 外部动作必须在可访问名称或描述中表明“由系统/外部工具执行”，不能让用户误以为是 Kite 内建动作。

平台投影：

```text
Windows:
  system.native-menu → IContextMenu(IShellItemArray)
macOS:
  system.native-menu → 已验证的 AppKit 原生能力平面
```

如果 macOS 原生平面不可用，入口仍存在并禁用，原因是 `NATIVE_PLANE_UNAVAILABLE`。

---

## 7. `Open` / `Open With` / `Open In` / `Compare With`

### 7.1 `open.default` — 默认打开

语义：执行目标的主激活行为，不要求用户选择工具。

规则：

- 文件：交给 OS 默认关联应用；如果 Kite 被注册为该类型默认处理器，则由 Kite 处理。
- 浏览器内的目录：在主浏览表面内进入该目录；这是 Kite 文件管理器声明的默认激活，不弹出 Finder/Explorer。
- 侧边栏目录：切换到该目录根。
- 搜索结果文件：按 OS 默认关联打开。
- 设备项目：交给命名空间的默认打开逻辑。
- 多选：仅当所有目标都是文件、且实现声明支持批量默认打开时显示；按可见顺序执行。
- 不得因为文件是文本、Markdown 或 XMind 就自动改成“在 Kite 编辑器中打开”；该行为必须通过 `open.in` 或注册的默认处理器表达。

### 7.2 `open.with` — 选择应用

语义：为本次打开选择一个明确的应用。

规则：

- v1 只适用于文件，不适用于目录、仓库和设备命名空间项目。
- 子菜单顺序：
  1. 默认应用（明确标注“默认应用”）
  2. 最近使用（最多 5 个）
  3. 系统确认兼容的已安装应用
  4. 选择其他应用…
- 只列出 LaunchServices/Shell 确认可以处理目标类型的应用。
- 多文件必须只列出声明支持全部选中文件的应用。
- v1 不提供“始终使用此应用打开”；该设置回到 Kite 或 OS 设置。
- Kite 编辑器、终端、IDE 不得混入兼容应用列表；它们属于 `open.in`。

### 7.3 `open.in` — 交给工具

语义：把文件、目录、仓库或工作区交给一个已验证的外部或内建工具。

规则：

- 工具必须声明支持的目标形态：文件、目录、多路径、仓库根、工作区根。
- 只列出已验证可用的工具。
- 工具顺序：固定工具 → 最近使用 → 其余已验证工具。
- 如果只有一个可用工具，可提升为根级明确命令；此时不得同时保留一个只含该项的重复子菜单。
- Kite 文本编辑器、Kite Mindmap、Kite Git 客户端都是工具选项，不得再以独立临时命令出现在其他位置。
- `open.in` 不是 `open.with` 的目录版本；两者语义不同，不能合并。

### 7.4 `compare.with` — 比较或合并

语义：把目标交给差异/合并工具。

规则：

- 单文件：选择一个比较对象或通过工具选择器进行比较。
- 两个文件：直接比较两个目标。
- 多于两个文件：v1 隐藏，避免不同工具语义不一致。
- 只列出声明 `diff`/`merge` 能力的工具。
- VCS 的“与 HEAD 比较”“与修订比较”使用 `git.*`/`svn.*` 动作，不复用 `compare.with`。
- 目录比较在 v1 不承诺；如后续支持，必须新增明确 ID，例如 `compare.directory`。

### 7.5 边界示例

| 用户意图 | 正确动作 | 错误做法 |
|---|---|---|
| 用默认程序打开 PDF | `open.default` | 把“用 Preview 打开”硬编码成 Open |
| 选择 Photoshop 打开图片 | `open.with` | 将 Photoshop 放进 Open In |
| 在 VS Code 打开仓库 | `open.in` | 伪装成 Open With |
| 在终端打开当前目录 | `open.in` | 新建一个独立的“终端”临时 Provider |
| 比较两个文件 | `compare.with` | 放入 Open With |
| 查看 Git 文件历史 | `git.log` | 复用 Open With 或 Compare With |

---

## 8. Git/SVN 契约

### 8.1 四个 VCS 上下文

| 上下文 | 目标 | 动作示例 |
|---|---|---|
| 文件上下文 | 一个或多个文件 | add、unstage、diff、log、discard |
| 选择上下文 | 当前多选 | 批量 add/unstage/diff/discard，严格全目标安全 |
| 工作区上下文 | 仓库根或子目录 | commit、conflict resolve、clean、rebase |
| 仓库/远端上下文 | 上游、远程、操作状态 | pull、push、fetch、remotes、operation continue/abort |

### 8.2 冻结与复核

1. 菜单打开时读取 `RepositorySnapshot`。
2. Composer 只基于该快照决定可见性和禁用原因。
3. 用户执行写动作前，分发器重新读取 `state_revision`。
4. 如果目标、索引、冲突或操作状态发生漂移：
   - 不执行；
   - 关闭/刷新菜单状态；
   - 提示“仓库状态已变化，请重新选择”。

### 8.3 Git 状态字段

至少需要：

```text
repository_root
worktree_root
target_relations: outside | root | inside | untracked | ignored | submodule
branch_or_detached_head
upstream_present
ahead
behind
staged_count
unstaged_count
untracked_count
conflicted_paths
operation:
  none
  merge
  rebase
  cherry_pick
  revert
  bisect
  busy
```

目标与 worktree 的关系：

| 关系 | 菜单规则 |
|---|---|
| `outside` | 不显示该仓库的 Git 动作 |
| `root` | 允许仓库级、远端级、恢复级动作 |
| `inside` | 允许文件/目录限定动作；仓库级动作只从容器或显式仓库上下文提升 |
| `untracked` | 允许 `git.add`、文件 diff、历史不适用；不得伪装为已跟踪 |
| `ignored` | 默认隐藏 Git 动作，除非用户显式进入仓库高级模式 |
| `submodule` | 只显示子模块/独立仓库客户端动作，不在父仓库上批量执行 |

多选目标必须属于同一个 repository/worktree 才能显示仓库动作；跨仓库选择只保留不依赖仓库的文件动作。

动作前置条件：

| 动作 | 前置条件 |
|---|---|
| `git.add` | 目标在 worktree 内，存在可添加的修改/未跟踪项 |
| `git.unstage` | 目标有暂存内容 |
| `git.commit` | 无进行中的操作（或状态机明确允许该操作），暂存区非空，冲突已解决 |
| `git.diff` | 目标有可比较内容，或仓库可用 |
| `git.log` | 仓库存在 |
| `git.discard` | 目标为已跟踪且有工作区/暂存变化，必须高风险确认 |
| `git.pull` | 正常状态、工作区干净、存在上游、在线 |
| `git.push` | 正常状态、存在上游、在线；无 ahead 时禁用并解释 |
| `git.fetch-current` | 正常状态、在线 |
| `git.fetch-all` | 正常状态、在线 |
| `git.remotes.manage` | 仓库存在 |
| `git.clone` | 当前容器可写，在线 |
| `git.init` | 当前容器可写，且尚未是仓库根 |
| `git.rebase.interactive` | 正常状态、工作区干净、没有进行中的操作 |
| `git.clean-untracked` | 仓库存在且存在未跟踪项，必须高风险确认 |
| `git.conflict.resolve` | 存在冲突 |
| `git.operation.continue` | 存在可继续操作，且没有未解决冲突 |
| `git.operation.skip` | 当前操作支持 skip |
| `git.operation.abort` | 当前操作支持 abort，高风险确认 |

### 8.4 操作状态提升

| 当前状态 | 必须优先显示 |
|---|---|
| 正常 | add/unstage/diff/commit/pull/push/fetch |
| 冲突 | 解决冲突、继续或中止对应操作 |
| Merge | 继续 Merge、中止 Merge；冲突时先解决冲突 |
| Rebase | 继续 Rebase、跳过、中止 Rebase |
| Cherry-pick | 继续、跳过、中止 Cherry-pick |
| Revert | 继续、跳过、中止 Revert |
| 忙 | 所有 VCS 写动作禁用，原因 `OPERATION_IN_PROGRESS` |

特殊状态动作优先于普通动作；普通动作不得在不适用的状态中显示为可用。

### 8.5 SVN 状态字段

SVN 使用 working copy，而不是 Git index：

```text
working_copy_root
versioned_state
added_paths
modified_paths
deleted_paths
conflicted_paths
locked_paths
switched_paths
changelists
operation: none | update | merge | switch | commit | cleanup_required
```

规则：

- 不提供 `svn.unstage`。
- `svn.update`、`svn.commit`、`svn.diff`、`svn.log`、`svn.revert`、`svn.cleanup` 必须依赖 working-copy 状态。
- `svn.lock`/`svn.unlock` 只在支持锁定的目标上出现。
- `svn.revert` 是高风险动作，必须确认。
- `svn.cleanup` 在普通状态可作为低/中风险恢复动作，在锁或中断状态中提升为恢复入口。

### 8.6 VCS 与外部扩展

- Kite 的 Git 客户端通过 `open.in` 的工具选项打开，不再有独立的重复“打开 Git 客户端”命令。
- TortoiseGit/TortoiseSVN 的命令树只通过 `system.native-menu` 访问。
- 外部 Git GUI 可以作为已验证工具进入 `open.in`，但只能执行“打开仓库”，不得复制外部客户端内部动作。
- 内建命令与外部命令文案必须区分，例如“提交…”与“在 TortoiseGit 中提交…”。后者若存在，必须在原生平面中显示。

---

## 9. 动作目录

下表是 v1 的核心语义目录。`Risk` 为基础风险，最终风险可以按上下文提升；`Feedback` 是默认反馈。未列出的新动作必须先追加到此目录并定义稳定 ID，不能直接由 Provider 临时生成。

### 9.1 打开、导航与剪贴板

| ActionId | 默认文案 | Scope | Risk | Effect | 规则 |
|---|---|---|---|---|---|
| `navigation.refresh` | 刷新 | container | none | 读取/刷新 | 背景和栏表面可用；文件实体菜单默认隐藏 |
| `navigation.open-parent` | 打开所在文件夹 | container | none | 导航 | 用于搜索结果；在 Kite 内导航并选中目标 |
| `navigation.edit-path` | 编辑路径… | container | none | 导航 | 仅工具栏/地址路径辅助 Surface |
| `tab.new` | 新建页签 | workspace | low | 导航 | 辅助 Surface |
| `tab.close` | 关闭页签 | workspace | low | 导航 | 辅助 Surface |
| `open.default` | 打开 | collection | low | 进程启动/导航 | 单文件或全部文件；目录按浏览表面默认激活 |
| `open.with` | 打开方式… | collection | low | 进程启动 | 仅文件；见第 7 节 |
| `open.in` | 在工具中打开 | workspace | low | 进程启动 | 工具必须声明目标形态 |
| `open.quick-look` | 快速查看 | collection | none | 读取 | 只读预览，不改变选择和导航 |
| `compare.with` | 比较… | collection | low | 进程启动 | 单文件或恰好两个文件 |
| `clipboard.copy` | 复制 | collection | low | 剪贴板 | 文件对象，不是路径文本 |
| `clipboard.cut` | 剪切 | collection | low | 剪贴板 | 只对可移动目标 |
| `clipboard.paste` | 粘贴 | container | medium | 文件变更 | 目标为容器；只显示可写容器 |
| `clipboard.copy-to` | 复制到… | collection | medium | 文件变更 | 目标为选择集，目标目录由用户选择 |
| `clipboard.copy-path` | 复制完整路径 | collection | none | 剪贴板 | 多选时使用平台稳定的换行格式，并通过描述明确“路径文本” |

### 9.2 文件与收藏

| ActionId | 默认文案 | Scope | Risk | Effect | 规则 |
|---|---|---|---|---|---|
| `file.new-folder` | 新建文件夹… | container | medium | 文件变更 | 只对可写容器 |
| `file.new-file` | 新建文件… | container | medium | 文件变更 | 只对可写容器 |
| `file.rename` | 重命名… | entity | medium | 文件变更 | 多选隐藏 |
| `file.move-to-trash` | 移到回收站/废纸篓 | collection | medium → high | 文件变更 | 多选、设备、不可恢复时升级为 high |
| `favorite.add` | 添加到收藏 | collection | low | 文件变更 | 可作用于文件、目录或背景当前容器 |
| `favorite.remove` | 从收藏移除 | collection | low | 文件变更 | 收藏条目或选择集 |
| `archive.compress.zip` | 压缩为 ZIP | collection | medium | 文件变更 | 选择集必须全部可压缩 |
| `archive.compress.tar-gz` | 压缩为 TAR.GZ | collection | medium | 文件变更 | 选择集必须全部可压缩 |
| `archive.extract-here` | 解压到当前文件夹 | entity | medium | 文件变更 | 仅受支持压缩包 |
| `archive.extract-to` | 解压到… | entity | medium | 文件变更 | 仅受支持压缩包 |

### 9.3 Agent

| ActionId | 默认文案 | Scope | Risk | Effect | 规则 |
|---|---|---|---|---|---|
| `agent.ask` | 询问 Agent… | collection | none | 打开 Kite UI | 不自动发送 |
| `agent.explain-file` | 解释此文件… | collection | none | 打开 Kite UI | 仅文本类文件 |
| `agent.summarize-folder` | 总结此文件夹… | collection | none | 打开 Kite UI | 仅目录 |
| `agent.review-changes` | 审查选中变更… | collection | none | 打开 Kite UI | 仅 VCS 可用上下文 |
| `agent.generate-commit-message` | 生成提交说明… | collection | none | 打开 Kite UI | 仅 VCS 可用上下文 |
| `agent.configure` | 配置 Agent… | workspace | none | 打开设置 | 不依赖 Agent 已配置 |

### 9.4 Git

| ActionId | 默认文案 | Scope | Risk | Effect | 规则 |
|---|---|---|---|---|---|
| `git.add` | 暂存 | collection | medium | VCS 写 | 只对可添加路径 |
| `git.unstage` | 取消暂存 | collection | medium | VCS 写 | 无暂存时禁用 |
| `git.commit` | 提交… | repository | medium | VCS 写 | 暂存区为空时禁用 |
| `git.pull` | 拉取… | repository | medium | VCS 写/网络 | 工作区脏、冲突、操作中时禁用 |
| `git.push` | 推送… | repository | medium | VCS 写/网络 | 无上游或非 ahead 时禁用并解释 |
| `git.fetch-current` | 获取当前远程 | repository | low | 网络读 | 离线禁用 |
| `git.fetch-all` | 获取全部远程 | repository | low | 网络读 | 离线禁用 |
| `git.remotes.manage` | 管理远程… | repository | medium | VCS 写 | 仓库存在时可用 |
| `git.clone` | 克隆仓库… | workspace | medium | VCS 写/网络 | 当前容器可写 |
| `git.init` | 初始化仓库 | workspace | medium | VCS 写 | 当前容器不是仓库根 |
| `git.diff` | 显示差异 | collection | low | VCS 读 | 有可比较内容时可用 |
| `git.log` | 查看历史 | repository | none | VCS 读 | 可按文件/目录限定 |
| `git.rebase.interactive` | 交互式 Rebase… | repository | high | VCS 写 | 干净工作区且无操作时可用 |
| `git.discard` | 放弃更改… | collection | high | VCS 写 | 高风险确认；不得用“还原”模糊文案 |
| `git.clean-untracked` | 清理未跟踪文件… | repository | high | VCS 写 | 高风险确认；不得与 SVN cleanup 混淆 |
| `git.conflict.resolve` | 解决冲突… | repository | medium | VCS 写 | 仅冲突状态 |
| `git.operation.continue` | 继续操作… | repository | medium | VCS 写 | 由当前操作动态生成文案 |
| `git.operation.skip` | 跳过当前步骤… | repository | high | VCS 写 | 仅支持 skip 的操作 |
| `git.operation.abort` | 中止操作… | repository | high | VCS 写 | 高风险确认 |

### 9.5 SVN

| ActionId | 默认文案 | Scope | Risk | Effect | 规则 |
|---|---|---|---|---|---|
| `svn.update` | 更新 | repository | medium | VCS 写/网络 | 离线禁用 |
| `svn.add` | 添加 | collection | medium | VCS 写 | 未版本化路径 |
| `svn.commit` | 提交… | repository | medium | VCS 写 | 有可提交变更 |
| `svn.lock` | 锁定 | collection | medium | VCS 写 | 仅可锁定目标 |
| `svn.unlock` | 解锁 | collection | medium | VCS 写 | 仅已锁定目标 |
| `svn.diff` | 显示差异 | collection | low | VCS 读 | 有可比较内容 |
| `svn.log` | 显示日志 | repository | none | VCS 读 | 工作副本存在 |
| `svn.revert` | 还原… | collection | high | VCS 写 | 高风险确认 |
| `svn.cleanup` | 清理工作副本 | repository | medium | VCS 写 | 锁或中断状态可提升为恢复入口 |

### 9.6 系统、设备与原生平面

| ActionId | 默认文案 | Scope | Risk | Effect | 规则 |
|---|---|---|---|---|---|
| `system.reveal` | 在 Finder/Explorer 中显示 | collection | low | OS UI/进程启动 | 不能替代 Open In |
| `system.share` | 分享… | collection | low | OS UI | 多选时分享选择集 |
| `system.properties` | 属性/显示简介 | entity | none | OS UI | 多选默认隐藏 |
| `device.delete` | 删除设备项目 | entity | high | 删除 | 与 `file.move-to-trash` 区分；必须确认 |
| `system.native-menu` | 显示系统原生菜单… | system | external_unknown | 外部未知 | `plane = os_native_extension`；始终为最后根级入口；由 OS/扩展负责 |

---

## 10. 上下文矩阵

### 10.1 矩阵规则

- “文件/目录”优先指 `browser.list` 和 `browser.columns`；`preview`、`search.results`、`sidebar` 是独立 Surface。
- 表中条件默认在能力全部可用时展示；未列出的动作在该上下文隐藏。
- 所有上下文都必须有 `system.native-menu`，即便其他动作全部隐藏。
- 所有菜单打开行为不得改变选择、预览、导航和加载状态。
- 多选上下文使用“全部目标支持”规则。

### 10.2 文件

| 分类 | 动作 |
|---|---|
| 打开与启动 | `open.default`、`open.with`、`open.in`、`open.quick-look` |
| 文件操作 | `clipboard.copy`、`clipboard.cut`、`clipboard.copy-to`、`clipboard.copy-path`、`file.rename`、`favorite.add` |
| 压缩 | `archive.compress.zip`、`archive.compress.tar-gz`；压缩包额外显示解压 |
| Agent | `agent.ask`、文本文件 `agent.explain-file`、Git 仓库 `agent.review-changes`/`agent.generate-commit-message`、`agent.configure` |
| Git | 文件安全的 `git.add`、`git.unstage`、`git.diff`、`git.log`、`git.discard`；冲突时 `git.conflict.resolve` |
| SVN | 文件安全的 `svn.add`、`svn.lock`、`svn.unlock`、`svn.diff`、`svn.log`、`svn.revert` |
| 系统 | `system.reveal`、`system.share`、`system.properties` |
| 危险 | `file.move-to-trash` |
| 原生 | `system.native-menu` |

禁止：`open.with` 之外的目录专用动作、`file.new-folder`、`file.new-file`、`clipboard.paste` 在文件实体菜单默认不出现；它们属于 Container。

### 10.3 目录

| 分类 | 动作 |
|---|---|
| 打开与启动 | `open.default`、`open.in`、`open.quick-look` |
| 文件操作 | `clipboard.copy`、`clipboard.cut`、`clipboard.copy-to`、`clipboard.copy-path`、`clipboard.paste`、`file.new-folder`、`file.new-file`、`file.rename`、`favorite.add` |
| 压缩 | `archive.compress.zip`、`archive.compress.tar-gz` |
| Agent | `agent.ask`、`agent.summarize-folder`、仓库上下文中的 `agent.review-changes`/`agent.generate-commit-message`、`agent.configure` |
| Git | 初始化/克隆仅在非仓库目录；仓库目录显示 `git.commit`、`git.diff`、`git.log`、`git.pull`、`git.push`、`git.fetch-current`、`git.fetch-all`、`git.remotes.manage`、`git.rebase.interactive`、`git.clean-untracked` 及适用文件/选择动作 |
| SVN | `svn.update`、`svn.add`、`svn.commit`、`svn.lock`、`svn.unlock`、`svn.diff`、`svn.log`、`svn.revert`、`svn.cleanup` |
| 系统 | `system.reveal`、`system.share`、`system.properties` |
| 危险 | `file.move-to-trash` |
| 原生 | `system.native-menu` |

禁止：`open.with`、`compare.with`、`archive.extract-here`、`archive.extract-to`（目录不是压缩包）。

### 10.4 多选

多选目标是冻结的有序集合；默认只显示对集合安全的动作。

| 条件 | 动作 |
|---|---|
| 所有目标为文件 | `open.default`、`open.with`、`open.quick-look`、`clipboard.copy`、`clipboard.cut`、`clipboard.copy-to`、`clipboard.copy-path`、`system.reveal`、`system.share`、`file.move-to-trash` |
| 恰好两个文件且存在差异工具 | `compare.with` |
| 所有目标可压缩 | 压缩动作；实现可合并为一个压缩输出 |
| 同一 Git 仓库且动作全部支持 | `git.add`、`git.unstage`、`git.diff`、`git.discard` |
| 同一 SVN 工作副本且动作全部支持 | `svn.add`、`svn.lock`、`svn.unlock`、`svn.diff`、`svn.revert` |
| Agent 可用且上下文合法 | `agent.ask`、`agent.review-changes`、`agent.generate-commit-message`、`agent.configure` |
| 任意本地目标 | `system.native-menu` |

禁止：`file.rename`、`file.new-folder`、`file.new-file`、`clipboard.paste`、`favorite.add`（逐项收藏不是 v1 批量语义）、`system.properties`、任何只支持单个目标的命令。

### 10.5 空白背景

背景的 `TargetSet.cardinality = 0`，`container` 是当前目录/栏。背景菜单不得包含 Entity 动作。

| 分类 | 动作 |
|---|---|
| 刷新 | `navigation.refresh` |
| 工具启动 | `open.in`（当前容器作为工作区） |
| 文件操作 | `clipboard.paste`、`file.new-folder`、`file.new-file`、`favorite.add` |
| 压缩 | `archive.compress.zip`、`archive.compress.tar-gz`（压缩当前容器时） |
| Agent | `agent.ask`、`agent.summarize-folder`、仓库上下文中的审查/提交说明、`agent.configure` |
| Git | 当前仓库的 `git.commit`、`git.diff`、`git.log`、`git.pull`、`git.push`、`git.fetch-current`、`git.fetch-all`、`git.remotes.manage`、`git.rebase.interactive`、`git.clean-untracked` |
| SVN | 当前工作副本的 `svn.update`、`svn.commit`、`svn.diff`、`svn.log`、`svn.cleanup` |
| 系统 | `system.reveal`、`system.share` |
| 原生 | `system.native-menu` |

背景中的 `favorite.add`、压缩、Agent 和系统 Collection 动作必须声明 `container_fallback = true`；仓库动作使用 `WorkspaceSnapshot`/`RepositorySnapshot` 的根路径。

禁止：`open.default`（没有 Entity 目标）、`open.with`、`open.quick-look`、`compare.with`、`file.rename`、`file.move-to-trash`、`device.delete`。

### 10.6 搜索结果

| 分类 | 动作 |
|---|---|
| 打开与定位 | `open.default`、`open.with`、`open.in`、`open.quick-look` |
| 导航 | `navigation.open-parent` |
| 路径 | `clipboard.copy-path` |
| 系统 | `system.reveal`、`system.native-menu` |

搜索结果不得出现新建、粘贴、重命名、删除、压缩、Git/SVN 写操作。搜索结果是高流动性 Surface，菜单只做位置和文件身份相关动作。

### 10.7 侧边栏/收藏

| 条目 | 动作 |
|---|---|
| 位置 | `open.default`、`open.in`、`favorite.add`、`system.reveal`、`system.properties`、`system.native-menu` |
| 收藏目录/文件 | `open.default`、`open.in`（目录）、`clipboard.copy-path`、`favorite.remove`、`system.reveal`、`system.properties`、`system.native-menu` |
| 卷/设备 | `open.default`、`system.reveal`、`system.properties`、`system.native-menu` |

侧边栏不得出现文件内容级写操作、压缩、批量 VCS 或重命名。

### 10.8 预览更多菜单

预览的“更多”菜单必须使用与当前预览目标相同的文件上下文，而不是另写一套 SwiftUI/GPUI 命令。

- `TargetSet` 来自当前预览目标，不因右键而改变预览对象。
- 允许：文件上下文中的打开、文件操作、压缩/解压、Agent、文件级 Git/SVN、系统动作、删除。
- 禁止：`navigation.refresh`、`clipboard.paste`、`file.new-folder`、`file.new-file`（预览 Surface 不是容器）。
- 预览底部固定动作网格不等同于菜单；网格中的动作必须有相同 `ActionId`，不得形成第二套语义。

### 10.9 设备/Shell 命名空间

设备命名空间目标由 `locator = namespace_uri` 表示，Kite 只能使用验证过的命名空间能力。

| 目标 | 动作 |
|---|---|
| 设备子项 | `open.default`、`clipboard.copy`（若可读）、`clipboard.copy-path`（若命名空间支持）、`device.delete`（若命名空间声明可删除）、`system.properties`、`system.native-menu` |
| 设备容器 | `navigation.refresh`、`open.default`、`clipboard.paste`（若可写）、`file.new-folder`/`file.new-file`（若命名空间声明支持）、`system.native-menu` |

禁止：

- `open.with`，除非平台原生平面确认存在应用选择器；
- `open.in`，除非工具声明支持命名空间 locator；
- `compare.with`；
- 所有 Git/SVN；
- 使用普通文件路径假设对设备项目做重命名、移动到回收站或属性查询。

---

## 11. 期望命令集合夹具

以下是 #222 要求的最小 golden fixture 集合。实现可以增加节点，但不得改变基线顺序、语义 Scope 或安全规则。

### 11.1 通用基线配置

除特别说明外：

- Kite 是主菜单；用户没有隐藏动作。
- Agent、Kite Editor、终端、编辑器、差异工具已验证可用。
- Git 仓库可用，SVN 不可用。
- 能力完整，路径本地可读写。
- 临时不可用项必须保留并带 reason；测试快照必须记录 reason。

为了保持测试可读，子菜单用 `{ ... }` 表示，`disabled(reason)` 表示临时不可用；这仍是命令集合快照的一部分。

### 11.2 `file.text.git-modified`

目标：一个已跟踪、有未暂存修改的文本文件，位于 Git 仓库。

```text
root:
  open.default
  open.with
  open.in
  open.quick-look
  file-actions {
    clipboard.copy
    clipboard.cut
    clipboard.copy-to
    clipboard.copy-path
    file.rename
    favorite.add
  }
  archive {
    archive.compress.zip
    archive.compress.tar-gz
  }
  agent {
    agent.ask
    agent.explain-file
    agent.review-changes
    agent.generate-commit-message
    agent.configure
  }
  git {
    git.add
    git.unstage(disabled:NO_STAGED_CHANGES)
    git.diff
    git.log
    git.discard
  }
  system {
    system.reveal
    system.share
    system.properties
  }
  file.move-to-trash
  system.native-menu
```

不得出现：`clipboard.paste`、`file.new-folder`、`file.new-file`、`open.with` 以外的目录应用动作、`compare.with`（单文件）、`git.commit`（仓库级动作在文件上下文默认下沉）。

### 11.3 `directory.repo-root.dirty`

目标：Git 仓库根目录，容器可写、可粘贴；工作区有未暂存修改和至少一个未跟踪项。

```text
root:
  navigation.refresh
  open.default
  open.in
  open.quick-look
  file-actions {
    clipboard.copy
    clipboard.cut
    clipboard.copy-to
    clipboard.paste
    clipboard.copy-path
    file.new-folder
    file.new-file
    file.rename
    favorite.add
  }
  archive {
    archive.compress.zip
    archive.compress.tar-gz
  }
  agent {
    agent.ask
    agent.summarize-folder
    agent.review-changes
    agent.generate-commit-message
    agent.configure
  }
  git {
    git.add
    git.commit(disabled:NO_STAGED_CHANGES)
    git.diff
    git.log
    git.pull(disabled:NO_UPSTREAM)
    git.push(disabled:NO_UPSTREAM)
    git.fetch-current
    git.fetch-all
    git.remotes.manage
    git.rebase.interactive
    git.clean-untracked
  }
  system {
    system.reveal
    system.share
    system.properties
  }
  file.move-to-trash
  system.native-menu
```

不得出现：`open.with`、`compare.with`、`archive.extract-here`、`archive.extract-to`、`git.init`、`git.clone`。

### 11.4 `multi.two-files.same-repo`

目标：两个已跟踪、可比较、同一 Git 仓库的文本文件。

```text
root:
  open.default
  open.with
  open.in
  open.quick-look
  compare.with
  file-actions {
    clipboard.copy
    clipboard.cut
    clipboard.copy-to
    clipboard.copy-path
  }
  archive {
    archive.compress.zip
    archive.compress.tar-gz
  }
  agent {
    agent.ask
    agent.review-changes
    agent.generate-commit-message
    agent.configure
  }
  git {
    git.add
    git.unstage(disabled:NO_STAGED_CHANGES)
    git.diff
    git.discard
  }
  system {
    system.reveal
    system.share
  }
  file.move-to-trash
  system.native-menu
```

不得出现：`file.rename`、`favorite.add`、`clipboard.paste`、`file.new-folder`、`file.new-file`、`system.properties`、任何只支持单目标的 Git 历史或仓库级提交动作。

### 11.5 `background.repo-root.paste`

目标：仓库根背景，没有 Entity 目标。

```text
root:
  navigation.refresh
  open.in
  file-actions {
    clipboard.paste
    file.new-folder
    file.new-file
  }
  favorite.add
  archive {
    archive.compress.zip
    archive.compress.tar-gz
  }
  agent {
    agent.ask
    agent.summarize-folder
    agent.review-changes
    agent.generate-commit-message
    agent.configure
  }
  git {
    git.commit(disabled:NO_STAGED_CHANGES)
    git.diff
    git.log
    git.pull(disabled:NO_UPSTREAM)
    git.push(disabled:NO_UPSTREAM)
    git.fetch-current
    git.fetch-all
    git.remotes.manage
    git.rebase.interactive
    git.clean-untracked
  }
  system {
    system.reveal
    system.share
  }
  system.native-menu
```

不得出现：`open.default`、`open.with`、`open.quick-look`、`compare.with`、`clipboard.copy`、`clipboard.cut`、`clipboard.copy-to`、`clipboard.copy-path`、`file.rename`、`file.move-to-trash`、`device.delete`。

### 11.6 `search.single-text-file`

目标：搜索结果列表中的一个本地文本文件；不在仓库或仓库状态未知。

```text
root:
  open.default
  open.with
  open.in
  open.quick-look
  navigation.open-parent
  clipboard.copy-path
  system.reveal
  system.native-menu
```

不得出现：新建、粘贴、重命名、删除、压缩、Agent、Git/SVN、分享、属性。

### 11.7 `sidebar.favorite.directory`

目标：收藏中的一个本地目录。

```text
root:
  open.default
  open.in
  clipboard.copy-path
  favorite.remove
  system.reveal
  system.properties
  system.native-menu
```

不得出现：`open.with`、`compare.with`、`clipboard.copy`、`clipboard.cut`、`clipboard.paste`、`file.new-folder`、`file.new-file`、`file.rename`、`file.move-to-trash`、Git/SVN。

### 11.8 `preview.text-file.git-modified`

目标：预览面板中当前文件与 `file.text.git-modified` 相同。

```text
root:
  open.default
  open.with
  open.in
  open.quick-look
  file-actions {
    clipboard.copy
    clipboard.cut
    clipboard.copy-to
    clipboard.copy-path
    file.rename
    favorite.add
  }
  archive {
    archive.compress.zip
    archive.compress.tar-gz
  }
  agent {
    agent.ask
    agent.explain-file
    agent.review-changes
    agent.generate-commit-message
    agent.configure
  }
  git {
    git.add
    git.unstage(disabled:NO_STAGED_CHANGES)
    git.diff
    git.log
    git.discard
  }
  system {
    system.reveal
    system.share
    system.properties
  }
  file.move-to-trash
  system.native-menu
```

不得出现：`navigation.refresh`、`clipboard.paste`、`file.new-folder`、`file.new-file`、目录动作。

### 11.9 `device.namespace.item`

目标：设备子项，可读、可复制，命名空间不支持重命名；设备声明可删除。

```text
root:
  open.default
  clipboard.copy
  clipboard.copy-path
  device.delete
  system.properties
  system.native-menu
```

不得出现：`open.with`、`open.in`、`open.quick-look`、`compare.with`、`file.rename`、`file.move-to-trash`、所有 Git/SVN。

---

## 12. 当前实现到新模型的映射

### 12.1 Windows/GPUI 当前入口

当前 Rust 菜单由 `src/actions.rs` 的 `ContextMenuCommand`/`VcsMenuCommand` 和 `src/context_menu/mod.rs` 的 Provider 生成。

| 当前命令/文案 | 新 ActionId | 备注 |
|---|---|---|
| `ContextMenuCommand::Refresh` / 刷新 | `navigation.refresh` | 仅非 compact 策略；新模型按 Surface 决定 |
| `ContextMenuCommand::Open` / 打开文件/文件夹 | `open.default` | 目录标签统一为“打开”，类型由目标决定 |
| `ContextMenuCommand::Copy` / 复制 | `clipboard.copy` | 与 macOS“拷贝”统一文案 |
| `ContextMenuCommand::CopyTo` / 拷贝到… | `clipboard.copy-to` | 统一“复制到…” |
| `ContextMenuCommand::CopyPath` / 复制完整路径 | `clipboard.copy-path` | macOS 的“拷贝路径”归一 |
| `ContextMenuCommand::Cut` / 剪切 | `clipboard.cut` | |
| `ContextMenuCommand::Paste` / 粘贴 | `clipboard.paste` | Container |
| `ContextMenuCommand::NewFolder` / 新建文件夹 | `file.new-folder` | |
| `ContextMenuCommand::NewFile` / 新建文件 | `file.new-file` | |
| `ContextMenuCommand::AddFavorite` / 添加到收藏 | `favorite.add` | |
| `ContextMenuCommand::RemoveFavorite` / 从收藏移除 | `favorite.remove` | 当前枚举存在但 Composer 未统一生成 |
| `ContextMenuCommand::Rename` / 重命名 | `file.rename` | |
| `ContextMenuCommand::Recycle` / 移到回收站 | `file.move-to-trash` | 设备目标改用 `device.delete` |
| `ContextMenuCommand::ShowProperties` / 属性 | `system.properties` | |
| `ContextMenuCommand::CompressZip` | `archive.compress.zip` | |
| `ContextMenuCommand::CompressTarGz` | `archive.compress.tar-gz` | |
| `ContextMenuCommand::ExtractHere` | `archive.extract-here` | |
| `ContextMenuCommand::ExtractTo` | `archive.extract-to` | |
| `ContextMenuCommand::Agent(Ask)` | `agent.ask` | |
| `ContextMenuCommand::Agent(ExplainFile)` | `agent.explain-file` | |
| `ContextMenuCommand::Agent(SummarizeFolder)` | `agent.summarize-folder` | |
| `ContextMenuCommand::Agent(ReviewChanges)` | `agent.review-changes` | |
| `ContextMenuCommand::Agent(GenerateCommitMessage)` | `agent.generate-commit-message` | |
| `ContextMenuCommand::Agent(Configure)` | `agent.configure` | |
| `VcsMenuCommand::GitOpenClient` / `OpenGitClient` | `open.in` / tool `kite.git-client` | 不再作为 Git 子菜单内的重复命令 |
| `VcsMenuCommand::GitAdd` | `git.add` | |
| `VcsMenuCommand::GitUnstage` | `git.unstage` | |
| `VcsMenuCommand::GitCommit` | `git.commit` | |
| `VcsMenuCommand::GitPull` | `git.pull` | |
| `VcsMenuCommand::GitPush` | `git.push` | |
| `VcsMenuCommand::GitFetch` | `git.fetch-current` | |
| `VcsMenuCommand::GitFetchAll` | `git.fetch-all` | |
| `VcsMenuCommand::GitManageRemotes` | `git.remotes.manage` | |
| `VcsMenuCommand::GitClone` | `git.clone` | |
| `VcsMenuCommand::GitInit` | `git.init` | |
| `VcsMenuCommand::GitDiff` | `git.diff` | |
| `VcsMenuCommand::GitLog` | `git.log` | |
| `VcsMenuCommand::GitRebase` | `git.rebase.interactive` | |
| `VcsMenuCommand::GitRevert` | `git.discard` | 当前底层为 restore HEAD；新文案“放弃更改…” |
| `VcsMenuCommand::GitClean` | `git.clean-untracked` | 新文案明确清理未跟踪文件 |
| `VcsMenuCommand::SvnUpdate` | `svn.update` | |
| `VcsMenuCommand::SvnAdd` | `svn.add` | |
| `VcsMenuCommand::SvnCommit` | `svn.commit` | |
| `VcsMenuCommand::SvnLock` | `svn.lock` | |
| `VcsMenuCommand::SvnUnlock` | `svn.unlock` | |
| `VcsMenuCommand::SvnDiff` | `svn.diff` | |
| `VcsMenuCommand::SvnLog` | `svn.log` | |
| `VcsMenuCommand::SvnRevert` | `svn.revert` | |
| `VcsMenuCommand::SvnCleanup` | `svn.cleanup` | |
| `ContextMenuCommand::SystemMenu` / 显示更多选项 | `system.native-menu` | Windows Shell `IContextMenu` |
| `ContextMenuCommand::Refresh` | `navigation.refresh` | |

### 12.2 macOS/SwiftUI 当前入口

当前 macOS 的主要手写入口位于：

- `apps/macos/Sources/KiteColumnBrowser.swift`：`KiteEntryMenu`、`KiteDirectoryMenu`、列表行和搜索结果行
- `apps/macos/Sources/KiteGitDialogs.swift`：`KiteGitMenu`
- `apps/macos/Sources/KiteSidebar.swift`：位置、收藏、卷
- `apps/macos/Sources/ContentView.swift`：路径栏、页签、Git 工具条
- `apps/macos/Sources/KitePreviewPane.swift`：预览动作网格

映射：

| 当前 SwiftUI 文案/行为 | 新 ActionId | 备注 |
|---|---|---|
| 刷新 | `navigation.refresh` | 新模型只允许背景/容器语义 |
| 使用 Agent > 询问 Agent… | `agent.ask` | |
| 使用 Agent > 解释此文件… | `agent.explain-file` | |
| 使用 Agent > 总结此文件夹… | `agent.summarize-folder` | |
| 使用 Agent > 审查选中变更… | `agent.review-changes` | |
| 使用 Agent > 生成提交说明… | `agent.generate-commit-message` | |
| 使用 Agent > 配置 Agent… | `agent.configure` | |
| 打开 | `open.default` | |
| 打开 Git 客户端 | `open.in` / tool `kite.git-client` | 只保留一个入口 |
| Git > 打开 Git 客户端… | `open.in` / tool `kite.git-client` | 删除重复入口 |
| KiteGitMenu > 拉取… | `git.pull` | |
| KiteGitMenu > 推送… | `git.push` | |
| KiteGitMenu > 获取当前远程 | `git.fetch-current` | |
| KiteGitMenu > 获取全部远程 | `git.fetch-all` | |
| KiteGitMenu > 管理远程… | `git.remotes.manage` | |
| KiteGitMenu > 克隆仓库… | `git.clone` | |
| KiteGitMenu > 初始化仓库 | `git.init` | |
| 查看目录/文件 Git 历史 | `git.log` | 按目标范围限定 |
| 交互式 Rebase… | `git.rebase.interactive` | 不再散落在一级菜单 |
| 在 Kite 编辑器中打开 | `open.in` / tool `kite.text-editor` | 作为工具选项 |
| 使用 Mindmap 编辑 | `open.in` / tool `kite.mindmap` | 作为工具选项 |
| 在 Finder 中显示 | `system.reveal` | |
| 快速查看 | `open.quick-look` | |
| 拷贝路径/复制完整路径 | `clipboard.copy-path` | 统一文案 |
| 拷贝/复制 | `clipboard.copy` | |
| 剪切 | `clipboard.cut` | |
| 粘贴 | `clipboard.paste` | |
| 拷贝到… | `clipboard.copy-to` | |
| 压缩 > ZIP/TAR.GZ | `archive.compress.zip` / `archive.compress.tar-gz` | |
| 解压 > 当前文件夹/到… | `archive.extract-here` / `archive.extract-to` | |
| 新建文件夹… | `file.new-folder` | |
| 新建文件… | `file.new-file` | |
| 添加到收藏/从收藏移除 | `favorite.add` / `favorite.remove` | |
| 分享… | `system.share` | |
| 重命名… | `file.rename` | |
| 移到废纸篓/将所选项目移到废纸篓 | `file.move-to-trash` | 多选升级风险 |
| 显示简介 | `system.properties` | |
| 编辑路径 | `navigation.edit-path` | 辅助 Surface |
| 关闭页签/新建页签 | `tab.close` / `tab.new` | 辅助 Surface |
| 无对应入口 | `open.with` | 当前缺失，需由 #224 补齐 |
| 无对应入口 | `compare.with` | 当前缺失，需由 #225 补齐 |
| 无对应入口 | `system.native-menu` | 当前缺失；macOS 需映射到可用原生能力平面 |

### 12.3 VCS 内部 Surface（辅助，不属于 v1 主菜单）

Git 分支、提交历史和变更文件的右键菜单属于专用 VCS Surface，不是文件管理器的根级上下文菜单。v1 不要求把它们并入同一个 Root Composer，但必须先保留稳定 ID，避免未来迁移时再次产生文案漂移。

| 当前来源 | 当前文案 | 保留 ActionId | 处理 |
|---|---|---|---|
| `KiteGitBranches.swift` | 检出 | `git.branch.checkout` | 专用 Surface；脏工作区需确认 |
| `KiteGitBranches.swift` | 与 HEAD 比较 | `git.branch.compare-head` | 只读 |
| `KiteGitBranches.swift` | 设置上游 | `git.branch.set-upstream` | VCS 写 |
| `KiteGitBranches.swift` | 在新 worktree 中检出 | `git.worktree.create` | VCS 写 |
| `KiteGitBranches.swift` | 重命名 | `git.branch.rename` | VCS 写 |
| `KiteGitBranches.swift` | 保护/取消保护 | `git.branch.protection.toggle` | 本地策略 |
| `KiteGitBranches.swift` | 删除 | `git.branch.delete-local` | 高风险 |
| `KiteGitBranches.swift` | 删除远程分支 | `git.branch.delete-remote` | 高风险、需在线 |
| `KiteGitBranches.swift` | 推送标签 | `git.tag.push` | VCS 写、需在线 |
| `KiteGitBranches.swift` | 删除本地/远程标签 | `git.tag.delete-local` / `git.tag.delete-remote` | 高风险 |
| `KiteGitLogWindow.swift` / `src/git_log_window.rs` | 查看变更 | `git.commit.show-changes` | 专用 Surface |
| 同上 | 查看文件历史 | `git.log` / variant `file:<path>` | 复用日志语义 |
| 同上 | 复制完整哈希 | `git.commit.copy-hash` | 剪贴板 |
| 同上 | 在提交时浏览文件 / 加载变更文件… | `git.commit.show-tree` | 专用 Surface |
| `src/git_log_window.rs` | 显示内嵌并排 Diff | `git.diff.inline` | 专用 Surface |
| `src/git_log_window.rs` | 在独立窗口打开 Diff | `git.diff.window` | 专用 Surface |

这些动作不得从内部 VCS Surface 提升到文件浏览器根菜单；它们只负责自己的专用 UI。若后续迁移到统一 Composer，再补入正式动作目录和上下文矩阵。

### 12.4 当前重复与缺口基线

#### 结构重复

- **D1 macOS `KiteEntryMenu` / `KiteDirectoryMenu` 出现两个同名 `Git` 菜单容器。**
- **D2 macOS 同一菜单中“打开 Git 客户端”直接项与 Git 子菜单内“打开 Git 客户端…”重复。**
- **D3 macOS `KiteEntryMenu` / `KiteDirectoryMenu` 在一级菜单散落 Rebase 和 Git 历史，与 Git 子菜单形成同一语义族的多个入口位置。**
- **D4 Windows Provider 在文件、目录、背景上下文复用同一套 Core 节点，导致目录目标把 `file.rename`、`file.move-to-trash` 等 Entity/Container 语义混入背景菜单。**
- **D5 文案漂移：Windows“复制/拷贝到”与 macOS“拷贝/拷贝到”并存；“复制完整路径”和“拷贝路径”指向同一语义。**
- **D6 同一动作在不同 Surface 通过不同按钮/菜单实现，没有统一 ActionId。**
- **D7 Windows 侧边栏与便携设备菜单在 `src/app.rs` 中直接手写，绕过 `ContextMenuProvider` / `render_context_menu`。**
- **D8 Windows 原生 Shell 桥当前按单一路径创建 `IShellItem`，没有把完整多选目标集传入 `IShellItemArray`。**

#### 缺失能力

- **M1** 两平台均缺少统一的 `open.with`。
- **M2** 两平台均缺少统一、工具注册表驱动的 `open.in`；macOS 仅有零散的 Kite Editor、Mindmap、Git 客户端入口。
- **M3** 两平台均缺少 `compare.with`。
- **M4** macOS 缺少与 Windows 对应的完整 SVN 菜单。
- **M5** macOS 缺少 `system.native-menu` 的明确原生扩展平面入口。
- **M6** 搜索结果只有极简手写命令，没有统一 `navigation.open-parent` 和完整 Surface 规则。
- **M7** 预览只有动作网格，没有统一“更多”菜单。
- **M8** Windows 多选路径虽进入 `ContextMenuAction.paths`，但大多数命令仍只使用 `target`；没有一致的全目标支持检查。
- **M9** macOS 的多个菜单在临时不可用时直接隐藏禁用项，而 Windows 保留禁用项；违反统一可见性规则。
- **M10** Git/SVN 没有统一的 repository/worktree/index/operation-state 契约；状态变化主要由各窗口自行处理。
- **M11** 没有稳定的 action ID、风险、确认、反馈和命令集合快照。

#### v1 必须消除的基线

- 同层级重复 `ActionId`：0。
- 背景菜单 Entity-only 泄漏：0。
- 多选菜单单目标泄漏：0。
- 子菜单为空：0。
- 原生入口缺失的受支持上下文：0。
- 每个矩阵上下文没有 golden fixture：0。

---

## 13. 测试与夹具策略

### 13.1 测试层次

1. **Descriptor 单元测试**
   - 每个 ActionId 唯一。
   - 每个动作有 Scope、Risk、Owner、Effect、反馈。
   - 风险变化必须显式声明。

2. **Composer golden 测试**
   - 输入固定 `ContextSnapshot`。
   - 输出稳定菜单树、可见性、禁用原因、根级顺序和子菜单顺序。
   - 比较 `ActionInstanceKey`，不比较平台渲染类。

3. **不变量属性测试**
   - 遍历 Surface × TargetSet × Capability × RepositoryState × UserPolicy 的受控组合。
   - 自动断言第 4 节所有不变量。

4. **VCS 状态迁移测试**
   - repository/worktree/index/conflict/operation/offline 的状态矩阵。
   - 每次操作前后检查 `state_revision`、目标不变性和反馈。

5. **平台投影测试**
   - GPUI 与 SwiftUI 对同一菜单树生成相同的 ActionId 序列。
   - 文案可本地化，但顺序和可见性必须相同。

6. **交互/可访问性测试**
   - 打开菜单不改变选择、预览、历史和加载。
   - 键盘上下/左右/Escape/回车可用。
   - 禁用原因可被屏幕阅读器读出。
   - 长菜单滚动后所有节点可达。
   - 右键已选中的一项作用于整个选择；右键未选中项只作用于该项。

### 13.2 夹具目录建议

未来实现应建立独立的跨平台 fixture 集合，例如：

```text
tests/fixtures/context-menu/
  contexts/
    file.text.git-modified.json
    directory.repo-root.dirty.json
    multi.two-files.same-repo.json
    background.repo-root.paste.json
    search.single-text-file.json
    sidebar.favorite.directory.json
    preview.text-file.git-modified.json
    device.namespace.item.json
  expected/
    file.text.git-modified.menu.json
    directory.repo-root.dirty.menu.json
    multi.two-files.same-repo.menu.json
    background.repo-root.paste.menu.json
    search.single-text-file.menu.json
    sidebar.favorite.directory.menu.json
    preview.text-file.git-modified.menu.json
    device.namespace.item.menu.json
  vcs/
    git.normal.clean.json
    git.normal.dirty.json
    git.normal.staged.json
    git.merge.conflict.json
    git.rebase.in-progress.json
    git.cherry-pick.in-progress.json
    git.offline.json
    svn.normal.json
    svn.conflict.json
    svn.cleanup-required.json
```

### 13.3 快照规范化

- 时间、路径、随机 ID、最近应用 ID 必须替换为稳定 token。
- 真实目标名替换为 `{{target.primary}}` 等占位符。
- 平台标签可以不同，但 `ActionInstanceKey`、顺序、可见性和 reason 必须相同。
- 快照必须包含 `disabled(reason)`，不能只比较可用项。
- 每个 fixture 必须至少断言一次“不得出现的动作集合”，防止背景泄入 Entity 动作、多选泄入单目标动作。

### 13.4 最低验收 fixture

| Fixture | 必须验证 |
|---|---|
| `file.text.git-modified` | Open/Open With/Open In/文件级 Git；无 Container 动作 |
| `directory.repo-root.dirty` | 目录可作为 Container，且 Git 仓库状态动作正确 |
| `multi.two-files.same-repo` | 批量安全、Compare With、单目标动作隐藏 |
| `background.repo-root.paste` | 零 Entity 目标、粘贴/新建、仓库动作 |
| `search.single-text-file` | 搜索只保留轻量动作和 `navigation.open-parent` |
| `sidebar.favorite.directory` | 收藏删除与导航，不出现内容级操作 |
| `preview.text-file.git-modified` | 预览更多菜单与文件实体一致 |
| `device.namespace.item` | 命名空间能力和 `device.delete`，不假设普通路径 |

### 13.5 VCS 状态验收

必须至少覆盖：

- 正常干净工作区。
- 有未暂存修改。
- 有暂存内容。
- 有未跟踪文件。
- merge 冲突。
- rebase 进行中。
- cherry-pick 进行中。
- revert 进行中。
- 仓库忙。
- 离线/无上游。
- SVN 冲突和 cleanup-required。

对每种状态，测试必须验证：

1. 特殊恢复动作是否被提升。
2. 普通动作是否被禁用及原因。
3. 是否出现错误的 Git index 假设。
4. 是否意外出现外部 Tortoise 命令。
5. 状态变更后执行是否被安全取消。

---

## 14. v1 根级顺序基线

在正常文件上下文中，根级可见节点按以下顺序投影：

```text
1. navigation.refresh                （仅背景/容器表面）
2. open.default
3. open.with                         （文件）
4. open.in
5. open.quick-look
6. compare.with                      （恰好两个文件）
7. file-actions                      （命名任务子菜单）
8. archive                           （压缩/解压）
9. agent                             （Agent）
10. git                              （仅仓库/适用文件）
11. svn                              （仅工作副本）
12. system                           （定位、分享、属性）
13. file.move-to-trash               （危险组）
14. system.native-menu               （固定最后）
```

说明：

- `file-actions` 内部顺序固定为复制、剪切、复制到、粘贴、复制完整路径、新建文件夹、新建文件、收藏、重命名；不适用的项隐藏。
- `archive` 内部顺序固定为 ZIP、TAR.GZ、解压到当前文件夹、解压到…；不适用的项隐藏。
- `git` 和 `svn` 内部按“状态摘要（只读）→ 变更 → 同步 → 历史 → 高级 → 恢复/危险”分组。
- `system.native-menu` 不得被放入 `system` 子菜单；它必须是最末根级入口。
- 该顺序是语义基线，不是最终像素样式；GPUI 与 SwiftUI 可以不同地绘制分隔线和图标。

---

## 15. 平台边界与残余假设

1. **macOS 没有 Windows Shell 的完全等价完整原生菜单。**
   - macOS 入口应使用已验证的 AppKit/Services/LaunchServices 原生能力平面；
   - 不允许把 Kite 手写的少量 Finder 动作冒充“完整原生菜单”；
   - 平台能力缺口必须在实现 issue 中显式记录。

2. **Open With 的应用发现属于 #224。**
   - 本文只规定兼容性、排序、默认项和交互；
   - 本文不规定 LaunchServices、Windows Shell 或工具注册表的具体 API。

3. **Open In / Compare With 的工具注册表属于 #225。**
   - 本文只规定工具能力声明、目标形态和菜单投影；
   - 具体发现、验证、参数模板和设置 UI 由 #225 定稿。

4. **Git/SVN 状态读取与执行属于 #226。**
   - 本文规定状态字段、动作前置条件和恢复优先级；
   - 本文不改变现有 Git/SVN 业务实现。

5. **上下文补齐属于 #227。**
   - 本文给出全部矩阵与 golden fixture；
   - 多选、背景、搜索、侧边栏、设备命名空间的实现可以在后续 issue 分阶段完成。

6. **平台验收属于 #228。**
   - 本文只定义交互不变量、测试策略和验收基线；
   - 键盘、屏幕阅读器、性能与双平台像素验收由 #228 负责。

---

## 16. 最终验收清单

- [ ] 每个当前可用动作都有稳定 `ActionId`、Scope、Risk、Owner 和反馈。
- [ ] `open.default`、`open.with`、`open.in`、`compare.with` 的语义和示例无歧义。
- [ ] 背景菜单没有 Entity-only 动作。
- [ ] 多选菜单没有单目标动作。
- [ ] 同一层级的 `ActionInstanceKey` 重复基线为 0。
- [ ] 临时不可用动作可见、可解释；永久不支持动作隐藏。
- [ ] `system.native-menu` 在全部受支持上下文存在且为最后根级入口。
- [ ] Git/SVN 的 repository、worktree、index、conflict、operation state 映射可测试。
- [ ] 文件、目录、多选、背景、搜索、侧边栏、预览、设备命名空间均有 expected command-set fixture。
- [ ] 文档不依赖 GPUI 或 SwiftUI 类名即可理解。
- [ ] 规范中的每个 ActionId 都能被 GPUI 与 SwiftUI 映射到同一语义动作。
