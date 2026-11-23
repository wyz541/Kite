# Kite 0.6.88 Git 后端对比

结论：在单台 macOS arm64 的 release smoke 基准中，`gix 0.61.0` 的 status 慢于 Git CLI，log、blame 与 diff 快于 Git CLI。反向执行顺序的检查保持相同方向，但证据仍不足以支持立即混用两套后端，当前继续使用 Git CLI。

## 1. 来源

- 基准工具：`src/bin/kite-perf.rs`
- 主结果 commit：`33e5418`
- 顺序控制 commit：`dc60418`
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
| status | 33.224 | 72.175 | 2.172 | 50.667 | 118.521 | 2.339 |
| log 第一屏（100） | 44.119 | 16.913 | 0.383 | 66.436 | 17.741 | 0.267 |
| blame | 78.698 | 32.752 | 0.416 | 83.557 | 33.646 | 0.403 |
| diff `HEAD^..HEAD` | 42.198 | 13.040 | 0.309 | 98.321 | 14.533 | 0.148 |

## 4. 顺序检查

CLI-first 会让 gix 在后续场景中受益于文件系统缓存。为了检查方向是否只来自执行顺序，另一次 debug 运行先执行全部 gix 场景，再执行 Git CLI 场景，使用 3 个样本。

下表只比较比值，不把 debug 绝对值与 release 主结果混合。

| 操作 | gix-first p50 比值 | gix-first p95 比值 | 方向 |
| --- | ---: | ---: | --- |
| status | 1.814 | 1.714 | gix 更慢 |
| log 第一屏（100） | 0.633 | 0.679 | gix 更快 |
| blame | 0.827 | 0.830 | gix 更快 |
| diff `HEAD^..HEAD` | 0.661 | 0.669 | gix 更快 |

方向与主结果一致。`kite-perf --gix-first` 保留在工具中，发布前应在 Windows 和 macOS 都运行两种顺序。

## 5. 判断

- **status**：Git CLI 的 p50 和 p95 都明显更快。gix 的外部 status 命令还有输出格式差异，不能据此证明库内 API 更慢，但也不支持现在替换。
- **log**：gix 在两种执行顺序下都更快。
- **blame**：gix 在两种执行顺序下都更快。
- **diff**：gix 在两种执行顺序下都更快。

合并后端会增加错误分类、配置兼容、路径编码和测试矩阵的维护成本。status 仍然明显更慢，log、blame 和 diff 的局部优势不足以支付两套读后端的复杂度，因此当前结论是：

1. 所有写操作继续使用 Git CLI。
2. status、log、blame、diff 继续使用现有 Git CLI 读路径。
3. 在客户端窗口和 diff 查看器稳定后，如仍有明确性能目标，再评估把 blame/diff 单独迁到 gix 库内 API。
4. 任何后端切换必须附带 Windows 和 macOS 的多轮 release 结果。

## 6. 限制

- 这是外部进程级对比，不是 gix 库内 API 与 `std::process::Command` 的微基准。
- Git status 输出 porcelain v1；gix 0.61 只支持 simplified status 展示。两者工作量相近但输出不同。
- blame 和 diff 的终端输出格式不同，结论只使用耗时，不比较文本吞吐。
- 主结果只有一台 macOS arm64 机器和一次 5 样本 release 运行；顺序检查是 debug、3 样本，没有 Windows 数据和跨机重复。
- 文件系统缓存未清理，读取类结果通常偏热。

跨平台发布判断仍应使用 `.github/workflows/performance.yml` 的多轮聚合结果。
