import AppKit
import SwiftUI

@MainActor
final class KiteGitClientWindowController: NSWindowController, NSWindowDelegate {
    static let shared = KiteGitClientWindowController()

    private var hostingController: NSHostingController<AnyView>?

    func show(model: KiteModel, repositoryPath: String) {
        let root = AnyView(
            KiteGitClientWindowView(model: model, repositoryPath: repositoryPath)
        )
        if let hostingController {
            hostingController.rootView = root
        } else {
            let hosting = NSHostingController(rootView: root)
            let gitWindow = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 1160, height: 740),
                styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                backing: .buffered,
                defer: false
            )
            gitWindow.title = "Git"
            gitWindow.titlebarAppearsTransparent = true
            gitWindow.minSize = NSSize(width: 900, height: 580)
            gitWindow.isReleasedWhenClosed = false
            gitWindow.contentViewController = hosting
            gitWindow.delegate = self
            gitWindow.center()
            gitWindow.setFrameAutosaveName("KiteGitClientWindow")
            hostingController = hosting
            window = gitWindow
        }
        if window?.isMiniaturized == true {
            window?.deminiaturize(nil)
        }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
}

private struct GitLineSelectionKey: Hashable {
    let hunkIndex: UInt32
    let oldLine: UInt32?
    let newLine: UInt32?

    init(_ selection: GitLineSelectionDto) {
        hunkIndex = selection.hunkIndex
        oldLine = selection.oldLine
        newLine = selection.newLine
    }

}

private enum GitDisplayRow: Identifiable {
    case group(GitChangeGroupDto, Int)
    case directory(path: String, label: String, depth: Int, group: GitChangeGroupDto)
    case file(GitChangeFileDto, depth: Int)

    var id: String {
        switch self {
        case .group(let group, let count):
            return "group:\(gitGroupCode(group)):\(count)"
        case .directory(let path, _, _, let group):
            return "dir:\(gitGroupCode(group)):\(path)"
        case .file(let file, _):
            return "file:\(gitGroupCode(file.group)):\(file.path)"
        }
    }
}

private struct KiteGitClientWindowView: View {
    @ObservedObject var model: KiteModel
    @State private var repositoryPath: String
    @State private var changes: GitRepositoryChangesDto?
    @State private var selectedPath: String?
    @State private var selectedGroup: GitChangeGroupDto?
    @State private var selectedIsDirectory = false
    @State private var diff: GitFileChangesDiffDto?
    @State private var checkedLines: Set<GitLineSelectionKey> = []
    @State private var filter = ""
    @State private var showIgnored = false
    @State private var treeMode = true
    @State private var loading = true
    @State private var loadingDiff = false
    @State private var operating = false
    @State private var statusMessage: String?
    @State private var errorMessage: String?
    @State private var showDiscardConfirmation = false
    @State private var pendingDiscardLines: [GitLineSelectionDto]?
    @State private var pendingDiscardPaths: [String]?
    @State private var discardDescription = "此操作无法撤销。"

    init(model: KiteModel, repositoryPath: String) {
        self.model = model
        _repositoryPath = State(initialValue: repositoryPath)
    }

    var body: some View {
        ZStack {
            KiteGlassBackdrop()

            VStack(spacing: 0) {
                header
                Rectangle()
                    .fill(Color.primary.opacity(0.07))
                    .frame(height: 0.5)

                HSplitView {
                    changesPanel
                        .frame(minWidth: 300, idealWidth: 360, maxWidth: 460)
                    diffPanel
                        .frame(minWidth: 520, maxWidth: .infinity)
                }
            }
        }
        .frame(minWidth: 900, minHeight: 580)
        .task(id: repositoryPath) {
            await loadChanges()
        }
        .alert("丢弃所选改动？", isPresented: $showDiscardConfirmation) {
            Button("取消", role: .cancel) {
                pendingDiscardLines = nil
                pendingDiscardPaths = nil
            }
            Button("丢弃", role: .destructive) {
                Task { await confirmDiscard() }
            }
        } message: {
            Text(discardDescription)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 12, weight: .semibold))
                Text(branchLabel)
                    .font(.system(size: 12.5, weight: .semibold))
                    .lineLimit(1)
                if let changes, changes.ahead > 0 || changes.behind > 0 {
                    Text("↑\(changes.ahead) ↓\(changes.behind)")
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                if let operation = changes?.operation {
                    Text(operation)
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(.orange)
                }
                Spacer()
                Text(repositoryPath)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Button {
                    Task { await loadChanges() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .disabled(loading || operating)

                Toggle("显示忽略", isOn: $showIgnored)
                    .toggleStyle(.checkbox)
                    .controlSize(.small)
                    .onChange(of: showIgnored) { _ in
                        Task { await loadChanges() }
                    }
                Toggle("目录树", isOn: $treeMode)
                    .toggleStyle(.checkbox)
                    .controlSize(.small)
            }
            HStack(spacing: 8) {
                if let statusMessage {
                    Text(statusMessage)
                        .foregroundStyle(.secondary)
                }
                if let errorMessage {
                    Text(errorMessage)
                        .foregroundStyle(.red)
                }
                if let warning = changes?.crlfWarning {
                    Text(warning)
                        .foregroundStyle(.orange)
                }
            }
            .font(.system(size: 10.5))
            .lineLimit(2)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial)
    }

    private var changesPanel: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("变更")
                        .font(.system(size: 12, weight: .semibold))
                    Spacer()
                    Text("\(changes?.files.count ?? 0) 项")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                }
                Text(countsLabel)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)

                TextField("按路径筛选", text: $filter)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)

                selectedPathActions
            }
            .padding(12)
            .background(Color.primary.opacity(0.025))

            Rectangle()
                .fill(Color.primary.opacity(0.07))
                .frame(height: 0.5)

            if loading && changes == nil {
                emptyState("正在读取 Git 变更…")
            } else if displayRows.isEmpty {
                emptyState(changes?.files.isEmpty == true ? "工作区干净" : "没有匹配的变更")
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(displayRows) { row in
                            displayRow(row)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var selectedPathActions: some View {
        if let selectedPath, let selectedGroup {
            HStack(spacing: 6) {
                switch selectedGroup {
                case .staged:
                    Button("取消暂存路径") {
                        Task { await unstagePaths([selectedPath]) }
                    }
                case .unstaged, .untracked, .conflicted:
                    Button("暂存路径") {
                        Task { await stagePaths([selectedPath]) }
                    }
                    if selectedGroup != .conflicted {
                        Button("丢弃路径", role: .destructive) {
                            pendingDiscardPaths = [selectedPath]
                            pendingDiscardLines = nil
                            discardDescription = selectedGroup == .untracked
                                ? "未跟踪项目会移到废纸篓。"
                                : "工作区改动会还原到暂存区内容。"
                            showDiscardConfirmation = true
                        }
                    }
                case .ignored:
                    EmptyView()
                }
                if !selectedIsDirectory && (selectedGroup == .unstaged || selectedGroup == .untracked) {
                    Menu("忽略") {
                        Button("添加到 .gitignore") {
                            Task { await addIgnore(selectedPath, local: false) }
                        }
                        Button("添加到 .git/info/exclude") {
                            Task { await addIgnore(selectedPath, local: true) }
                        }
                    }
                    .menuStyle(.borderlessButton)
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(operating)
        }
    }

    private var diffPanel: some View {
        VStack(spacing: 0) {
            if let selectedPath {
                if selectedIsDirectory {
                    VStack(spacing: 8) {
                        Image(systemName: "folder")
                            .font(.system(size: 28))
                            .foregroundStyle(.secondary)
                        Text(selectedPath)
                            .font(.system(size: 12, weight: .medium))
                        Text("目录操作会应用到其下所有变更")
                            .font(.system(size: 10.5))
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    diffContent
                }
            } else {
                emptyState("选择一个文件查看差异")
            }
        }
    }

    @ViewBuilder
    private var diffContent: some View {
        if loadingDiff {
            emptyState("正在加载差异…")
        } else if let diff {
            VStack(spacing: 0) {
                diffToolbar(diff)
                Rectangle()
                    .fill(Color.primary.opacity(0.07))
                    .frame(height: 0.5)
                if let warning = diff.lineEndingWarning {
                    Text(warning)
                        .font(.system(size: 10.5))
                        .foregroundStyle(.orange)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 7)
                        .background(Color.orange.opacity(0.08))
                }
                if diff.binary {
                    emptyState("二进制文件已变化，仅支持整文件操作")
                } else if diff.truncated {
                    emptyState("差异过大，仅显示部分内容；请使用整文件操作")
                } else if diff.hunks.isEmpty {
                    emptyState(selectedGroup.map { $0 == .conflicted } == true
                        ? "冲突文件请先解决冲突，然后暂存整个文件"
                        : "没有可显示的文本差异")
                } else {
                    ScrollView([.vertical, .horizontal]) {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(diff.hunks, id: \.index) { hunk in
                                hunkView(diff: diff, hunk: hunk)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        } else {
            emptyState("无法加载差异")
        }
    }

    private func diffToolbar(_ diff: GitFileChangesDiffDto) -> some View {
        let checked = selectedSelections()
        return HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(diff.path)
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(diff.scope == .staged ? "暂存区与 HEAD" : "工作区与暂存区")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if !checked.isEmpty {
                if let selectedGroup {
                    switch selectedGroup {
                    case .staged:
                        Button("取消暂存所选 (\(checked.count))") {
                            Task { await unstageLines(checked) }
                        }
                    case .unstaged, .untracked:
                        Button("暂存所选 (\(checked.count))") {
                            Task { await stageLines(checked) }
                        }
                        Button("丢弃所选 (\(checked.count))", role: .destructive) {
                            pendingDiscardLines = checked
                            pendingDiscardPaths = nil
                            discardDescription = "所选行会从工作区移除。"
                            showDiscardConfirmation = true
                        }
                    default:
                        EmptyView()
                    }
                }
            }
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial)
        .disabled(operating)
    }

    private func hunkView(diff: GitFileChangesDiffDto, hunk: GitDiffHunkDto) -> some View {
        let hunkKeys = Set(hunk.selections.map(GitLineSelectionKey.init))
        let allChecked = !hunkKeys.isEmpty && hunkKeys.isSubset(of: checkedLines)
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Button {
                    if allChecked {
                        checkedLines.subtract(hunkKeys)
                    } else {
                        checkedLines.formUnion(hunkKeys)
                    }
                } label: {
                    Image(systemName: allChecked ? "checkmark.square.fill" : "square")
                }
                .buttonStyle(.borderless)

                Text(hunk.header)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                hunkActions(hunk)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Color.accentColor.opacity(0.055))

            ForEach(Array(hunk.lines.enumerated()), id: \.offset) { _, line in
                diffLine(diff: diff, hunk: hunk, line: line)
            }
        }
    }

    @ViewBuilder
    private func hunkActions(_ hunk: GitDiffHunkDto) -> some View {
        let selections = hunk.selections
        if let selectedGroup {
            switch selectedGroup {
            case .staged:
                Button("取消暂存 Hunk") { Task { await unstageLines(selections) } }
            case .unstaged, .untracked:
                Button("暂存 Hunk") { Task { await stageLines(selections) } }
                Button("丢弃 Hunk", role: .destructive) {
                    pendingDiscardLines = selections
                    pendingDiscardPaths = nil
                    discardDescription = "该 Hunk 会从工作区移除。"
                    showDiscardConfirmation = true
                }
            default:
                EmptyView()
            }
        }
    }

    private func diffLine(
        diff: GitFileChangesDiffDto,
        hunk: GitDiffHunkDto,
        line: GitHunkLineDto
    ) -> some View {
        let canonical = canonicalSelection(hunk: hunk, line: line)
        let checked = canonical.map { checkedLines.contains(GitLineSelectionKey($0)) } ?? false
        let foreground: Color
        let background: Color
        let symbol: String
        switch line.kind {
        case .added:
            foreground = .green
            background = Color.green.opacity(0.10)
            symbol = "+"
        case .removed:
            foreground = .red
            background = Color.red.opacity(0.10)
            symbol = "-"
        case .context:
            foreground = .primary
            background = .clear
            symbol = " "
        }
        return HStack(spacing: 0) {
            if let canonical {
                Button {
                    let key = GitLineSelectionKey(canonical)
                    if checkedLines.remove(key) == nil {
                        checkedLines.insert(key)
                    }
                } label: {
                    Image(systemName: checked ? "checkmark.square.fill" : "square")
                        .font(.system(size: 10))
                }
                .buttonStyle(.borderless)
                .frame(width: 22)
            } else {
                Color.clear.frame(width: 22)
            }
            Text(line.oldLine.map { String($0) } ?? "")
                .frame(width: 38, alignment: .trailing)
            Text(line.newLine.map { String($0) } ?? "")
                .frame(width: 38, alignment: .trailing)
            Text(symbol)
                .frame(width: 18)
                .foregroundStyle(foreground)
            Text(line.text.isEmpty ? " " : line.text)
                .foregroundStyle(foreground)
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.system(size: 10.5, design: .monospaced))
        .frame(minHeight: 19)
        .background(background)
        .foregroundStyle(line.kind == .context ? Color.primary : foreground)
    }

    private func displayRow(_ row: GitDisplayRow) -> AnyView {
        switch row {
        case .group(let group, let count):
            return AnyView(HStack {
                Text(groupLabel(group))
                    .font(.system(size: 10.5, weight: .semibold))
                Spacer()
                Text("\(count)")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Color.primary.opacity(0.035)))
        case .directory(let path, let label, let depth, let group):
            return AnyView(Button {
                select(path: path, group: group, isDirectory: true)
            } label: {
                HStack(spacing: 7) {
                    Image(systemName: "folder")
                        .foregroundStyle(.secondary)
                    Text(label).lineLimit(1)
                    Spacer()
                }
                .padding(.leading, CGFloat(10 + depth * 14))
                .padding(.trailing, 10)
                .padding(.vertical, 5)
                .contentShape(Rectangle())
                .background(isSelected(path: path, group: group)
                    ? Color.accentColor.opacity(0.12)
                    : Color.clear)
            }
            .buttonStyle(.plain))
        case .file(let file, let depth):
            return AnyView(Button {
                select(path: file.path, group: file.group, isDirectory: false)
            } label: {
                HStack(spacing: 7) {
                    Image(systemName: iconName(for: file.path))
                        .foregroundStyle(.secondary)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(fileLabel(file, depth: depth))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        if let oldPath = file.oldPath {
                            Text("← \(oldPath)")
                                .font(.system(size: 9.5))
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                        }
                    }
                    Spacer()
                    Text(file.statusCode)
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(statusColor(file.statusCode))
                }
                .padding(.leading, CGFloat(10 + depth * 14))
                .padding(.trailing, 10)
                .padding(.vertical, 5)
                .contentShape(Rectangle())
                .background(isSelected(path: file.path, group: file.group)
                    ? Color.accentColor.opacity(0.12)
                    : Color.clear)
            }
            .buttonStyle(.plain))
        }
    }

    private var displayRows: [GitDisplayRow] {
        guard let changes else { return [] }
        let query = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        var rows: [GitDisplayRow] = []
        for group in changeGroups {
            let files = changes.files.filter {
                $0.group == group && (query.isEmpty || $0.path.lowercased().contains(query))
            }
            guard !files.isEmpty else { continue }
            rows.append(.group(group, files.count))

            if treeMode {
                var directories: Set<String> = []
                for file in files {
                    let parts = file.path.split(separator: "/")
                    guard parts.count > 1 else { continue }
                    var prefix = ""
                    for part in parts.dropLast() {
                        prefix += prefix.isEmpty ? String(part) : "/\(part)"
                        directories.insert(prefix)
                    }
                }
                for directory in directories.sorted() {
                    let depth = directory.filter { $0 == "/" }.count
                    rows.append(.directory(
                        path: directory,
                        label: (directory as NSString).lastPathComponent,
                        depth: depth,
                        group: group
                    ))
                }
            }

            for file in files.sorted(by: { $0.path < $1.path }) {
                let depth = treeMode ? file.path.filter { $0 == "/" }.count : 0
                rows.append(.file(file, depth: depth))
            }
        }
        return rows
    }

    private var changeGroups: [GitChangeGroupDto] {
        [.staged, .unstaged, .conflicted, .untracked, .ignored]
    }

    private var branchLabel: String {
        guard let changes else { return "未命名分支" }
        return changes.branch ?? changes.headShortHash.map { "分离 HEAD \($0)" } ?? "无分支"
    }

    private var countsLabel: String {
        guard let changes else { return "" }
        return "\(changes.stagedCount) 暂存 · \(changes.unstagedCount) 未暂存 · \(changes.conflictedCount) 冲突"
    }

    private func groupLabel(_ group: GitChangeGroupDto) -> String {
        switch group {
        case .staged: return "已暂存"
        case .unstaged: return "未暂存"
        case .untracked: return "未跟踪"
        case .conflicted: return "冲突"
        case .ignored: return "已忽略"
        }
    }

    private func fileLabel(_ file: GitChangeFileDto, depth: Int) -> String {
        if treeMode {
            return (file.path as NSString).lastPathComponent
        }
        return file.path
    }

    private func isSelected(path: String, group: GitChangeGroupDto) -> Bool {
        selectedPath == path && selectedGroup.map { $0 == group } == true
    }

    private func select(path: String, group: GitChangeGroupDto, isDirectory: Bool) {
        selectedPath = path
        selectedGroup = group
        selectedIsDirectory = isDirectory
        checkedLines.removeAll()
        if isDirectory {
            diff = nil
            loadingDiff = false
        } else {
            Task { await loadDiff() }
        }
    }

    private func statusColor(_ code: String) -> Color {
        switch code {
        case "C", "!", "D": return .red
        case "R", "A": return .green
        case "P", "M": return .orange
        case "?": return .cyan
        default: return .secondary
        }
    }

    private func iconName(for path: String) -> String {
        switch (path as NSString).pathExtension.lowercased() {
        case "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico":
            return "photo"
        case "mp3", "wav", "flac", "aac", "ogg":
            return "music.note"
        case "mp4", "mkv", "mov", "avi", "webm":
            return "film"
        case "zip", "7z", "rar", "tar", "gz":
            return "archivebox"
        case "rs", "py", "js", "ts", "tsx", "jsx", "html", "css", "c", "cpp", "h", "cs", "java", "go", "kt":
            return "chevron.left.forwardslash.chevron.right"
        default:
            return "doc"
        }
    }

    private func canonicalSelection(
        hunk: GitDiffHunkDto,
        line: GitHunkLineDto
    ) -> GitLineSelectionDto? {
        guard line.kind != .context else { return nil }
        return hunk.selections.first { selection in
            (line.oldLine != nil && selection.oldLine == line.oldLine)
                || (line.newLine != nil && selection.newLine == line.newLine)
        }
    }

    private func selectedSelections() -> [GitLineSelectionDto] {
        guard let diff else { return [] }
        return diff.hunks
            .flatMap(\.selections)
            .filter { checkedLines.contains(GitLineSelectionKey($0)) }
    }

    private func emptyState(_ message: String) -> some View {
        Text(message)
            .font(.system(size: 11.5))
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(24)
    }

    private func loadChanges() async {
        loading = true
        errorMessage = nil
        do {
            let snapshot = try await model.gitRepositoryChanges(
                path: repositoryPath,
                includeIgnored: showIgnored
            )
            repositoryPath = snapshot.repositoryRoot
            changes = snapshot
            loading = false

            if let selectedPath, let selectedGroup,
               snapshot.files.contains(where: { $0.path == selectedPath && $0.group == selectedGroup }) {
                if selectedIsDirectory {
                    diff = nil
                } else {
                    await loadDiff()
                }
            } else if let first = snapshot.files.first(where: { !isDirectoryChange($0) }) {
                self.selectedPath = first.path
                self.selectedGroup = first.group
                self.selectedIsDirectory = false
                checkedLines.removeAll()
                await loadDiff()
            } else {
                self.selectedPath = nil
                self.selectedGroup = nil
                self.selectedIsDirectory = false
                diff = nil
                checkedLines.removeAll()
            }
        } catch {
            loading = false
            errorMessage = error.localizedDescription
            changes = nil
            diff = nil
        }
    }

    private func loadDiff() async {
        guard let selectedPath, let selectedGroup, !selectedIsDirectory else {
            diff = nil
            return
        }
        loadingDiff = true
        errorMessage = nil
        do {
            diff = try await model.gitChangesDiff(
                repositoryRoot: repositoryPath,
                path: selectedPath,
                scope: selectedGroup == .staged ? .staged : .unstaged
            )
            checkedLines = Set(checkedLines.filter { key in
                diff?.hunks.contains { hunk in
                    hunk.selections.contains { GitLineSelectionKey($0) == key }
                } == true
            })
        } catch {
            diff = nil
            errorMessage = error.localizedDescription
        }
        loadingDiff = false
    }

    private func applyResult(_ result: GitChangeOperationResultDto) async {
        if showIgnored {
            statusMessage = result.message
            await loadChanges()
            return
        }
        changes = result.changes
        repositoryPath = result.changes.repositoryRoot
        statusMessage = result.message
        errorMessage = nil
        checkedLines.removeAll()

        if let selectedPath, let selectedGroup,
           result.changes.files.contains(where: { $0.path == selectedPath && $0.group == selectedGroup }) {
            if !selectedIsDirectory {
                await loadDiff()
            }
        } else if let first = result.changes.files.first(where: { !isDirectoryChange($0) }) {
            self.selectedPath = first.path
            self.selectedGroup = first.group
            self.selectedIsDirectory = false
            await loadDiff()
        } else {
            self.selectedPath = nil
            self.selectedGroup = nil
            self.selectedIsDirectory = false
            diff = nil
        }
    }

    private func stageLines(_ selections: [GitLineSelectionDto]) async {
        guard let selectedPath, !selections.isEmpty else { return }
        operating = true
        do {
            let result = try await model.gitStageLines(
                repositoryRoot: repositoryPath,
                path: selectedPath,
                selections: selections
            )
            await applyResult(result)
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func unstageLines(_ selections: [GitLineSelectionDto]) async {
        guard let selectedPath, !selections.isEmpty else { return }
        operating = true
        do {
            let result = try await model.gitUnstageLines(
                repositoryRoot: repositoryPath,
                path: selectedPath,
                selections: selections
            )
            await applyResult(result)
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func discardLines(_ selections: [GitLineSelectionDto]) async {
        guard let selectedPath, !selections.isEmpty else { return }
        operating = true
        do {
            let result = try await model.gitDiscardLines(
                repositoryRoot: repositoryPath,
                path: selectedPath,
                selections: selections
            )
            await applyResult(result)
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func stagePaths(_ paths: [String]) async {
        guard !paths.isEmpty else { return }
        operating = true
        do {
            let result = try await model.gitStagePaths(repositoryRoot: repositoryPath, paths: paths)
            await applyResult(result)
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func unstagePaths(_ paths: [String]) async {
        guard !paths.isEmpty else { return }
        operating = true
        do {
            let result = try await model.gitUnstagePaths(repositoryRoot: repositoryPath, paths: paths)
            await applyResult(result)
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func discardPaths(_ paths: [String]) async {
        guard !paths.isEmpty else { return }
        operating = true
        do {
            let result = try await model.gitDiscardPaths(repositoryRoot: repositoryPath, paths: paths)
            await applyResult(result)
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func confirmDiscard() async {
        if let lines = pendingDiscardLines {
            pendingDiscardLines = nil
            await discardLines(lines)
        } else if let paths = pendingDiscardPaths {
            pendingDiscardPaths = nil
            await discardPaths(paths)
        }
    }

    private func addIgnore(_ path: String, local: Bool) async {
        operating = true
        do {
            statusMessage = try await model.gitAddToGitignore(
                repositoryRoot: repositoryPath,
                path: path,
                pattern: nil,
                localExclude: local
            )
            await loadChanges()
        } catch {
            errorMessage = error.localizedDescription
        }
        operating = false
    }

    private func isDirectoryChange(_ file: GitChangeFileDto) -> Bool {
        false
    }
}

private func gitGroupCode(_ group: GitChangeGroupDto) -> String {
    switch group {
    case .staged: return "staged"
    case .unstaged: return "unstaged"
    case .untracked: return "untracked"
    case .conflicted: return "conflicted"
    case .ignored: return "ignored"
    }
}



