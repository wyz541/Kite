import AppKit
import SwiftUI

@MainActor
final class KiteGitMergeWindowController: NSWindowController, NSWindowDelegate {
    static let shared = KiteGitMergeWindowController()

    private weak var model: KiteModel?

    func show(model: KiteModel, path: String) {
        self.model = model
        let rootView = KiteGitMergeWindowView(model: model, path: path)
        if window == nil {
            let mergeWindow = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 1180, height: 760),
                styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                backing: .buffered,
                defer: false
            )
            mergeWindow.title = "Git 合并与冲突解决"
            mergeWindow.titlebarAppearsTransparent = true
            mergeWindow.minSize = NSSize(width: 980, height: 640)
            mergeWindow.isReleasedWhenClosed = false
            mergeWindow.contentViewController = NSHostingController(rootView: rootView)
            mergeWindow.delegate = self
            mergeWindow.center()
            mergeWindow.setFrameAutosaveName("KiteGitMergeWindow")
            window = mergeWindow
        } else {
            window?.contentViewController = NSHostingController(rootView: rootView)
        }
        if window?.isMiniaturized == true {
            window?.deminiaturize(nil)
        }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        model = nil
    }
}

private enum KiteMergeStrategyChoice: String, CaseIterable, Identifiable {
    case merge
    case noFastForward
    case fastForwardOnly
    case squash

    var id: String { rawValue }

    var title: String {
        switch self {
        case .merge: return "普通合并"
        case .noFastForward: return "保留合并提交"
        case .fastForwardOnly: return "仅快进"
        case .squash: return "压缩合并"
        }
    }

    var dto: GitMergeStrategyDto {
        switch self {
        case .merge: return .merge
        case .noFastForward: return .noFastForward
        case .fastForwardOnly: return .fastForwardOnly
        case .squash: return .squash
        }
    }
}

private struct KiteGitMergeWindowView: View {
    @ObservedObject var model: KiteModel
    let path: String

    @State private var branches: [GitBranchDto] = []
    @State private var target = ""
    @State private var strategy: KiteMergeStrategyChoice = .merge
    @State private var mergeMessage = ""
    @State private var conflictState: GitConflictStateDto?
    @State private var selectedEntryIndex: Int?
    @State private var sides: GitConflictSidesDto?
    @State private var mergeDocument: GitThreeWayMergeDto?
    @State private var choices: [UInt32: GitConflictChoiceSideDto] = [:]
    @State private var currentHunk = 0
    @State private var resolvedText = ""
    @State private var manualText = ""
    @State private var manualEditing = false
    @State private var busy = false
    @State private var statusMessage: String?
    @State private var errorMessage: String?
    @State private var confirmAbort = false

    private var selectedEntry: GitConflictEntryDto? {
        guard let index = selectedEntryIndex,
              let entries = conflictState?.entries,
              entries.indices.contains(index) else { return nil }
        return entries[index]
    }

    private var hasConflicts: Bool {
        !(conflictState?.entries.isEmpty ?? true)
    }

    private var choicesArray: [GitConflictChoiceDto] {
        choices
            .sorted { $0.key < $1.key }
            .map { GitConflictChoiceDto(index: $0.key, side: $0.value) }
    }

    private var allHunksChosen: Bool {
        guard let mergeDocument, !mergeDocument.conflicts.isEmpty else { return false }
        return mergeDocument.conflicts.allSatisfy { choices[$0.index] != nil }
    }

    private var canResolveSelectedText: Bool {
        guard let mergeDocument, !mergeDocument.binary else { return false }
        return manualEditing || allHunksChosen
    }

    var body: some View {
        ZStack {
            KiteGlassBackdrop()
            VStack(spacing: 0) {
                header
                Divider()
                mergeForm
                Divider()
                if busy && conflictState == nil {
                    ProgressView("正在读取 Git 状态…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    conflictWorkspace
                }
                Divider()
                footer
            }
        }
        .frame(minWidth: 980, minHeight: 640)
        .task { await reloadAll() }
        .alert("中止当前 Git 操作？", isPresented: $confirmAbort) {
            Button("取消", role: .cancel) {}
            Button("中止", role: .destructive) { Task { await abortOperation() } }
        } message: {
            Text("未提交的合并状态会被放弃。")
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "arrow.triangle.merge")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Color.orange)
            VStack(alignment: .leading, spacing: 2) {
                Text("Git 合并与冲突解决")
                    .font(.system(size: 16, weight: .semibold))
                Text(path)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            if let operation = conflictState?.operation {
                Text(operation)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Color.orange.opacity(0.12), in: Capsule())
            }
            Button {
                Task { await reloadAll() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .help("刷新")
            .disabled(busy)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 13)
    }

    private var mergeForm: some View {
        HStack(alignment: .bottom, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text("合并到当前分支").font(.system(size: 10, weight: .medium))
                    .foregroundStyle(.secondary)
                HStack(spacing: 6) {
                    TextField("分支、标签或提交", text: $target)
                        .textFieldStyle(.roundedBorder)
                        .frame(minWidth: 220)
                    Menu {
                        ForEach(branches, id: \.fullName) { branch in
                            Button(branchLabel(branch)) { target = branch.name }
                        }
                        if branches.isEmpty {
                            Text("没有其他分支")
                        }
                    } label: {
                        Image(systemName: "chevron.down.circle")
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .disabled(branches.isEmpty)
                }
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("策略").font(.system(size: 10, weight: .medium)).foregroundStyle(.secondary)
                Picker("策略", selection: $strategy) {
                    ForEach(KiteMergeStrategyChoice.allCases) { choice in
                        Text(choice.title).tag(choice)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(width: 150)
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("合并提交说明（可选）")
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(.secondary)
                TextField("使用 Git 默认说明", text: $mergeMessage)
                    .textFieldStyle(.roundedBorder)
                    .frame(minWidth: 210)
            }

            Button {
                Task { await startMerge() }
            } label: {
                Label("开始合并", systemImage: "arrow.triangle.merge")
            }
            .buttonStyle(.borderedProminent)
            .disabled(busy || target.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.35))
    }

    @ViewBuilder
    private var conflictWorkspace: some View {
        if hasConflicts, let entries = conflictState?.entries {
            HStack(spacing: 0) {
                conflictSidebar(entries)
                    .frame(width: 238)
                Divider()
                conflictDetail
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            VStack(spacing: 12) {
                Image(systemName: conflictState?.canContinue == true ? "checkmark.circle" : "arrow.triangle.branch")
                    .font(.system(size: 34, weight: .light))
                    .foregroundStyle(conflictState?.canContinue == true ? Color.green : Color.secondary)
                Text(conflictState?.canContinue == true ? "所有冲突已解决" : "没有待解决的冲突")
                    .font(.system(size: 16, weight: .semibold))
                if conflictState?.canContinue == true {
                    Text("可以继续完成当前操作，或中止并回到操作前状态。")
                        .foregroundStyle(.secondary)
                } else {
                    Text("选择上方目标分支后开始合并。")
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func conflictSidebar(_ entries: [GitConflictEntryDto]) -> some View {
        VStack(spacing: 0) {
            HStack {
                Text("冲突文件").font(.system(size: 11, weight: .semibold))
                Spacer()
                Text("\(entries.count)").font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            Divider()
            ScrollView {
                LazyVStack(spacing: 3) {
                    ForEach(Array(entries.enumerated()), id: \.offset) { index, entry in
                        Button {
                            Task { await selectConflict(index) }
                        } label: {
                            HStack(alignment: .top, spacing: 7) {
                                Image(systemName: entry.binary ? "doc.badge.gearshape" : "exclamationmark.triangle.fill")
                                    .font(.system(size: 10, weight: .semibold))
                                    .foregroundStyle(Color.red)
                                    .padding(.top, 1)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(entryFileName(entry))
                                        .font(.system(size: 11, weight: .medium))
                                        .lineLimit(2)
                                        .multilineTextAlignment(.leading)
                                    Text(conflictKindLabel(entry))
                                        .font(.system(size: 9.5))
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                                Spacer(minLength: 0)
                            }
                            .padding(.horizontal, 9)
                            .padding(.vertical, 7)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(
                                selectedEntryIndex == index
                                    ? Color.red.opacity(0.13)
                                    : Color.clear,
                                in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                            )
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(7)
            }
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.22))
    }

    @ViewBuilder
    private var conflictDetail: some View {
        if let entry = selectedEntry {
            VStack(spacing: 0) {
                conflictToolbar(entry)
                Divider()
                if entry.binary {
                    binaryConflictBody(entry)
                } else if isRenameConflict(entry) {
                    renameConflictBody(entry)
                } else if let mergeDocument {
                    textConflictBody(entry, mergeDocument)
                } else if let sides {
                    sideComparisonBody(sides)
                } else {
                    ProgressView("正在载入冲突内容…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        } else {
            Text("选择一个冲突文件").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func conflictToolbar(_ entry: GitConflictEntryDto) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(entry.path).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                Text("\(conflictKindLabel(entry)) · \(entry.binary ? "二进制文件" : "文本文件")")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button(sideActionTitle("采用自己", present: entry.oursPresent)) {
                Task { await chooseWholeSide(entry, side: .ours) }
            }
            .disabled(busy)
            Button(sideActionTitle("采用对方", present: entry.theirsPresent)) {
                Task { await chooseWholeSide(entry, side: .theirs) }
            }
            .disabled(busy)
            if isRenameConflict(entry) {
                Button("两边都保留") { Task { await chooseWholeSide(entry, side: .both) } }
                    .disabled(busy)
            }
            Button {
                Task { await openMergeTool(entry) }
            } label: {
                Label("外部工具", systemImage: "arrow.up.right.square")
            }
            .disabled(busy)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private func binaryConflictBody(_ entry: GitConflictEntryDto) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "doc.badge.gearshape")
                .font(.system(size: 40, weight: .light))
                .foregroundStyle(.secondary)
            Text("二进制文件无法逐行合并")
                .font(.system(size: 16, weight: .semibold))
            Text("请选择采用自己或对方的完整版本，或使用配置的 merge.tool。")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            HStack {
                Button(sideActionTitle("采用自己的版本", present: entry.oursPresent)) {
                    Task { await chooseWholeSide(entry, side: .ours) }
                }
                .buttonStyle(.borderedProminent)
                Button(sideActionTitle("采用对方的版本", present: entry.theirsPresent)) {
                    Task { await chooseWholeSide(entry, side: .theirs) }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(30)
    }

    private func renameConflictBody(_ entry: GitConflictEntryDto) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("双方以不同名称重命名了同一个文件")
                .font(.system(size: 15, weight: .semibold))
            renameRow("基线", entry.basePath ?? "—")
            renameRow("自己", entry.oursPath ?? "—")
            renameRow("对方", entry.theirsPath ?? "—")
            if let sides {
                HStack(alignment: .top, spacing: 10) {
                    sidePane("自己的内容", sides.oursText, present: sides.oursPresent)
                    sidePane("对方的内容", sides.theirsText, present: sides.theirsPresent)
                }
                .frame(maxHeight: .infinity)
            }
            Spacer(minLength: 0)
            Text("选择一边会删除另外两个路径并暂存选中的重命名；“两边都保留”会同时暂存两份文件。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func renameRow(_ label: String, _ path: String) -> some View {
        HStack(spacing: 10) {
            Text(label).font(.system(size: 10, weight: .semibold)).frame(width: 36, alignment: .leading)
            Text(path).font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
    }

    private func textConflictBody(
        _ entry: GitConflictEntryDto,
        _ document: GitThreeWayMergeDto
    ) -> some View {
        VStack(spacing: 0) {
            conflictNavigation(document)
            Divider()
            if manualEditing {
                TextEditor(text: $manualText)
                    .font(.system(size: 11.5, design: .monospaced))
                    .padding(8)
                    .background(Color(nsColor: .textBackgroundColor))
            } else {
                HStack(spacing: 0) {
                    sidePane("自己的版本", sides?.oursText, present: sides?.oursPresent ?? false)
                    Divider()
                    mergeResultPane(document)
                        .frame(minWidth: 320)
                    Divider()
                    sidePane("对方的版本", sides?.theirsText, present: sides?.theirsPresent ?? false)
                }
            }
            Divider()
            HStack {
                if document.conflicts.indices.contains(currentHunk) {
                    let current = document.conflicts[currentHunk]
                    Text("冲突 \(currentHunk + 1) / \(document.conflicts.count)")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("采用自己") { applyChoice(current.index, side: .ours) }
                    Button("采用对方") { applyChoice(current.index, side: .theirs) }
                    Button("两边都要") { applyChoice(current.index, side: .both) }
                    if let chosen = choices[current.index] {
                        Text(choiceLabel(chosen))
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.green)
                    }
                } else {
                    Text("没有可导航的冲突块").foregroundStyle(.secondary)
                    Spacer()
                }
                Button(manualEditing ? "返回分栏" : "手动编辑") {
                    if !manualEditing {
                        manualText = resolvedText
                    } else {
                        resolvedText = manualText
                    }
                    manualEditing.toggle()
                }
                Button("标记已解决") { Task { await resolveSelectedText(entry) } }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || !canResolveSelectedText)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
        }
    }

    private func conflictNavigation(_ document: GitThreeWayMergeDto) -> some View {
        HStack {
            Text("第 \(currentHunk + 1) / \(max(document.conflicts.count, 1)) 个冲突")
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(.secondary)
            Spacer()
            Button {
                currentHunk = max(0, currentHunk - 1)
            } label: {
                Image(systemName: "chevron.up")
            }
            .buttonStyle(.borderless)
            .disabled(currentHunk == 0 || document.conflicts.isEmpty)
            Button {
                currentHunk = min(max(document.conflicts.count - 1, 0), currentHunk + 1)
            } label: {
                Image(systemName: "chevron.down")
            }
            .buttonStyle(.borderless)
            .disabled(document.conflicts.isEmpty || currentHunk >= document.conflicts.count - 1)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
    }

    private func mergeResultPane(_ document: GitThreeWayMergeDto) -> some View {
        let highlightedConflict = document.conflicts.indices.contains(currentHunk)
            ? document.conflicts[currentHunk].index
            : nil
        return VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("合并结果")
                    .font(.system(size: 10.5, weight: .semibold))
                Spacer()
                if allHunksChosen {
                    Label("所有冲突块已选择", systemImage: "checkmark.circle.fill")
                        .font(.system(size: 9.5, weight: .semibold))
                        .foregroundStyle(.green)
                }
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 6)
            .background(Color.primary.opacity(0.035))
            Divider()
            ScrollViewReader { proxy in
                ScrollView([.vertical, .horizontal]) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(document.lines.enumerated()), id: \.offset) { index, line in
                            HStack(alignment: .top, spacing: 7) {
                                Text("\(index + 1)")
                                    .font(.system(size: 9.5, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                                    .frame(width: 32, alignment: .trailing)
                                Text(line.text.isEmpty ? " " : line.text)
                                    .font(.system(size: 11, design: .monospaced))
                                    .foregroundStyle(lineColor(line.kind))
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .padding(.horizontal, 7)
                            .padding(.vertical, 1)
                            .background(
                                line.conflictIndex != nil
                                    && line.conflictIndex == highlightedConflict
                                    ? Color.orange.opacity(0.13)
                                    : lineBackground(line.kind)
                            )
                            .id(index)
                        }
                    }
                    .padding(.vertical, 4)
                }
                .onChange(of: currentHunk) { _ in
                    guard document.conflicts.indices.contains(currentHunk),
                          let lineIndex = document.lines.firstIndex(where: {
                              $0.conflictIndex == document.conflicts[currentHunk].index
                          }) else { return }
                    withAnimation(.easeOut(duration: 0.16)) {
                        proxy.scrollTo(lineIndex, anchor: .top)
                    }
                }
            }
            .background(Color(nsColor: .textBackgroundColor).opacity(0.72))
        }
    }

    private func sidePane(_ title: String, _ text: String?, present: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.system(size: 10.5, weight: .semibold))
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.primary.opacity(0.035))
            Divider()
            ScrollView([.vertical, .horizontal]) {
                Text(present ? (text ?? "") : "（此版本已删除）")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(present ? Color.primary : Color.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
            }
            .background(Color(nsColor: .textBackgroundColor).opacity(0.72))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func sideComparisonBody(_ sides: GitConflictSidesDto) -> some View {
        HStack(spacing: 0) {
            sidePane("自己的版本", sides.oursText, present: sides.oursPresent)
            Divider()
            sidePane("对方的版本", sides.theirsText, present: sides.theirsPresent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var footer: some View {
        HStack(spacing: 9) {
            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.red)
                    .lineLimit(2)
            } else if let statusMessage {
                Label(statusMessage, systemImage: "checkmark.circle")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer()
            if hasConflicts {
                Text("解决所有文件后才能继续")
                    .font(.system(size: 9.5))
                    .foregroundStyle(.tertiary)
            }
            Button("中止操作") { confirmAbort = true }
                .disabled(busy || conflictState?.canAbort != true)
            Button("继续操作") { Task { await continueOperation() } }
                .buttonStyle(.borderedProminent)
                .disabled(busy || hasConflicts || conflictState?.canContinue != true)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }

    private func reloadAll() async {
        busy = true
        errorMessage = nil
        do {
            async let loadedBranches = model.gitBranchSnapshot(path: path)
            async let loadedState = model.gitConflicts(path: path)
            let branchSnapshot = try await loadedBranches
            branches = branchSnapshot.localBranches + branchSnapshot.remoteBranches
            conflictState = try await loadedState
            if target.isEmpty, let first = branches.first {
                target = first.name
            }
            if let entries = conflictState?.entries, !entries.isEmpty {
                let index = min(selectedEntryIndex ?? 0, entries.count - 1)
                await selectConflict(index)
            } else {
                selectedEntryIndex = nil
                sides = nil
                mergeDocument = nil
                choices = [:]
            }
        } catch {
            errorMessage = error.localizedDescription
        }
        busy = false
    }

    private func selectConflict(_ index: Int) async {
        guard let entries = conflictState?.entries, entries.indices.contains(index) else { return }
        selectedEntryIndex = index
        sides = nil
        mergeDocument = nil
        choices = [:]
        currentHunk = 0
        manualEditing = false
        errorMessage = nil
        busy = true
        do {
            let entry = entries[index]
            sides = try await model.gitConflictSides(path: path, conflict: entry)
            if !entry.binary && !isRenameConflict(entry) {
                let document = try await model.gitThreeWayMerge(path: path, conflict: entry)
                mergeDocument = document
                resolvedText = document.mergedText
                manualText = document.mergedText
            }
        } catch {
            errorMessage = error.localizedDescription
        }
        busy = false
    }

    private func startMerge() async {
        busy = true
        errorMessage = nil
        statusMessage = nil
        do {
            let trimmedMessage = mergeMessage.trimmingCharacters(in: .whitespacesAndNewlines)
            let outcome = try await model.mergeGit(
                path: path,
                request: GitMergeRequestDto(
                    target: target.trimmingCharacters(in: .whitespacesAndNewlines),
                    strategy: strategy.dto,
                    message: trimmedMessage.isEmpty ? nil : trimmedMessage
                )
            )
            statusMessage = outcome.message
            conflictState = outcome.conflicts
            if let entries = outcome.conflicts?.entries, !entries.isEmpty {
                selectedEntryIndex = 0
                await selectConflict(0)
            } else {
                await reloadAll()
            }
        } catch {
            errorMessage = error.localizedDescription
        }
        busy = false
    }

    private func chooseWholeSide(
        _ entry: GitConflictEntryDto,
        side: GitConflictSideDto
    ) async {
        busy = true
        errorMessage = nil
        do {
            try await model.gitChooseConflictSide(path: path, conflict: entry, side: side)
            statusMessage = "已采用\(sideLabel(side))版本"
            await reloadAll()
        } catch {
            errorMessage = error.localizedDescription
            busy = false
        }
    }

    private func openMergeTool(_ entry: GitConflictEntryDto) async {
        busy = true
        errorMessage = nil
        do {
            statusMessage = try await model.gitOpenMergeTool(path: path, conflict: entry)
            await reloadAll()
        } catch {
            errorMessage = error.localizedDescription
            busy = false
        }
    }

    private func applyChoice(_ index: UInt32, side: GitConflictChoiceSideDto) {
        guard let mergeDocument else { return }
        choices[index] = side
        resolvedText = mergeDocument.mergedText
        Task {
            resolvedText = await model.gitApplyConflictChoices(
                mergedText: mergeDocument.mergedText,
                choices: choicesArray
            )
            manualText = resolvedText
        }
    }

    private func resolveSelectedText(_ entry: GitConflictEntryDto) async {
        busy = true
        errorMessage = nil
        do {
            let content = manualEditing ? manualText : resolvedText
            try await model.gitResolveConflict(
                path: path,
                relativePath: entry.oursPath ?? entry.path,
                content: content
            )
            statusMessage = "已暂存合并结果"
            await reloadAll()
        } catch {
            errorMessage = error.localizedDescription
            busy = false
        }
    }

    private func continueOperation() async {
        busy = true
        errorMessage = nil
        do {
            statusMessage = try await model.gitContinueOperation(path: path)
            await reloadAll()
        } catch {
            errorMessage = error.localizedDescription
            busy = false
        }
    }

    private func abortOperation() async {
        busy = true
        errorMessage = nil
        do {
            statusMessage = try await model.gitAbortOperation(path: path)
            await reloadAll()
        } catch {
            errorMessage = error.localizedDescription
            busy = false
        }
    }

    private func isRenameConflict(_ entry: GitConflictEntryDto) -> Bool {
        entry.kind == .rename
    }

    private func conflictKindLabel(_ entry: GitConflictEntryDto) -> String {
        switch entry.kind {
        case .bothModified: return "双方修改"
        case .modifyDeleteOurs: return "自己删除，对方修改"
        case .modifyDeleteTheirs: return "自己修改，对方删除"
        case .addAdd: return "双方新增同名文件"
        case .rename: return "重命名冲突"
        case .other: return "其他冲突"
        }
    }

    private func entryFileName(_ entry: GitConflictEntryDto) -> String {
        let value = entry.oursPath ?? entry.theirsPath ?? entry.path
        return (value as NSString).lastPathComponent
    }

    private func branchLabel(_ branch: GitBranchDto) -> String {
        let scope = branch.remote == nil ? "本地" : "远程"
        return "\(branch.name)  \(scope) · \(branch.shortHash)"
    }

    private func sideLabel(_ side: GitConflictSideDto) -> String {
        switch side {
        case .ours: return "自己"
        case .theirs: return "对方"
        case .both: return "两边"
        }
    }

    private func sideActionTitle(_ title: String, present: Bool) -> String {
        present ? title : "\(title)（删除）"
    }

    private func choiceLabel(_ side: GitConflictChoiceSideDto) -> String {
        switch side {
        case .ours: return "已选自己"
        case .theirs: return "已选对方"
        case .both: return "已选两边"
        }
    }

    private func lineColor(_ kind: GitMergeLineKindDto) -> Color {
        switch kind {
        case .context: return .primary
        case .ours: return .blue
        case .base: return .secondary
        case .theirs: return .purple
        case .marker: return .orange
        }
    }

    private func lineBackground(_ kind: GitMergeLineKindDto) -> Color {
        switch kind {
        case .context: return .clear
        case .ours: return Color.blue.opacity(0.06)
        case .base: return Color.secondary.opacity(0.05)
        case .theirs: return Color.purple.opacity(0.06)
        case .marker: return Color.orange.opacity(0.07)
        }
    }
}
