import AppKit
import SwiftUI

@MainActor
final class KiteGitClientWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: KiteGitClientWindowController] = [:]

    private let model: KiteGitClientModel

    private init(repositoryRoot: String, model: KiteGitClientModel) {
        self.model = model
        let size = NSSize(
            width: max(CGFloat(model.windowWidth), 760),
            height: max(CGFloat(model.windowHeight), 520)
        )
        let gitWindow = NSWindow(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        let title = URL(fileURLWithPath: repositoryRoot).lastPathComponent
        gitWindow.title = "Git — \(title)"
        gitWindow.titlebarAppearsTransparent = true
        gitWindow.minSize = NSSize(width: 760, height: 520)
        gitWindow.isReleasedWhenClosed = false
        gitWindow.contentViewController = NSHostingController(
            rootView: KiteGitClientWindowView(model: model)
        )
        super.init(window: gitWindow)
        gitWindow.delegate = self
        gitWindow.center()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    /// Resolves the repository root before opening the window. The git probe is
    /// bounded and runs off the main thread.
    static func open(path: String) {
        guard !path.isEmpty else { return }
        Task { @MainActor in
            let core = KiteCore()
            do {
                let root = try await Task.detached(priority: .userInitiated) {
                    try core.gitRepositoryRoot(path: path)
                }.value
                guard let root else {
                    NSSound.beep()
                    return
                }
                show(repositoryRoot: root)
            } catch {
                NSSound.beep()
            }
        }
    }

    static func show(repositoryRoot: String) {
        let canonical = URL(fileURLWithPath: repositoryRoot)
            .standardizedFileURL
            .resolvingSymlinksInPath()
            .path
        let key = canonical
        if let existing = controllers[key] {
            existing.showWindow(nil)
            existing.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let model = KiteGitClientModel(repositoryRoot: canonical)
        let controller = KiteGitClientWindowController(repositoryRoot: canonical, model: model)
        controllers[key] = controller
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        model.refresh()
    }

    func windowWillClose(_ notification: Notification) {
        model.persist(windowSize: window?.frame.size)
        let key = model.repositoryRoot
        Self.controllers.removeValue(forKey: key)
    }
}

@MainActor
private enum KiteGitClientView: String, CaseIterable, Identifiable {
    case changes
    case log
    case details

    var id: String { rawValue }

    var title: String {
        switch self {
        case .changes: return "变更"
        case .log: return "日志"
        case .details: return "提交详情"
        }
    }
}

@MainActor
private enum KiteGitNoticeKind {
    case success
    case info
    case warning
    case error
    case conflict

    var color: Color {
        switch self {
        case .success: return Color(red: 0.33, green: 0.73, blue: 0.47)
        case .info: return Color(red: 0.37, green: 0.61, blue: 1.0)
        case .warning: return Color(red: 0.96, green: 0.48, blue: 0.20)
        case .error, .conflict: return Color(red: 0.90, green: 0.28, blue: 0.30)
        }
    }
}

@MainActor
private enum KiteGitNoticeAction {
    case refresh
    case openConsole
}

@MainActor
private struct KiteGitNotice: Identifiable {
    let id = UUID()
    let kind: KiteGitNoticeKind
    let title: String
    let message: String
    var action: KiteGitNoticeAction?
    var actionTitle: String?
}

@MainActor
private final class KiteGitClientModel: ObservableObject {
    let repositoryRoot: String
    private let core: KiteCore
    private var refreshGeneration: UInt64 = 0
    private var taskGeneration: UInt64 = 0
    private var busyCancellation: VcsCancellation?

    @Published var snapshot: GitRepositorySnapshotDto?
    @Published var selectedCommitHash: String?
    @Published var activeView: KiteGitClientView = .log
    @Published var commitMessage = ""
    @Published var loading = false
    @Published var busyTask: String?
    @Published var consoleExpanded = true
    @Published var consoleLines: [String] = ["Git 客户端已就绪。"]
    @Published var notice: KiteGitNotice?
    @Published var showShortcuts = false
    @Published var changesWidth: CGFloat = 280
    @Published var detailsWidth: CGFloat = 330
    @Published var consoleHeight: CGFloat = 150
    @Published var windowWidth: UInt32 = 1_180
    @Published var windowHeight: UInt32 = 780

    private var lastContentWidth: CGFloat = 1_180
    private var savedChangesRatio: UInt16 = 2_800
    private var savedDetailsRatio: UInt16 = 3_300
    private var savedConsoleRatio: UInt16 = 1_800

    init(repositoryRoot: String, core: KiteCore = KiteCore()) {
        self.repositoryRoot = repositoryRoot
        self.core = core
        if let saved = core.loadGitClientWindowState(repositoryRoot: repositoryRoot) {
            windowWidth = max(saved.width, 760)
            windowHeight = max(saved.height, 520)
            savedChangesRatio = saved.changesRatioBp
            savedDetailsRatio = saved.detailsRatioBp
            savedConsoleRatio = saved.consoleRatioBp
            consoleExpanded = saved.consoleExpanded
            activeView = KiteGitClientView(rawValue: saved.lastView) ?? .log
            let width = CGFloat(saved.width)
            changesWidth = max(220, width * CGFloat(saved.changesRatioBp) / 10_000)
            detailsWidth = max(250, width * CGFloat(saved.detailsRatioBp) / 10_000)
            consoleHeight = max(110, CGFloat(saved.height) * CGFloat(saved.consoleRatioBp) / 10_000)
            lastContentWidth = width
        }
    }

    var branchTitle: String {
        if let branch = snapshot?.branch, !branch.isEmpty {
            return branch
        }
        if let hash = snapshot?.headShortHash, !hash.isEmpty {
            return "分离 HEAD \(hash)"
        }
        return "无分支"
    }

    var selectedCommit: GitCommitSummaryDto? {
        guard let commits = snapshot?.recentCommits, !commits.isEmpty else { return nil }
        if let selectedCommitHash,
           let selected = commits.first(where: { $0.hash == selectedCommitHash }) {
            return selected
        }
        return commits.first
    }

    var selectedCommitIndex: Int {
        guard let commits = snapshot?.recentCommits, let selected = selectedCommit else {
            return -1
        }
        return commits.firstIndex(where: { $0.hash == selected.hash }) ?? -1
    }

    func recordContentWidth(_ width: CGFloat) {
        if width > 0 {
            lastContentWidth = width
        }
    }

    func refresh() {
        guard busyTask == nil else { return }
        refreshGeneration &+= 1
        let generation = refreshGeneration
        loading = true
        busyTask = "读取仓库状态"
        appendConsole("刷新仓库状态…")
        let core = self.core
        let root = repositoryRoot
        Task {
            do {
                let snapshot = try await Task.detached(priority: .utility) {
                    try core.gitRepositorySnapshot(path: root)
                }.value
                guard generation == refreshGeneration else { return }
                loading = false
                busyTask = nil
                guard let snapshot else {
                    notice = KiteGitNotice(
                        kind: .error,
                        title: "无法打开仓库",
                        message: "当前路径不在 Git 工作区中。",
                        action: .openConsole,
                        actionTitle: "查看控制台"
                    )
                    return
                }
                self.snapshot = snapshot
                if selectedCommitHash == nil ||
                    !snapshot.recentCommits.contains(where: { $0.hash == selectedCommitHash }) {
                    selectedCommitHash = snapshot.recentCommits.first?.hash
                }
                appendConsole(
                    "已读取 \(snapshot.changedFiles.count) 个变更文件和 "
                        + "\(snapshot.recentCommits.count) 个提交。"
                )
            } catch {
                guard generation == refreshGeneration else { return }
                loading = false
                busyTask = nil
                appendConsole("读取失败：\(error.localizedDescription)")
                notice = KiteGitNotice(
                    kind: .error,
                    title: "无法读取仓库",
                    message: error.localizedDescription,
                    action: .openConsole,
                    actionTitle: "查看控制台"
                )
            }
        }
    }

    func cancelTask() {
        let label = busyTask ?? "Git 任务"
        busyCancellation?.cancel()
        busyCancellation = nil
        busyTask = nil
        loading = false
        refreshGeneration &+= 1
        taskGeneration &+= 1
        appendConsole("已取消：\(label)")
        notice = KiteGitNotice(
            kind: .info,
            title: "任务已取消",
            message: label,
            action: nil,
            actionTitle: nil
        )
    }

    func commit() {
        let message = commitMessage.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else {
            notice = KiteGitNotice(
                kind: .warning,
                title: "提交信息为空",
                message: "输入提交信息后才能提交。",
                action: nil,
                actionTitle: nil
            )
            return
        }
        run(operation: .commit(message: message), label: "Git 提交", clearCommit: true)
    }

    func stageAll() {
        run(operation: .add, label: "暂存全部变更", clearCommit: false)
    }

    func unstageAll() {
        run(operation: .unstage, label: "取消暂存全部变更", clearCommit: false)
    }

    func fetch() {
        run(operation: .fetch, label: "Git 获取", clearCommit: false)
    }

    func pull() {
        run(operation: .pull, label: "Git 拉取", clearCommit: false)
    }

    func push() {
        run(operation: .push, label: "Git 推送", clearCommit: false)
    }

    func stash() {
        run(operation: .stash, label: "Git 贮藏", clearCommit: false)
    }
    func runControl(_ control: GitOperationControlDto, title: String) {
        guard busyTask == nil else { return }
        taskGeneration &+= 1
        let generation = taskGeneration
        busyTask = title
        appendConsole("\(title)进行中的 Git 操作…")
        let core = self.core
        let root = repositoryRoot
        Task {
            do {
                let message = try await Task.detached(priority: .userInitiated) {
                    try core.runGitOperationControl(path: root, control: control)
                }.value
                guard generation == taskGeneration else { return }
                busyTask = nil
                appendConsole(message)
                notice = KiteGitNotice(
                    kind: .success,
                    title: "Git 操作已继续",
                    message: message,
                    action: .refresh,
                    actionTitle: "刷新"
                )
                refresh()
            } catch {
                guard generation == taskGeneration else { return }
                busyTask = nil
                appendConsole("\(title)失败：\(error.localizedDescription)")
                notice = noticeForError(title: title, error: error.localizedDescription)
            }
        }
    }

    func applyNoticeAction(_ action: KiteGitNoticeAction) {
        notice = nil
        switch action {
        case .refresh:
            refresh()
        case .openConsole:
            consoleExpanded = true
        }
    }

    func dismiss() {
        if showShortcuts {
            showShortcuts = false
        } else {
            notice = nil
        }
    }

    func selectPreviousCommit() {
        guard let commits = snapshot?.recentCommits, selectedCommitIndex > 0 else { return }
        selectedCommitHash = commits[selectedCommitIndex - 1].hash
        activeView = .details
    }

    func selectNextCommit() {
        guard let commits = snapshot?.recentCommits,
              selectedCommitIndex >= 0,
              selectedCommitIndex + 1 < commits.count else { return }
        selectedCommitHash = commits[selectedCommitIndex + 1].hash
        activeView = .details
    }

    func persist(windowSize: CGSize?) {
        let width = UInt32(max(windowSize?.width ?? CGFloat(windowWidth), 760))
        let height = UInt32(max(windowSize?.height ?? CGFloat(windowHeight), 520))
        let contentWidth = max(lastContentWidth, 1)
        let changesRatio = UInt16(
            min(max(changesWidth / contentWidth * 10_000, 1_400), 4_200)
        )
        let detailsRatio = UInt16(
            min(max(detailsWidth / contentWidth * 10_000, 1_600), 4_600)
        )
        let consoleRatio = UInt16(
            min(max(consoleHeight / max(CGFloat(height), 1) * 10_000, 1_200), 4_500)
        )
        let state = GitClientWindowSessionDto(
            repositoryRoot: repositoryRoot,
            width: width,
            height: height,
            changesRatioBp: changesRatio,
            detailsRatioBp: detailsRatio,
            consoleRatioBp: consoleRatio,
            consoleExpanded: consoleExpanded,
            lastView: activeView.rawValue
        )
        do {
            try core.saveGitClientWindowState(state: state)
        } catch {
            NSLog("无法保存 Git 客户端窗口状态：%@", error.localizedDescription)
        }
    }

    private func run(operation: VcsOperationDto, label: String, clearCommit: Bool) {
        guard busyTask == nil else { return }
        taskGeneration &+= 1
        let generation = taskGeneration
        let cancellation = VcsCancellation()
        busyCancellation = cancellation
        busyTask = label
        appendConsole("\(label)…")
        let core = self.core
        let root = repositoryRoot
        Task {
            do {
                let message = try await Task.detached(priority: .userInitiated) {
                    try core.runVcsOperationCancellable(
                        tool: .git,
                        operation: operation,
                        path: root,
                        cancellation: cancellation
                    )
                }.value
                guard generation == taskGeneration else { return }
                busyCancellation = nil
                busyTask = nil
                if clearCommit {
                    commitMessage = ""
                }
                appendConsole(message)
                notice = KiteGitNotice(
                    kind: .success,
                    title: label,
                    message: message,
                    action: .refresh,
                    actionTitle: "刷新"
                )
                refresh()
            } catch {
                guard generation == taskGeneration else { return }
                busyCancellation = nil
                busyTask = nil
                appendConsole("\(label)失败：\(error.localizedDescription)")
                notice = noticeForError(title: label, error: error.localizedDescription)
            }
        }
    }

    private func appendConsole(_ message: String) {
        consoleLines.append("› \(message)")
        if consoleLines.count > 200 {
            consoleLines.removeFirst(consoleLines.count - 200)
        }
    }

    private func noticeForError(title: String, error: String) -> KiteGitNotice {
        let lower = error.lowercased()
        let rejected = lower.contains("non-fast-forward")
            || lower.contains("rejected")
            || lower.contains("fetch first")
        let conflict = lower.contains("conflict")
            || lower.contains("unmerged")
            || error.contains("冲突")
        let kind: KiteGitNoticeKind = rejected || conflict ? .conflict : .error
        let resolvedTitle = rejected ? "推送被拒绝" : (conflict ? "需要解决冲突" : title)
        return KiteGitNotice(
            kind: kind,
            title: resolvedTitle,
            message: error,
            action: .openConsole,
            actionTitle: "查看控制台"
        )
    }
}
private struct KiteGitClientShortcut: Identifiable {
    let id = UUID()
    let label: String
    let keys: String
}

private let kiteGitClientShortcuts: [KiteGitClientShortcut] = [
    .init(label: "打开 Git 客户端", keys: "⇧⌘G"),
    .init(label: "刷新仓库", keys: "⇧⌘R"),
    .init(label: "获取", keys: "⌥⌘F"),
    .init(label: "拉取", keys: "⌥⌘P"),
    .init(label: "推送", keys: "⌥⌘K"),
    .init(label: "提交", keys: "⌘K"),
    .init(label: "贮藏", keys: "⌥⌘S"),
    .init(label: "显示或隐藏控制台", keys: "⌘J"),
    .init(label: "聚焦变更", keys: "⌥1"),
    .init(label: "聚焦日志", keys: "⌥2"),
    .init(label: "聚焦提交详情", keys: "⌥3"),
    .init(label: "上一个提交", keys: "⌥↑"),
    .init(label: "下一个提交", keys: "⌥↓"),
    .init(label: "快捷键", keys: "F1"),
    .init(label: "关闭提示或浮层", keys: "Esc"),
]

private struct KiteGitClientWindowView: View {
    @ObservedObject var model: KiteGitClientModel

    var body: some View {
        ZStack {
            KiteGlassBackdrop()

            VStack(spacing: 8) {
                toolbar
                if model.snapshot?.operation != nil {
                    operationBanner
                }
                GeometryReader { geometry in
                    VStack(spacing: 0) {
                        HStack(spacing: 0) {
                            changesPanel
                                .frame(width: model.changesWidth)
                            KiteGitHorizontalResizeHandle(
                                value: $model.changesWidth,
                                range: 220...min(480, max(220, geometry.size.width * 0.42)),
                                growsWhenDraggingLeft: false
                            )
                            logPanel
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                            KiteGitHorizontalResizeHandle(
                                value: $model.detailsWidth,
                                range: 250...min(520, max(250, geometry.size.width * 0.46)),
                                growsWhenDraggingLeft: true
                            )
                            detailsPanel
                                .frame(width: model.detailsWidth)
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)

                        if model.consoleExpanded {
                            KiteGitVerticalResizeHandle(
                                value: $model.consoleHeight,
                                range: 110...max(110, min(360, geometry.size.height * 0.45))
                            )
                        }
                        consolePanel
                            .frame(
                                height: model.consoleExpanded
                                    ? min(max(model.consoleHeight, 110), geometry.size.height * 0.45)
                                    : 32
                            )
                    }
                    .onAppear {
                        model.recordContentWidth(geometry.size.width)
                    }
                    .onChange(of: geometry.size.width) { width in
                        model.recordContentWidth(width)
                    }
                }
                statusBar
            }
            .padding(8)
        }
        .frame(minWidth: 760, minHeight: 520)
        .overlay(alignment: .bottomTrailing) {
            if let notice = model.notice {
                KiteGitNoticeView(model: model, notice: notice)
                    .padding(.trailing, 18)
                    .padding(.bottom, 38)
            }
        }
        .sheet(isPresented: $model.showShortcuts) {
            KiteGitShortcutSheet(model: model)
        }
        .onExitCommand {
            model.dismiss()
        }
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 28, height: 28)
                    .background(Color.accentColor.opacity(0.13), in: RoundedRectangle(cornerRadius: 7))
                VStack(alignment: .leading, spacing: 1) {
                    Text("GIT 客户端")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(.tertiary)
                    Text(repositoryName)
                        .font(.system(size: 11.5, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }

            Button {
                model.activeView = .log
            } label: {
                Label(model.branchTitle, systemImage: "arrow.triangle.branch")
                    .font(.system(size: 10.5, weight: .medium))
                    .lineLimit(1)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 9)
            .frame(height: 26)
            .background(Color.primary.opacity(0.055), in: Capsule())
            .help(model.snapshot?.upstream ?? "当前分支")

            if let snapshot = model.snapshot,
               snapshot.ahead > 0 || snapshot.behind > 0 {
                Text("↑\(snapshot.ahead)  ↓\(snapshot.behind)")
                    .font(.system(size: 9.5, design: .rounded))
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 8)

            KiteGitToolbarButton(title: "刷新", systemImage: "arrow.clockwise", shortcut: "⇧⌘R") {
                model.refresh()
            }
            .disabled(model.busyTask != nil)
            .keyboardShortcut("r", modifiers: [.command, .shift])
            KiteGitToolbarButton(title: "获取", systemImage: "arrow.clockwise", shortcut: "⌥⌘F") {
                model.fetch()
            }
            .disabled(model.busyTask != nil)
            .keyboardShortcut("f", modifiers: [.command, .option])

            KiteGitToolbarButton(title: "拉取", systemImage: "arrow.down", shortcut: "⌥⌘P") {
                model.pull()
            }
            .disabled(model.busyTask != nil)
            .keyboardShortcut("p", modifiers: [.command, .option])

            KiteGitToolbarButton(title: "推送", systemImage: "arrow.up", shortcut: "⌥⌘K") {
                model.push()
            }
            .disabled(model.busyTask != nil)
            .keyboardShortcut("k", modifiers: [.command, .option])

            KiteGitToolbarButton(
                title: "提交",
                systemImage: "checkmark",
                shortcut: "⌘K",
                prominent: true
            ) {
                model.commit()
            }
            .disabled(model.busyTask != nil || model.commitMessage.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .keyboardShortcut("k", modifiers: [.command])

            KiteGitToolbarButton(title: "贮藏", systemImage: "archivebox", shortcut: "⌥⌘S") {
                model.stash()
            }
            .disabled(model.busyTask != nil)
            .keyboardShortcut("s", modifiers: [.command, .option])

            KiteGitToolbarButton(
                title: "控制台",
                systemImage: "rectangle.bottomthird.inset.filled",
                shortcut: "⌘J"
            ) {
                model.consoleExpanded.toggle()
            }
            .keyboardShortcut("j", modifiers: [.command])

            KiteGitToolbarButton(title: "快捷键", systemImage: "keyboard", shortcut: "F1") {
                model.showShortcuts = true
            }
            .keyboardShortcut(.f1, modifiers: [])
        }
        .padding(.horizontal, 10)
        .frame(height: 46)
        .kiteGlass(.panel, cornerRadius: 13)
    }

    private var operationBanner: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 2) {
                Text(operationTitle)
                    .font(.system(size: 11.5, weight: .semibold))
                Text(operationDetail)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            if operationKind != .bisecting {
                Button("继续") {
                    model.runControl(.continueOperation, title: "继续")
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            }
            if operationKind != .merging && operationKind != .bisecting {
                Button("跳过") {
                    model.runControl(.skip, title: "跳过")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            Button(operationKind == .bisecting ? "结束二分" : "中止") {
                model.runControl(.abort, title: "中止")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.orange.opacity(0.10), in: RoundedRectangle(cornerRadius: 11))
        .overlay {
            RoundedRectangle(cornerRadius: 11)
                .strokeBorder(.orange.opacity(0.32), lineWidth: 0.5)
        }
    }

    private var operationKind: GitOperationKindDto? {
        model.snapshot?.operation
    }

    private var operationTitle: String {
        switch operationKind {
        case .rebasing: return "正在变基"
        case .merging: return "正在合并"
        case .cherryPicking: return "正在拣选提交"
        case .reverting: return "正在还原提交"
        case .bisecting: return "正在二分查找"
        case nil: return ""
        }
    }

    private var operationDetail: String {
        switch operationKind {
        case .rebasing: return "解决冲突后可继续、跳过当前提交或中止变基。"
        case .merging: return "解决冲突后可继续合并，或中止并回到合并前。"
        case .cherryPicking: return "解决冲突后可继续、跳过当前提交或中止拣选。"
        case .reverting: return "解决冲突后可继续、跳过当前提交或中止还原。"
        case .bisecting: return "标记 good / bad 后继续，或结束并重置二分。"
        case nil: return ""
        }
    }

    private var repositoryName: String {
        URL(fileURLWithPath: model.repositoryRoot).lastPathComponent
    }
}
private struct KiteGitPanelHeader: View {
    let title: String
    let count: Int?

    var body: some View {
        HStack {
            Text(title)
                .font(.system(size: 9.5, weight: .semibold))
                .foregroundStyle(.tertiary)
                .textCase(.uppercase)
                .tracking(0.6)
            Spacer()
            if let count {
                Text("\(count)")
                    .font(.system(size: 9, weight: .semibold, design: .rounded))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Color.primary.opacity(0.055), in: Capsule())
            }
        }
    }
}

private struct KiteGitPanelModifier: ViewModifier {
    let active: Bool

    func body(content: Content) -> some View {
        content
            .padding(10)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(
                        active ? Color.accentColor.opacity(0.34) : Color.primary.opacity(0.065),
                        lineWidth: active ? 1 : 0.5
                    )
            }
    }
}

private extension View {
    func kiteGitPanel(active: Bool) -> some View {
        modifier(KiteGitPanelModifier(active: active))
    }
}

private extension KiteGitClientWindowView {
    var changesPanel: some View {
        VStack(spacing: 8) {
            KiteGitPanelHeader(
                title: "变更",
                count: model.snapshot?.changedFiles.count ?? 0
            )

            HStack(spacing: 6) {
                Button {
                    model.stageAll()
                } label: {
                    Label("全部暂存", systemImage: "plus")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.busyTask != nil || model.snapshot?.changedFiles.isEmpty != false)

                Button {
                    model.unstageAll()
                } label: {
                    Label("全部取消暂存", systemImage: "minus")
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .disabled(model.busyTask != nil || stagedFiles.isEmpty)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if changedFiles.isEmpty {
                        KiteGitEmptyState(
                            title: "工作区干净",
                            detail: "当前没有已暂存、未暂存或未跟踪的变更。"
                        )
                    } else {
                        if !stagedFiles.isEmpty {
                            KiteGitChangeGroup(
                                title: "已暂存",
                                color: .green,
                                files: stagedFiles,
                                staged: true
                            )
                        }
                        if !unstagedFiles.isEmpty {
                            KiteGitChangeGroup(
                                title: "未暂存",
                                color: .orange,
                                files: unstagedFiles,
                                staged: false
                            )
                        }
                        if !untrackedFiles.isEmpty {
                            KiteGitChangeGroup(
                                title: "未跟踪",
                                color: .cyan,
                                files: untrackedFiles,
                                staged: false
                            )
                        }
                        if !conflictedFiles.isEmpty {
                            KiteGitChangeGroup(
                                title: "冲突",
                                color: .red,
                                files: conflictedFiles,
                                staged: false
                            )
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollIndicators(.hidden)

            Divider()

            VStack(alignment: .leading, spacing: 6) {
                Text("提交信息")
                    .font(.system(size: 9.5, weight: .semibold))
                    .foregroundStyle(.tertiary)

                ZStack(alignment: .topLeading) {
                    TextEditor(text: $model.commitMessage)
                        .font(.system(size: 11.5))
                        .frame(height: 92)
                        .scrollContentBackground(.hidden)
                        .padding(4)
                        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 8))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8)
                                .strokeBorder(Color.primary.opacity(0.10), lineWidth: 0.5)
                        }

                    if model.commitMessage.isEmpty {
                        Text("说明这次修改的原因…")
                            .font(.system(size: 10.5))
                            .foregroundStyle(.tertiary)
                            .padding(.leading, 10)
                            .padding(.top, 10)
                            .allowsHitTesting(false)
                    }
                }

                HStack {
                    Text("⌘K 提交")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.tertiary)
                    Spacer()
                    Button {
                        model.commit()
                    } label: {
                        Label("提交", systemImage: "checkmark")
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(
                        model.busyTask != nil
                            || model.commitMessage.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    )
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .kiteGitPanel(active: model.activeView == .changes)
        .contentShape(Rectangle())
        .onTapGesture {
            model.activeView = .changes
        }
        .background {
            Button("") { model.activeView = .changes }
                .keyboardShortcut("1", modifiers: [.option])
                .frame(width: 0, height: 0)
                .opacity(0)
        }
    }

    var logPanel: some View {
        VStack(spacing: 8) {
            KiteGitPanelHeader(
                title: "日志",
                count: model.snapshot?.recentCommits.count ?? 0
            )

            ScrollView {
                LazyVStack(spacing: 3) {
                    if let commits = model.snapshot?.recentCommits, !commits.isEmpty {
                        ForEach(Array(commits.enumerated()), id: \.offset) { index, commit in
                            Button {
                                model.selectedCommitHash = commit.hash
                                model.activeView = .details
                            } label: {
                                HStack(alignment: .top, spacing: 8) {
                                    VStack(spacing: 3) {
                                        Circle()
                                            .fill(
                                                commit.hash == model.selectedCommit?.hash
                                                    ? Color.accentColor
                                                    : Color.secondary.opacity(0.55)
                                            )
                                            .frame(width: 8, height: 8)
                                        Rectangle()
                                            .fill(Color.primary.opacity(0.08))
                                            .frame(width: 1)
                                            .frame(maxHeight: .infinity)
                                    }
                                    .frame(width: 16)

                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(commit.subject)
                                            .font(.system(size: 11.5, weight: .medium))
                                            .lineLimit(2)
                                            .multilineTextAlignment(.leading)
                                        HStack(spacing: 8) {
                                            Text(commit.shortHash)
                                            Text(commit.author)
                                            Text(commitTime(commit.unixSeconds))
                                        }
                                        .font(.system(size: 9.5))
                                        .foregroundStyle(.tertiary)
                                    }
                                    Spacer(minLength: 0)
                                }
                                .padding(.horizontal, 7)
                                .padding(.vertical, 7)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background {
                                    if commit.hash == model.selectedCommit?.hash {
                                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                                            .fill(Color.accentColor.opacity(0.09))
                                    }
                                }
                                .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                            }
                            .buttonStyle(.plain)
                        }
                    } else {
                        KiteGitEmptyState(
                            title: "暂无提交",
                            detail: "仓库还没有可显示的提交记录。"
                        )
                    }
                }
            }
            .scrollIndicators(.hidden)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .kiteGitPanel(active: model.activeView == .log)
        .contentShape(Rectangle())
        .onTapGesture {
            model.activeView = .log
        }
        .background {
            HStack {
                Button("") { model.selectPreviousCommit() }
                    .keyboardShortcut(.upArrow, modifiers: [.option])
                Button("") { model.selectNextCommit() }
                    .keyboardShortcut(.downArrow, modifiers: [.option])
                Button("") { model.activeView = .log }
                    .keyboardShortcut("2", modifiers: [.option])
            }
            .frame(width: 0, height: 0)
            .opacity(0)
        }
    }

    var detailsPanel: some View {
        VStack(spacing: 8) {
            KiteGitPanelHeader(title: "提交详情", count: nil)

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if let commit = model.selectedCommit {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(spacing: 8) {
                                Text(commit.shortHash)
                                    .font(.system(size: 10, weight: .semibold))
                                    .foregroundStyle(Color.accentColor)
                                Text(commitTime(commit.unixSeconds))
                                    .font(.system(size: 9.5))
                                    .foregroundStyle(.tertiary)
                            }
                            Text(commit.subject)
                                .font(.system(size: 15, weight: .semibold))
                                .lineLimit(3)
                            Text(commit.author)
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
                        .overlay {
                            RoundedRectangle(cornerRadius: 10)
                                .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.5)
                        }

                        VStack(alignment: .leading, spacing: 5) {
                            Text("完整提交哈希")
                                .font(.system(size: 9.5, weight: .semibold))
                                .foregroundStyle(.tertiary)
                            Text(commit.hash)
                                .font(.system(size: 10, design: .monospaced))
                                .textSelection(.enabled)
                                .padding(8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
                        }
                    } else {
                        KiteGitEmptyState(
                            title: "选择一个提交",
                            detail: "在日志中选择提交以查看作者、时间和提交信息。"
                        )
                    }

                    Divider()

                    VStack(alignment: .leading, spacing: 7) {
                        HStack {
                            Text("变更文件")
                                .font(.system(size: 9.5, weight: .semibold))
                                .foregroundStyle(.tertiary)
                            Spacer()
                            Text("待接入")
                                .font(.system(size: 9))
                                .foregroundStyle(.tertiary)
                        }
                        Text("当前版本先显示提交元数据；提交文件清单将在后续日志视图中提供。")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .padding(10)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 8))
                    }
                }
            }
            .scrollIndicators(.hidden)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .kiteGitPanel(active: model.activeView == .details)
        .contentShape(Rectangle())
        .onTapGesture {
            model.activeView = .details
        }
        .background {
            Button("") { model.activeView = .details }
                .keyboardShortcut("3", modifiers: [.option])
                .frame(width: 0, height: 0)
                .opacity(0)
        }
    }

    var consolePanel: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "rectangle.bottomthird.inset.filled")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.tertiary)
                Text("Git 控制台")
                    .font(.system(size: 10, weight: .semibold))
                Text(model.busyTask == nil ? "就绪" : "后台任务运行中")
                    .font(.system(size: 9.5))
                    .foregroundStyle(.tertiary)
                Spacer()
                Button(model.consoleExpanded ? "收起" : "展开") {
                    model.consoleExpanded.toggle()
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
            }
            .padding(.horizontal, 10)
            .frame(height: 32)

            if model.consoleExpanded {
                Divider()
                if let task = model.busyTask {
                    HStack(spacing: 10) {
                        VStack(alignment: .leading, spacing: 5) {
                            Text(task)
                                .font(.system(size: 10, weight: .medium))
                            ProgressView()
                                .progressViewStyle(.linear)
                                .controlSize(.small)
                        }
                        Button("取消") {
                            model.cancelTask()
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(Color.accentColor.opacity(0.055))
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 3) {
                        ForEach(Array(model.consoleLines.enumerated()), id: \.offset) { _, line in
                            Text(line)
                                .font(.system(size: 9.5, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .scrollIndicators(.hidden)
            }
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.5)
        }
    }

    var statusBar: some View {
        HStack(spacing: 12) {
            Text(model.repositoryRoot)
                .lineLimit(1)
                .truncationMode(.middle)
                .layoutPriority(1)
            Spacer(minLength: 8)
            Text(model.branchTitle)
            Text(model.activeView.title)
            Text("F1 快捷键")
        }
        .font(.system(size: 9.5))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .frame(height: 28)
        .kiteGlass(.panel, cornerRadius: 11)
    }

    var changedFiles: [GitChangedFileDto] {
        model.snapshot?.changedFiles ?? []
    }

    var stagedFiles: [GitChangedFileDto] {
        changedFiles.filter { $0.stagedCode != nil }
    }

    var unstagedFiles: [GitChangedFileDto] {
        changedFiles.filter {
            $0.stagedCode != "U" && $0.unstagedCode != nil && $0.unstagedCode != "?"
        }
    }

    var untrackedFiles: [GitChangedFileDto] {
        changedFiles.filter { $0.unstagedCode == "?" }
    }

    var conflictedFiles: [GitChangedFileDto] {
        changedFiles.filter { $0.stagedCode == "U" || $0.unstagedCode == "U" }
    }
}

private struct KiteGitChangeGroup: View {
    let title: String
    let color: Color
    let files: [GitChangedFileDto]
    let staged: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 5) {
                Circle()
                    .fill(color)
                    .frame(width: 6, height: 6)
                Text("\(title)  \(files.count)")
                    .font(.system(size: 9.5, weight: .semibold))
                    .foregroundStyle(.secondary)
            }

            ForEach(Array(files.enumerated()), id: \.offset) { _, file in
                HStack(spacing: 8) {
                    Text(file.path)
                        .font(.system(size: 10))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    Text((staged ? file.stagedCode : file.unstagedCode) ?? "•")
                        .font(.system(size: 9, weight: .bold, design: .rounded))
                        .foregroundStyle(color)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(color.opacity(0.11), in: Capsule())
                }
                .padding(.horizontal, 7)
                .padding(.vertical, 4)
                .background(Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 6))
            }
        }
    }
}

private struct KiteGitEmptyState: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(spacing: 5) {
            Text(title)
                .font(.system(size: 11.5, weight: .semibold))
            Text(detail)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(14)
        .frame(maxWidth: .infinity)
        .background(Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(Color.primary.opacity(0.06), lineWidth: 0.5)
        }
    }
}
private struct KiteGitToolbarButton: View {
    let title: String
    let systemImage: String
    let shortcut: String
    var prominent = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(.system(size: 10.5, weight: .medium))
                .padding(.horizontal, 8)
                .frame(height: 26)
        }
        .buttonStyle(.plain)
        .foregroundStyle(prominent ? Color.white : Color.primary)
        .background(
            prominent ? Color.accentColor : Color.primary.opacity(0.045),
            in: Capsule()
        )
        .help("\(title) (\(shortcut))")
    }
}

private struct KiteGitHorizontalResizeHandle: View {
    @Binding var value: CGFloat
    let range: ClosedRange<CGFloat>
    let growsWhenDraggingLeft: Bool
    @State private var startValue: CGFloat?

    var body: some View {
        ZStack {
            Color.clear
            Rectangle()
                .fill(Color.primary.opacity(0.10))
                .frame(width: 1)
        }
        .frame(width: 6)
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 1)
                .onChanged { gesture in
                    let base = startValue ?? value
                    if startValue == nil {
                        startValue = value
                    }
                    let delta = growsWhenDraggingLeft
                        ? -gesture.translation.width
                        : gesture.translation.width
                    value = min(max(base + delta, range.lowerBound), range.upperBound)
                }
                .onEnded { _ in
                    startValue = nil
                }
        )
        .help("拖动调整面板宽度")
    }
}

private struct KiteGitVerticalResizeHandle: View {
    @Binding var value: CGFloat
    let range: ClosedRange<CGFloat>
    @State private var startValue: CGFloat?

    var body: some View {
        ZStack {
            Color.clear
            Rectangle()
                .fill(Color.primary.opacity(0.10))
                .frame(height: 1)
        }
        .frame(height: 6)
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 1)
                .onChanged { gesture in
                    let base = startValue ?? value
                    if startValue == nil {
                        startValue = value
                    }
                    value = min(
                        max(base - gesture.translation.height, range.lowerBound),
                        range.upperBound
                    )
                }
                .onEnded { _ in
                    startValue = nil
                }
        )
        .help("拖动调整控制台高度")
    }
}

private struct KiteGitNoticeView: View {
    @ObservedObject var model: KiteGitClientModel
    let notice: KiteGitNotice

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            RoundedRectangle(cornerRadius: 2)
                .fill(notice.kind.color)
                .frame(width: 3)

            VStack(alignment: .leading, spacing: 5) {
                Text(notice.title)
                    .font(.system(size: 11.5, weight: .semibold))
                    .foregroundStyle(notice.kind.color)
                Text(notice.message)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
                if let action = notice.action, let actionTitle = notice.actionTitle {
                    Button(actionTitle) {
                        model.applyNoticeAction(action)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }

            Spacer(minLength: 4)

            Button {
                model.notice = nil
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .semibold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(.tertiary)
        }
        .padding(12)
        .frame(width: 380, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(notice.kind.color.opacity(0.32), lineWidth: 0.5)
        }
        .shadow(color: .black.opacity(0.16), radius: 16, y: 7)
    }
}

private struct KiteGitShortcutSheet: View {
    @ObservedObject var model: KiteGitClientModel

    var body: some View {
        VStack(spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Git 客户端快捷键")
                        .font(.system(size: 18, weight: .semibold))
                    Text("Windows 使用 Ctrl / Alt；macOS 对应 Command / Option。")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    model.showShortcuts = false
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.plain)
            }

            ScrollView {
                VStack(spacing: 4) {
                    ForEach(kiteGitClientShortcuts) { shortcut in
                        HStack {
                            Text(shortcut.label)
                                .font(.system(size: 11.5))
                            Spacer()
                            Text(shortcut.keys)
                                .font(.system(size: 10.5, weight: .medium))
                                .padding(.horizontal, 8)
                                .padding(.vertical, 3)
                                .background(Color.primary.opacity(0.055), in: RoundedRectangle(cornerRadius: 5))
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .background(Color.primary.opacity(0.022), in: RoundedRectangle(cornerRadius: 7))
                    }
                }
            }
            .scrollIndicators(.hidden)

            HStack {
                Spacer()
                Button("完成") {
                    model.showShortcuts = false
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 570, height: 510)
    }
}

private func commitTime(_ unixSeconds: Int64) -> String {
    Date(timeIntervalSince1970: TimeInterval(unixSeconds))
        .formatted(date: .numeric, time: .shortened)
}