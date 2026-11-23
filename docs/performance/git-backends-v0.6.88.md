# Kite 0.6.88 Git 后端对比

结论：在单台 macOS arm64 的两次 release smoke 基准中，`gix 0.61.0` 的 status 稳定慢于 Git CLI；log 和 diff 的 p50 稳定更快；blame 和 log 的 p95 仍受执行顺序与单次抖动影响。证据不足以支持立即混用两套后端，当前继续使用 Git CLI。

## 1. 来源

- 基准工具：`src/bin/kite-perf.rs`
- 源码 commit：`465b29b`
- 配置：`smoke`
- 构建：`release`
- Git：`git 2.54.0`
- gix：`gix 0.61.0`
- 每场景交互样本：5
- 冷启动样本：1

两次运行只改变后端执行顺序：

```bash
target/release/kite-perf \
  --profile smoke \
  --git-only \
  --samples 5 \
  --cold-samples 1 \
  --gix /path/to/gix \
  --output target/performance/git-smoke-release-cli-first.json \
  --markdown target/performance/git-smoke-release-cli-first.md

target/release/kite-perf \
  --profile smoke \
  --git-only \
  --gix-first \
  --samples 5 \
  --cold-samples 1 \
  --gix /path/to/gix \
  --output target/performance/git-smoke-release-gix-first.json \
  --markdown target/performance/git-smoke-release-gix-first.md
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

### Git CLI first

| 操作 | CLI p50 | gix p50 | p50 比值 | CLI p95 | gix p95 | p95 比值 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| status | 43.964 | 54.566 | 1.241 | 54.188 | 257.696 | 4.756 |
| log 第一屏（100） | 34.224 | 26.004 | 0.760 | 45.074 | 31.858 | 0.707 |
| blame | 43.278 | 44.401 | 1.026 | 63.323 | 53.265 | 0.841 |
| diff `HEAD^..HEAD` | 44.654 | 32.488 | 0.728 | 48.141 | 38.728 | 0.804 |

### gix first

| 操作 | CLI p50 | gix p50 | p50 比值 | CLI p95 | gix p95 | p95 比值 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| status | 71.895 | 121.813 | 1.694 | 87.290 | 179.237 | 2.053 |
| log 第一屏（100） | 43.072 | 32.110 | 0.746 | 122.637 | 127.419 | 1.039 |
| blame | 72.438 | 51.417 | 0.710 | 77.338 | 139.012 | 1.797 |
| diff `HEAD^..HEAD` | 31.909 | 24.788 | 0.777 | 71.146 | 28.096 | 0.395 |

## 4. 判断

- **status**：两种顺序下 gix p50 和 p95 都更慢。外部 gix 0.61 只支持 simplified status 展示，因此结论只用于进程级对比，但足以否定现在替换。
- **log**：两种顺序下 gix p50 都更快；p95 在 CLI-first 更快，在 gix-first 略慢，尚未达到稳定的 20% 门槛。
- **blame**：两种顺序下 gix p50 都更快；p95 方向随顺序翻转，单次样本中存在明显抖动。
- **diff**：两种顺序下 gix p50 和 p95 都更快。

合并后端会增加错误分类、配置兼容、路径编码和测试矩阵的维护成本。status 需要继续用 CLI，而 log/blame/diff 尚未在两种顺序下同时满足稳定的 p95 门槛，因此当前结论是：

1. 所有写操作继续使用 Git CLI。
2. status、log、blame、diff 继续使用现有 Git CLI 读路径。
3. 在客户端窗口和 diff 查看器稳定后，如仍有明确性能目标，再评估把 diff 等局部读路径迁到 gix 库内 API。
4. 任何后端切换必须附带 Windows 和 macOS 的多轮 release 结果，并同时满足既定的 20% p95 门槛。

## 5. 限制

- 这是外部进程级对比，不是 gix 库内 API 与 `std::process::Command` 的微基准。
- Git status 输出 porcelain v1；gix 0.61 只支持 simplified status 展示。两者工作量相近但输出不同。
- blame 和 diff 的终端输出格式不同，结论只使用耗时，不比较文本吞吐。
- 只有一台 macOS arm64 机器和两次 5 样本 release 运行，没有 Windows 数据、跨机重复或独立进程聚合。
- 文件系统缓存未清理；反向执行只能降低，不能消除缓存和单次抖动。

跨平台发布判断仍应使用 `.github/workflows/performance.yml` 的多轮聚合结果。
