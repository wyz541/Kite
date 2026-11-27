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
    let repositoryPath: String
    let embeddedDiffModel = KiteDiffViewerModel()

    @Published var scope: GitGraphScopeDto
    @Published var context: GitGraphContextDto?
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
    @Published var selectedChangedFile: String?
    @Published var loading = true
    @Published var loadingMore = false
    @Published var detailsLoading = false
    @Published var treeLoading = false
    @Published var errorMessage: String?

    var onContextUpdated: ((GitGraphContextDto) -> Void)?

    private var cursor: String?
    private var hasMore = false
    private var loadGeneration = 0

    init(model: KiteModel, repositoryPath: String, scope: GitGraphScopeDto) {
        self.model = model
        self.repositoryPath = repositoryPath
        self.scope = scope
        embeddedDiffModel.host = self
    }

    var logContext: GitLogContextDto? {
        context?.log
    }

    var repositoryRoot: String {
        context?.log.repositoryRoot ?? repositoryPath
    }

    var scopePath: String? {
        context?.pathspec
    }

    var isScoped: Bool {
        scopePath != nil
    }

    var workspaceTitle: String {
        isScoped ? "Git Log" : "Git Graph"
    }

    var scopeChipTitle: String {
        guard let path = scopePath, !path.isEmpty else {
            return "Repository"
        }
        switch scope {
        case .directory(_):
            return "Directory: \(path)"
        case .file(_):
            return "File: \(path)"
        case .repository:
            return "Repository"
        }
    }

    var scopeChipIcon: String {
        switch scope {
        case .directory(_):
            return "folder"
        case .file(_):
            return "doc.text"
        case .repository:
            return "arrow.triangle.branch"
        }
    }

    var emptyHistoryMessage: String {
        isScoped ? "该目录没有可显示的提交历史" : "没有匹配的提交"
    }

    var selectedCommit: GitLogCommitDto? {
        guard let selectedHash else { return nil }
        return commits.first { $0.hash == selectedHash }
    }

    var branchOptions: [GitRefDto] {
        logContext?.refs.filter {
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
            path: nil,
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
        return filter
    }

    func setScope(_ newScope: GitGraphScopeDto) {
        scope = newScope
    }

    func reload() async {
        loadGeneration &+= 1
        let generation = loadGeneration
        loading = true
        loadingMore = false
        detailsLoading = false
        treeLoading = false
        errorMessage = nil
        context = nil
        commits = []
        selectedHash = nil
        details = nil
        treeEntries = []
        selectedChangedFile = nil
        detailTab = .commit
        cursor = nil
        hasMore = false

        do {
            let context = try await model.gitGraphContext(
                repositoryPath: repositoryPath,
                scope: scope
            )
            guard generation == loadGeneration else { return }
            self.context = context
            onContextUpdated?(context)

            let page = try await model.gitGraphPage(
                repositoryPath: repositoryPath,
                scope: context.scope,
                filter: currentFilter,
                cursor: nil,
                pageSize: 200
            )
            guard generation == loadGeneration else { return }
            apply(page)
            loading = false
            if let first = commits.first {
                await select(first)
            }
            if canLoadMore {
                await loadMore()
            }
        } catch {
            guard generation == loadGeneration else { return }
            loading = false
            errorMessage = error.localizedDescription
        }
    }

    func loadMore() async {
        guard let cursor, hasMore, !loadingMore, !loading else { return }
        let generation = loadGeneration
        loadingMore = true
        defer {
            if generation == loadGeneration {
                loadingMore = false
            }
        }
        do {
            let page = try await model.gitGraphPage(
                repositoryPath: repositoryPath,
                scope: context?.scope ?? scope,
                filter: currentFilter,
                cursor: cursor,
                pageSize: 200
            )
            guard generation == loadGeneration else { return }
            apply(page)
        } catch {
            guard generation == loadGeneration else { return }
            errorMessage = error.localizedDescription
        }
    }

    func select(_ commit: GitLogCommitDto) async {
        if selectedHash != commit.hash {
            selectedHash = commit.hash
            details = nil
            treeEntries = []
            selectedChangedFile = nil
            detailTab = .commit
        }
        detailsLoading = true
        let hash = commit.hash
        do {
            let result = try await model.gitGraphCommitDetails(
                repositoryPath: repositoryPath,
                scope: context?.scope ?? scope,
                hash: hash
            )
            guard selectedHash == hash else { return }
            details = result
            if selectedChangedFile == nil {
                selectedChangedFile = result.changedFiles.first?.path
            }
            showEmbeddedDiff()
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
            selectedChangedFile = nil
        }
        detailTab = .tree
        treeLoading = true
        do {
            let entries = try await model.gitGraphCommitTree(
                repositoryPath: repositoryPath,
                scope: context?.scope ?? scope,
                hash: hash
            )
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

    func showEmbeddedDiff() {
        guard let details, let filePath = selectedChangedFile else { return }
        let absolutePath = URL(fileURLWithPath: repositoryRoot)
            .appendingPathComponent(filePath)
            .path
        embeddedDiffModel.oldRevision = "\(details.commit.hash)^"
        embeddedDiffModel.newRevision = details.commit.hash
        embeddedDiffModel.show(
            path: absolutePath,
            appModel: model,
            preset: .custom,
            logScope: .file(filePath)
        )
    }

    func showFileHistory(_ filePath: String) async {
        scope = .file(path: filePath)
        await reload()
    }

    func clearScope() async {
        scope = .repository
        await reload()
    }

    func showAllCommits() async {
        branch = nil
        scope = .repository
        await reload()
    }

    func isInScope(_ path: String) -> Bool {
        guard let scopePath, !scopePath.isEmpty, scopePath != "." else {
            return false
        }
        if path == scopePath {
            return true
        }
        if case .directory(_) = scope {
            return path.hasPrefix(scopePath + "/")
        }
        return false
    }

    func branchLabel(for fullName: String?) -> String {
        guard let fullName else { return "分支 · 全部" }
        let name = logContext?.refs.first { $0.fullName == fullName }?.name ?? fullName
        return "分支 · \(name)"
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

extension KiteGitLogViewModel: KiteDiffViewerHost {
    func openGitLog(scope: KiteDiffLogScope) {
        switch scope {
        case .file(let path):
            self.scope = .file(path: path)
        case .directory(let path):
            self.scope = .directory(path: path)
        }
        Task { await reload() }
    }
}

@MainActor
final class KiteGitLogWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: KiteGitLogWindowController] = [:]

    private let repositoryRoot: String
    private let viewModel: KiteGitLogViewModel

    private init(model: KiteModel, repositoryRoot: String, scope: GitGraphScopeDto) {
        self.repositoryRoot = repositoryRoot
        let viewModel = KiteGitLogViewModel(
            model: model,
            repositoryPath: repositoryRoot,
            scope: scope
        )
        self.viewModel = viewModel
        let hostingController = NSHostingController(
            rootView: KiteGitLogWindowView(viewModel: viewModel)
        )
        let historyWindow = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1280, height: 820),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        historyWindow.titlebarAppearsTransparent = true
        historyWindow.minSize = NSSize(width: 1040, height: 680)
        historyWindow.isReleasedWhenClosed = false
        historyWindow.contentViewController = hostingController
        historyWindow.center()
        historyWindow.setFrameAutosaveName("KiteGitWorkspaceWindow")
        super.init(window: historyWindow)
        historyWindow.delegate = self
        viewModel.onContextUpdated = { [weak self] context in
            self?.updateWindowTitle(context)
        }
        updateWindowTitle(nil)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show(
        model: KiteModel,
        path: String,
        scope: GitGraphScopeDto = .repository
    ) {
        Task { @MainActor in
            let root = await model.gitRepositoryRoot(path: path) ?? path
            let canonical = URL(fileURLWithPath: root)
                .standardizedFileURL
                .resolvingSymlinksInPath()
                .path

            if let existing = controllers[canonical] {
                existing.viewModel.setScope(scope)
                Task { await existing.viewModel.reload() }
                existing.showWindow(nil)
                existing.window?.makeKeyAndOrderFront(nil)
                NSApp.activate(ignoringOtherApps: true)
                return
            }

            let controller = KiteGitLogWindowController(
                model: model,
                repositoryRoot: canonical,
                scope: scope
            )
            controllers[canonical] = controller
            controller.showWindow(nil)
            controller.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    func windowWillClose(_ notification: Notification) {
        Self.controllers.removeValue(forKey: repositoryRoot)
    }

    private func updateWindowTitle(_ context: GitGraphContextDto?) {
        let root = context?.log.repositoryRoot ?? repositoryRoot
        let repositoryName = URL(fileURLWithPath: root).lastPathComponent
        let name = repositoryName.isEmpty ? "Git" : repositoryName
        if let path = context?.pathspec, !path.isEmpty {
            window?.title = "Git Log — \(path)"
        } else {
            window?.title = "Git Graph — \(name)"
        }
    }
}

private struct KiteGitLogWindowView: View {
    @StateObject private var viewModel: KiteGitLogViewModel
    @State private var sidebarCollapsed = false
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
                HSplitView {
                    if !sidebarCollapsed {
                        sidebar
                            .frame(minWidth: 190, idealWidth: 224, maxWidth: 260)
                            .transition(.move(edge: .leading).combined(with: .opacity))
                    }
                    VSplitView {
                        graphPane
                            .frame(minHeight: 240)
                        detailPane
                            .frame(minHeight: 230)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .animation(.easeInOut(duration: 0.16), value: sidebarCollapsed)
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(height: 0.5)
                statusBar
            }
        }
        .frame(minWidth: 900, minHeight: 640)
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
        VStack(spacing: 9) {
            HStack(spacing: 10) {
                Button {
                    withAnimation(.easeInOut(duration: 0.16)) {
                        sidebarCollapsed.toggle()
                    }
                } label: {
                    Image(systemName: sidebarCollapsed ? "sidebar.left" : "sidebar.leading")
                        .font(.system(size: 12, weight: .medium))
                        .frame(width: 26, height: 26)
                }
                .buttonStyle(.borderless)
                .help(sidebarCollapsed ? "显示 Git 导航" : "折叠 Git 导航")

                Image(systemName: "point.3.connected.trianglepath.dotted")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 30, height: 30)
                    .background(
                        Color.accentColor.opacity(0.12),
                        in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                    )

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 8) {
                        Text(viewModel.workspaceTitle)
                            .font(.system(size: 13, weight: .semibold))
                        if let branch = viewModel.logContext?.currentBranch {
                            Text(branch)
                                .font(.system(size: 10, weight: .medium))
                                .foregroundStyle(Color.accentColor)
                                .padding(.horizontal, 7)
                                .padding(.vertical, 2)
                                .background(Color.accentColor.opacity(0.12), in: Capsule())
                        }
                    }
                    Text(viewModel.repositoryRoot)
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Spacer(minLength: 8)

                HStack(spacing: 6) {
                    Image(systemName: viewModel.scopeChipIcon)
                        .font(.system(size: 9.5, weight: .semibold))
                    Text(viewModel.scopeChipTitle)
                        .font(.system(size: 10.5, weight: .medium))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if viewModel.isScoped {
                        Button {
                            Task { await viewModel.clearScope() }
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .font(.system(size: 10))
                        }
                        .buttonStyle(.plain)
                        .help("清除范围，回到 Repository Graph")
                    }
                }
                .foregroundStyle(viewModel.isScoped ? Color.accentColor : Color.secondary)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(
                    (viewModel.isScoped ? Color.accentColor : Color.secondary).opacity(0.12),
                    in: Capsule()
                )
                .frame(maxWidth: 360)
                .accessibilityLabel("当前 Git scope：\(viewModel.scopeChipTitle)")

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
                    Label(
                        "筛选 · \(viewModel.searchMode.title)",
                        systemImage: "line.3.horizontal.decrease.circle"
                    )
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
            .background(
                Color.primary.opacity(0.045),
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
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
            Label(viewModel.branchLabel(for: viewModel.branch), systemImage: "arrow.triangle.branch")
                .font(.system(size: 10.5, weight: .medium))
                .lineLimit(1)
        }
        .menuStyle(.borderlessButton)
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: 180)
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
            Text("\(viewModel.commits.count) 个提交 · \(viewModel.scopeChipTitle)")
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
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
    var sidebar: some View {
        let references = viewModel.logContext?.refs ?? []
        let localBranches = references.filter { $0.kind == .localBranch }
        let remoteBranches = references.filter { $0.kind == .remoteBranch }
        let tags = references.filter { $0.kind == .tag }

        return VStack(spacing: 0) {
            HStack(spacing: 7) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                Text("Git Workspace")
                    .font(.system(size: 11, weight: .semibold))
                Spacer()
                Text("\(references.count)")
                    .font(.system(size: 10, design: .rounded))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10)
            .frame(height: 36)

            Divider()

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    navButton(
                        "All Commits",
                        icon: "point.3.connected.trianglepath.dotted",
                        selected: viewModel.branch == nil && !viewModel.isScoped
                    ) {
                        Task { await viewModel.showAllCommits() }
                    }

                    navButton("Local Changes", icon: "square.and.pencil", selected: false) {
                        KiteGitClientWindowController.show(
                            model: viewModel.model,
                            repositoryPath: viewModel.repositoryRoot
                        )
                    }

                    sectionLabel("Branches", icon: "arrow.triangle.branch", count: localBranches.count + remoteBranches.count)
                    referenceGroup(
                        title: "本地分支",
                        references: localBranches,
                        icon: "arrow.triangle.branch"
                    )
                    referenceGroup(
                        title: "远程分支",
                        references: remoteBranches,
                        icon: "cloud"
                    )

                    sectionLabel("Tags", icon: "tag", count: tags.count)
                    referenceGroup(
                        title: "标签",
                        references: tags,
                        icon: "tag"
                    )

                    sectionLabel("Worktrees", icon: "square.stack.3d.up", count: 1)
                    worktreeRow

                    navButton(
                        "Filter",
                        icon: "line.3.horizontal.decrease.circle",
                        selected: searchFocused
                    ) {
                        searchFocused = true
                    }
                }
                .padding(8)
            }
        }
        .frame(width: 224)
        .background(Color.primary.opacity(0.025))
    }

    func navButton(
        _ title: String,
        icon: String,
        selected: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                    .frame(width: 14)
                Text(title)
                    .font(.system(size: 11.5, weight: selected ? .semibold : .regular))
                    .lineLimit(1)
                Spacer(minLength: 4)
            }
            .padding(.horizontal, 8)
            .frame(height: 27)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                selected ? Color.accentColor.opacity(0.14) : Color.clear,
                in: RoundedRectangle(cornerRadius: 6, style: .continuous)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(title)
    }

    func sectionLabel(_ title: String, icon: String, count: Int) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 9.5, weight: .semibold))
            Text(title)
                .font(.system(size: 10, weight: .semibold))
            Spacer()
            Text("\(count)")
                .font(.system(size: 9.5, design: .rounded))
        }
        .foregroundStyle(.secondary)
        .padding(.horizontal, 8)
        .padding(.top, 9)
        .padding(.bottom, 2)
    }

    func referenceGroup(
        title: String,
        references: [GitRefDto],
        icon: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            if references.isEmpty {
                Text("无")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
                    .padding(.leading, 30)
                    .frame(height: 22)
            } else {
                ForEach(references, id: \.fullName) { reference in
                    referenceRow(
                        label: reference.name,
                        fullName: reference.fullName,
                        icon: icon,
                        isHead: reference.isHead
                    )
                }
            }
        }
    }

    func referenceRow(
        label: String,
        fullName: String?,
        icon: String,
        isHead: Bool
    ) -> some View {
        let selected = viewModel.branch == fullName
        return Button {
            chooseBranch(fullName)
        } label: {
            HStack(spacing: 7) {
                Image(systemName: icon)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                    .frame(width: 14)
                Text(label)
                    .font(.system(size: 11.5))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                if isHead {
                    Text("HEAD")
                        .font(.system(size: 8.5, weight: .semibold))
                        .foregroundStyle(Color.accentColor)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 26)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                selected ? Color.accentColor.opacity(0.14) : Color.clear,
                in: RoundedRectangle(cornerRadius: 6, style: .continuous)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(label)
    }

    var worktreeRow: some View {
        Button {
            KiteGitClientWindowController.show(
                model: viewModel.model,
                repositoryPath: viewModel.repositoryRoot
            )
        } label: {
            HStack(spacing: 7) {
                Image(systemName: "folder")
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(.secondary)
                    .frame(width: 14)
                VStack(alignment: .leading, spacing: 1) {
                    Text(URL(fileURLWithPath: viewModel.repositoryRoot).lastPathComponent)
                        .font(.system(size: 11))
                        .lineLimit(1)
                    Text(viewModel.logContext?.currentBranch ?? "detached HEAD")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
            }
            .padding(.horizontal, 8)
            .frame(height: 34)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("打开 Git 客户端")
    }

    func chooseBranch(_ fullName: String?) {
        guard viewModel.branch != fullName else { return }
        viewModel.branch = fullName
        Task { await viewModel.reload() }
    }

    var graphPane: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text("提交图")
                    .font(.system(size: 11, weight: .semibold))
                Spacer()
                Text("\(viewModel.commits.count) 个提交")
                    .font(.system(size: 10, design: .rounded))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12)
            .frame(height: 34)
            .background(Color.primary.opacity(0.025))

            graphHeader
            Divider()
            graphContent
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    var graphHeader: some View {
        HStack(spacing: 8) {
            Text("Graph")
                .frame(width: 92, alignment: .leading)
            Text("Refs")
                .frame(width: 210, alignment: .leading)
            Text("Subject")
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("Author")
                .frame(width: 120, alignment: .leading)
            Text("Date")
                .frame(width: 125, alignment: .leading)
            Text("Hash")
                .frame(width: 74, alignment: .leading)
        }
        .font(.system(size: 10, weight: .medium))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .frame(height: 30)
        .background(Color.primary.opacity(0.035))
    }

    @ViewBuilder
    var graphContent: some View {
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
                Image(systemName: viewModel.isScoped ? "folder.badge.questionmark" : "arrow.triangle.branch")
                    .font(.system(size: 30, weight: .light))
                    .foregroundStyle(.tertiary)
                Text(viewModel.emptyHistoryMessage)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
                if viewModel.isScoped {
                    Button("清除范围") {
                        Task { await viewModel.clearScope() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
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
                        .onTapGesture(count: 2) {
                            Task {
                                await viewModel.select(commit)
                                viewModel.detailTab = .files
                            }
                        }
                        .contextMenu {
                            Button("复制完整哈希") {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(commit.hash, forType: .string)
                            }
                            Button("在提交时浏览文件") {
                                Task { await viewModel.loadTree(commit) }
                            }
                            if let details = viewModel.details,
                               details.commit.hash == commit.hash,
                               !details.changedFiles.isEmpty {
                                Menu("查看文件历史") {
                                    ForEach(details.changedFiles, id: \.path) { file in
                                        Button(file.path) {
                                            Task { await viewModel.showFileHistory(file.path) }
                                        }
                                    }
                                }
                            } else {
                                Button("加载变更文件…") {
                                    Task {
                                        await viewModel.select(commit)
                                        viewModel.detailTab = .files
                                    }
                                }
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
        case .commit: return "number"
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
            return .secondary
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
            .frame(height: 38)
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
                Text(viewModel.scopeChipTitle)
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
            .padding(.horizontal, 12)
            .frame(height: 26)
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
        return "选择提交查看详情"
    }

    @ViewBuilder
    private var commitDetailContent: some View {
        if let details = viewModel.details {
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
                            .background(
                                Color.primary.opacity(0.045),
                                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
                            )
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

    @ViewBuilder
    private var changedFilesContent: some View {
        if let details = viewModel.details, !details.changedFiles.isEmpty {
            HStack(spacing: 0) {
                KiteChangedFileTree(viewModel: viewModel, files: details.changedFiles)
                    .frame(minWidth: 240, idealWidth: 320, maxWidth: 440)
                Divider()
                changedFileDetail(details)
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

    @ViewBuilder
    private func changedFileDetail(_ details: GitCommitDetailsDto) -> some View {
        let file = details.changedFiles.first(where: { $0.path == viewModel.selectedChangedFile })
            ?? details.changedFiles.first
        if let file {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 8) {
                    Text(String(file.status.prefix(1)))
                        .font(.system(size: 10, weight: .bold, design: .monospaced))
                        .foregroundStyle(kiteGitStatusColor(file.status))
                        .frame(width: 18, alignment: .leading)
                    Text(file.path)
                        .font(.system(size: 11, design: .monospaced))
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Spacer()
                }

                if let oldPath = file.oldPath {
                    Text("重命名自 \(oldPath)")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }

                HStack(spacing: 8) {
                    Button("在独立窗口打开") {
                        openDiff(file, commit: details.commit)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)

                    Button("查看文件历史") {
                        Task { await viewModel.showFileHistory(file.path) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)

                    Spacer()
                }

                Divider()

                KiteDiffViewerView(model: viewModel.embeddedDiffModel, embedded: true)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .padding(16)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        } else {
            Text("选择一个变更文件")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func openDiff(_ file: GitLogChangedFileDto, commit: GitLogCommitDto) {
        let absolutePath = URL(fileURLWithPath: viewModel.repositoryRoot)
            .appendingPathComponent(file.path)
            .path
        KiteDiffViewerWindowController.shared.show(
            path: absolutePath,
            appModel: viewModel.model,
            preset: .custom,
            oldRevision: "\(commit.hash)^",
            newRevision: commit.hash
        )
    }

    @ViewBuilder
    private var treeContent: some View {
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
            KiteCommitFileTree(viewModel: viewModel, entries: viewModel.treeEntries)
        }
    }

    private static func fullTime(_ seconds: Int64) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "zh_CN")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
    }
}

private struct KiteChangedFileTree: View {
    @ObservedObject var viewModel: KiteGitLogViewModel
    let files: [GitLogChangedFileDto]

    var body: some View {
        let nodes = KiteChangedFileNode.make(from: files)
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 1) {
                OutlineGroup(nodes, children: \.children) { node in
                    if let file = node.file {
                        Button {
                            viewModel.selectedChangedFile = file.path
                            viewModel.showEmbeddedDiff()
                        } label: {
                            HStack(spacing: 7) {
                                Text(String(file.status.prefix(1)))
                                    .font(.system(size: 9.5, weight: .bold, design: .monospaced))
                                    .foregroundStyle(kiteGitStatusColor(file.status))
                                    .frame(width: 16, alignment: .leading)
                                Image(systemName: "doc.text")
                                    .font(.system(size: 9))
                                    .foregroundStyle(.secondary)
                                Text(node.name)
                                    .font(.system(size: 10.5, design: .monospaced))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                Spacer(minLength: 4)
                            }
                            .padding(.horizontal, 7)
                            .frame(height: 26)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(
                                viewModel.selectedChangedFile == file.path
                                    ? Color.accentColor.opacity(0.14)
                                    : Color.clear,
                                in: RoundedRectangle(cornerRadius: 5, style: .continuous)
                            )
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .help(file.path)
                    } else {
                        Label(node.name, systemImage: "folder")
                            .font(.system(size: 10.5, weight: .medium))
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 3)
                    }
                }
            }
            .padding(6)
        }
    }
}

private struct KiteChangedFileNode: Identifiable {
    let id: String
    let name: String
    let path: String?
    let file: GitLogChangedFileDto?
    let children: [KiteChangedFileNode]?

    static func make(from files: [GitLogChangedFileDto]) -> [KiteChangedFileNode] {
        let root = MutableChangedFileNode(name: "", path: "")
        for file in files {
            let parts = file.path.split(separator: "/").map(String.init)
            var current = root
            var currentPath = ""
            for (index, part) in parts.enumerated() {
                currentPath = currentPath.isEmpty ? part : "\(currentPath)/\(part)"
                let child = current.children[part] ?? MutableChangedFileNode(name: part, path: currentPath)
                current.children[part] = child
                if index == parts.count - 1 {
                    child.file = file
                }
                current = child
            }
        }
        return root.children.values
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
            .map { makeNode($0) }
    }

    private static func makeNode(_ node: MutableChangedFileNode) -> KiteChangedFileNode {
        let children = node.children.values
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
            .map { makeNode($0) }
        return KiteChangedFileNode(
            id: node.file == nil ? "dir:\(node.path)" : "file:\(node.path)",
            name: node.name,
            path: node.file?.path,
            file: node.file,
            children: children.isEmpty ? nil : children
        )
    }
}

private final class MutableChangedFileNode {
    let name: String
    let path: String
    var children: [String: MutableChangedFileNode] = [:]
    var file: GitLogChangedFileDto?

    init(name: String, path: String) {
        self.name = name
        self.path = path
    }
}

private struct KiteCommitFileTree: View {
    @ObservedObject var viewModel: KiteGitLogViewModel
    let entries: [GitTreeEntryDto]

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 1) {
                    ForEach(entries, id: \.path) { entry in
                        let depth = max(0, entry.path.split(separator: "/").count - 1)
                        HStack(spacing: 8) {
                            Image(systemName: "doc.text")
                                .font(.system(size: 9))
                                .foregroundStyle(.secondary)
                                .frame(width: 14)
                            Text(String(repeating: "  ", count: depth) + entry.name)
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
                        .background(
                            viewModel.isInScope(entry.path)
                                ? Color.accentColor.opacity(0.10)
                                : Color.clear
                        )
                        .id(entry.path)
                    }
                }
                .padding(.vertical, 6)
            }
            .onAppear {
                scrollToScope(proxy)
            }
            .onChange(of: entries.count) { _ in
                scrollToScope(proxy)
            }
        }
    }

    private func scrollToScope(_ proxy: ScrollViewProxy) {
        guard let scopePath = viewModel.scopePath, !scopePath.isEmpty else { return }
        let match = entries.first {
            $0.path == scopePath || $0.path.hasPrefix(scopePath + "/")
        }
        guard let match else { return }
        DispatchQueue.main.async {
            proxy.scrollTo(match.path, anchor: .center)
        }
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

private func kiteGitStatusColor(_ status: String) -> Color {
    switch String(status.prefix(1)) {
    case "A": return Color(hex: 0x55b978)
    case "D": return Color(hex: 0xef6b73)
    case "R", "C": return Color(hex: 0x5e9cff)
    default: return Color(hex: 0xe5a84b)
    }
}
