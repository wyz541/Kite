# XMind 主题样式与日期插入审计

- Issues：#216、#217
- 设计讨论：#201
- 前置审计：#212
- 实现：`src/mindmap/topic_style.rs`、`src/mindmap/date.rs`
- 测试：`tests/mindmap_topic_style_date.rs`

## 结论

主题本地格式保存在：

```text
sheet.rootTopic.style.properties
sheet.topic.style.properties
```

读取和写入必须只改动目标主题的 allow-list 字段，不能从 typed model 重建整个主题、style 或 properties 对象。否则会丢失未知属性和未来 XMind 字段。

日期插入本切片写入普通可编辑文本：

- 主题标题：`topic.title`
- 主题备注：`topic.notes.plain.content`

没有自动写入 XMind 的结构化任务字段。`taskStartDate`、`taskDueDate`、`taskPresentation` 属于任务/Gantt 语义，不能和“插入一个日期文本”混为一谈。

## 主题样式字段矩阵

证据来源：

1. 本机 XMind for macOS `26.05.01107` renderer style-key registry。
2. `static/styles/themes/*.json` 中真实序列化的主题样式。
3. `tests/fixtures/mindmap/kite-rust-gpui-capability.xmind` 中 654 个带样式主题。
4. 兼容目标仍为 XMind `27.01.05089`；27.01 视觉复核前不宣称像素级一致。

| 能力 | XMind 字段 | 已观察值 | Kite 类型 | 状态 |
|---|---|---|---|---|
| 字体族 | `fo:font-family` | `JetBrains Mono`、逗号分隔 fallback 列表 | `FontFamily` | 已确认 |
| 字号 | `fo:font-size` | `10pt`、`11pt`、`14pt` | `FontSize` | 已确认 |
| 字重 | `fo:font-weight` | `normal`、`bold`、`400`、`500`、`600`、`700` | `FontWeight` | 已确认 |
| 斜体 | `fo:font-style` | `normal`、`italic` | `FontStyle` | 已确认 |
| 下划线/删除线 | `fo:text-decoration` | `none`、`underline`、`line-through`、空白分隔组合 | `TextDecoration` + patch flags | 已确认 |
| 文本色 | `fo:color` | `#RRGGBB` | `StyleColor` | 已确认 |
| 对齐 | `fo:text-align` | `left`、`center`、`right` | `TextAlignment` | 已确认 |
| 节点背景 | `svg:fill` | `none`、`#RRGGBB` | `StyleColor` | 已确认 |
| 节点边框色 | `border-line-color` | `#RRGGBB` | `StyleColor` | 已确认 |
| 节点边框宽 | `border-line-width` | `0pt`、`1pt`、`2pt`、数字 | `StyleMeasurement` | 已确认读取；写入使用 typed unit |
| 节点边框线型 | `border-line-pattern` | XMind style registry 中存在；主题夹具未观察到序列化值 | `BorderLinePattern` | Schema 已确认，27.01 视觉复核待做 |
| 节点圆角 | `shape-corner` | XMind style parser 中存在；审计夹具未观察到序列化值 | `NodeCornerRadius` | **实验性**；单位和视觉消费待 27.01 验证 |
| 行高 | `fo:line-height` | XMind renderer 没有对应 style-key 映射 | `StyleMeasurement` | **实验性隔离**；Kite 可 round-trip，但不能宣称 XMind 已识别 |
| 字距 | `fo:letter-spacing` | XMind renderer 没有对应 style-key 映射 | `StyleMeasurement` | **实验性隔离**；Kite 可 round-trip，但不能宣称 XMind 已识别 |

### 行高与字距的明确限制

XMind 的文本 renderer 内部存在 `lineHeight` 布局参数，但审计到的 style-key registry 没有公开的 `fo:line-height` 或 `fo:letter-spacing` 持久化 key。当前 Core 实现使用：

```text
fo:line-height
fo:letter-spacing
```

作为隔离的实验性映射，原因是：

- 它能提供稳定的 typed API 和 round-trip 测试；
- 不修改其他属性；
- 不做“XMind 已支持”的错误兼容承诺；
- 后续 #217 完成 27.01 富文本/富格式审计后，可迁移到正确字段而不影响 UI 调用方。

## 局部更新语义

`apply_topic_style(topic, patch)`：

- 只处理传入的一个 topic JSON 对象。
- `None` 表示保持现有属性不变。
- 修改下划线/删除线时保留 `fo:text-decoration` 中未知 token，例如 `overline`。
- 已存在的未知 style property、未知 `style` 字段、未知 topic 字段原样保留。
- `style` 不存在时才创建稳定 style id、`type: "topic"` 和空 `properties`。
- 验证失败时目标 topic 保持原值。

`read_topic_style(topic)`：

- 只读取主题本地 override。
- 缺失本地 style 返回默认值。
- 不把 theme 继承值错误地当成主题本地值；theme fallback 留给后续布局/主题 resolver。

## 日期模型与插入

`StaticDateValue`：

- 固定日期；
- 可选时间；
- 不包含时区；
- `today_local()` 只负责生成静态快照。

`DateDisplayFormat` 支持：

- ISO：`2026-11-27`、`2026-11-27 14:05`；
- 中文 locale：`2026年11月27日 14:05:09`；
- English (US) locale：`Nov 27, 2026 2:05 PM`；
- System pattern：宿主解析 locale 后传入 `DatePartOrder` 和分隔符，Core 不自行读取平台 locale 资源，避免同一文件在不同机器上被静默改写。

`DateInsertionRequest` 强制提供：

- `target: Title | Note`
- `placement: Start | End`
- 精度：仅日期、分钟、秒

日期是静态文本，不包含动态日期 token。插入后仍是普通可编辑标题/备注。

## 缺失字体策略

Core 不枚举 macOS/Windows 字体，也不根据平台名称猜测字体是否存在。调用方完成存在性判断后，使用：

- `MissingFontPolicy::PreserveRequested`
  - 文件中保留原字体族；
  - renderer 由平台选择 fallback；
  - warning 明确说明没有替换持久化值。
- `MissingFontPolicy::SubstituteForLayout(fallback)`
  - 文件中仍保留原字体族；
  - 只把 fallback 用于当前布局/渲染；
  - warning 同时报告 requested、persisted 和 render family。

任何策略都不会静默把 fallback 写入 XMind 文件。这保证缺失字体不会损坏文件语义，也避免跨平台布局测量悄悄改变保存内容。

## Rust API

```rust
use kite_core::mindmap::{
    apply_topic_style, insert_date_into_topic, read_topic_style, DateDisplayFormat,
    DateInsertionPlacement, DateInsertionRequest, DateInsertionTarget, FontFamily, FontSize,
    FontStyle, NodeStylePatch, StaticDateValue, StyleColor, TextStylePatch, TopicStylePatch,
};

let current = read_topic_style(topic)?;
let patch = TopicStylePatch {
    text: TextStylePatch {
        font_family: Some(FontFamily::new("Kite Industrial Mono")?),
        font_size: Some(FontSize::points(13.0)?),
        font_style: Some(FontStyle::Italic),
        color: Some(StyleColor::new("#1F2328")?),
        ..TextStylePatch::default()
    },
    node: NodeStylePatch::default(),
};
apply_topic_style(topic, &patch)?;

insert_date_into_topic(
    topic,
    &DateInsertionRequest {
        value: StaticDateValue::today_local(),
        format: DateDisplayFormat::ISO_DATE,
        target: DateInsertionTarget::Title,
        placement: DateInsertionPlacement::End,
    },
)?;
```

## 验证

覆盖：

- 全部 typed 文本/节点样式写入和读取；
- 真实未知 style property 和未知 topic/notes 字段保留；
- 只改目标主题，兄弟主题 JSON 完全不变；
- 保存并重新打开后样式和日期仍存在；
- 中文、英文、ISO、System pattern 的确定性格式；
- 日期目标缺少时间时原子失败；
- style 结构非法时原子失败；
- 缺失字体 Preserve / Substitute 两种 warning 语义；
- `overline` 等未知 text-decoration token 在切换下划线/删除线时保留。

运行：

```bash
cargo test --test mindmap_topic_style_date
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
```
