# Kite Agent ACP 性能基线

## 结论

`kite-agent-perf` 使用同一个 ACP fixture 记录 Agent 接入的性能边界。
它测量协议启动、流式输出、子进程内存和长输出准备，不测量模型推理质量或不同供应商排名。

## 场景

- `long`：256 个 1 KiB 文本块，记录吞吐、累积字节和长输出追加耗时。
- `slow`：3 个带间隔的小文本块，验证流式节奏不会阻塞读取线程。
- `markdown`：4 个 Markdown/代码块分片，验证分片顺序和内容完整。

## 指标

| 指标 | 含义 |
|---|---|
| `startup_ms` | 从启动 fixture 到 `initialize` 和 `session/new` 完成 |
| `peak_agent_memory_bytes` | 输出期间采样到的 Agent 子进程峰值工作集 |
| `output_bytes` | 收到的 Agent 文本总字节数 |
| `output_elapsed_ms` | 从发送 `session/prompt` 到收到完成响应 |
| `throughput_mib_per_second` | 文本吞吐，仅用于同平台回归比较 |
| `long_output_append_ms` | 2,000 条上限的对话队列追加耗时 |
| `process_cleanup_ms` | 结束并回收 fixture 进程 |

## 运行

```bash
cargo build --locked --release --bin kite-agent-fixture --bin kite-agent-perf
target/release/kite-agent-perf \
  --scenario long \
  --runs 3 \
  --output docs/performance/results/v0.6.94-agent-<platform>.json
```

Windows PowerShell：

```powershell
cargo build --locked --release --bin kite-agent-fixture --bin kite-agent-perf
target\release\kite-agent-perf.exe `
  --scenario long `
  --runs 3 `
  --output docs/performance/results/v0.6.94-agent-windows-x64.json
```

## 结果

Windows x64 基线见 `results/v0.6.94-agent-windows-x64.json`。
macOS arm64 必须在 Apple Silicon 实机运行同一命令后生成对应 JSON；不得从 Windows 推导 macOS 数值。

### Windows x64

- 日期：2026-11-24。
- 环境：Windows 11 Enterprise，Intel Core i5-14600KF，20 个逻辑核心。
- 构建：release。
- 运行次数：3 次独立 fixture 进程，取中位数。

| 指标 | 结果 |
|---|---:|
| 启动到 ACP Ready | 124 ms |
| Agent 子进程峰值内存 | 4,591,616 bytes（约 4.38 MiB） |
| 输出字节 | 262,144 bytes |
| 输出完成时间 | 159 ms |
| 文本吞吐 | 1.56 MiB/s |
| 2,000 条长输出追加 | 0.117 ms |
| 进程清理 | 44 ms |

macOS arm64 结果仍待 Apple Silicon CI 或实机运行，未以推测值补齐。

## 解释边界

- fixture 不调用模型，因此数值不包含模型首 token 延迟和生成速度。
- 子进程内存包含 fixture 运行时，不包含外部 Agent 的模型缓存或工具子进程。
- 长输出滚动必须在两端 UI 验收中另测帧率和输入响应；本基准只提供可复现的数据准备成本。
- 比较时必须使用相同 fixture 版本、场景、采样间隔和 release 构建。
