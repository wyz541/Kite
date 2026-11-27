# XMind 图结构与连线格式兼容性审计

- 状态：Core foundation 已实现，XMind 27.01 视觉复核待完成
- Issues：#212、#213
- 设计讨论：#201
- 实现：`src/mindmap/graph_style.rs`
- 测试：`tests/mindmap_graph_style.rs`

## 结论摘要

现代 XMind 的图结构和连线样式不在同一个对象上：

- 图结构是根主题上的语义字段：`sheet.rootTopic.structureClass`。
- 连线样式是主题样式属性：`line-class`、`line-corner`、`line-width`、`line-color`、`line-pattern`。
- 样式可来自主题分类（`sheet.theme.*.properties`），也可由主题本地 `style.properties` 覆盖。
- `sheet.extensions` 中还有一个 `org.xmind.ui.skeleton.structure.style` 元数据扩展；本次审计确认它存在，但没有足够证据安全推导其更新语义，因此只保留、不重写。

Kite 当前只修改已知字段，不把 typed model 反向序列化成整棵主题树。因此未知 JSON 字段、未知样式属性和未知 ZIP 条目都不会因图结构/连线编辑而丢失。

## 证据来源

本次审计读取了本机可用的以下真实来源：

1. `tests/fixtures/mindmap/kite-rust-gpui-capability.xmind`
   - `sheet.rootTopic.structureClass = org.xmind.ui.logic.right`
   - 1,533,092 字节 `content.json`
2. macOS Xmind `26.05.01107` 的应用资源
   - `static/shapes/__deprecated_branch-connections.json`
   - `static/styles/themes/*.json`
   - `static/snowbird/resource/templates/files/basic/{mind_map,logic_chart,org_chart,tree_chart}.xmind`
   - `static/mas-demo/The_Use_of_Mind_Mapping.xmind`
3. XMind style key registry
   - 确认存在 `line-class`、`line-corner`、`line-width`、`line-color`、`line-pattern`
   - 确认存在 `shape-corner`，但它属于主题形状，不是 connector radius

兼容基线仍是讨论 #185 锁定的 XMind for macOS `27.01.05089`。本机当前安装的是 `26.05.01107`，所以本文把 26.05 已确认的内容标为“已观察”，把仅有 key、缺少稳定夹具的内容标为“实验性”；正式宣称一比一前必须在 27.01 上复验。

## 字段矩阵

| 能力 | XMind 字段 | 已观察值 | Kite typed model | 状态 |
|---|---|---|---|---|
| 思维导图 | `rootTopic.structureClass` | `org.xmind.ui.map`、`org.xmind.ui.map.clockwise` | `GraphStructure::MindMap` | 已确认；读取接受 `org.xmind.ui.map*`，写入 canonical `org.xmind.ui.map` |
| 右向逻辑图 | `rootTopic.structureClass` | `org.xmind.ui.logic.right`、旧别名 `org.xmind.ui.logic-chart.right` | `GraphStructure::LogicRight` | 已确认 |
| 左向逻辑图 | `rootTopic.structureClass` | `org.xmind.ui.logic.left`、旧别名 `org.xmind.ui.logic-chart.left` | `GraphStructure::LogicLeft` | 已确认 |
| 组织架构图 | `rootTopic.structureClass` | `org.xmind.ui.org-chart.down` | `GraphStructure::OrgChart` | 已确认；当前只模型化向下结构 |
| 树形图 | `rootTopic.structureClass` | `org.xmind.ui.tree.right` | `GraphStructure::Tree` | 已确认；当前只模型化右向树 |
| 未知结构 | `rootTopic.structureClass` | 任意未来值 | `GraphStructure::Other(String)` | 保留，不擅自改写 |
| 曲线 | style `line-class` | `org.xmind.branchConnection.curve` | `ConnectorKind::Curved` | 已确认 |
| 直线 | style `line-class` | `org.xmind.branchConnection.straight` | `ConnectorKind::Straight` | 已确认 |
| 直角折线 | style `line-class` | `org.xmind.branchConnection.elbow` | `ConnectorKind::Elbow` | 已确认 |
| 圆角折线 | style `line-class` | `org.xmind.branchConnection.roundedElbow` | `ConnectorKind::RoundedElbow` | 已确认 |
| 其他连线 | style `line-class` | `fold`、`roundedfold`、`bight` 等 | `ConnectorKind::Other(String)` | 保留，暂不提供 UI 写入 |
| 折线圆角 | style `line-corner` | 未在模板中找到已序列化实例 | `ConnectorCornerRadius(u16)` | **实验性**；只写数字字符串，须在 27.01 做视觉/往返验证 |
| 线宽 | style `line-width` | `1`、`2`、`3`、`1pt`、`2pt`、`inherited` | `ConnectorWidth` | 已确认读取/保留；Kite 写入使用 `Npt` |
| 线色 | style `line-color` | `#RRGGBB`、`#RRGGBBAA`、`inherited` | `ConnectorColor` | 已确认，8 位色包含 alpha |
| 线型 | style `line-pattern` | `solid`、`dash`、`dot`、`inherited` | `ConnectorLinePattern` | 已确认常见值 |
| Skeleton structure 扩展 | `extensions[provider=org.xmind.ui.skeleton.structure.style]` | `content.centralTopic`、`content.mainTopic` | 无写入模型 | 只保留；更新语义未确认 |

## 样式作用域

XMind 主题中同一个 connector 属性可以出现在多个分类：

```text
sheet.theme.centralTopic.properties
sheet.theme.mainTopic.properties
sheet.theme.subTopic.properties
sheet.theme.level3.properties
sheet.theme.floatingTopic.properties
...
```

主题本地覆盖则位于：

```text
sheet.rootTopic.style.properties
sheet.topic.style.properties
```

Kite 的 document-level API 采用最保守的双写策略：

1. 始终写 `rootTopic.style.properties`，它是 XMind 自身模板中已出现的有效本地覆盖位置。
2. 如果 `theme.centralTopic` 已存在，同时镜像到 `theme.centralTopic.properties`，使主题渲染路径也能读取。
3. 不主动创建完整 theme，不改写 `mainTopic`、`subTopic`、`levelN` 或主题本地样式。
4. 后续 branch-level API 应直接定位目标主题的 `style.properties`，而不是复制整套 theme。

`line-corner` 在 26.05 的 style key registry 中存在，但没有观察到主题模板写入该字段；26.05 的可见 style projection 也没有把它投影到 renderer。因此 Kite 现阶段只保证：

- typed read/write 和 round-trip；
- 不损坏未知字段；
- 不把“已写入文件”误报成“XMind 26.05 已视觉验证”。

完成 27.01 验证前，圆角半径不能作为发布级一比一能力。

## Rust API

```rust
use kite_core::mindmap::{
    ConnectorColor, ConnectorCornerRadius, ConnectorKind, ConnectorLinePattern,
    ConnectorStylePatch, ConnectorWidth, DocumentGraphStylePatch, GraphStructure,
};

let current = package.document_graph_style(0)?;
let patch = DocumentGraphStylePatch {
    structure: Some(GraphStructure::LogicLeft),
    connector: Some(ConnectorStylePatch {
        kind: Some(ConnectorKind::RoundedElbow),
        corner_radius: Some(ConnectorCornerRadius::new(12)?),
        width: Some(ConnectorWidth::points(2)),
        color: Some(ConnectorColor::new("#112233")?),
        pattern: Some(ConnectorLinePattern::Solid),
    }),
};
package.apply_document_graph_style(0, &patch)?;
package.save()?;
```

对 UI 的约束：

- UI 读取 typed model，不直接解析或替换 `content.json`。
- 布局引擎根据 `GraphStructure` 选择锚点和排列方向。
- 连线绘制根据 `ConnectorKind` 选择曲线、直线、直角或圆角路径。
- `ConnectorCornerRadius` 只作用于圆角折线；`None` 表示采用渲染器默认半径。
- `GraphStructure::Other` / `ConnectorKind::Other` 必须使用 fallback 绘制，不能自动归一化或覆盖原值。
- 未知样式属性、`extensions`、`mainTopic` 等未纳入本次写入范围的内容由 Rust package 边界继续保留。

## 未决风险

1. XMind `27.01.05089` 是否仍使用完全相同的 `line-*` 属性和 class 值。
2. `line-corner` 的单位、合法范围和 renderer 消费方式。
3. `extensions[org.xmind.ui.skeleton.structure.style]` 是否需要与 `structureClass` 同步更新。
4. 文档级切换是否应同时改写所有 theme 层级，还是只改 central/root 后由 XMind 自行继承。
5. `org.xmind.ui.tree.left`、`org.xmind.ui.org-chart.up` 等方向变体目前被保留为 `Other`，尚未进入编辑器枚举。

这些问题必须在 #212 的 27.01 复核和 #213 的 macOS 画布接入前关闭或明确降级策略。

## 验证

已覆盖：

- 五个支持的结构值及其 canonical XMind class。
- 四个支持的 connector class。
- 真实 3,019 主题 fixture 的 `LogicRight` 读取。
- 真实 fixture 应用结构/连线修改后，其他 topic-local style 不变。
- synthetic package 的未知 JSON、未知 style property、未知 ZIP entry 保留。
- 未知 connector class 在只修改结构或颜色时保持原值。
- 无效 sheet/rootTarget 不产生部分内存修改。

运行：

```bash
cargo test --test mindmap_graph_style
cargo test --test mindmap_xmind
```
