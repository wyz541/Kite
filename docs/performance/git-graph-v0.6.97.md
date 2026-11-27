# Git Graph scope 性能（100,000 提交 / 10,000 文件）

## 结论

目录 scope 使用 `--full-history` 后，100,000 提交线性仓库的首屏 Core 总耗时为 **93 ms**，仓库级 Graph 为 **90 ms**。额外开销约 **3 ms**，低于 500 ms 首屏预算。10,000 文件目录 scope 的首屏为 **91 ms**。

## 环境

- 日期：2026-11-27
- 系统：Windows 11 Enterprise 10.0.26200
- CPU：Intel Core i5-14600KF，14 核 / 20 线程
- 内存：64 GiB
- Rust：1.99.1
- Git：2.54.0.windows.1
- 构建：`cargo test` 的 `test` profile（项目设置 `opt-level = 1`，依赖 `opt-level = 3`）
- 缓存：未清理操作系统缓存；以下结果是热缓存基准

## 数据夹具

`git::log::tests::benchmark_graph_scope_history` 使用 `git fast-import` 生成两个夹具：

- 100,000 个线性提交，每个提交修改根目录 `file.txt`；
- 单个提交包含 `src/git/` 下 10,000 个文件。

```powershell
cargo test --lib git::log::tests::benchmark_graph_scope_history -- --ignored --nocapture
```

## 结果

| 场景 | Core 总耗时 | Git 子进程 | 解析 | 图布局 | 返回提交 | 活跃泳道 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 100,000 提交，Repository | 90 ms | 48 ms | < 1 ms | < 1 ms | 200 | 1 |
| 100,000 提交，Directory `.` + `--full-history` | 93 ms | 52 ms | < 1 ms | < 1 ms | 200 | 1 |
| 10,000 文件，Directory `src/git` | 91 ms | 48 ms | < 1 ms | < 1 ms | 1 | 0 |


## macOS arm64 复核

- 日期：2026-11-27
- 系统：macOS（Apple Silicon）
- 构建：`cargo test` 的 `test` profile
- 缓存：未清理操作系统缓存；热缓存基准
- 隐私：按截图策略不记录具体设备型号、内存或磁盘容量

```bash
cargo test --locked --lib git::log::tests::benchmark_graph_scope_history -- --ignored --nocapture
```

| 场景 | Core 总耗时 | Git 子进程 | 解析 | 图布局 | 返回提交 | 活跃泳道 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 100,000 提交，Repository | 82 ms | 44 ms | < 1 ms | < 1 ms | 200 | 1 |
| 100,000 提交，Directory `.` + `--full-history` | 67 ms | 33 ms | < 1 ms | < 1 ms | 200 | 1 |
| 10,000 文件，Directory `src/git` | 51 ms | 25 ms | < 1 ms | < 1 ms | 1 | 0 |

macOS 复核同样低于 500 ms 首屏预算。`--full-history` 的耗时小于仓库级 Graph 的本次测量值，说明该夹具下额外历史简化开销没有形成可见回归；不同仓库仍需持续采样。

## 边界

- 这是 Core 数据路径基准，不包含 GPUI 或 SwiftUI 的合成耗时。
- 100,000 提交夹具为线性历史，用于测量分页和 scope 开销；merge 拓扑由 `graph_directory_scope_preserves_merge_topology_with_full_history` 单独锁定。
- 10,000 文件夹具只有一次提交，用于测量宽目录 pathspec；不代表 10,000 个文件各自都有历史。
- 页面读取继续使用 `git log --skip`，极深分页仍可能需要预取来隐藏等待。
