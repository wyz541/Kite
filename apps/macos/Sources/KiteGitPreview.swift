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
    @State private var blameVisible = false
    @State private var blameLoading = false
    @State private var blamePages: [GitBlamePageDto] = []
    @State private var blameError: String?
    @State private var ignoreWhitespace = true
    @State private var selectedBlame: GitBlameLineDto?
    @State private var blameGeneration = 0

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

    private var blameLines: [GitBlameLineDto] {
        blamePages.flatMap(\.lines)
    }

    private var blameEndLine: UInt32 {
        blamePages.last?.endLine ?? 0
    }

    private var blameTotalLines: UInt32 {
        blamePages.last?.totalLines ?? 0
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
                if let submodule = entry.submoduleStatus {
                    Text(submodule).foregroundStyle(.secondary)
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

        if blameVisible {
            blameView
        }
    }

    private func actions(_ summary: GitPathSummaryDto) -> some View {
        let changed = summary.stagedStatus != nil || summary.unstagedStatus != nil
        return HStack(spacing: 6) {
            if changed {
                actionButton(diffVisible ? "隐藏差异" : "查看差异") { toggleDiff() }
            }
            if summary.unstagedStatus != nil {
                actionButton("暂存") { run(.add) }
            }
            if summary.stagedStatus != nil {
                actionButton("取消暂存") { run(.unstage) }
            }
            actionButton(blameVisible ? "隐藏 Blame" : "Blame") { toggleBlame() }
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

    @ViewBuilder
    private var blameView: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("逐行 Blame")
                    .font(.system(size: 10.5, weight: .semibold))
                Spacer()
                Toggle("忽略空白", isOn: $ignoreWhitespace)
                    .toggleStyle(.checkbox)
                    .font(.system(size: 9.5))
                    .onChange(of: ignoreWhitespace) { _ in
                        reloadBlame()
                    }
            }

            if let blameError {
                Text(blameError)
                    .font(.system(size: 10))
                    .foregroundStyle(.red)
            } else if blameLoading, blamePages.isEmpty {
                Text("正在读取 Blame…")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.tertiary)
            } else if !blamePages.isEmpty {
                ScrollView(.vertical) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(blameLines, id: \.lineNumber) { line in
                            Button {
                                selectedBlame = line
                                model.openGitLog(
                                    path: entry.path,
                                    initialHash: line.commitHash
                                )
                            } label: {
                                HStack(alignment: .top, spacing: 6) {
                                    Text("\(line.lineNumber)")
                                        .foregroundStyle(.quaternary)
                                        .frame(width: 30, alignment: .trailing)
                                    Text(line.shortHash)
                                        .foregroundStyle(blameColor(line))
                                        .frame(width: 48, alignment: .leading)
                                    Text(line.author)
                                        .foregroundStyle(blameColor(line))
                                        .frame(width: 72, alignment: .leading)
                                        .lineLimit(1)
                                    Text(line.code)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                    Spacer(minLength: 0)
                                }
                                .font(.system(size: 9.5, design: .monospaced))
                                .padding(.horizontal, 4)
                                .frame(height: 18)
                                .background(
                                    selectedBlame?.lineNumber == line.lineNumber
                                        ? Color.accentColor.opacity(0.12)
                                        : Color.clear
                                )
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .help(blameTooltip(line))
                        }
                    }
                    .padding(.vertical, 3)
                }
                .frame(maxHeight: 340)
                .background(
                    Color.primary.opacity(0.035),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )

                HStack {
                    Text("显示 \(blameLines.count) / \(blameTotalLines) 行")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.tertiary)
                    Spacer()
                    if blameEndLine < blameTotalLines {
                        Button("加载更多") { loadMoreBlame() }
                            .buttonStyle(.borderless)
                            .font(.system(size: 10))
                    }
                }
            }

            if let selectedBlame {
                VStack(alignment: .leading, spacing: 3) {
                    HStack {
                        Text(selectedBlame.shortHash)
                            .font(.system(size: 10, weight: .semibold, design: .monospaced))
                        Spacer()
                        Button {
                            self.selectedBlame = nil
                        } label: {
                            Image(systemName: "xmark")
                        }
                        .buttonStyle(.borderless)
                    }
                    Text(selectedBlame.summary)
                        .font(.system(size: 10.5, weight: .medium))
                        .lineLimit(2)
                    Text("\(selectedBlame.author) · \(commitDate(selectedBlame.authorTime))")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.tertiary)
                }
                .padding(8)
                .background(
                    Color.accentColor.opacity(0.075),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
            }
        }
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
        blameVisible = false
        blamePages = []
        blameError = nil
        selectedBlame = nil
        blameGeneration += 1
        summary = await model.gitPathSummary(path: entry.path)
        loading = false
    }

    private func toggleBlame() {
        blameVisible.toggle()
        if blameVisible && blamePages.isEmpty {
            reloadBlame()
        }
    }

    private func reloadBlame() {
        guard blameVisible else { return }
        blamePages = []
        blameError = nil
        selectedBlame = nil
        blameGeneration += 1
        loadBlame(startLine: 1)
    }

    private func loadMoreBlame() {
        guard let page = blamePages.last, blameEndLine < blameTotalLines else { return }
        loadBlame(startLine: page.endLine + 1)
    }

    private func loadBlame(startLine: UInt32) {
        blameLoading = true
        let path = entry.path
        let ignoreWhitespace = ignoreWhitespace
        let generation = blameGeneration
        Task {
            do {
                let page = try await model.gitBlame(
                    path: path,
                    startLine: startLine,
                    lineCount: 400,
                    ignoreWhitespace: ignoreWhitespace
                )
                guard generation == blameGeneration else { return }
                if startLine == 1 {
                    blamePages = [page]
                } else {
                    blamePages.append(page)
                }
            } catch {
                guard generation == blameGeneration else { return }
                blameError = error.localizedDescription
            }
            blameLoading = false
        }
    }

    private func blameColor(_ line: GitBlameLineDto) -> Color {
        if line.isUncommitted {
            return .red
        }
        let age = max(0, Date().timeIntervalSince1970 - TimeInterval(line.authorTime))
        let days = age / 86_400
        return Color.accentColor.opacity(max(0.42, 1 - min(days, 730) / 900))
    }

    private func blameTooltip(_ line: GitBlameLineDto) -> String {
        var parts = [
            line.shortHash,
            line.author,
            commitDate(line.authorTime),
            line.summary,
        ]
        if let mail = line.authorMail {
            parts.append(mail)
        }
        return parts.joined(separator: " · ")
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
        model.openGitLog(path: entry.path)
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
