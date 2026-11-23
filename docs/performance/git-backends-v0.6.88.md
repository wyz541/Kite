# Kite 0.6.88 Git 后端对比

结论：在单台 macOS arm64 的 release smoke 基准中，`gix 0.61.0` 的 status 和 log p95 慢于 Git CLI，blame 与 diff p95 快于 Git CLI。证据不足以支持立即混用两套后端，当前继续使用 Git CLI。

## 1. 来源

- 基准工具：`src/bin/kite-perf.rs`
- 工作分支基于 commit：`7a9b086`
- 配置：`smoke`
- 构建：`release`
- Git：`git 2.54.0`
- gix：`gix 0.61.0`
- 每场景交互样本：5
- 冷启动样本：1

命令：

```bash
target/release/kite-perf \
  --profile smoke \
  --git-only \
  --samples 5 \
  --cold-samples 1 \
  --gix /path/to/gix \
  --output target/performance/git-smoke-release.json \
  --markdown target/performance/git-smoke-release.md
```

## 2. 环境

| 字段 | 内容 |
| --- | --- |
| 平台 | macOS arm64 |
| 设备 | Generic Apple Silicon |
| 文件系统 | APFS |
| Git 文件数 | 1,000 |
| Git 历史 | 100 个修改同一路径的提交 |
| 脏文件 | 100 |
| blame 文件 | 64 行 |
| 外部进程 | Git CLI 和 gix CLI 都包含进程启动与仓库打开 |

## 3. 结果

延迟单位是毫秒。比值是 `gix / CLI`，小于 1 表示 gix 用时更短。

| 操作 | CLI p50 | gix p50 | p50 比值 | CLI p95 | gix p95 | p95 比值 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| status | 42.451 | 79.268 | 1.867 | 53.918 | 115.904 | 2.150 |
| log 第一屏（100） | 42.252 | 36.133 | 0.855 | 47.326 | 54.415 | 1.150 |
| blame | 78.434 | 51.203 | 0.653 | 80.003 | 61.371 | 0.767 |
| diff `HEAD^..HEAD` | 32.689 | 27.519 | 0.842 | 44.878 | 28.326 | 0.631 |

## 4. 判断

- **status**：Git CLI 的 p50 和 p95 都明显更快。gix 的外部 status 命令还有输出格式差异，不能据此证明库内 API 更慢，但也不支持现在替换。
- **log**：gix p50 更快，p95 更慢，差距接近本轮噪声。
- **blame**：gix p50 和 p95 均更快。
- **diff**：gix p50 和 p95 均更快。

合并后端会增加错误分类、配置兼容、路径编码和测试矩阵的维护成本。只有 blame 和 diff 的局部优势不足以支付这套复杂度，因此当前结论是：

1. 所有写操作继续使用 Git CLI。
2. status、log、blame、diff 继续使用现有 Git CLI 读路径。
3. 在客户端窗口和 diff 查看器稳定后，如仍有明确性能目标，再评估把 blame/diff 单独迁到 gix 库内 API。
4. 任何后端切换必须附带 Windows 和 macOS 的多轮 release 结果。

## 5. 限制

- 这是外部进程级对比，不是 gix 库内 API 与 `std::process::Command` 的微基准。
- Git status 输出 porcelain v1；gix 0.61 只支持 simplified status 展示。两者工作量相近但输出不同。
- blame 和 diff 的终端输出格式不同，结论只使用耗时，不比较文本吞吐。
- 只有一台 macOS arm64 机器和一次 5 样本运行，没有 Windows 数据和跨机重复。
- 文件系统缓存未清理，读取类结果通常偏热。

跨平台发布判断仍应使用 `.github/workflows/performance.yml` 的多轮聚合结果。
