# Mindmap 导入语料与双平台验收基础

对应 Issue：#211
父功能：#204
设计讨论：#195

## 目标

这套固定语料用于证明 Markdown、DOCX、TXT、HTML、OPML 五种转换适配器在不同平台共享同一套可解释输入、结构映射、警告和 XMind 写出结果。它不是 UI 截图基线，也不替代 XMind 27.01 的人工视觉验证。

自动化测试入口：

```text
tests/mindmap_import_corpus.rs
```

固定文件：

```text
tests/fixtures/mindmap/import/
├── manifest.json
├── markdown-{normal,edge,malformed}.md
├── docx-{normal,edge,malformed}.docx
├── text-{normal,edge,malformed}.txt
├── html-{normal,edge,malformed}.html
└── opml-{normal,edge,malformed}.opml
```

`manifest.json` 记录每个文件的格式、角色、字节数、SHA-256，以及按路径和哈希排序计算出的语料总哈希。任何样本变化都必须显式更新 manifest，并重新审阅结构断言。

## 覆盖范围

| 格式 | 正常样本 | 边界样本 | 损坏样本 |
| --- | --- | --- | --- |
| Markdown | 标题、列表、任务、表格、代码、图片、链接、中英文拆句 | Front matter、多 H1、引用、URL、版本号、Raw HTML 降级 | 非 UTF-8 |
| DOCX | Heading 层级、正文拆句、原始段落备注 | 表格、超链接、图片附件、脚注、批注、未知样式 | 非 ZIP/非法 DOCX |
| TXT | 中文章节、嵌套列表、正文拆句 | 混合缩进、歧义短行、长段落分组 | 非 UTF-8 |
| HTML | 标题、列表、表格、链接、图片 | 可恢复的错误标签、忽略导航区、非语义容器降级 | 非 UTF-8 |
| OPML | 原生树、备注、链接、TODO | 非 outline 元素降级、深层结构 | 非法 XML |

正常和边界样本都必须满足：

- 结构标题和父子层级稳定。
- 有正文的适配器按中英文句号、问号、感叹号和分号拆分内容节点。
- 原始段落作为最近结构主题的 `Original paragraph` 备注保留。
- 跳过、拍平或仅保留引用的内容产生稳定警告。
- 写出结果可被生产级 `XmindPackage` 重新打开。
- 同一输入重复写出得到完全相同的字节。

## 警告基线

测试明确覆盖以下降级：

- Markdown 图片仅保留链接引用，Raw HTML 拍平为文本。
- HTML 图片仅保留引用，导航区域被忽略，未知容器被遍历并告警。
- DOCX 脚注和批注进入主题备注，未知段落样式按正文处理。
- TXT 混合 tab/空格缩进时归一化层级，并报告歧义短行。
- OPML 非 outline 元素被忽略或跳过。
- 表格和附件在 writer 报告中分别产生 `TableFlattened` 与 `AttachmentReferenceOnly`。

这些警告是产品行为的一部分，不允许通过静默删除警告来让测试通过。

## 性能基线

固定回归测试生成 3,000 个同级 Markdown 列表主题，并验证：

- Outline 节点数固定为 3,001（包含自动建立的 `Overview` 容器）。
- 生成 `.xmind` 的 topic 数固定为 3,002（包含文档根主题）。
- 两次写出字节完全一致。
- 包大小不超过 8 MiB。
- 提取加两次写出的测试耗时上限为 30 秒。

这是防止数量级退化的构建机 sanity baseline，不是跨设备吞吐 benchmark。当前开发机构建中整组 6 个语料测试约 1 秒完成。

## 未知字段安全

测试会在 writer 产物中加入：

- 未知 sheet JSON 字段。
- 未知 topic JSON 字段。
- 未知 ZIP entry。

随后通过 `XmindPackage` 编辑标题并保存，再次验证未知 JSON 字段和 ZIP entry 全部保留。该测试把 writer、生产容器读取器和 lossless save 串成一条端到端边界。

## 平台对等状态

当前基础是平台无关的纯 Rust Core：

- 不包含 macOS/Windows 条件编译。
- 语料路径通过 `CARGO_MANIFEST_DIR` 解析，不依赖当前工作目录或平台路径分隔符。
- 测试不依赖字体、系统语言、网络或外部 XMind 安装。
- 固定语料通过 `.gitattributes` 的 `tests/fixtures/mindmap/** -text` 保持逐字节检出，避免 Windows `core.autocrlf=true` 改变 manifest 哈希。
- Windows x64 本机已执行：
  - `cargo test --test mindmap_import_corpus`
  - 9 tests passed，包含五格式保存重开、警告/统计精确断言、跨平台树 golden 和性能 sanity。
- macOS ARM64 曾执行同一语料测试；本轮新增 `.github/workflows/mindmap-macos.yml`，在 macOS CI 上运行同一组 Mindmap 测试。
- `tests/fixtures/mindmap/expected/` 和 `tests/fixtures/mindmap/ui-tree-parity/expected.canonical.json` 是已提交 golden；Windows Rust UI parser 和 macOS Swift canonicalizer 都对齐同一份输出。
- XMind 27.1 的人工打开和视觉回读不属于本测试文件能独立证明的范围，仍是发布前人工闸门。

在 XMind 27.1 人工验证完成前，#211 不应关闭。

## 验证命令

```bash
cargo fmt --all -- --check
cargo test --test mindmap_import
cargo test --test mindmap_import_adapters
cargo test --test mindmap_import_docx_txt
cargo test --test mindmap_import_corpus
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
```
