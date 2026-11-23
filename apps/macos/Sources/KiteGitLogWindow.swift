import AppKit
import SwiftUI

enum KiteGitLogSearchMode: String, CaseIterable, Identifiable {
    case text
    case author
    case path
    case hash

    var id: String { rawValue }

    var title: String {
        switch self {
        case .text: return "文本"
        case .author: return "作者"
        case .path: return "路径"
        case .hash: return "哈希"
        }
    }

    var placeholder: String {
        switch self {
        case .text: return "搜索提交标题或说明"
        case .author: return "按作者过滤"
        case .path: return "按仓库相对路径过滤"
        case .hash: return "输入提交哈希后按 Enter"
        }
    }
}

enum KiteGitLogDateRange: String, CaseIterable, Identifiable {
    case any
    case day
    case week
    case month

    var id: String { rawValue }

    var title: String {
        switch self {
        case .any: return "全部时间"
        case .day: return "最近 24 小时"
        case .week: return "最近 7 天"
        case .month: return "最近 30 天"
        }
    }

    var since: String? {
        switch self {
        case .any: return nil
        case .day: return "24 hours ago"
        case .week: return "7 days ago"
        case .month: return "30 days ago"
        }
    }
}

enum KiteGitLogDetailTab: String, CaseIterable, Identifiable {
    case commit = "提交"
    case files = "变更文件"
    case tree = "文件树"

    var id: String { rawValue }
}

@MainActor
final class KiteGitLogViewModel: ObservableObject {
    let model: KiteModel
    let path: String

    @Published var context: GitLogContextDto?
    @Published var commits: [GitLogCommitDto] = []
    @Published var selectedHash: String?
    @Published var details: GitCommitDetailsDto?
    @Published var treeEntries: [GitTreeEntryDto] = []
    @Published var detailTab: KiteGitLogDetailTab = .commit
    @Published var searchText = ""
    @Published var searchMode: KiteGitLogSearchMode = .text
    @Published var searchRegex = false
    @Published var branch: String?
    @Published var dateRange: KiteGitLogDateRange = .any
    @Published var historyPath: String?
    @Published var loading = true
    @Published var loadingMore = false
    @Published var detailsLoading = false
    @Published var treeLoading = false
    @Published var errorMessage: String?

    private var cursor: String?
    private var hasMore = false
    private var initialHash: String?

    init(model: KiteModel, path: String, initialHash: String? = nil) {
        self.model = model
        self.path = path
        self.initialHash = initialHash
    }

    var selectedCommit: GitLogCommitDto? {
        guard let selectedHash else { return nil }
        return commits.first { $0.hash == selectedHash }
    }

    var branchOptions: [GitRefDto] {
        context?.refs.filter {
            $0.kind == .localBranch || $0.kind == .remoteBranch
        } ?? []
    }

    var canLoadMore: Bool {
        hasMore && !loadingMore && !loading
    }

    var currentFilter: GitLogFilterDto {
        var filter = GitLogFilterDto(
            branch: branch,
            author: nil,
            since: dateRange.since,
            until: nil,
            path: historyPath,
            text: nil,
            regex: searchRegex,
            hash: nil
        )
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        if !query.isEmpty {
            switch searchMode {
            case .text:
                filter.text = query
            case .author:
                filter.author = query
            case .path:
                filter.path = query
            case .hash:
                filter.hash = query
            }
        }
        if let historyPath, !historyPath.isEmpty {
            filter.path = historyPath
        }
        return filter
    }

    func reload() async {
        loading = true
        loadingMore = false
        detailsLoading = false
        treeLoading = false
        errorMessage = nil
        commits = []
        selectedHash = nil
        details = nil
        treeEntries = []
        cursor = nil
        hasMore = false
        do {
            context = try await model.gitLogContext(path: path)
            let page = try await model.gitLogPage(
                path: path,
                filter: currentFilter,
                cursor: nil,
                pageSize: 200
            )
            let pendingInitialHash = initialHash
            apply(page)
            loading = false
            if let initialHash = pendingInitialHash {
                if let initial = commits.first(where: { $0.hash == initialHash }) {
                    self.initialHash = nil
                    await select(initial)
                } else {
                    self.initialHash = nil
                    searchMode = .hash
                    searchText = initialHash
                    await reload()
                    return
                }
            } else if let first = commits.first {
                await select(first)
            }
            if canLoadMore {
                await loadMore()
            }
        } catch {
            loading = false
            errorMessage = error.localizedDescription
        }
    }

    func loadMore() async {
        guard let cursor, hasMore, !loadingMore, !loading else { return }
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page = try await model.gitLogPage(
                path: path,
                filter: currentFilter,
                cursor: cursor,
                pageSize: 200
            )
            apply(page)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func select(_ commit: GitLogCommitDto) async {
        if selectedHash != commit.hash {
            selectedHash = commit.hash
            details = nil
            treeEntries = []
            detailTab = .commit
        }
        detailsLoading = true
        let hash = commit.hash
        do {
            let result = try await model.gitCommitDetails(path: path, hash: hash)
            guard selectedHash == hash else { return }
            details = result
        } catch {
            guard selectedHash == hash else { return }
            errorMessage = error.localizedDescription
        }
        if selectedHash == hash {
            detailsLoading = false
        }
    }

    func loadTree(_ commit: GitLogCommitDto? = nil) async {
        let hash = commit?.hash ?? selectedHash
        guard let hash else { return }
        if selectedHash != hash {
            selectedHash = hash
            details = nil
            treeEntries = []
        }
        detailTab = .tree
        treeLoading = true
        do {
            let entries = try await model.gitCommitTree(path: path, hash: hash)
            guard selectedHash == hash else { return }
            treeEntries = entries
        } catch {
            guard selectedHash == hash else { return }
            errorMessage = error.localizedDescription
        }
        if selectedHash == hash {
            treeLoading = false
        }
    }

    func showFileHistory(_ filePath: String) async {
        historyPath = filePath
        searchMode = .path
        await reload()
    }

    func clearFileHistory() async {
        historyPath = nil
        await reload()
    }

    private func apply(_ page: GitLogPageDto) {
        hasMore = page.hasMore
        cursor = page.nextCursor
        commits.append(contentsOf: page.commits)
        if selectedHash == nil {
            selectedHash = page.commits.first?.hash
        }
    }
}

@MainActor
final class KiteGitLogWindowController: NSWindowController, NSWindowDelegate {
    static let shared = KiteGitLogWindowController()

    func show(model: KiteModel, path: String, initialHash: String? = nil) {
        let viewModel = KiteGitLogViewModel(model: model, path: path, initialHash: initialHash)
        let rootView = KiteGitLogWindowView(viewModel: viewModel)
        let hostingController = NSHostingController(rootView: rootView)

        if window == nil {
            let historyWindow = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 1280, height: 800),
                styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                backing: .buffered,
                defer: false
            )
            historyWindow.titlebarAppearsTransparent = true
            historyWindow.minSize = NSSize(width: 1120, height: 600)
            historyWindow.isReleasedWhenClosed = false
            historyWindow.delegate = self
            historyWindow.center()
            historyWindow.setFrameAutosaveName("KiteGitLogWindow")
            window = historyWindow
        }

        let repositoryName = URL(fileURLWithPath: path).lastPathComponent
        window?.title = "Kite 提交历史 — \(repositoryName.isEmpty ? "Git" : repositoryName)"
        window?.contentViewController = hostingController
        if window?.isMiniaturized == true {
            window?.deminiaturize(nil)
        }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        window?.contentViewController = nil
    }
}

private struct KiteGitLogWindowView: View {
    @StateObject private var viewModel: KiteGitLogViewModel
    @FocusState private var searchFocused: Bool

    init(viewModel: KiteGitLogViewModel) {
        _viewModel = StateObject(wrappedValue: viewModel)
    }

    var body: some View {
        ZStack {
            KiteGlassBackdrop()

            VStack(spacing: 0) {
                toolbar
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(height: 0.5)
                HStack(spacing: 0) {
                    commitList
                    Rectangle()
                        .fill(Color.primary.opacity(0.08))
                        .frame(width: 0.5)
                    detailPane
                        .frame(width: 390)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(height: 0.5)
                statusBar
            }
        }
        .frame(minWidth: 1120, minHeight: 600)
        .task {
            await viewModel.reload()
        }
        .onAppear {
            DispatchQueue.main.async {
                searchFocused = false
            }
        }
    }

    private var toolbar: some View {
        VStack(spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 30, height: 30)
                    .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 8, style: .continuous))

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 8) {
                        Text("提交历史")
                            .font(.system(size: 13, weight: .semibold))
                        if let branch = viewModel.context?.currentBranch {
                            Text(branch)
                                .font(.system(size: 10, weight: .medium))
                                .foregroundStyle(Color.accentColor)
                                .padding(.horizontal, 7)
                                .padding(.vertical, 2)
                                .background(Color.accentColor.opacity(0.12), in: Capsule())
                        }
                    }
                    Text(viewModel.context?.repositoryRoot ?? viewModel.path)
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Spacer()

                Button {
                    Task { await viewModel.reload() }
                } label: {
                    Label("刷新", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .disabled(viewModel.loading)
            }

            HStack(spacing: 8) {
                Menu {
                    ForEach(KiteGitLogSearchMode.allCases) { mode in
                        Button {
                            viewModel.searchMode = mode
                        } label: {
                            if viewModel.searchMode == mode {
                                Label(mode.title, systemImage: "checkmark")
                            } else {
                                Text(mode.title)
                            }
                        }
                    }
                } label: {
                    Label("筛选 · \(viewModel.searchMode.title)", systemImage: "line.3.horizontal.decrease.circle")
                        .font(.system(size: 11, weight: .medium))
                }
                .menuStyle(.borderlessButton)
                .fixedSize()

                TextField(viewModel.searchMode.placeholder, text: $viewModel.searchText)
                    .textFieldStyle(.plain)
                    .font(.system(size: 11.5))
                    .focused($searchFocused)
                    .onSubmit {
                        Task { await viewModel.reload() }
                    }

                if !viewModel.searchText.isEmpty {
                    Button {
                        viewModel.searchText = ""
                        Task { await viewModel.reload() }
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help("清除筛选")
                }

                Button("正则") {
                    viewModel.searchRegex.toggle()
                    Task { await viewModel.reload() }
                }
                .buttonStyle(.borderless)
                .font(.system(size: 10.5, weight: viewModel.searchRegex ? .semibold : .regular))
                .foregroundStyle(viewModel.searchRegex ? Color.accentColor : Color.secondary)

                branchMenu

                dateMenu
            }
            .padding(.horizontal, 10)
            .frame(height: 34)
            .background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 10, style: .continuous))

            if let historyPath = viewModel.historyPath {
                HStack(spacing: 8) {
                    Image(systemName: "doc.text")
                        .foregroundStyle(.secondary)
                    Text("文件历史 · \(historyPath)")
                        .font(.system(size: 10.5))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button("清除") {
                        Task { await viewModel.clearFileHistory() }
                    }
                    .buttonStyle(.borderless)
                    .font(.system(size: 10.5))
                }
                .padding(.horizontal, 10)
                .frame(height: 28)
                .background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial)
    }

    private var branchMenu: some View {
        Menu {
            Button {
                viewModel.branch = nil
                Task { await viewModel.reload() }
            } label: {
                if viewModel.branch == nil {
                    Label("全部分支", systemImage: "checkmark")
                } else {
                    Text("全部分支")
                }
            }

            Divider()

            ForEach(viewModel.branchOptions, id: \.fullName) { reference in
                Button {
                    viewModel.branch = reference.fullName
                    Task { await viewModel.reload() }
                } label: {
                    if viewModel.branch == reference.fullName {
                        Label(reference.name, systemImage: "checkmark")
                    } else {
                        Text(reference.name)
                    }
                }
            }
        } label: {
            Label(branchLabel, systemImage: "arrow.triangle.branch")
                .font(.system(size: 10.5, weight: .medium))
                .lineLimit(1)
        }
        .menuStyle(.borderlessButton)
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: 180)
    }

    private var branchLabel: String {
        guard let branch = viewModel.branch else { return "分支 · 全部" }
        let name = viewModel.context?.refs.first { $0.fullName == branch }?.name ?? branch
        return "分支 · \(name)"
    }

    private var dateMenu: some View {
        Menu {
            ForEach(KiteGitLogDateRange.allCases) { range in
                Button {
                    viewModel.dateRange = range
                    Task { await viewModel.reload() }
                } label: {
                    if viewModel.dateRange == range {
                        Label(range.title, systemImage: "checkmark")
                    } else {
                        Text(range.title)
                    }
                }
            }
        } label: {
            Label(viewModel.dateRange.title, systemImage: "calendar")
                .font(.system(size: 10.5, weight: .medium))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
    }

    private var statusBar: some View {
        HStack {
            Text(statusText)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer()
            Text("\(viewModel.commits.count) 个提交 · Rust Core 提交图")
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 12)
        .frame(height: 28)
        .background(.ultraThinMaterial)
    }

    private var statusText: String {
        if let error = viewModel.errorMessage {
            return error
        }
        if viewModel.loading {
            return "正在加载提交历史…"
        }
        if viewModel.loadingMore {
            return "正在加载下一页…"
        }
        return "就绪"
    }
}

private extension KiteGitLogWindowView {
    var commitList: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text("提交图")
                    .frame(width: 92, alignment: .leading)
                Text("引用")
                    .frame(width: 210, alignment: .leading)
                Text("提交")
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text("作者")
                    .frame(width: 120, alignment: .leading)
                Text("日期")
                    .frame(width: 125, alignment: .leading)
                Text("哈希")
                    .frame(width: 74, alignment: .leading)
            }
            .font(.system(size: 10, weight: .medium))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 12)
            .frame(height: 32)
            .background(Color.primary.opacity(0.035))

            Divider()

            if viewModel.loading && viewModel.commits.isEmpty {
                Spacer()
                ProgressView("正在读取提交历史…")
                    .controlSize(.small)
                Spacer()
            } else if let error = viewModel.errorMessage, viewModel.commits.isEmpty {
                Spacer()
                VStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 28, weight: .light))
                        .foregroundStyle(.orange)
                    Text(error)
                        .font(.system(size: 11.5))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 520)
                    Button("重试") {
                        Task { await viewModel.reload() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                .padding(24)
                Spacer()
            } else if viewModel.commits.isEmpty {
                Spacer()
                VStack(spacing: 10) {
                    Image(systemName: "arrow.triangle.branch")
                        .font(.system(size: 30, weight: .light))
                        .foregroundStyle(.tertiary)
                    Text("没有匹配的提交")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(.secondary)
                }
                Spacer()
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(viewModel.commits, id: \.hash) { commit in
                            KiteGitCommitRow(
                                commit: commit,
                                isSelected: viewModel.selectedHash == commit.hash
                            )
                            .contentShape(Rectangle())
                            .onTapGesture {
                                Task { await viewModel.select(commit) }
                            }
                            .contextMenu {
                                Button("复制完整哈希") {
                                    NSPasteboard.general.clearContents()
                                    NSPasteboard.general.setString(commit.hash, forType: .string)
                                }
                                Button("在提交时浏览文件") {
                                    Task { await viewModel.loadTree(commit) }
                                }
                            }
                            .onAppear {
                                guard commit.hash == viewModel.commits.last?.hash else { return }
                                Task { await viewModel.loadMore() }
                            }
                        }

                        if viewModel.loadingMore {
                            ProgressView()
                                .controlSize(.small)
                                .padding(12)
                        } else if viewModel.canLoadMore {
                            Button("加载更多") {
                                Task { await viewModel.loadMore() }
                            }
                            .buttonStyle(.borderless)
                            .controlSize(.small)
                            .padding(12)
                        } else {
                            Text("已到达历史末尾")
                                .font(.system(size: 10))
                                .foregroundStyle(.tertiary)
                                .padding(12)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct KiteGitCommitRow: View {
    let commit: GitLogCommitDto
    let isSelected: Bool

    var body: some View {
        HStack(spacing: 8) {
            KiteGitGraphCell(row: commit.graph)
                .frame(width: 88, height: 32)
                .frame(width: 92, alignment: .leading)

            HStack(spacing: 4) {
                ForEach(Array(commit.refs.prefix(3).enumerated()), id: \.offset) { _, reference in
                    KiteGitRefBadge(reference: reference)
                }
                if commit.refs.count > 3 {
                    Text("+\(commit.refs.count - 3)")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.secondary)
                }
            }
            .frame(width: 210, alignment: .leading)
            .lineLimit(1)
            .clipped()

            Text(commit.subject)
                .font(.system(size: 11.5))
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)

            Text(commit.authorName)
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(width: 120, alignment: .leading)

            Text(Self.timeText(commit.committedAt))
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(width: 125, alignment: .leading)

            Text(commit.shortHash)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 74, alignment: .leading)
        }
        .padding(.horizontal, 12)
        .frame(height: 32)
        .background(isSelected ? Color.accentColor.opacity(0.13) : Color.clear)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.primary.opacity(0.045))
                .frame(height: 0.5)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(commit.subject)，\(commit.authorName)，\(commit.shortHash)")
    }

    private static func timeText(_ seconds: Int64) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "zh_CN")
        formatter.dateFormat = "yyyy-MM-dd HH:mm"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
    }
}

private struct KiteGitGraphCell: View {
    let row: GitGraphRowDto

    private static let colors: [Color] = [
        Color(hex: 0x5e9cff), Color(hex: 0x55b978), Color(hex: 0xf0a45c),
        Color(hex: 0xc678dd), Color(hex: 0x56b6c2), Color(hex: 0xe06c75),
        Color(hex: 0xe5c07b), Color(hex: 0x7f9cf5), Color(hex: 0x64d1a7),
        Color(hex: 0xd884e0),
    ]

    var body: some View {
        Canvas { context, size in
            // Keep this graph geometry in sync with graph_lane_width on Windows.
            let laneWidth = max(4, min(CGFloat(13), 80 / CGFloat(max(row.laneCount, 1))))
            let edgeWidth: CGFloat = 2
            let originX: CGFloat = 4
            let centerY = size.height / 2

            func laneX(_ lane: UInt32) -> CGFloat {
                originX + CGFloat(lane) * laneWidth + laneWidth / 2
            }

            for edge in row.edges {
                let color = Self.color(edge.colorIndex)
                let fromX = laneX(edge.fromLane)
                let toX = laneX(edge.toLane)
                var path = Path()
                if edge.fromLane == edge.toLane {
                    path.move(to: CGPoint(x: fromX, y: 0))
                    path.addLine(to: CGPoint(x: fromX, y: size.height))
                } else {
                    path.move(to: CGPoint(x: fromX, y: 0))
                    path.addLine(to: CGPoint(x: fromX, y: centerY))
                    path.addLine(to: CGPoint(x: toX, y: centerY))
                    path.addLine(to: CGPoint(x: toX, y: size.height))
                }
                context.stroke(path, with: .color(color), lineWidth: edgeWidth)
            }

            let nodeRect = CGRect(
                x: laneX(row.lane) - 4,
                y: centerY - 4,
                width: 8,
                height: 8
            )
            context.fill(Path(ellipseIn: nodeRect), with: .color(Self.color(row.colorIndex)))
            context.stroke(
                Path(ellipseIn: nodeRect),
                with: .color(Color(nsColor: .textBackgroundColor)),
                lineWidth: 2
            )
        }
        .drawingGroup()
        .accessibilityHidden(true)
    }

    private static func color(_ index: UInt32) -> Color {
        colors[Int(index) % colors.count]
    }
}

private struct KiteGitRefBadge: View {
    let reference: GitRefDto

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: icon)
                .font(.system(size: 8.5, weight: .semibold))
            Text(reference.name)
                .font(.system(size: 9.5, weight: .medium))
                .lineLimit(1)
        }
        .foregroundStyle(color)
        .padding(.horizontal, 5)
        .padding(.vertical, 2)
        .background(color.opacity(0.13), in: RoundedRectangle(cornerRadius: 5, style: .continuous))
    }

    private var icon: String {
        switch reference.kind {
        case .localBranch: return "arrow.triangle.branch"
        case .remoteBranch: return "cloud"
        case .tag: return "tag"
        case .commit: return "circle.fill"
        case .other: return "link"
        }
    }

    private var color: Color {
        switch reference.kind {
        case .localBranch:
            return reference.isHead ? .accentColor : Color(hex: 0x55b978)
        case .remoteBranch:
            return Color(hex: 0xc678dd)
        case .tag:
            return Color(hex: 0xe5c07b)
        case .commit:
            return .accentColor
        case .other:
            return .secondary
        }
    }
}

private extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xff) / 255,
            green: Double((hex >> 8) & 0xff) / 255,
            blue: Double(hex & 0xff) / 255,
            opacity: 1
        )
    }
}

private extension KiteGitLogWindowView {
    var detailPane: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text(viewModel.selectedCommit?.shortHash ?? "提交详情")
                    .font(.system(size: 12, weight: .semibold))
                    .lineLimit(1)
                Spacer()
                if let commit = viewModel.selectedCommit {
                    Button {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(commit.hash, forType: .string)
                    } label: {
                        Image(systemName: "doc.on.doc")
                    }
                    .buttonStyle(.borderless)
                    .help("复制完整提交哈希")

                    Button {
                        Task { await viewModel.loadTree(commit) }
                    } label: {
                        Image(systemName: "folder")
                    }
                    .buttonStyle(.borderless)
                    .help("浏览这个提交的文件树")
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 42)
            .background(Color.primary.opacity(0.03))

            Picker("", selection: $viewModel.detailTab) {
                ForEach(KiteGitLogDetailTab.allCases) { tab in
                    Text(tab.rawValue).tag(tab)
                }
            }
            .pickerStyle(.segmented)
            .controlSize(.small)
            .labelsHidden()
            .padding(.horizontal, 10)
            .padding(.bottom, 8)
            .onChange(of: viewModel.detailTab) { tab in
                switch tab {
                case .commit:
                    if viewModel.details == nil, let commit = viewModel.selectedCommit {
                        Task { await viewModel.select(commit) }
                    }
                case .tree:
                    Task { await viewModel.loadTree() }
                case .files:
                    break
                }
            }

            Divider()

            Group {
                switch viewModel.detailTab {
                case .commit:
                    commitDetailContent
                case .files:
                    changedFilesContent
                case .tree:
                    treeContent
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            Divider()

            HStack {
                Text(detailStatusText)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                Spacer()
            }
            .padding(.horizontal, 12)
            .frame(height: 28)
        }
        .background(.ultraThinMaterial)
    }

    private var detailStatusText: String {
        if viewModel.detailsLoading {
            return "正在读取提交详情…"
        }
        if viewModel.treeLoading {
            return "正在读取提交文件树…"
        }
        return "选择左侧提交查看详情"
    }

    @ViewBuilder
    private var commitDetailContent: some View {
        if let details {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(details.commit.subject)
                            .font(.system(size: 14, weight: .semibold))
                            .textSelection(.enabled)

                        ScrollView(.horizontal) {
                            HStack(spacing: 4) {
                                ForEach(Array(details.commit.refs.enumerated()), id: \.offset) { _, reference in
                                    KiteGitRefBadge(reference: reference)
                                }
                            }
                        }
                        .scrollIndicators(.hidden)
                        .frame(height: 22)

                        Text(details.body.isEmpty ? details.commit.subject : details.body)
                            .font(.system(size: 11.5))
                            .lineSpacing(3)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                            .background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
                    }

                    VStack(spacing: 7) {
                        KiteGitDetailRow(label: "提交", value: details.commit.hash, monospaced: true)
                        KiteGitDetailRow(
                            label: "作者",
                            value: "\(details.commit.authorName) <\(details.commit.authorEmail)>"
                        )
                        KiteGitDetailRow(
                            label: "提交时间",
                            value: Self.fullTime(details.commit.committedAt)
                        )
                        KiteGitDetailRow(
                            label: "提交者",
                            value: "\(details.commit.committerName) <\(details.commit.committerEmail)>"
                        )
                        KiteGitDetailRow(
                            label: "父提交",
                            value: details.commit.parents.isEmpty
                                ? "根提交"
                                : details.commit.parents.map { String($0.prefix(8)) }.joined(separator: "  "),
                            monospaced: true
                        )
                    }

                    VStack(spacing: 7) {
                        KiteGitDetailRow(label: "签名", value: details.signature.statusLabel)
                        KiteGitDetailRow(
                            label: "签名者",
                            value: details.signature.signer ?? "—"
                        )
                        KiteGitDetailRow(
                            label: "拣选状态",
                            value: {
                                switch details.cherryPicked {
                                case true?: return "上游已有等价补丁"
                                case false?: return "上游尚未包含"
                                case nil: return "无法确定"
                                }
                            }()
                        )
                    }

                    if !details.branchesContaining.isEmpty {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("包含此提交的分支")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(.secondary)

                            ScrollView(.horizontal) {
                                HStack(spacing: 5) {
                                    ForEach(details.branchesContaining, id: \.self) { branch in
                                        Text(branch)
                                            .font(.system(size: 10))
                                            .foregroundStyle(Color.accentColor)
                                            .padding(.horizontal, 7)
                                            .padding(.vertical, 3)
                                            .background(Color.accentColor.opacity(0.1), in: Capsule())
                                    }
                                }
                            }
                            .scrollIndicators(.hidden)
                        }
                    }

                    HStack {
                        Button {
                            viewModel.detailTab = .files
                        } label: {
                            Label("\(details.changedFiles.count) 个变更文件", systemImage: "doc.on.doc")
                        }
                        .buttonStyle(.borderless)
                        .font(.system(size: 10.5))

                        Spacer()

                        if details.signature.statusCode == "B" {
                            Text("签名无效")
                                .font(.system(size: 10, weight: .medium))
                                .foregroundStyle(.red)
                        } else if details.signature.statusCode == "E" {
                            Text("缺少签名公钥")
                                .font(.system(size: 10, weight: .medium))
                                .foregroundStyle(.orange)
                        }
                    }
                }
                .padding(14)
            }
        } else {
            VStack(spacing: 10) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 30, weight: .light))
                    .foregroundStyle(.tertiary)
                Text(viewModel.detailsLoading ? "正在读取提交详情…" : "选择一个提交")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var changedFilesContent: some View {
        Group {
            if let details, !details.changedFiles.isEmpty {
                ScrollView {
                    LazyVStack(spacing: 1) {
                        ForEach(Array(details.changedFiles.enumerated()), id: \.offset) { _, file in
                            Button {
                                Task { await viewModel.showFileHistory(file.path) }
                            } label: {
                                HStack(spacing: 8) {
                                    Text(String(file.status.prefix(1)))
                                        .font(.system(size: 10, weight: .bold, design: .monospaced))
                                        .foregroundStyle(statusColor(file.status))
                                        .frame(width: 18, alignment: .leading)
                                    Text(fileLabel(file))
                                        .font(.system(size: 10.5, design: .monospaced))
                                        .foregroundStyle(.primary)
                                        .lineLimit(1)
                                        .truncationMode(.middle)
                                    Spacer()
                                    Image(systemName: "clock.arrow.circlepath")
                                        .font(.system(size: 9))
                                        .foregroundStyle(.secondary)
                                }
                                .padding(.horizontal, 10)
                                .frame(height: 30)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .help("查看文件历史")
                        }
                    }
                    .padding(.vertical, 6)
                }
            } else {
                VStack(spacing: 10) {
                    Image(systemName: "doc.text")
                        .font(.system(size: 28, weight: .light))
                        .foregroundStyle(.tertiary)
                    Text("选择一个提交查看变更文件")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    private var treeContent: some View {
        Group {
            if viewModel.treeLoading {
                ProgressView("正在读取提交文件树…")
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if viewModel.treeEntries.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "folder")
                        .font(.system(size: 28, weight: .light))
                        .foregroundStyle(.tertiary)
                    Text("选择“文件树”加载此提交的内容")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 1) {
                        ForEach(Array(viewModel.treeEntries.enumerated()), id: \.offset) { _, entry in
                            HStack(spacing: 8) {
                                Image(systemName: "doc.text")
                                    .font(.system(size: 9))
                                    .foregroundStyle(.secondary)
                                    .frame(width: 14)
                                Text(entry.path)
                                    .font(.system(size: 10.5, design: .monospaced))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                    .textSelection(.enabled)
                                Spacer()
                                Text(Self.fileSize(entry.size))
                                    .font(.system(size: 9.5, design: .monospaced))
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 10)
                            .frame(height: 27)
                        }
                    }
                    .padding(.vertical, 6)
                }
            }
        }
    }

    private var details: GitCommitDetailsDto? {
        viewModel.details
    }

    private func fileLabel(_ file: GitChangedFileDto) -> String {
        if let oldPath = file.oldPath {
            return "\(oldPath) → \(file.path)"
        }
        return file.path
    }

    private func statusColor(_ status: String) -> Color {
        switch String(status.prefix(1)) {
        case "A": return Color(hex: 0x55b978)
        case "D": return Color(hex: 0xef6b73)
        case "R", "C": return Color(hex: 0x5e9cff)
        default: return Color(hex: 0xe5a84b)
        }
    }

    private static func fullTime(_ seconds: Int64) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "zh_CN")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
    }

    private static func fileSize(_ size: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file)
    }
}

private struct KiteGitDetailRow: View {
    let label: String
    let value: String
    var monospaced = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Text(label)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .frame(width: 64, alignment: .leading)
            Text(value)
                .font(.system(size: 10, design: monospaced ? .monospaced : .default))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}


