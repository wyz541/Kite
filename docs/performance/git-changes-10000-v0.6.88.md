# Git 变更列表 1 万文件基线

## 结论

在 Windows x64 上，`repository_changes` 读取 10,000 个已修改文件耗时 **257 ms**。

该时间覆盖一次 `git rev-parse` 和一次 `git status --porcelain=v2 --branch -z`，不包含
文件内容 diff、UI 首帧或目录树构建。

## 环境

- Kite 基线：`0.6.88`
- 日期：2026-11-23
- 平台：Windows x64
- Git：本机 PATH 中的 Git CLI
- 仓库：临时仓库，100 个目录，每目录 100 个文本文件
- 变更：10,000 个已跟踪文件全部修改

## 复现

```powershell
cargo test --lib git_changes::tests::benchmark_ten_thousand_changed_files -- --ignored --nocapture
```

测试创建临时仓库、提交 10,000 个文件、修改全部文件，再计时 `repository_changes`。
整个测试过程包含仓库创建和提交，约 38.8 秒；上面的 257 ms 仅为核心列表读取。

## 边界

- 结果受磁盘缓存、杀毒软件和 Git 版本影响。
- 500 ms 内完成列表读取作为当前交互基线；实际发布机器仍应在 #129 的统一性能矩阵中复测。
