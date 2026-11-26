# Mindmap XMind core

- 状态：P0 技术验证
- Feature issue：https://github.com/wyz541/Kite/issues/186
- 设计讨论：https://github.com/wyz541/Kite/discussions/185

## 目标

为 Kite 的 Mindmap 编辑器建立一个安全、可验证的现代 `.xmind` 文档核心。

当前切片只处理 ZIP 容器和 `content.json`。它不解析完整节点树，也不实现 SwiftUI/GPUI 界面；目标是在任何 UI 大规模开发前，先证明未知 XMind 内容能被保留。

## 不可破坏的不变量

1. 未修改的 `.xmind` 保存后与源文件字节相同。
2. 只修改 `content.json` 时，其他 ZIP 条目通过 `raw_copy_file` 原样复制。
3. `content.json` 中未知的 JSON 字段必须随已知字段一起保留。
4. 外部应用修改文件后，Kite 拒绝覆盖。
5. 保存先写完整临时包，再原子替换目标文件。
6. XMind 8 的 `content.xml` 明确返回 Legacy 错误，不伪装成现代格式成功。

## 核心模型

`XmindPackage` 同时保留两层视图：

```text
source_bytes
  ├── entries: ZIP 条目清单
  └── source_content: serde_json::Value
        └── content: 可编辑 Value
```

保存时只重建 dirty entries：

```text
source ZIP
  ├── content.json → 序列化后的 Value
  ├── 其他已知条目 → 后续版本可按名称替换
  └── 未知条目 → raw_copy_file
        ↓
完整性校验
        ↓
AtomicFile 原子写入
```

这不是 XMind 字段级迁移器；它是后续迁移器和编辑器共同依赖的容器边界。

## 当前 API

- `XmindPackage::open`
- `XmindPackage::from_bytes`
- `XmindPackage::summary`
- `XmindPackage::content`
- `XmindPackage::edit_content_json`
- `XmindPackage::save`
- `XmindPackage::save_as`

后续应在此模块上增加 typed topic model、命令栈和布局场景，而不是让 UI 直接操作 `serde_json::Value`。

## 限制

- 当前只解析现代 `content.json`。
- 整个包暂存于内存，上限为 512 MiB。
- `content.json` 上限为 64 MiB。
- 外部修改检测采用“读取哈希 → 原子写入”，不是跨进程文件锁。
- 尚未实现 XMind 8 `content.xml` 解析与迁移。

## UI 与能力测试基准

`tests/fixtures/mindmap/kite-rust-gpui-capability.xmind` 是固定 UI/能力测试对象，机器可读基线位于同目录的 `kite-rust-gpui-capability.manifest.json`：

- 1 个 sheet
- 3,019 个 topic
- 654 个带样式节点
- 140 个 shape-class 属性
- 3 个 ZIP 条目

测试要求：

- 打开该文件后 sheet、topic、样式和形状数量不变。
- 不编辑直接保存时字节完全一致。
- 修改根主题后，其他能力和 `metadata.json` / `manifest.json` 条目不变。
- 后续 macOS 画布用同一文件验证首屏、缩放、命中测试和视觉表现。

## 下一步

1. 建立 typed topic tree，同时保留 opaque 字段。
2. 增加命令/撤销模型。
3. 增加 XMind 8 读取和迁移。
4. 在 macOS 原生画布上加载该能力地图并记录 500～5,000 节点性能。
