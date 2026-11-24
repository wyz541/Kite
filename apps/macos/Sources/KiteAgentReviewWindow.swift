import AppKit
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class KiteAgentReviewModel: ObservableObject {
    let core: KiteCore
    let review: AgentReviewDto
    let appModel: KiteModel
    let taskID: String
    let onPermissionReply: ((String) -> Void)?

    @Published var changes: [AgentFileChangeDto]
    @Published private(set) var appliedIndices = Set<Int>()
    @Published private(set) var discarded = false
    @Published private(set) var busy = false
    @Published private(set) var notice: String?
    @Published var errorMessage: String?

    init(
        review: AgentReviewDto,
        appModel: KiteModel,
        taskID: String? = nil,
        onPermissionReply: ((String) -> Void)? = nil
    ) {
        self.core = appModel.core
        self.review = review
        self.appModel = appModel
        self.changes = review.fileChanges
        self.taskID = taskID ?? review.id
        self.onPermissionReply = onPermissionReply
        self.recordAudit(confirmed: false, outcome: .prepared)
    }

    var hasPendingWrites: Bool {
        !discarded && !changes.isEmpty && appliedIndices.count < changes.count
    }

    var permissionLabel: String {
        review.transport == .cli ? "权限由 Agent 管理" : "写入需确认"
    }

    var needsReconfirmation: Bool {
        changes.contains { $0.requiresReconfirmation }
    }

    func copyAnswer() {
        guard !review.answer.isEmpty else { return }
        KitePlatform.copyText(review.answer)
        notice = "已复制 Agent 回答"
        errorMessage = nil
    }

    func copyCommand(_ command: AgentCommandSuggestionDto) {
        KitePlatform.copyText(command.command)
        notice = "已复制命令；Kite 不会自动执行"
        errorMessage = nil
    }

    func locate(_ path: String) {
        guard FileManager.default.fileExists(atPath: path) else {
            errorMessage = "引用路径已失效"
            return
        }
        appModel.revealExternalItems([path])
        errorMessage = nil
    }

    func open(_ path: String) {
        guard FileManager.default.fileExists(atPath: path) else {
            errorMessage = "引用路径已失效"
            return
        }
        KitePlatform.open(path)
    }

    func insertAnswerIntoEditor() {
        do {
            let path = try core.agentMaterializeDraft(
                text: review.answer,
                suggestedName: "agent-result.md"
            )
            KitePlatform.openTextEditor(path)
            notice = "已在编辑器中打开 Agent 回答"
            errorMessage = nil
            recordAudit(confirmed: false, outcome: .savedAs)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func applyAll() {
        guard !busy, !changes.isEmpty else { return }
        guard !needsReconfirmation else {
            errorMessage = "文件已变化，请先刷新审核"
            return
        }
        busy = true
        errorMessage = nil
        notice = "正在应用 Agent 修改…"
        let changes = self.changes
        let core = self.core

        Task {
            do {
                let outcomes = try await Task.detached(priority: .userInitiated) {
                    try core.agentApplyChanges(changes: changes, writeConfirmed: true)
                }.value
                appliedIndices = Set(changes.indices)
                notice = "已应用 \(outcomes.count) 个文件修改"
                errorMessage = nil
                recordAudit(confirmed: true, outcome: .applied)
                appModel.refresh()
            } catch {
                errorMessage = error.localizedDescription
                notice = nil
                recordAudit(confirmed: true, outcome: .failed)
            }
            busy = false
        }
    }

    func reconfirmChange(at index: Int) {
        guard changes.indices.contains(index) else { return }
        do {
            let updated = try core.agentReconfirmChange(change: changes[index])
            changes[index] = updated
            appliedIndices.remove(index)
            notice = "已按最新文件内容重新生成 diff，请再次确认"
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
            notice = nil
        }
    }

    func saveAnswerAs() {
        guard let destination = chooseSaveDestination(
            suggestedName: "agent-result.md"
        ) else { return }
        do {
            let overwrite = FileManager.default.fileExists(atPath: destination)
            if overwrite && !confirmOverwrite(destination) { return }
            let outcome = try core.agentSaveAnswerAs(
                answer: review.answer,
                destination: destination,
                overwriteConfirmed: overwrite
            )
            notice = outcome.message
            errorMessage = nil
            recordAudit(confirmed: true, outcome: .savedAs)
        } catch {
            errorMessage = error.localizedDescription
            notice = nil
        }
    }

    func saveChangeAs(at index: Int) {
        guard changes.indices.contains(index) else { return }
        let change = changes[index]
        let suggestedName = URL(fileURLWithPath: change.relativePath).lastPathComponent
        guard let destination = chooseSaveDestination(suggestedName: suggestedName) else {
            return
        }
        do {
            let overwrite = FileManager.default.fileExists(atPath: destination)
            if overwrite && !confirmOverwrite(destination) { return }
            let outcome = try core.agentSaveChangeAs(
                change: change,
                destination: destination,
                overwriteConfirmed: overwrite
            )
            notice = outcome.message
            errorMessage = nil
            recordAudit(confirmed: true, outcome: .savedAs)
        } catch {
            errorMessage = error.localizedDescription
            notice = nil
        }
    }

    func respond(
        to request: AgentPermissionRequestDto,
        decision: AgentPermissionDecisionDto
    ) {
        do {
            let response = try core.agentRespondPermission(
                request: request,
                decision: decision,
                taskId: taskID,
                transport: review.transport
            )
            onPermissionReply?(response.responseJson)
            notice = response.message
            errorMessage = nil
            recordAudit(
                confirmed: false,
                outcome: response.allowed ? .permissionAllowed : .permissionDenied
            )
        } catch {
            errorMessage = error.localizedDescription
            notice = nil
        }
    }

    func discard() {
        discarded = true
        recordAudit(confirmed: false, outcome: .discarded)
    }

    func recordAudit(confirmed: Bool, outcome: AgentAuditOutcomeDto) {
        _ = try? core.agentRecordAudit(
            review: review,
            userConfirmedWrite: confirmed,
            outcome: outcome,
            path: nil
        )
    }

    private func chooseSaveDestination(suggestedName: String) -> String? {
        let panel = NSSavePanel()
        panel.canCreateDirectories = true
        panel.nameFieldStringValue = suggestedName
        return panel.runModal() == .OK ? panel.url?.path : nil
    }

    private func confirmOverwrite(_ path: String) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "目标文件已存在"
        alert.informativeText = "确认覆盖以下文件？\n\(path)"
        alert.addButton(withTitle: "覆盖")
        alert.addButton(withTitle: "取消")
        return alert.runModal() == .alertFirstButtonReturn
    }
}

@MainActor
final class KiteAgentReviewWindowController: NSWindowController, NSWindowDelegate {
    private var reviewModel: KiteAgentReviewModel?

    func windowWillClose(_ notification: Notification) {
        KiteAgentReviewWindow.release(self)
    }

    init(
        review: AgentReviewDto,
        appModel: KiteModel,
        taskID: String?,
        onPermissionReply: ((String) -> Void)?
    ) {
        let model = KiteAgentReviewModel(
            review: review,
            appModel: appModel,
            taskID: taskID,
            onPermissionReply: onPermissionReply
        )
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 980, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Kite Agent 结果 — \(review.agent)"
        window.titlebarAppearsTransparent = true
        window.minSize = NSSize(width: 720, height: 520)
        window.isReleasedWhenClosed = false
        window.center()
        window.setFrameAutosaveName("KiteAgentReviewWindow")
        window.contentViewController = NSHostingController(
            rootView: KiteAgentReviewView(model: model)
        )
        self.reviewModel = model
        super.init(window: window)
        window.delegate = self
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func show() {
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard let model = reviewModel else { return true }
        if model.busy {
            let alert = NSAlert()
            alert.alertStyle = .informational
            alert.messageText = "正在应用 Agent 修改"
            alert.informativeText = "请等待写入完成后再关闭窗口。"
            alert.runModal()
            return false
        }
        guard model.hasPendingWrites else { return true }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "仍有未应用的 Agent 修改"
        alert.informativeText = "关闭窗口不会写入文件。你也可以继续审阅。"
        alert.addButton(withTitle: "保留审阅")
        alert.addButton(withTitle: "放弃并关闭")
        if alert.runModal() == .alertSecondButtonReturn {
            model.discard()
            model.errorMessage = nil
            return true
        }
        return false
    }
}

enum KiteAgentReviewWindow {
    @MainActor private static var controllers: [KiteAgentReviewWindowController] = []

    @MainActor
    static func show(
        review: AgentReviewDto,
        appModel: KiteModel,
        taskID: String? = nil,
        onPermissionReply: ((String) -> Void)? = nil
    ) -> KiteAgentReviewWindowController {
        let controller = KiteAgentReviewWindowController(
            review: review,
            appModel: appModel,
            taskID: taskID,
            onPermissionReply: onPermissionReply
        )
        controllers.append(controller)
        controller.show()
        return controller
    }

    @MainActor
    static func release(_ controller: KiteAgentReviewWindowController) {
        controllers.removeAll { $0 === controller }
    }
}

@MainActor
extension KiteModel {
    func presentAgentReviewFile() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.json]
        panel.message = "选择包含 Agent 结构化结果的 JSON 文件"
        guard panel.runModal() == .OK, let url = panel.url else { return }

        do {
            let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
            let size = (attributes[.size] as? NSNumber)?.uint64Value ?? 0
            guard size <= 16 * 1024 * 1024 else {
                throw NSError(
                    domain: "dev.kite.agent",
                    code: 1,
                    userInfo: [NSLocalizedDescriptionKey: "Agent 结果文件超过 16 MiB"]
                )
            }
            let payload = try String(contentsOf: url, encoding: .utf8)
            let review = try core.agentParseReviewJson(payload: payload)
            _ = KiteAgentReviewWindow.show(review: review, appModel: self)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

private struct KiteAgentReviewView: View {
    @ObservedObject var model: KiteAgentReviewModel

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if model.review.transport == .cli {
                        warningBanner(model.review.boundaryNotice)
                    }
                    if let error = model.errorMessage {
                        statusBanner(error, color: .red, systemImage: "exclamationmark.triangle.fill")
                    } else if let notice = model.notice {
                        statusBanner(notice, color: .secondary, systemImage: "info.circle.fill")
                    }
                    answerSection
                    if !model.review.references.isEmpty {
                        referencesSection
                    }
                    if !model.changes.isEmpty {
                        changesSection
                    }
                    if !model.review.permissionRequests.isEmpty {
                        permissionsSection
                    }
                    if !model.review.commands.isEmpty {
                        commandsSection
                    }
                }
                .padding(18)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Divider()
            footer
        }
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "sparkles")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(Color.accentColor)
                .frame(width: 36, height: 36)
                .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 9))

            VStack(alignment: .leading, spacing: 3) {
                Text(model.review.agent)
                    .font(.system(size: 17, weight: .semibold))
                Text("\(model.review.model ?? "Agent 默认模型") · \(model.permissionLabel)")
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
                Text("工作目录：\(model.review.workingDirectory)")
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer()

            Button {
                model.copyAnswer()
            } label: {
                Label("复制回答", systemImage: "doc.on.doc")
            }
            .disabled(model.review.answer.isEmpty)

            Button {
                model.insertAnswerIntoEditor()
            } label: {
                Label("插入编辑器", systemImage: "square.and.pencil")
            }
            .disabled(model.review.answer.isEmpty)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 13)
    }

    private var answerSection: some View {
        reviewSection("回答", systemImage: "text.bubble") {
            Group {
                if model.review.answer.isEmpty {
                    Text("Agent 没有返回文字回答")
                        .foregroundStyle(.secondary)
                } else {
                    Text(model.review.answer)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .font(.system(size: 12.5))
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 8))
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .stroke(Color.primary.opacity(0.08), lineWidth: 1)
            }
        }
    }

    private var referencesSection: some View {
        reviewSection("引用文件", systemImage: "link") {
            VStack(spacing: 7) {
                ForEach(Array(model.review.references.enumerated()), id: \.offset) { _, reference in
                    HStack(spacing: 9) {
                        Image(systemName: reference.isDirectory ? "folder.fill" : "doc.text.fill")
                            .foregroundStyle(reference.exists ? Color.accentColor : Color.red)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(reference.label)
                                .font(.system(size: 12, weight: .medium))
                            Text(reference.exists ? reference.path : "路径已失效：\(reference.path)")
                                .font(.system(size: 10.5, design: .monospaced))
                                .foregroundStyle(reference.exists ? Color.secondary : Color.red)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer()
                        Button("定位") { model.locate(reference.path) }
                            .buttonStyle(.borderless)
                            .disabled(!reference.exists)
                        Button("打开") { model.open(reference.path) }
                            .buttonStyle(.borderless)
                            .disabled(!reference.exists)
                    }
                    .padding(.horizontal, 10)
                    .frame(height: 42)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
                }
            }
        }
    }

    private var changesSection: some View {
        reviewSection("文件修改", systemImage: "doc.badge.gearshape") {
            VStack(spacing: 12) {
                ForEach(Array(model.changes.enumerated()), id: \.offset) { index, change in
                    changeCard(index: index, change: change)
                }
            }
        }
    }

    private func changeCard(index: Int, change: AgentFileChangeDto) -> some View {
        let applied = model.appliedIndices.contains(index)
        return VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .top, spacing: 9) {
                Image(systemName: "doc.text")
                    .foregroundStyle(change.requiresReconfirmation ? Color.red : Color.accentColor)
                VStack(alignment: .leading, spacing: 3) {
                    Text(change.relativePath)
                        .font(.system(size: 12.5, weight: .semibold))
                    Text(
                        "写入：\(change.targetPath) · +\(change.addedLines) −\(change.removedLines) · \(change.overwriteRisk ? "覆盖已有文件" : "创建新文件")"
                    )
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                }
                Spacer()
                if applied {
                    Label("已应用", systemImage: "checkmark.circle.fill")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.green)
                }
            }

            if change.requiresReconfirmation {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                    Text("文件在审核后发生变化。Kite 不会覆盖；请重新生成 diff 并再次确认。")
                        .font(.system(size: 11))
                        .foregroundStyle(.red)
                    Spacer()
                    Button("重新审核") {
                        model.reconfirmChange(at: index)
                    }
                }
                .padding(9)
                .background(Color.red.opacity(0.07), in: RoundedRectangle(cornerRadius: 7))
            }

            ScrollView(.vertical) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(diffLines(change.unifiedDiff).enumerated()), id: \.offset) { _, line in
                        diffLine(line)
                    }
                    if change.diffTruncated {
                        Text("diff 已截断；应用前请确认受影响文件。")
                            .font(.system(size: 10.5))
                            .foregroundStyle(.orange)
                            .padding(8)
                    }
                }
            }
            .frame(maxHeight: 280)
            .background(Color(nsColor: .textBackgroundColor))
            .overlay {
                RoundedRectangle(cornerRadius: 7)
                    .stroke(Color.primary.opacity(0.09), lineWidth: 1)
            }
            .clipShape(RoundedRectangle(cornerRadius: 7))

            HStack {
                Spacer()
                Button("刷新审核") {
                    model.reconfirmChange(at: index)
                }
                .disabled(model.busy)
                Button("另存为…") {
                    model.saveChangeAs(at: index)
                }
                .disabled(applied || change.requiresReconfirmation || model.busy)
            }
        }
        .padding(12)
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .stroke(
                    change.requiresReconfirmation ? Color.red.opacity(0.35) : Color.primary.opacity(0.08),
                    lineWidth: 1
                )
        }
    }

    private var permissionsSection: some View {
        reviewSection("权限请求", systemImage: "checkmark.shield") {
            VStack(spacing: 10) {
                ForEach(Array(model.review.permissionRequests.enumerated()), id: \.offset) { _, request in
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(spacing: 8) {
                            Image(systemName: "checkmark.shield.fill")
                                .foregroundStyle(.orange)
                            Text(request.title)
                                .font(.system(size: 12.5, weight: .semibold))
                        }
                        if !request.detail.isEmpty {
                            Text(request.detail)
                                .font(.system(size: 11.5))
                                .foregroundStyle(.secondary)
                        }
                        if !request.targetPaths.isEmpty {
                            Text(request.targetPaths.joined(separator: " · "))
                                .font(.system(size: 10.5, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        HStack {
                            Button("仅本次允许") {
                                model.respond(to: request, decision: .allowOnce)
                            }
                            .disabled(model.review.transport == .cli)
                            Button("本任务允许") {
                                model.respond(to: request, decision: .allowTask)
                            }
                            .disabled(model.review.transport == .cli || !request.canAllowTask)
                            Button("拒绝", role: .destructive) {
                                model.respond(to: request, decision: .deny)
                            }
                            .disabled(model.review.transport == .cli)
                            Spacer()
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        if model.review.transport == .cli {
                            Text("普通 CLI 无法接收 Kite 的交互式权限回复；请在 CLI 自身界面处理。")
                                .font(.system(size: 10.5))
                                .foregroundStyle(.orange)
                        }
                    }
                    .padding(11)
                    .background(Color.orange.opacity(0.055), in: RoundedRectangle(cornerRadius: 8))
                    .overlay {
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(Color.orange.opacity(0.22), lineWidth: 1)
                    }
                }
            }
        }
    }

    private var commandsSection: some View {
        reviewSection("命令建议（仅文本，不会自动执行）", systemImage: "terminal") {
            VStack(spacing: 8) {
                ForEach(Array(model.review.commands.enumerated()), id: \.offset) { _, command in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(alignment: .top, spacing: 8) {
                            Text(command.command)
                                .font(.system(size: 11, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Button {
                                model.copyCommand(command)
                            } label: {
                                Image(systemName: "doc.on.doc")
                            }
                            .buttonStyle(.borderless)
                            .help("复制命令")
                        }
                        if !command.rationale.isEmpty {
                            Text(command.rationale)
                                .font(.system(size: 10.5))
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(10)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
                }
            }
        }
    }

    private var footer: some View {
        HStack(spacing: 9) {
            Text("任何写入都必须先查看上方 diff。Agent 建议的命令不会自动执行。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
            Spacer()
            Button("回答另存为…") {
                model.saveAnswerAs()
            }
            .disabled(model.review.answer.isEmpty || model.busy)
            Button(model.busy ? "正在应用…" : "应用全部修改") {
                model.applyAll()
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.busy || model.changes.isEmpty || model.needsReconfirmation)
            Button("放弃") {
                model.discard()
                NSApp.keyWindow?.performClose(nil)
            }
            .disabled(model.busy)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 11)
    }

    private func reviewSection<Content: View>(
        _ title: String,
        systemImage: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: systemImage)
                .font(.system(size: 12.5, weight: .semibold))
            content()
        }
    }

    private func diffLines(_ diff: String) -> [String] {
        let lines = diff.components(separatedBy: "\n")
        return Array(lines.prefix(1_500))
    }

    private func diffLine(_ line: String) -> some View {
        let color: Color
        let background: Color
        if line.hasPrefix("+") && !line.hasPrefix("+++") {
            color = .green
            background = Color.green.opacity(0.09)
        } else if line.hasPrefix("-") && !line.hasPrefix("---") {
            color = .red
            background = Color.red.opacity(0.09)
        } else if line.hasPrefix("@@") {
            color = .accentColor
            background = Color.accentColor.opacity(0.07)
        } else {
            color = .primary
            background = .clear
        }
        return Text(line.isEmpty ? " " : line)
            .font(.system(size: 10.5, design: .monospaced))
            .foregroundStyle(color)
            .textSelection(.enabled)
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(background)
    }

    private func warningBanner(_ text: String) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text(text)
                .font(.system(size: 11.5))
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(11)
        .background(Color.orange.opacity(0.07), in: RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.orange.opacity(0.25), lineWidth: 1)
        }
    }

    private func statusBanner(_ text: String, color: Color, systemImage: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: systemImage)
                .foregroundStyle(color)
            Text(text)
                .font(.system(size: 11.5))
                .textSelection(.enabled)
            Spacer()
        }
        .padding(10)
        .background(color.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
    }
}
