import AppKit
import SwiftUI

enum KiteGitToolsTab: String, CaseIterable, Identifiable, Hashable {
    case stashes = "Stash"
    case log = "历史"
    case actions = "操作"

    var id: String { rawValue }
}

private enum GitToolsConfirmation: Identifiable {
    case drop(GitStashEntryDto)
    case clear
    case hardReset(String)

    var id: String {
        switch self {
        case .drop(let stash): return "drop-\(stash.selector)"
        case .clear: return "clear"
        case .hardReset: return "hard-reset"
        }
    }
}

@MainActor
final class KiteGitToolsWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: KiteGitToolsWindowController] = [:]
    private let repositoryPath: String

    static func show(
        model: KiteModel,
        path: String,
        selectedPaths: [String]? = nil
    ) {
        let normalized = URL(fileURLWithPath: path).standardizedFileURL.path
        if let existing = controllers[normalized] {
            existing.showWindow(nil)
            existing.window?.makeKeyAndOrderFront(nil)
            return
        }

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Kite Git"
        window.minSize = NSSize(width: 780, height: 560)
        window.center()
        window.isReleasedWhenClosed = false

        let selectedPaths = selectedPaths ?? model.selectedColumnEntries.map(\.path)
        let root = KiteGitToolsView(
            model: model,
            path: normalized,
            selectedPaths: selectedPaths
        )
        window.contentViewController = NSHostingController(rootView: root)

        let controller = KiteGitToolsWindowController(window: window, path: normalized)
        window.delegate = controller
        controllers[normalized] = controller
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
    }

    init(window: NSWindow, path: String) {
        repositoryPath = path
        super.init(window: window)
    }

    required init?(coder: NSCoder) {
        fatalError("KiteGitToolsWindowController is created in code")
    }

    func windowWillClose(_ notification: Notification) {
        KiteGitToolsWindowController.controllers.removeValue(forKey: repositoryPath)
    }
}

struct KiteGitToolsView: View {
    @ObservedObject var model: KiteModel
    let path: String
    let selectedPaths: [String]

    @State private var tab: KiteGitToolsTab = .stashes
    @State private var status: GitRepositoryStatusDto?
    @State private var stashes: [GitStashEntryDto] = []
    @State private var selectedStashIndex: UInt32?
    @State private var stashDiff: [GitDiffLineDto]?
    @State private var logText = ""
    @State private var loading = true
    @State private var busy = false
    @State private var notice: String?
    @State private var errorMessage: String?

    @State private var stashMessage = ""
    @State private var includeUntracked = false
    @State private var keepIndex = false
    @State private var stashBranch = ""
    @State private var resetModeIndex = 1
    @State private var resetTarget = "HEAD~1"
    @State private var commitSelectors = ""
    @State private var confirmation: GitToolsConfirmation?

    init(
        model: KiteModel,
        path: String,
        selectedPaths: [String],
        initialTab: KiteGitToolsTab = .stashes
    ) {
        self.model = model
        self.path = path
        self.selectedPaths = selectedPaths
        _tab = State(initialValue: initialTab)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            tabPicker
            Divider()
            operationBanner
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    content
                }
                .padding(20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(minWidth: 780, minHeight: 560)
        .background(Color(nsColor: .windowBackgroundColor))
        .task(id: path) {
            await reload()
        }
        .alert(item: $confirmation) { confirmation in
            switch confirmation {
            case .drop(let stash):
                return Alert(
                    title: Text("永久删除 \(stash.selector)？"),
                    message: Text("Stash 中的改动将被丢弃。此操作不可撤销。"),
                    primaryButton: .destructive(Text("删除 Stash")) {
                        perform("正在删除 Stash…") {
                            try await model.gitStashDrop(
                                path: path,
                                index: selectedStashIndex ?? 0,
                                confirmed: true
                            )
                        }
                    },
                    secondaryButton: .cancel(Text("取消"))
                )
            case .clear:
                return Alert(
                    title: Text("清空全部 Git Stash？"),
                    message: Text("所有 Stash 都将被永久删除。此操作不可撤销。"),
                    primaryButton: .destructive(Text("清空全部 Stash")) {
                        perform("正在清空 Stash…") {
                            try await model.gitStashClear(path: path, confirmed: true)
                        }
                    },
                    secondaryButton: .cancel(Text("取消"))
                )
            case .hardReset(let target):
                return Alert(
                    title: Text("执行 Git reset --hard？"),
                    message: Text(
                        "目标：\(target)\n\n未提交的已跟踪改动将被永久丢弃。未跟踪文件会保留。"
                    ),
                    primaryButton: .destructive(Text("执行 reset --hard")) {
                        runReset(target: target, confirmed: true)
                    },
                    secondaryButton: .cancel(Text("取消"))
                )
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 12) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 34, height: 34)
                    .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 9))

                VStack(alignment: .leading, spacing: 2) {
                    Text(status?.branch ?? "分离 HEAD")
                        .font(.system(size: 17, weight: .semibold))
                        .lineLimit(1)
                    Text(status?.repositoryRoot ?? path)
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Spacer()

                if loading || busy {
                    ProgressView()
                        .controlSize(.small)
                }

                Button {
                    Task { await reload() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .disabled(loading || busy)
                .help("刷新 Git 状态")
            }

            if let notice {
                Text(notice)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.system(size: 11))
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 15)
    }

    private var tabPicker: some View {
        Picker("Git 区域", selection: $tab) {
            ForEach(KiteGitToolsTab.allCases) { item in
                Text(item == .stashes ? "Stash (\(stashes.count))" : item.rawValue)
                    .tag(item)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
    }

    @ViewBuilder
    private var operationBanner: some View {
        if let status, status.operation != nil || status.hasConflicts {
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text(status.operation ?? "存在 Git 冲突")
                    .font(.system(size: 12, weight: .medium))
                Spacer()
                Button("继续") { continueOperation() }
                    .buttonStyle(.borderedProminent)
                if status.operation?.contains("拣选") == true {
                    Button("跳过") { skipOperation() }
                        .buttonStyle(.bordered)
                }
                Button("中止") { abortOperation() }
                    .buttonStyle(.bordered)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(Color.orange.opacity(0.10))
            .overlay(alignment: .bottom) { Divider() }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch tab {
        case .stashes:
            stashContent
        case .log:
            logContent
        case .actions:
            actionsContent
        }
    }

    private var stashContent: some View {
        VStack(alignment: .leading, spacing: 16) {
            card("保存 Stash") {
                VStack(alignment: .leading, spacing: 11) {
                    TextField("Stash 说明（可选）", text: $stashMessage)
                        .textFieldStyle(.roundedBorder)
                    HStack(spacing: 18) {
                        Toggle("包含未跟踪文件", isOn: $includeUntracked)
                        Toggle("保留暂存区", isOn: $keepIndex)
                    }
                    .toggleStyle(.checkbox)
                    if !selectedPaths.isEmpty {
                        Text("只保存 \(selectedPaths.count) 个选中项目")
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                    Button("保存 Stash") { saveStash() }
                        .buttonStyle(.borderedProminent)
                        .disabled(busy)
                }
            }

            card("Stash 列表") {
                VStack(alignment: .leading, spacing: 11) {
                    if stashes.isEmpty {
                        Text("还没有 Stash。")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 10)
                    } else {
                        VStack(spacing: 1) {
                            ForEach(Array(stashes.enumerated()), id: \.element.selector) { index, stash in
                                stashRow(stash, index: UInt32(index))
                            }
                        }
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    }

                    HStack(spacing: 8) {
                        Button("应用") { applyStash() }
                            .disabled(selectedStashIndex == nil || busy)
                        Button("弹出") { popStash() }
                            .disabled(selectedStashIndex == nil || busy)
                        Button("查看差异") { showStashDiff() }
                            .disabled(selectedStashIndex == nil || busy)
                        Button("Diff 查看器") { openStashDiffViewer() }
                            .disabled(selectedStashIndex == nil || busy)
                        Spacer()
                        Button("删除") { requestDrop() }
                            .foregroundStyle(.red)
                            .disabled(selectedStashIndex == nil || busy)
                        Button("全部清空") { confirmation = .clear }
                            .foregroundStyle(.red)
                            .disabled(stashes.isEmpty || busy)
                    }

                    HStack(spacing: 8) {
                        TextField("从当前 Stash 创建新分支", text: $stashBranch)
                            .textFieldStyle(.roundedBorder)
                        Button("新建分支") { createBranchFromStash() }
                            .disabled(
                                selectedStashIndex == nil
                                    || stashBranch.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                    || busy
                            )
                    }
                }
            }

            if let stashDiff {
                card(selectedStashLabel) {
                    if stashDiff.isEmpty {
                        Text("此 Stash 没有可显示的文本差异。")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                    } else {
                        ScrollView([.vertical, .horizontal]) {
                            LazyVStack(alignment: .leading, spacing: 0) {
                                ForEach(Array(stashDiff.enumerated()), id: \.offset) { _, line in
                                    diffLine(line)
                                }
                            }
                            .padding(8)
                        }
                        .frame(maxHeight: 260)
                        .background(
                            Color.primary.opacity(0.035),
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                    }
                }
            }
        }
    }

    private func stashRow(_ stash: GitStashEntryDto, index: UInt32) -> some View {
        let selected = selectedStashIndex == index
        return Button {
            selectedStashIndex = index
            stashDiff = nil
        } label: {
            HStack(spacing: 12) {
                Text(stash.selector)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .frame(width: 78, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text(stash.message)
                        .font(.system(size: 12, weight: selected ? .semibold : .regular))
                        .lineLimit(1)
                    Text("\(stash.shortHash) · \(stash.date)")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? Color.accentColor.opacity(0.12) : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var logContent: some View {
        card("Git 历史") {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Spacer()
                    Button("最新提交 Diff") {
                        KiteDiffViewerWindowController.shared.show(
                            path: status?.repositoryRoot ?? path,
                            appModel: model,
                            preset: .custom,
                            oldRevision: "HEAD~1",
                            newRevision: "HEAD"
                        )
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                ScrollView([.vertical, .horizontal]) {
                    Text(logText.isEmpty ? "正在读取 Git 历史…" : logText)
                        .font(.system(size: 10.5, design: .monospaced))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: true, vertical: false)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                }
                .frame(maxHeight: 560)
                .background(
                    Color.primary.opacity(0.035),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
            }
        }
    }

    private var actionsContent: some View {
        VStack(alignment: .leading, spacing: 16) {
            card("Reset") {
                VStack(alignment: .leading, spacing: 11) {
                    Picker("Reset 模式", selection: $resetModeIndex) {
                        Text("Soft").tag(0)
                        Text("Mixed").tag(1)
                        Text("Hard").tag(2)
                        Text("Keep").tag(3)
                    }
                    .pickerStyle(.segmented)

                    Text(resetModeDetail)
                        .font(.system(size: 11))
                        .foregroundStyle(isHardReset ? Color.red : Color.secondary)

                    TextField("HEAD~1、分支或提交", text: $resetTarget)
                        .textFieldStyle(.roundedBorder)

                    Button("执行 reset --\(resetModeName)") { requestReset() }
                        .buttonStyle(.borderedProminent)
                        .tint(isHardReset ? .red : .accentColor)
                        .disabled(
                            resetTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                || busy
                        )
                }
            }

            card("Revert / Cherry-pick") {
                VStack(alignment: .leading, spacing: 11) {
                    Text("输入一个或多个提交，用逗号或换行分隔。")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                    TextField("提交哈希、分支或标签", text: $commitSelectors, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(2...5)
                    HStack(spacing: 8) {
                        Button("Revert") { revertCommits() }
                            .disabled(parsedCommits.isEmpty || busy)
                        Button("Cherry-pick") { cherryPickCommits() }
                            .buttonStyle(.borderedProminent)
                            .disabled(parsedCommits.isEmpty || busy)
                    }
                }
            }
        }
    }

    private var selectedStashLabel: String {
        guard let selectedStashIndex, Int(selectedStashIndex) < stashes.count else {
            return "Stash 差异"
        }
        return "\(stashes[Int(selectedStashIndex)].selector) 差异"
    }

    private func card<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.system(size: 12, weight: .semibold))
            content()
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            Color(nsColor: .controlBackgroundColor).opacity(0.34),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.5)
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
            background = Color.green.opacity(0.10)
        case .removed:
            prefix = "-"
            color = .red
            background = Color.red.opacity(0.10)
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

    private var resetModeName: String {
        switch resetModeIndex {
        case 0: return "soft"
        case 2: return "hard"
        case 3: return "keep"
        default: return "mixed"
        }
    }

    private var isHardReset: Bool {
        resetModeIndex == 2
    }

    private var resetModeDetail: String {
        switch resetModeIndex {
        case 0: return "保留所有改动，并放回暂存区。"
        case 2: return "丢弃未提交的已跟踪改动。"
        case 3: return "保留不冲突的工作区改动。"
        default: return "保留工作区改动，清空暂存区。"
        }
    }

    private var resetMode: GitResetModeDto {
        switch resetModeIndex {
        case 0: return .soft
        case 2: return .hard
        case 3: return .keep
        default: return .mixed
        }
    }

    private var parsedCommits: [String] {
        commitSelectors
            .components(separatedBy: CharacterSet(charactersIn: ",\n"))
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    private func saveStash() {
        let message = stashMessage.trimmingCharacters(in: .whitespacesAndNewlines)
        perform("正在创建 Stash…") {
            try await model.gitStashSave(
                path: path,
                message: message.isEmpty ? nil : message,
                includeUntracked: includeUntracked,
                keepIndex: keepIndex,
                paths: selectedPaths
            )
        }
        stashMessage = ""
    }

    private func applyStash() {
        guard let index = selectedStashIndex else { return }
        perform("正在应用 Stash…") {
            let outcome = try await model.gitStashApply(path: path, index: index)
            return gitOutcomeMessage(outcome)
        }
    }

    private func popStash() {
        guard let index = selectedStashIndex else { return }
        perform("正在弹出 Stash…") {
            let outcome = try await model.gitStashPop(path: path, index: index)
            return gitOutcomeMessage(outcome)
        }
    }

    private func showStashDiff() {
        guard let index = selectedStashIndex else { return }
        busy = true
        errorMessage = nil
        Task { @MainActor in
            do {
                stashDiff = try await model.gitStashDiff(path: path, index: index)
                notice = nil
            } catch {
                errorMessage = error.localizedDescription
            }
            busy = false
        }
    }

    private func openStashDiffViewer() {
        guard let index = selectedStashIndex, Int(index) < stashes.count else { return }
        let selector = stashes[Int(index)].selector
        KiteDiffViewerWindowController.shared.show(
            path: status?.repositoryRoot ?? path,
            appModel: model,
            preset: .custom,
            oldRevision: "\(selector)^1",
            newRevision: selector
        )
    }

    private func requestDrop() {
        guard let index = selectedStashIndex, Int(index) < stashes.count else { return }
        confirmation = .drop(stashes[Int(index)])
    }

    private func createBranchFromStash() {
        guard let index = selectedStashIndex else { return }
        let branch = stashBranch.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !branch.isEmpty else { return }
        perform("正在从 Stash 创建分支…") {
            let outcome = try await model.gitStashBranch(
                path: path,
                index: index,
                branch: branch
            )
            return gitOutcomeMessage(outcome)
        }
        stashBranch = ""
    }

    private func requestReset() {
        let target = resetTarget.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty else { return }
        if isHardReset {
            confirmation = .hardReset(target)
        } else {
            runReset(target: target, confirmed: false)
        }
    }

    private func runReset(target: String, confirmed: Bool) {
        let mode = resetMode
        perform("正在执行 Git reset…") {
            try await model.gitReset(
                path: path,
                mode: mode,
                target: target,
                confirmed: confirmed
            )
        }
    }

    private func revertCommits() {
        let commits = parsedCommits
        guard !commits.isEmpty else { return }
        perform("正在还原提交…") {
            let outcome = try await model.gitRevert(path: path, commits: commits)
            return gitOutcomeMessage(outcome)
        }
    }

    private func cherryPickCommits() {
        let commits = parsedCommits
        guard !commits.isEmpty else { return }
        perform("正在拣选提交…") {
            let outcome = try await model.gitCherryPick(path: path, commits: commits)
            return gitOutcomeMessage(outcome)
        }
    }

    private func continueOperation() {
        let cherryPicking = status?.operation?.contains("拣选") == true
        perform("正在继续 Git 操作…") {
            let outcome: GitCommandOutcomeDto
            if cherryPicking {
                outcome = try await model.gitCherryPickContinue(path: path)
            } else {
                outcome = try await model.gitRevertContinue(path: path)
            }
            return gitOutcomeMessage(outcome)
        }
    }

    private func skipOperation() {
        perform("正在跳过当前提交…") {
            let outcome = try await model.gitCherryPickSkip(path: path)
            return gitOutcomeMessage(outcome)
        }
    }

    private func abortOperation() {
        let cherryPicking = status?.operation?.contains("拣选") == true
        perform("正在中止 Git 操作…") {
            if cherryPicking {
                return try await model.gitCherryPickAbort(path: path)
            }
            return try await model.gitRevertAbort(path: path)
        }
    }

    private func perform(
        _ pending: String,
        operation: @escaping @MainActor () async throws -> String
    ) {
        guard !busy else { return }
        busy = true
        notice = pending
        errorMessage = nil
        Task { @MainActor in
            do {
                notice = try await operation()
                await reload()
            } catch {
                notice = nil
                errorMessage = error.localizedDescription
            }
            busy = false
        }
    }

    @MainActor
    private func reload() async {
        loading = true
        errorMessage = nil
        do {
            status = try await model.gitOperationStatus(path: path)
            stashes = try await model.gitStashList(path: path)
            logText = try await model.gitLogText(path: path)
            if selectedStashIndex == nil || Int(selectedStashIndex ?? 0) >= stashes.count {
                self.selectedStashIndex = stashes.isEmpty ? nil : 0
                stashDiff = nil
            }
        } catch {
            errorMessage = error.localizedDescription
        }
        loading = false
    }

}

private func gitOutcomeMessage(_ outcome: GitCommandOutcomeDto) -> String {
    if outcome.hasConflicts {
        return outcome.message + " 请在修复冲突后继续或中止。"
    }
    return outcome.message
}
