import AppKit
import SwiftUI

@MainActor
final class KiteGitToolsWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: KiteGitToolsWindowController] = [:]
    private let repositoryPath: String

    private init(repositoryPath: String, model: KiteModel) {
        self.repositoryPath = URL(fileURLWithPath: repositoryPath).standardizedFileURL.path
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 760, height: 620),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Git Worktree 与 Submodule"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.minSize = NSSize(width: 680, height: 500)
        window.contentViewController = NSHostingController(
            rootView: KiteGitToolsView(repositoryPath: self.repositoryPath, model: model)
        )
        super.init(window: window)
        window.delegate = self
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func open(repositoryPath: String, model: KiteModel) {
        let key = URL(fileURLWithPath: repositoryPath).standardizedFileURL.path
        if let controller = controllers[key] {
            controller.showWindow(nil)
            controller.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let controller = KiteGitToolsWindowController(repositoryPath: key, model: model)
        controllers[key] = controller
        controller.showWindow(nil)
        controller.window?.center()
        controller.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        KiteGitToolsWindowController.controllers.removeValue(forKey: repositoryPath)
    }
}

private enum GitToolsTab: String, CaseIterable, Identifiable {
    case worktrees
    case submodules

    var id: String { rawValue }

    var title: String {
        switch self {
        case .worktrees: return "Worktree"
        case .submodules: return "Submodule"
        }
    }
}

private struct KiteGitToolsView: View {
    let repositoryPath: String
    @ObservedObject var model: KiteModel

    @State private var tab: GitToolsTab = .worktrees
    @State private var worktrees: [GitWorktreeDto] = []
    @State private var submodules: [GitSubmoduleDto] = []
    @State private var loading = true
    @State private var busy = false
    @State private var errorMessage: String?
    @State private var notice: String?

    @State private var showingCreate = false
    @State private var destination = ""
    @State private var branch = ""
    @State private var startPoint = "HEAD"
    @State private var createBranch = true

    var body: some View {
        ZStack {
            KiteGlassBackdrop()

            VStack(spacing: 12) {
                header
                tabPicker

                if loading {
                    ProgressView("正在读取 Git 工作区…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if tab == .worktrees {
                    worktreeContent
                } else {
                    submoduleContent
                }

                if let notice {
                    messageBar(notice, systemImage: "checkmark.circle.fill", color: .green)
                }
                if let errorMessage {
                    messageBar(errorMessage, systemImage: "exclamationmark.triangle.fill", color: .orange)
                }
            }
            .padding(14)
        }
        .frame(minWidth: 680, minHeight: 500)
        .task(id: repositoryPath) { await refreshAll() }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "arrow.triangle.branch")
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 2) {
                Text("Git 工作区")
                    .font(.system(size: 16, weight: .semibold))
                Text(repositoryPath)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer()

            Button {
                Task { await refreshAll() }
            } label: {
                Label("刷新", systemImage: "arrow.clockwise")
            }
            .disabled(busy)
        }
        .kiteGlassGroup(cornerRadius: 12)
    }

    private var tabPicker: some View {
        Picker("视图", selection: $tab) {
            ForEach(GitToolsTab.allCases) { tab in
                Text(tab.title).tag(tab)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
    }

    private var worktreeContent: some View {
        VStack(spacing: 10) {
            HStack {
                Text("全部 Worktree")
                    .font(.system(size: 12, weight: .semibold))
                Spacer()
                Button(showingCreate ? "取消新建" : "新建 Worktree") {
                    withAnimation(.snappy(duration: 0.2)) {
                        showingCreate.toggle()
                    }
                }
            }

            if showingCreate {
                createWorktreeForm
            }

            ScrollView {
                LazyVStack(spacing: 8) {
                    ForEach(worktrees, id: \.path) { worktree in
                        worktreeRow(worktree)
                    }
                }
                .padding(1)
            }
        }
    }

    private var createWorktreeForm: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("新建 Worktree")
                .font(.system(size: 12, weight: .semibold))

            LabeledContent("位置") {
                TextField("例如 ~/Projects/repo-feature", text: $destination)
                    .textFieldStyle(.roundedBorder)
            }
            LabeledContent("分支") {
                TextField(createBranch ? "新分支名" : "已有分支名", text: $branch)
                    .textFieldStyle(.roundedBorder)
            }
            Toggle("创建新分支", isOn: $createBranch)
            if createBranch {
                LabeledContent("起点") {
                    TextField("HEAD、分支或提交哈希", text: $startPoint)
                        .textFieldStyle(.roundedBorder)
                }
            }

            HStack {
                Spacer()
                Button("创建") {
                    Task { await createWorktree() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(
                    busy
                        || destination.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || branch.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
            }
        }
        .padding(12)
        .background(
            Color(nsColor: .controlBackgroundColor).opacity(0.35),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
    }

    private func worktreeRow(_ worktree: GitWorktreeDto) -> some View {
        HStack(spacing: 12) {
            Image(systemName: worktree.isPrimary ? "house.fill" : "arrow.triangle.branch")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(worktree.isCurrent ? Color.accentColor : .secondary)
                .frame(width: 22)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 7) {
                    Text(worktree.branch ?? "分离 HEAD")
                        .font(.system(size: 12, weight: .semibold))
                    if worktree.isPrimary {
                        badge("主工作区", color: .blue)
                    }
                    if worktree.isCurrent {
                        badge("当前", color: .green)
                    }
                    if worktree.lockedReason != nil {
                        badge("已锁定", color: .orange)
                    }
                    if worktree.prunableReason != nil {
                        badge("可清理", color: .red)
                    }
                }
                Text(worktree.path)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let head = worktree.headHash {
                    Text(String(head.prefix(12)))
                        .font(.system(size: 9.5, design: .monospaced))
                        .foregroundStyle(.quaternary)
                }
            }

            Spacer()

            Button("在 Kite 中打开") {
                model.loadRoot(worktree.path)
            }
            .disabled(worktree.isCurrent || worktree.prunableReason != nil)

            Button(role: .destructive) {
                Task { await removeWorktree(worktree) }
            } label: {
                Image(systemName: "trash")
            }
            .help("删除 Worktree")
            .disabled(busy || worktree.isPrimary || worktree.isCurrent)
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 66)
        .background(
            Color(nsColor: .controlBackgroundColor).opacity(0.24),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.055), lineWidth: 0.5)
        }
    }

    private var submoduleContent: some View {
        VStack(spacing: 10) {
            HStack {
                Text("全部 Submodule")
                    .font(.system(size: 12, weight: .semibold))
                Spacer()
                Button("初始化") {
                    Task { await runSubmoduleAction(initialize: true) }
                }
                .disabled(busy || submodules.isEmpty)
                Button("更新全部") {
                    Task { await runSubmoduleAction(initialize: false) }
                }
                .disabled(busy || submodules.isEmpty)
            }

            if submodules.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "shippingbox")
                        .font(.system(size: 28, weight: .light))
                        .foregroundStyle(.tertiary)
                    Text("没有 Submodule")
                        .font(.system(size: 12.5, weight: .semibold))
                    Text("仓库根目录没有 .gitmodules 配置。")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.tertiary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 8) {
                        ForEach(submodules, id: \.path) { submodule in
                            submoduleRow(submodule)
                        }
                    }
                    .padding(1)
                }
            }
        }
    }

    private func submoduleRow(_ submodule: GitSubmoduleDto) -> some View {
        HStack(spacing: 12) {
            Image(systemName: submodule.state == .initialized ? "shippingbox.fill" : "shippingbox")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(submoduleColor(submodule.state))
                .frame(width: 22)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 7) {
                    Text(submodule.name)
                        .font(.system(size: 12, weight: .semibold))
                    badge(submodule.stateLabel, color: submoduleColor(submodule.state))
                    if submodule.hasUncommittedChanges {
                        badge("有未提交改动", color: .orange)
                    }
                }
                Text(submodule.relativePath)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
                HStack(spacing: 8) {
                    Text(String(submodule.declaredCommit.prefix(12)))
                    if let describe = submodule.describe {
                        Text(describe)
                    }
                    if let branch = submodule.branch {
                        Text(branch)
                    }
                }
                .font(.system(size: 9.5, design: .monospaced))
                .foregroundStyle(.quaternary)
                .lineLimit(1)
            }

            Spacer()

            Button("打开") {
                model.loadRoot(submodule.path)
            }
            .disabled(!FileManager.default.fileExists(atPath: submodule.path))
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 62)
        .background(
            Color(nsColor: .controlBackgroundColor).opacity(0.24),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.055), lineWidth: 0.5)
        }
    }

    private func badge(_ text: String, color: Color) -> some View {
        Text(text)
            .font(.system(size: 9, weight: .medium))
            .foregroundStyle(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.12), in: Capsule())
    }

    private func messageBar(_ text: String, systemImage: String, color: Color) -> some View {
        Label(text, systemImage: systemImage)
            .font(.system(size: 11))
            .foregroundStyle(color)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(color.opacity(0.09), in: RoundedRectangle(cornerRadius: 9))
    }

    private func refreshAll(clearMessages: Bool = true) async {
        loading = true
        if clearMessages {
            errorMessage = nil
            notice = nil
        }
        async let worktreeResult = model.gitWorktrees(path: repositoryPath)
        async let submoduleResult = model.gitSubmodules(path: repositoryPath)
        let (worktreeList, submoduleList) = await (worktreeResult, submoduleResult)
        worktrees = worktreeList?.worktrees ?? []
        submodules = submoduleList?.submodules ?? []
        loading = false
    }

    private func createWorktree() async {
        busy = true
        errorMessage = nil
        notice = nil
        do {
            let created = try await model.gitAddWorktree(
                repositoryPath: repositoryPath,
                destination: (destination as NSString).expandingTildeInPath,
                branch: branch.trimmingCharacters(in: .whitespacesAndNewlines),
                createBranch: createBranch,
                startPoint: createBranch ? startPoint : nil
            )
            notice = "已创建 Worktree：\(created.path)"
            showingCreate = false
            destination = ""
            branch = ""
            await refreshAll(clearMessages: false)
        } catch {
            errorMessage = error.localizedDescription
        }
        busy = false
    }

    private func removeWorktree(_ worktree: GitWorktreeDto) async {
        busy = true
        errorMessage = nil
        notice = nil
        do {
            let check = try await model.gitWorktreeRemovalCheck(path: worktree.path)
            let force = confirmRemoval(worktree: worktree, check: check)
            guard force != nil else {
                busy = false
                return
            }
            try await model.gitRemoveWorktree(path: worktree.path, force: force == true)
            notice = "已删除 Worktree：\(worktree.path)"
            await refreshAll(clearMessages: false)
        } catch {
            errorMessage = error.localizedDescription
        }
        busy = false
    }

    /// Returns nil to cancel, false for a clean removal, and true after the
    /// user explicitly confirms that local changes will be discarded.
    private func confirmRemoval(
        worktree: GitWorktreeDto,
        check: GitWorktreeRemovalCheckDto
    ) -> Bool? {
        let alert = NSAlert()
        alert.messageText = "删除 Worktree？"
        alert.informativeText = "\(worktree.path)\n\n\(check.summary)"
        alert.alertStyle = .warning
        alert.addButton(withTitle: check.hasUncommittedChanges ? "放弃改动并删除" : "删除")
        alert.addButton(withTitle: "取消")
        return alert.runModal() == .alertFirstButtonReturn
            ? check.hasUncommittedChanges
            : nil
    }

    private func runSubmoduleAction(initialize: Bool) async {
        busy = true
        errorMessage = nil
        notice = nil
        do {
            notice = try await (initialize
                ? model.gitInitSubmodules(path: repositoryPath)
                : model.gitUpdateSubmodules(path: repositoryPath))
            await refreshAll(clearMessages: false)
        } catch {
            errorMessage = error.localizedDescription
        }
        busy = false
    }

    private func submoduleColor(_ state: GitSubmoduleStateDto) -> Color {
        switch state {
        case .initialized: return .green
        case .uninitialized: return .secondary
        case .outOfDate: return .orange
        case .conflicted: return .red
        }
    }
}
