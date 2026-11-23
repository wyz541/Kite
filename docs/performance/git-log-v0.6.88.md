# Git 提交历史性能（100,000 commits）

## 结论

提交历史采用 200 条一页的增量读取。第一页在本次 Windows 基准中为 **493 ms**，满足 500 ms 的 Core 目标；第二页在首屏详情加载期间后台预取。列表使用虚拟化渲染，只创建当前可见行。

## 环境

- 日期：2026-11-23
- 系统：Windows 11 Enterprise 10.0.26200
- CPU：Intel Core i5-14600KF，14 核 / 20 线程
- 内存：64 GiB
- Rust：1.99.1
- Git：2.54.0.windows.1
- 构建：`cargo test` 的 `test` profile（项目设置 `opt-level = 1`，依赖 `opt-level = 3`）
- 缓存：未清理操作系统缓存；以下结果是热缓存基准

## 数据夹具

`git::log::tests::benchmark_large_history` 使用 `git fast-import` 生成 100,000 个线性提交。每个提交修改同一个 `file.txt`。测试不依赖工作区仓库大小，可单独复现。

```powershell
cargo test --lib benchmark_large_history -- --ignored --nocapture
```

## 结果

### 第一页

| 指标 | 数值 |
| --- | ---: |
| Core 总耗时 | 493 ms |
| Git 子进程 | 159 ms |
| 解析 + 图布局 | < 1 ms |
| 返回提交 | 200 |
| 活跃泳道 | 1 |

同一夹具上的 Git 排序对比：

| 排序 | Git 子进程耗时 |
| --- | ---: |
| 默认倒序 | 134 ms |
| `--topo-order` | 1,024 ms |
| `--date-order` | 657 ms |
| `--author-date-order` | 898 ms |

`--topo-order` 必须先遍历完整历史，无法满足首屏预算。分页查询因此使用 Git 默认倒序；提交图仍由 Core 根据父提交哈希和跨页游标计算。

### 连续分页

首屏之后预取一页。随后连续读取 10 页、每页 200 条：

| 指标 | 数值 |
| --- | ---: |
| p50 | 411 ms |
| p95 | 702 ms |
| 最慢 | 745 ms |

分页等待不会阻塞列表绘制。Windows 使用 `uniform_list`，macOS 使用 `LazyVStack`；两端只创建可见行，并在接近页尾时继续加载。

## 边界

- 这是 Core 数据路径基准，不包含 GPUI 或 SwiftUI 的合成耗时。
- 100,000 提交夹具为线性历史，用于测量分页和首屏；合并、多父提交和多个根提交由独立图布局快照测试覆盖。
- 页面读取使用 `git log --skip`，因此页码越深，Git 需要跳过的提交越多。连续预取可隐藏通常的滚动等待，但极深分页仍可能达到亚秒级。


