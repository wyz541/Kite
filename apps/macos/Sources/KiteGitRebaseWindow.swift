import AppKit
import SwiftUI

private enum KiteRebaseAction: String, CaseIterable, Identifiable {
    case pick
    case reword
    case edit
    case squash
    case fixup
    case drop

    var id: String { rawValue }

    var title: String {
        switch self {
        case .pick: "Pick"
        case .reword: "Reword"
        case .edit: "Edit"
        case .squash: "Squash"
        case .fixup: "Fixup"
        case .drop: "Drop"
        }
    }

    var dto: GitRebaseActionDto {
        switch self {
        case .pick: .pick
        case .reword: .reword
        case .edit: .edit
        case .squash: .squash
        case .fixup: .fixup
        case .drop: .drop
        }
    }

    init(dto: GitRebaseActionDto) {
        switch dto {
        case .pick: self = .pick
        case .reword: self = .reword
        case .edit: self = .edit
        case .squash: self = .squash
        case .fixup: self = .fixup
        case .drop: self = .drop
        }
    }
}

private struct KiteRebaseTodoRow: Identifiable {
    let id: String
    let commit: GitRebaseCommitDto
    var action: KiteRebaseAction
    var message: String
}

private enum RebaseControl {
    case continueRebase
    case skip
    case abort
}

@MainActor
private final class KiteGitRebaseModel: ObservableObject {
    @Published private(set) var plan: GitRebasePlanDto?
    @Published var rows: [KiteRebaseTodoRow] = []
    @Published var upstream = ""
    @Published var onto = ""
    @Published var updateRefs = false
    @Published var autosquash = false
    @Published var preserveMerges = false
    @Published var allowRewritePushed = false
    @Published private(set) var state: GitRebaseStateDto?
    @Published private(set) var loading = false
    @Published private(set) var busy = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var notice: String?

    let path: String
    private let core = KiteCore()

    init(path: String) {
        self.path = path
    }

    var canStart: Bool {
        !busy && !rows.isEmpty && !(plan?.protected ?? false)
    }

    var canContinueOrSkip: Bool {
        guard !busy, let status = state?.status else { return false }
        return status == .stopped || status == .inProgress
    }

    var canAbort: Bool {
        guard !busy, let status = state?.status else { return false }
        return status == .stopped || status == .inProgress
    }

    func load() async {
        loading = true
        errorMessage = nil
        let path = path
        let core = self.core
        do {
            let (plan, status) = try await Task.detached(priority: .userInitiated) {
                let plan = try core.gitRebasePlan(path: path)
                let status = try? core.gitRebaseStatus(path: path)
                return (plan, status)
            }.value
            self.plan = plan
            if let status {
                self.state = status
            }
            upstream = plan.upstream
            onto = plan.onto
            rows = plan.commits.map {
                KiteRebaseTodoRow(
                    id: $0.hash,
                    commit: $0,
                    action: .pick,
                    message: $0.message
                )
            }
        } catch {
            errorMessage = error.localizedDescription
        }
        loading = false
    }

    func start() async {
        guard canStart else { return }
        busy = true
        errorMessage = nil
        notice = "正在改写历史…"
        let path = path
        let core = self.core
        let spec = GitRebaseSpecDto(
            upstream: upstream,
            onto: onto,
            options: GitRebaseOptionsDto(
                updateRefs: updateRefs,
                autosquash: autosquash,
                preserveMerges: preserveMerges,
                allowRewritePushed: allowRewritePushed
            ),
            todo: rows.map { row in
                let message: String?
                switch row.action {
                case .reword:
                    message = row.message
                case .squash:
                    message = row.message == row.commit.message ? nil : row.message
                default:
                    message = nil
                }
                return GitRebaseTodoDto(
                    commit: row.commit.hash,
                    action: row.action.dto,
                    message: message
                )
            }
        )
        do {
            let state = try await Task.detached(priority: .userInitiated) {
                try core.gitRebaseStart(path: path, spec: spec)
            }.value
            self.state = state
            notice = state.message
        } catch {
            notice = nil
            errorMessage = error.localizedDescription
        }
        busy = false
        if state?.status == .completed || state?.status == .stopped {
            await load()
        }
    }

    func fixupWorktree(commit: String) async {
        guard !busy else { return }
        busy = true
        errorMessage = nil
        notice = "正在把当前改动并入历史…"
        let path = path
        let core = self.core
        let allowRewritePushed = allowRewritePushed
        do {
            let state = try await Task.detached(priority: .userInitiated) {
                try core.gitRebaseFixupWorktree(
                    path: path,
                    commit: commit,
                    allowRewritePushed: allowRewritePushed
                )
            }.value
            self.state = state
            notice = state.message
        } catch {
            notice = nil
            errorMessage = error.localizedDescription
        }
        busy = false
        await load()
    }

    func continueRebase() async {
        guard canContinueOrSkip else { return }
        await control(.continueRebase, pending: "正在继续 Rebase…")
    }

    func skipRebase() async {
        guard canContinueOrSkip else { return }
        await control(.skip, pending: "正在跳过当前提交…")
    }

    func abortRebase() async {
        guard canAbort else { return }
        await control(.abort, pending: "正在中止 Rebase…")
    }

    private func control(_ operation: RebaseControl, pending: String) async {
        busy = true
        errorMessage = nil
        notice = pending
        let path = path
        let core = self.core
        do {
            let state = try await Task.detached(priority: .userInitiated) {
                switch operation {
                case .continueRebase:
                    try core.gitRebaseContinue(path: path)
                case .skip:
                    try core.gitRebaseSkip(path: path)
                case .abort:
                    try core.gitRebaseAbort(path: path)
                }
            }.value
            self.state = state
            notice = state.message
        } catch {
            notice = nil
            errorMessage = error.localizedDescription
        }
        busy = false
        await load()
    }

    func move(from offsets: IndexSet, to destination: Int) {
        rows.move(fromOffsets: offsets, toOffset: destination)
    }

    func actionBinding(for id: String) -> Binding<KiteRebaseAction> {
        Binding(
            get: { self.rows.first(where: { $0.id == id })?.action ?? .pick },
            set: { value in
                guard let index = self.rows.firstIndex(where: { $0.id == id }) else { return }
                self.rows[index].action = value
            }
        )
    }

    func messageBinding(for id: String) -> Binding<String> {
        Binding(
            get: { self.rows.first(where: { $0.id == id })?.message ?? "" },
            set: { value in
                guard let index = self.rows.firstIndex(where: { $0.id == id }) else { return }
                self.rows[index].message = value
            }
        )
    }
}

@MainActor
final class KiteGitRebaseWindowController: NSWindowController, NSWindowDelegate {
    private static var openWindows: [UUID: KiteGitRebaseWindowController] = [:]
    private let id = UUID()

    private init(path: String, model: KiteModel) {
        let state = KiteGitRebaseModel(path: path)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "交互式 Rebase"
        window.contentMinSize = NSSize(width: 760, height: 560)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        window.contentViewController = NSHostingController(
            rootView: KiteGitRebaseView(state: state, model: model)
        )
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show(path: String, model: KiteModel) {
        let controller = KiteGitRebaseWindowController(path: path, model: model)
        openWindows[controller.id] = controller
        controller.window?.center()
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
    }

    func windowWillClose(_ notification: Notification) {
        Self.openWindows.removeValue(forKey: id)
    }
}

private struct KiteGitRebaseView: View {
    @ObservedObject var state: KiteGitRebaseModel
    @ObservedObject var model: KiteModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if let error = state.errorMessage {
                messageBanner(error, color: .red, systemImage: "exclamationmark.triangle")
            } else if let notice = state.notice {
                messageBanner(notice, color: .gray, systemImage: "info.circle")
            }
            if let rebaseState = state.state {
                messageBanner(
                    rebaseStatusText(rebaseState),
                    color: rebaseStatusColor(rebaseState.status),
                    systemImage: rebaseStatusIcon(rebaseState.status)
                )
            }
            if let plan = state.plan {
                warnings(plan)
                fields
                options
                todoList
                actions
            } else if state.loading {
                ProgressView("正在读取提交…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Text("没有可编辑的提交。")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding(18)
        .frame(minWidth: 760, minHeight: 560)
        .background(KiteGlassBackdrop())
        .task { await state.load() }
        .onExitCommand { NSApp.keyWindow?.performClose(nil) }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.triangle.branch")
                .font(.system(size: 14, weight: .semibold))
            Text("交互式 Rebase")
                .font(.headline)
            Text(state.path)
                .font(.system(size: 10.5, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
            if state.busy {
                ProgressView().controlSize(.small)
            }
        }
    }

    private func warnings(_ plan: GitRebasePlanDto) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let operation = plan.operation, operation != "Rebase" {
                Label(
                    "\(operation) 正在进行，请先完成或中止该操作。",
                    systemImage: "exclamationmark.triangle"
                )
            }
            if plan.protected {
                Label(
                    "\(plan.branch ?? "当前分支") 是受保护分支，Kite 不允许在此分支改写历史。",
                    systemImage: "lock"
                )
            }
            if !plan.pushedCommits.isEmpty {
                Label(
                    "有 \(plan.pushedCommits.count) 个提交已推送到远程。改写会重写远端历史，需要确认。",
                    systemImage: "exclamationmark.triangle"
                )
            }
            if plan.hasMerges {
                Label(
                    "历史中包含合并提交。请开启“保留合并提交”，并且只使用 Pick。",
                    systemImage: "arrow.triangle.merge"
                )
            }
        }
        .font(.system(size: 10.5, weight: .medium))
        .foregroundStyle(.orange)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.09), in: RoundedRectangle(cornerRadius: 9))
    }

    private var fields: some View {
        HStack(alignment: .bottom, spacing: 10) {
            field("上游", text: $state.upstream)
            field("onto", text: $state.onto)
        }
    }

    private func field(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
            TextField(text: text) {
                Text(title)
            }
            .textFieldStyle(.roundedBorder)
            .font(.system(size: 11.5, design: .monospaced))
        }
        .frame(maxWidth: .infinity)
    }

    private var options: some View {
        HStack(spacing: 16) {
            Toggle("更新指向的引用", isOn: $state.updateRefs)
            Toggle("自动 squash/fixup", isOn: $state.autosquash)
            Toggle("保留合并提交", isOn: $state.preserveMerges)
            Toggle("允许改写已推送提交", isOn: $state.allowRewritePushed)
            Spacer()
        }
        .toggleStyle(.checkbox)
        .controlSize(.small)
        .font(.system(size: 10.5))
    }

    private var todoList: some View {
        List {
            ForEach(state.rows) { row in
                rowView(row)
            }
            .onMove { offsets, destination in
                state.move(from: offsets, to: destination)
            }
        }
        .listStyle(.inset)
        .frame(minHeight: 250, maxHeight: .infinity)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.18))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    private func rowView(_ row: KiteRebaseTodoRow) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                Picker(
                    "动作",
                    selection: state.actionBinding(for: row.id)
                ) {
                    ForEach(KiteRebaseAction.allCases) { action in
                        Text(action.title).tag(action)
                    }
                }
                .labelsHidden()
                .frame(width: 96)

                Text(row.commit.shortHash)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .frame(width: 64, alignment: .leading)

                Text(row.commit.subject)
                    .font(.system(size: 11.5, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)

                if row.commit.pushed {
                    Text("已推送")
                        .font(.system(size: 9.5, weight: .semibold))
                        .foregroundStyle(.red)
                }
                Spacer(minLength: 0)
                Button("并入当前改动") {
                    Task {
                        await state.fixupWorktree(commit: row.commit.hash)
                        model.refresh()
                    }
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .font(.system(size: 9.5))
                .disabled(state.busy)
            }

            if row.action == .reword || row.action == .squash {
                VStack(alignment: .leading, spacing: 3) {
                    TextEditor(text: state.messageBinding(for: row.id))
                        .font(.system(size: 11.5, design: .monospaced))
                        .frame(minHeight: 54, maxHeight: 86)
                        .padding(4)
                        .background(
                            Color(nsColor: .textBackgroundColor).opacity(0.72),
                            in: RoundedRectangle(cornerRadius: 6)
                        )
                    if row.action == .squash {
                        Text("保持原文时自动合并前一条提交信息。")
                            .font(.system(size: 9.5))
                            .foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .padding(.vertical, 3)
    }

    private var actions: some View {
        HStack(spacing: 8) {
            Button("开始 Rebase") {
                Task {
                    await state.start()
                    model.refresh()
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(!state.canStart)

            Button("继续") {
                Task {
                    await state.continueRebase()
                    model.refresh()
                }
            }
            .disabled(!state.canContinueOrSkip)

            Button("跳过") {
                Task {
                    await state.skipRebase()
                    model.refresh()
                }
            }
            .disabled(!state.canContinueOrSkip)

            Button("中止", role: .destructive) {
                Task {
                    await state.abortRebase()
                    model.refresh()
                }
            }
            .disabled(!state.canAbort)

            Spacer()

            Text("提交按从旧到新排列；拖动行调整顺序。")
                .font(.system(size: 9.5))
                .foregroundStyle(.tertiary)
        }
    }

    private func rebaseStatusColor(_ status: GitRebaseStatusDto) -> Color {
        switch status {
        case .completed: .green
        case .stopped, .inProgress: .orange
        case .aborted: .gray
        }
    }

    private func rebaseStatusIcon(_ status: GitRebaseStatusDto) -> String {
        switch status {
        case .completed: "checkmark.circle"
        case .stopped: "pause.circle"
        case .inProgress: "arrow.triangle.2.circlepath"
        case .aborted: "xmark.circle"
        }
    }

    private func rebaseStatusText(_ state: GitRebaseStateDto) -> String {
        var text = state.message
        if state.remaining > 0 {
            text += " · 剩余 \(state.remaining) 个提交"
        }
        return text
    }

    private func messageBanner(_ text: String, color: Color, systemImage: String) -> some View {
        Label(text, systemImage: systemImage)
            .font(.system(size: 10.5))
            .foregroundStyle(color)
            .padding(9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(color.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
    }
}
