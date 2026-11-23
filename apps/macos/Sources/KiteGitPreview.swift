import SwiftUI

/// Git section of the preview pane. Loads after the pane appears so git
/// queries never delay the Quick Look surface.
struct KiteGitPreviewSection: View {
    @ObservedObject var model: KiteModel
    let entry: BrowserEntry

    @State private var summary: GitPathSummaryDto?
    @State private var loading = false
    @State private var diffVisible = false
    @State private var diff: [GitDiffLineDto]?
    @State private var diffError: String?
    @State private var actionMessage: String?

    var body: some View {
        // A VStack always exists in the hierarchy, so `.task` runs even while
        // the section has nothing to show yet; an empty Group would not.
        VStack(spacing: 0) {
            if loading && summary == nil {
                container {
                    Text("正在读取 Git 信息…")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.tertiary)
                }
            } else if let summary {
                container { content(summary) }
            }
        }
        .task(id: entry.path) { await load() }
    }

    private func container<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 5) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 10, weight: .semibold))
                Text("Git")
                    .font(.system(size: 10, weight: .semibold))
                    .textCase(.uppercase)
                    .tracking(0.7)
                Spacer()
            }
            .foregroundStyle(.tertiary)

            content()
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            Color(nsColor: .controlBackgroundColor).opacity(0.28),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.06), lineWidth: 0.5)
        }
    }

    @ViewBuilder
    private func content(_ summary: GitPathSummaryDto) -> some View {
        Text(branchLine(summary))
            .font(.system(size: 12, weight: .medium))
            .lineLimit(1)
            .truncationMode(.middle)

        if let operation = summary.operation {
            Text(operation)
                .font(.system(size: 10.5, weight: .medium))
                .foregroundStyle(.orange)
        }

        if summary.isDirectory {
            Text(changeCountsText(summary.changes))
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
        } else {
            HStack(spacing: 8) {
                if let staged = summary.stagedStatus {
                    Text("已暂存 \(staged)")
                        .foregroundStyle(statusColor(summary.stagedCode))
                }
                if let unstaged = summary.unstagedStatus {
                    Text("工作区 \(unstaged)")
                        .foregroundStyle(statusColor(summary.unstagedCode))
                }
                if summary.ignored {
                    Text("已忽略").foregroundStyle(.secondary)
                } else if summary.stagedStatus == nil && summary.unstagedStatus == nil {
                    Text("无变更").foregroundStyle(.secondary)
                }
                if summary.linesAdded > 0 || summary.linesRemoved > 0 {
                    Text("+\(summary.linesAdded)").foregroundStyle(.green)
                    Text("-\(summary.linesRemoved)").foregroundStyle(.red)
                }
            }
            .font(.system(size: 10.5, weight: .medium))
        }

        if let commit = summary.lastCommit {
            VStack(alignment: .leading, spacing: 2) {
                Text("\(commit.shortHash)  \(commit.author)  \(commitDate(commit.unixSeconds))")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                Text(commit.subject)
                    .font(.system(size: 10.5))
                    .lineLimit(2)
                    .textSelection(.enabled)
            }
        }

        if !summary.isDirectory && !summary.ignored {
            actions(summary)
        }

        if let actionMessage {
            Text(actionMessage)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }

        if diffVisible {
            diffView
        }
    }

    private func actions(_ summary: GitPathSummaryDto) -> some View {
        let changed = summary.stagedStatus != nil || summary.unstagedStatus != nil
        let conflicted = summary.stagedCode == "C" || summary.unstagedCode == "C"
        return HStack(spacing: 6) {
            if changed {
                actionButton(diffVisible ? "隐藏差异" : "查看差异") { toggleDiff() }
            }
            if conflicted {
                actionButton("解决冲突") {
                    KiteGitMergeWindowController.shared.show(model: model, path: entry.path)
                }
            } else if summary.unstagedStatus != nil {
                actionButton("暂存") { run(.add) }
            }
            if !conflicted && summary.stagedStatus != nil {
                actionButton("取消暂存") { run(.unstage) }
            }
            actionButton("历史") { showLog() }
        }
    }

    private func actionButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(title, action: action)
            .buttonStyle(.borderless)
            .font(.system(size: 10.5, weight: .medium))
            .help(title)
    }

    @ViewBuilder
    private var diffView: some View {
        if let diffError {
            Text(diffError)
                .font(.system(size: 10.5))
                .foregroundStyle(.red)
        } else if let diff {
            if diff.isEmpty {
                Text("没有可显示的差异")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.tertiary)
            } else {
                ScrollView([.vertical, .horizontal]) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(diff.enumerated()), id: \.offset) { _, line in
                            diffLine(line)
                        }
                    }
                    .padding(4)
                }
                .frame(maxHeight: 320)
                .background(
                    Color.primary.opacity(0.035),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
            }
        } else {
            Text("正在读取差异…")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
        }
    }

    private func diffLine(_ line: GitDiffLineDto) -> some View {
        let prefix: String
        let color: Color
        let background: Color
        switch line.kind {
        case .added:
            prefix = "+"
            color = .green
            background = Color.green.opacity(0.12)
        case .removed:
            prefix = "-"
            color = .red
            background = Color.red.opacity(0.12)
        case .hunk:
            prefix = ""
            color = .secondary
            background = .clear
        case .context:
            prefix = " "
            color = .primary
            background = .clear
        }
        return Text(prefix + line.text)
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(color)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(background)
    }

    private func load() async {
        // Entries without a status in a column without Git are outside a work
        // tree; skip the extra git processes for them.
        guard entry.gitStatus != nil || model.isGitAvailable(forEntryPath: entry.path) else {
            summary = nil
            return
        }
        loading = true
        diff = nil
        diffError = nil
        diffVisible = false
        actionMessage = nil
        summary = await model.gitPathSummary(path: entry.path)
        loading = false
    }

    private func toggleDiff() {
        diffVisible.toggle()
        guard diffVisible, diff == nil, diffError == nil else { return }
        let path = entry.path
        Task {
            do {
                diff = try await model.gitFileDiff(path: path)
            } catch {
                diffError = error.localizedDescription
            }
        }
    }

    private func run(_ operation: VcsOperationDto) {
        let path = entry.path
        Task {
            actionMessage = await model.runVcsOperation(tool: .git, operation: operation, path: path)
            summary = await model.gitPathSummary(path: path)
            diff = nil
            diffError = nil
            if diffVisible {
                diffVisible = false
                toggleDiff()
            }
        }
    }

    private func showLog() {
        let path = entry.path
        Task {
            await model.openVcsLog(tool: .git, path: path)
        }
    }

    private func branchLine(_ summary: GitPathSummaryDto) -> String {
        var line = summary.branch
            ?? summary.headShortHash.map { "分离 HEAD \($0)" }
            ?? "无分支"
        if summary.ahead > 0 || summary.behind > 0 {
            line += "  ↑\(summary.ahead) ↓\(summary.behind)"
        }
        return line
    }

    private func changeCountsText(_ counts: GitChangeCountsDto) -> String {
        let parts: [(String, UInt32)] = [
            ("修改", counts.modified),
            ("新增", counts.added),
            ("删除", counts.deleted),
            ("重命名", counts.renamed),
            ("未跟踪", counts.untracked),
            ("冲突", counts.conflicted),
        ]
        let text = parts
            .filter { $0.1 > 0 }
            .map { "\($0.0) \($0.1)" }
            .joined(separator: " · ")
        return text.isEmpty ? "没有未提交的变更" : text
    }

    /// Matches the column badge palette in `KiteVcsBadge`.
    private func statusColor(_ code: String?) -> Color {
        switch code {
        case "C", "!", "D": return .red
        case "R", "A": return .green
        case "P", "M": return .orange
        case "?": return .cyan
        default: return .secondary
        }
    }

    private func commitDate(_ unixSeconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(unixSeconds))
            .formatted(date: .abbreviated, time: .shortened)
    }
}
