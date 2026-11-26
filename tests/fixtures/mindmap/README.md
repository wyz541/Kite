# Mindmap test fixtures

## `kite-rust-gpui-capability.xmind`

这是 Mindmap 编辑器的 UI 表现与能力验收基准文件，不是通用解析样本。

- 来源：Kite 自身的 Rust/GPUI 工程能力地图
- SHA-256：`46030710c6396bd99a322c67c1f048d1820dde2abdf751bfc9c4678ed1a93a8e`
- Sheet：1
- Topic：3,019
- 带样式节点：654
- 自定义 shape-class：140
- ZIP entries：3（`content.json`、`metadata.json`、`manifest.json`）
- 机器可读基线：`kite-rust-gpui-capability.manifest.json`

用途：

- 验证大型 XMind 文档的安全打开和无损保存。
- 验证节点树、样式字段和自定义形状在 round-trip 后不漂移。
- 后续 macOS SwiftUI/AppKit 画布把它作为首屏、缩放、命中测试和视觉验收输入。

除非明确更新视觉基线，不要替换此文件。
