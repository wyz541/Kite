# Blame 首屏性能基线（v0.6.89）

日期：2026-11-23

## 场景

- 临时 Git 仓库。
- 单文件 10,000 行。
- 文件只有一次提交。
- 调用共享 Core 的分页 Blame API，读取第 1 至 400 行。
- 计时包含读取文件行数和启动 `git blame --line-porcelain -L 1,400`。

## 环境

| 项目 | 值 |
| --- | --- |
| 机器 | Apple M1 Max |
| 系统 | macOS 27.0.1 |
| Git | 2.54.0 (Apple Git-157) |
| Rust | 1.99.1 |
| Profile | 测试 profile（Kite `opt-level = 1`，依赖 `opt-level = 3`） |

## 命令

```bash
cargo test --locked --lib \
  git::blame::tests::benchmark_ten_thousand_line_first_page \
  -- --ignored --nocapture
```

## 结果

| 运行 | 首屏耗时 |
| --- | ---: |
| 冷进程首次 | 42 ms |
| 热运行 1 | 38 ms |
| 热运行 2 | 36 ms |
| 热运行 3 | 36 ms |
| 热运行中位数 | 36 ms |

## 结论

- 首屏固定只请求 400 行，避免一次解析完 10,000 行。
- 最终 Git Service 路径的冷进程首次为 42 ms，热运行中位数 36 ms。
- 测量受机器负载影响；应使用同一夹具、同一页大小和同一 target profile 比较。
- 大文件剩余行按页追加，不阻塞首屏渲染。
- 后续若评估 `gix`，必须用同一夹具和同一页大小比较。
