import AppKit
import Combine
import SwiftUI

enum KiteRightPaneMode: String, Equatable {
    case preview
    case agent
}

enum KiteAgentAction: String, CaseIterable, Identifiable {
    case ask
    case explainFile
    case summarizeFolder
    case reviewChanges
    case generateCommitMessage

    var id: String { rawValue }

    var title: String {
        switch self {
        case .ask: return "询问 Agent…"
        case .explainFile: return "解释此文件…"
        case .summarizeFolder: return "总结此文件夹…"
        case .reviewChanges: return "审查选中变更…"
        case .generateCommitMessage: return "生成提交说明…"
        }
    }

    var prompt: String {
        switch self {
        case .ask:
            return ""
        case .explainFile:
            return "请解释这个文件的作用、关键结构和需要注意的风险。不要修改文件。"
        case .summarizeFolder:
            return "请总结这个文件夹的内容、结构和值得注意的事项。不要修改文件。"
        case .reviewChanges:
            return "请审查当前 Git 工作区中的变更，指出问题、风险和需要补充的测试。不要修改文件。"
        case .generateCommitMessage:
            return "请根据当前变更生成一条简洁、准确的提交说明。只生成准备内容，不要修改文件。"
        }
    }
}

enum KiteAgentContextMode: String, CaseIterable, Identifiable, Equatable {
    case fixed
    case followSelection

    var id: String { rawValue }

    var title: String {
        switch self {
        case .fixed: return "固定上下文"
        case .followSelection: return "跟随当前选择"
        }
    }
}

enum KiteAgentSessionPhase: Equatable {
    case empty
    case unconfigured
    case loading
    case running
    case error
    case ended

    var label: String {
        switch self {
        case .empty: return "等待上下文"
        case .unconfigured: return "未配置"
        case .loading: return "正在连接"
        case .running: return "运行中"
        case .error: return "连接失败"
        case .ended: return "已结束"
        }
    }
}

struct KiteAgentContextPath: Identifiable, Hashable {
    let path: String
    let isDirectory: Bool

    var id: String { path }

    var displayName: String {
        let name = FileManager.default.displayName(atPath: path)
        return name.isEmpty ? path : name
    }
}

@MainActor
final class KiteAgentPanelModel: ObservableObject {
    @Published private(set) var contextPaths: [KiteAgentContextPath] = []
    @Published private(set) var primaryPath: String?
    @Published private(set) var workingDirectory = ""
    @Published private(set) var workingDirectoryStatus: AgentWorkingDirectoryStatus = .needsSelection
    @Published private(set) var workingDirectoryMessage: String?
    @Published var contextMode: KiteAgentContextMode = .fixed
    @Published private(set) var phase: KiteAgentSessionPhase = .empty
    @Published var prompt = ""
    @Published private(set) var presentationToken = 0
    @Published private(set) var notice: String?
    @Published private(set) var profile: AgentProfile?
    @Published private(set) var profileSupportsSession = false
    @Published private(set) var output = ""
    @Published private(set) var errorMessage: String?
    @Published private(set) var permissionRequests: [AgentPermissionRequest] = []
    @Published private(set) var canCancel = false
    @Published private(set) var runtimeState: AgentRuntimeState?
    @Published var autoFollow = true

    private let core = KiteCore()
    private var runtime: AgentRuntime?
    private var pollTask: Task<Void, Never>?
    private var conversationId = ""
    private var restoredMessages: [AgentMessage] = []
    private var turnId: UInt64 = 0
    private var sentPrompt = ""
    private var pendingPrompt: String?
    private var pendingPromptContextPaths: [String] = []
    private var promptSent = false
    private var selectedWorkingDirectory: String?
    private var savedTerminalSnapshot = false
    private var windowWidth: UInt32 = 960
    private var windowHeight: UInt32 = 760

    init() {
        conversationId = core.newAgentConversationId()
        refreshProfile()
        restoreConversation()
    }

    deinit {
        pollTask?.cancel()
        runtime?.shutdown()
    }

    var hasContext: Bool {
        primaryPath != nil && !contextPaths.isEmpty
    }
    var profileName: String {
        profile?.name ?? "未配置 Agent"
    }

    var modelLabel: String {
        profile?.model ?? "Agent 默认模型"
    }

    var permissionLabel: String {
        guard let mode = profile?.permission else { return "只读" }
        switch mode {
        case .readOnly: return "只读"
        case .confirmWrites: return "写入前确认"
        case .agentManaged: return "由 Agent 自身管理"
        }
    }

    var canSend: Bool {
        profile != nil && profileSupportsSession && hasContext && phase != .loading && phase != .running
    }

    var workingDirectoryDisplay: String {
        switch workingDirectoryStatus {
        case .ready:
            return workingDirectory.isEmpty ? "—" : workingDirectory
        case .needsSelection:
            return "尚未选择（发送前选择）"
        case .invalid:
            return "不可用"
        @unknown default:
            return "—"
        }
    }

    var hasConfiguredAgent: Bool {
        profile != nil && profileSupportsSession
    }

    var transcriptMessages: [AgentMessage] {
        restoredMessages
    }

    var lastSubmittedPrompt: String {
        sentPrompt
    }

    var statusLabel: String {
        guard let runtimeState else { return phase.label }
        switch runtimeState {
        case .starting: return "启动中"
        case .initializing: return "握手中"
        case .ready: return "已就绪"
        case .running: return "运行中"
        case .waitingPermission: return "等待权限"
        case .cancelling: return "取消中"
        case .completed: return "完成"
        case .failed: return "失败"
        case .cancelled: return "已取消"
        case .closed: return "已关闭"
        @unknown default: return phase.label
        }
    }

    var canRetry: Bool {
        !sentPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && (phase == .ended || phase == .error)
            && phase != .loading
            && phase != .running
    }

    func refreshAgentProfiles(core: KiteCore) {
        let selected = core.agentDefaultProfile()
        self.profile = selected
        self.profileSupportsSession = selected.map {
            core.agentProfileCanStartSession(profile: $0)
        } ?? false
        refreshWorkingDirectory()
        if phase != .loading && phase != .running {
            if profile == nil || !profileSupportsSession {
                phase = .unconfigured
                errorMessage = nil
            } else if hasContext && workingDirectoryStatus != .ready {
                phase = .error
                errorMessage = workingDirectoryMessage
            } else {
                phase = .ended
                errorMessage = nil
            }
        }
    }

    var preferredWindowSize: NSSize {
        NSSize(width: CGFloat(windowWidth), height: CGFloat(windowHeight))
    }

    func present(
        action: KiteAgentAction,
        primary: String,
        entries: [BrowserEntry]
    ) {
        guard FileManager.default.fileExists(atPath: primary) else {
            contextPaths = []
            primaryPath = nil
            self.workingDirectory = ""
            phase = .error
            notice = "Agent 上下文目标已失效，请刷新后重试"
            presentationToken += 1
            return
        }
        let paths = normalizedPaths(primary: primary, entries: entries)
        guard paths.first != nil else { return }
        contextPaths = paths
        primaryPath = primary
        workingDirectory = ""
        workingDirectoryStatus = .needsSelection
        workingDirectoryMessage = nil
        selectedWorkingDirectory = nil
        contextMode = .fixed
        refreshProfile()
        prompt = action.prompt
        runtimeState = nil
        output = ""
        errorMessage = nil
        permissionRequests = []
        savedTerminalSnapshot = false
        sentPrompt = ""
        conversationId = core.newAgentConversationId()
        restoredMessages = []
        turnId = 0
        notice = nil
        applyInitialWorkingDirectoryState()
        presentationToken += 1
    }

    func presentDirectory(_ path: String, action: KiteAgentAction) {
        guard FileManager.default.fileExists(atPath: path) else {
            contextPaths = []
            primaryPath = nil
            workingDirectory = ""
            phase = .error
            notice = "Agent 上下文目标已失效，请刷新后重试"
            presentationToken += 1
            return
        }
        contextPaths = [KiteAgentContextPath(path: path, isDirectory: true)]
        primaryPath = path
        workingDirectory = ""
        workingDirectoryStatus = .needsSelection
        workingDirectoryMessage = nil
        selectedWorkingDirectory = nil
        contextMode = .fixed
        refreshProfile()
        prompt = action.prompt
        runtimeState = nil
        output = ""
        errorMessage = nil
        permissionRequests = []
        savedTerminalSnapshot = false
        sentPrompt = ""
        conversationId = core.newAgentConversationId()
        restoredMessages = []
        turnId = 0
        notice = nil
        applyInitialWorkingDirectoryState()
        presentationToken += 1
    }

    private func applyInitialWorkingDirectoryState() {
        if profile == nil || !profileSupportsSession {
            phase = .unconfigured
        } else if workingDirectoryStatus != .ready {
            phase = .error
            errorMessage = workingDirectoryMessage
            notice = workingDirectoryMessage
        } else {
            phase = .ended
        }
    }

    func updateFollowedSelection(entries: [BrowserEntry]) {
        guard contextMode == .followSelection, let first = entries.first else { return }
        let paths = entries.map {
            KiteAgentContextPath(path: $0.path, isDirectory: $0.isDirectory)
        }
        contextPaths = paths
        primaryPath = first.path
        selectedWorkingDirectory = nil
        refreshWorkingDirectory()
        notice = "Agent 上下文已跟随当前选择"
    }

    func addPaths(_ paths: [String]) {
        guard !paths.isEmpty else { return }
        let added = paths.map { path in
            var isDirectory: ObjCBool = false
            FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory)
            return KiteAgentContextPath(path: path, isDirectory: isDirectory.boolValue)
        }
        if hasContext {
            let existing = Set(contextPaths.map(\.path))
            let unique = added.filter { !existing.contains($0.path) }
            contextPaths.append(contentsOf: unique)
            notice = unique.isEmpty ? "所选路径已在上下文中" : "已添加 \(unique.count) 个上下文路径"
        } else if let first = added.first {
            contextPaths = added
            primaryPath = first.path
            selectedWorkingDirectory = nil
            refreshWorkingDirectory()
            applyInitialWorkingDirectoryState()
            notice = "已添加上下文路径"
        }
    }

    func removePath(_ path: String) {
        let primaryRemoved = primaryPath == path
        contextPaths.removeAll { $0.path == path }
        if primaryRemoved {
            primaryPath = contextPaths.first?.path
            selectedWorkingDirectory = nil
        }
        if contextPaths.isEmpty {
            primaryPath = nil
            workingDirectory = ""
            workingDirectoryStatus = .invalid
            workingDirectoryMessage = nil
            phase = .empty
        } else if primaryRemoved {
            refreshWorkingDirectory()
        }
        notice = "已从 Agent 上下文移除"
    }

    func setFollowSelection(
        _ followsSelection: Bool,
        entries: [BrowserEntry]
    ) {
        contextMode = followsSelection ? .followSelection : .fixed
        if followsSelection {
            updateFollowedSelection(entries: entries)
        }
    }

    func chooseWorkingDirectory() {
        guard hasContext else {
            notice = "请先选择 Agent 上下文"
            return
        }
        guard let chosen = KitePlatform.chooseDirectory(prompt: "选择 Agent 工作目录") else {
            notice = "尚未选择工作目录，Agent 未启动"
            return
        }
        selectedWorkingDirectory = chosen
        refreshWorkingDirectory()
        if workingDirectoryStatus == .ready {
            if phase == .error {
                phase = .ended
                errorMessage = nil
            }
            notice = "Agent 工作目录已选择"
        } else {
            phase = .error
            errorMessage = workingDirectoryMessage ?? "Agent 工作目录不可用"
            notice = errorMessage
        }
    }

    func sendPrompt() {
        refreshProfile()
        guard let profile, profileSupportsSession, hasContext else {
            phase = .unconfigured
            notice = "请先配置可运行的 ACP / JSON-RPC stdio Agent，并准备上下文"
            return
        }
        let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            notice = "提示不能为空"
            return
        }
        refreshWorkingDirectory()
        if workingDirectoryStatus == .needsSelection {
            guard let chosen = KitePlatform.chooseDirectory(prompt: "选择 Agent 工作目录") else {
                notice = "尚未选择工作目录，Agent 未启动"
                return
            }
            selectedWorkingDirectory = chosen
            refreshWorkingDirectory()
        }
        guard workingDirectoryStatus == .ready else {
            phase = .error
            errorMessage = workingDirectoryMessage ?? "Agent 工作目录不可用"
            notice = errorMessage
            return
        }
        do {
            turnId = UInt64(Date().timeIntervalSince1970 * 1_000)
            let runtime = try AgentRuntime.start(
                profile: profile,
                workingDirectory: workingDirectory,
                contextPaths: contextPaths.map(\.path),
                restoredMessages: restoredMessages
            )
            self.runtime = runtime
            runtimeState = .starting
            sentPrompt = trimmed
            pendingPrompt = trimmed
            pendingPromptContextPaths = contextPaths.map(\.path)
            promptSent = false
            output = ""
            autoFollow = true
            errorMessage = nil
            permissionRequests = []
            savedTerminalSnapshot = false
            phase = .loading
            notice = "Agent 已启动"
            startPolling(runtime)
        } catch {
            phase = .error
            runtimeState = .failed
            errorMessage = error.localizedDescription
            notice = error.localizedDescription
        }
    }

    func cancelTask() {
        do {
            try runtime?.cancel()
        } catch {
            notice = error.localizedDescription
        }
    }

    func retryPrompt() {
        let retry = sentPrompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !retry.isEmpty, phase == .ended || phase == .error else { return }
        prompt = retry
        sendPrompt()
    }

    func pauseAutoFollow() {
        autoFollow = false
    }

    func resumeAutoFollow() {
        autoFollow = true
    }

    func respondPermission(_ request: AgentPermissionRequest, decision: AgentPermissionDecision) {
        do {
            try runtime?.respondPermission(requestId: request.requestId, decision: decision)
        } catch {
            notice = error.localizedDescription
        }
    }

    func copyOutput() {
        guard !output.isEmpty else { return }
        KitePlatform.copyText(output)
        notice = "已复制 Agent 回答"
    }

    private func startPolling(_ runtime: AgentRuntime) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 80_000_000)
                guard let self else { return }
                let snapshot = runtime.snapshot()
                self.apply(snapshot)
                if snapshot.state == .ready,
                   !self.promptSent,
                   let prompt = self.pendingPrompt {
                    do {
                        try runtime.sendPrompt(prompt: prompt, contextPaths: self.pendingPromptContextPaths)
                        self.promptSent = true
                        self.pendingPrompt = nil
                        self.notice = "Agent 运行中"
                    } catch {
                        self.phase = .error
                        self.runtimeState = .failed
                        self.errorMessage = error.localizedDescription
                        self.notice = error.localizedDescription
                    }
                }
                if self.phase == .ended || self.phase == .error {
                    runtime.shutdown()
                    if !self.savedTerminalSnapshot {
                        self.savedTerminalSnapshot = true
                        self.persistConversation()
                    }
                    return
                }
            }
        }
    }

    private func apply(_ snapshot: AgentRuntimeSnapshot) {
        switch snapshot.state {
        case .starting, .initializing:
            phase = .loading
        case .ready, .running, .waitingPermission, .cancelling:
            phase = .running
        case .completed, .cancelled, .closed:
            phase = .ended
        case .failed:
            phase = .error
        }
        runtimeState = snapshot.state
        output = snapshot.output
        errorMessage = snapshot.error
        permissionRequests = snapshot.permissionRequests
        canCancel = snapshot.state == .running || snapshot.state == .waitingPermission
        switch snapshot.state {
        case .completed:
            notice = "Agent 已完成"
        case .cancelled:
            notice = "Agent 已取消"
        case .failed:
            notice = snapshot.error ?? "Agent 失败"
        case .closed:
            notice = "Agent 已关闭"
        case .starting, .initializing, .ready, .running, .waitingPermission, .cancelling:
            break
        @unknown default:
            break
        }
    }

    private func refreshProfile() {
        refreshAgentProfiles(core: core)
    }

    private func refreshWorkingDirectory() {
        guard let profile, let primaryPath, !primaryPath.isEmpty else {
            workingDirectory = ""
            workingDirectoryStatus = .invalid
            workingDirectoryMessage = "请先选择 Agent 上下文"
            return
        }
        switch profile.workingDirectory {
        case .ask:
            break
        default:
            selectedWorkingDirectory = nil
        }
        let resolution = core.resolveAgentWorkingDirectory(
            profile: profile,
            primaryPath: primaryPath,
            selectedWorkingDirectory: selectedWorkingDirectory
        )
        workingDirectoryStatus = resolution.status
        workingDirectoryMessage = resolution.message
        workingDirectory = resolution.path ?? ""
    }

    private func restoreConversation() {
        let restore = core.agentSessionState()
        refreshProfile()
        guard let profile else { return }
        let conversation = restore.store.conversations.first(where: {
            $0.id == restore.store.activeConversationId
                && $0.profileId == profile.id
        }) ?? restore.store.conversations.first(where: {
            $0.profileId == profile.id
        })
        guard let conversation else { return }
        contextPaths = conversation.contextPaths.map { path in
            var isDirectory: ObjCBool = false
            FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory)
            return KiteAgentContextPath(path: path, isDirectory: isDirectory.boolValue)
        }
        primaryPath = conversation.contextPaths.first
        selectedWorkingDirectory = nil
        contextMode = conversation.followSelection ? .followSelection : .fixed
        refreshWorkingDirectory()
        prompt = conversation.draft
        output = conversation.messages.last(where: { $0.role == .assistant })?.content ?? ""
        conversationId = conversation.id
        restoredMessages = conversation.messages
        turnId = 0
        windowWidth = conversation.windowWidth
        windowHeight = conversation.windowHeight
        if !restore.missingPaths.isEmpty {
            phase = .error
            errorMessage = "恢复的 Agent 上下文包含失效路径，请检查后重试"
            notice = errorMessage
        } else if workingDirectoryStatus != .ready {
            phase = .error
            errorMessage = workingDirectoryMessage ?? "Agent 工作目录不可用"
            notice = errorMessage
        } else {
            phase = .ended
            notice = primaryPath == nil
                ? "已恢复上次 Agent 会话；请重新选择上下文"
                : "已恢复上次 Agent 会话"
        }
    }

    private func persistConversation() {
        guard let profile, hasContext, workingDirectoryStatus == .ready else { return }
        let now = UInt64(Date().timeIntervalSince1970 * 1_000)
        let userMessageId = "\(conversationId):user:\(turnId)"
        let assistantMessageId = "\(conversationId):assistant:\(turnId)"
        var messages = restoredMessages.filter {
            $0.id != userMessageId && $0.id != assistantMessageId
        }
        if !sentPrompt.isEmpty {
            messages.append(
                AgentMessage(
                    id: userMessageId,
                    role: .user,
                    content: sentPrompt,
                    unixMs: now,
                    truncated: false
                )
            )
        }
        if !output.isEmpty {
            messages.append(
                AgentMessage(
                    id: assistantMessageId,
                    role: .assistant,
                    content: output,
                    unixMs: now,
                    truncated: false
                )
            )
        }
        let conversation = AgentConversation(
            id: conversationId,
            title: String(sentPrompt.prefix(48)),
            profileId: profile.id,
            model: profile.model,
            workingDirectory: workingDirectory,
            contextPaths: contextPaths.map(\.path),
            followSelection: contextMode == .followSelection,
            messages: messages,
            remoteSessionId: nil,
            draft: prompt,
            windowWidth: windowWidth,
            windowHeight: windowHeight,
            updatedUnixMs: now
        )
        _ = try? core.saveAgentConversation(conversation: conversation)
        restoredMessages = messages
    }

    func persistWindowState(width: UInt32, height: UInt32) {
        windowWidth = min(max(width, 680), 3_840)
        windowHeight = min(max(height, 560), 2_160)
        guard profile != nil, hasContext, workingDirectoryStatus == .ready else { return }
        let restore = core.agentSessionState()
        if var conversation = restore.store.conversations.first(where: {
            $0.id == conversationId
        }) {
            conversation.draft = prompt
            conversation.windowWidth = windowWidth
            conversation.windowHeight = windowHeight
            conversation.updatedUnixMs = UInt64(Date().timeIntervalSince1970 * 1_000)
            _ = try? core.saveAgentConversation(conversation: conversation)
        } else {
            persistConversation()
        }
    }
    private func normalizedPaths(
        primary: String,
        entries: [BrowserEntry]
    ) -> [KiteAgentContextPath] {
        var seen = Set<String>()
        var paths: [KiteAgentContextPath] = []
        if let primaryEntry = entries.first(where: { $0.path == primary })
            ?? entries.first {
            seen.insert(primaryEntry.path)
            paths.append(
                KiteAgentContextPath(
                    path: primaryEntry.path,
                    isDirectory: primaryEntry.isDirectory
                )
            )
        }
        for entry in entries where seen.insert(entry.path).inserted {
            paths.append(
                KiteAgentContextPath(
                    path: entry.path,
                    isDirectory: entry.isDirectory
                )
            )
        }
        return paths
    }
}

struct KiteRightPaneTabs: View {
    let mode: KiteRightPaneMode
    let onPreview: () -> Void
    let onAgent: () -> Void

    var body: some View {
        HStack(spacing: 3) {
            modeButton("预览", systemImage: "rectangle.righthalf.inset.filled", active: mode == .preview, action: onPreview)
            modeButton("Agent", systemImage: "sparkles", active: mode == .agent, action: onAgent)
        }
        .padding(2)
        .background(Color.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }

    private func modeButton(
        _ title: String,
        systemImage: String,
        active: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(.system(size: 10.5, weight: .semibold))
                .padding(.horizontal, 7)
                .frame(height: 23)
                .background {
                    if active {
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .fill(Color.accentColor.opacity(0.14))
                    }
                }
        }
        .buttonStyle(.plain)
        .foregroundStyle(active ? Color.accentColor : Color.secondary)
        .accessibilityLabel(title)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}

struct KiteAgentPane: View {
    @ObservedObject var model: KiteModel
    @EnvironmentObject private var agent: KiteAgentPanelModel
    let onShowPreview: () -> Void
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            header
            Rectangle()
                .fill(Color.primary.opacity(0.06))
                .frame(height: 0.5)
            KiteAgentPanelContent(model: model)
        }
        .frame(width: KiteTheme.agentWidth)
        .frame(maxHeight: .infinity)
        .kiteGlass(.panel, cornerRadius: KiteTheme.panelRadius)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Agent 栏")
    }

    private var header: some View {
        HStack(spacing: 7) {
            KiteRightPaneTabs(
                mode: .agent,
                onPreview: onShowPreview,
                onAgent: {}
            )
            Spacer(minLength: 4)
            KiteToolbarIconButton(systemName: "paperclip", title: "添加上下文") {
                let paths = KitePlatform.chooseContextPaths(prompt: "添加到 Agent 上下文")
                agent.addPaths(paths)
            }
            KiteToolbarIconButton(systemName: "doc.badge.gearshape", title: "审阅结果") {
                model.presentAgentReviewFile()
            }
            KiteToolbarIconButton(systemName: "gearshape", title: "Agent 设置") {
                KiteSettingsWindowController.open(pane: .agent)
            }
            KiteToolbarIconButton(
                systemName: "macwindow.on.rectangle",
                title: "在独立窗口中打开 Agent"
            ) {
                KiteAgentWindowController.shared.show(model: model, agent: agent)
            }
            KiteToolbarIconButton(systemName: "xmark", title: "关闭 Agent", action: onClose)
                .scaleEffect(0.9)
        }
        .padding(.horizontal, 10)
        .frame(height: 42)
    }
}

private struct KiteAgentKeyboardMonitor: NSViewRepresentable {
    let onKey: (UInt16, NSEvent.ModifierFlags) -> Bool

    func makeCoordinator() -> Coordinator {
        Coordinator(onKey: onKey)
    }

    func makeNSView(context: Context) -> NSView {
        context.coordinator.install()
        return NSView(frame: .zero)
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        context.coordinator.onKey = onKey
    }

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        coordinator.remove()
    }

    final class Coordinator {
        var onKey: (UInt16, NSEvent.ModifierFlags) -> Bool
        private var monitor: Any?

        init(onKey: @escaping (UInt16, NSEvent.ModifierFlags) -> Bool) {
            self.onKey = onKey
        }

        func install() {
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self, [36, 48, 49].contains(event.keyCode) else {
                    return event
                }
                return self.onKey(event.keyCode, event.modifierFlags) ? nil : event
            }
        }

        func remove() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
        }
    }
}

@MainActor
private final class KiteAgentKeyboardFocus: ObservableObject {
    @Published var target: KiteAgentFocusTarget?
}

private enum KiteAgentFocusTarget: Hashable {
    case prompt
    case contextReveal(String)
    case contextRemove(String)
    case addContext
    case followSelection
    case copyOutput
    case resumeFollow
    case permission(String, String)
    case reviewResult
    case settings
    case stop
    case retry
    case send
}

private struct KiteAgentPanelContent: View {
    @ObservedObject var model: KiteModel
    @EnvironmentObject private var agent: KiteAgentPanelModel
    @FocusState private var focusedTarget: KiteAgentFocusTarget?
    @StateObject private var keyboardFocus = KiteAgentKeyboardFocus()

    private var focusTargets: [KiteAgentFocusTarget] {
        var targets: [KiteAgentFocusTarget] = []
        for item in agent.contextPaths {
            targets.append(.contextReveal(item.path))
            targets.append(.contextRemove(item.path))
        }
        targets.append(.addContext)
        targets.append(.followSelection)
        if !agent.output.isEmpty {
            targets.append(.copyOutput)
            if !agent.autoFollow {
                targets.append(.resumeFollow)
            }
        }
        for request in agent.permissionRequests {
            for action in KiteAgentPermissionPresentation.actions(for: request) where action.enabled {
                targets.append(.permission(request.requestId, action.id))
            }
        }
        targets.append(.prompt)
        targets.append(.reviewResult)
        targets.append(.settings)
        if agent.phase == .loading || agent.phase == .running {
            targets.append(.stop)
        } else if agent.canRetry {
            targets.append(.retry)
        }
        if agent.canSend {
            targets.append(.send)
        }
        return targets
    }

    private func moveFocus(from target: KiteAgentFocusTarget, forward: Bool) {
        let targets = focusTargets
        guard !targets.isEmpty else { return }
        let current = targets.firstIndex(of: target) ?? (forward ? -1 : 0)
        let next = forward
            ? (current + 1 + targets.count) % targets.count
            : (current - 1 + targets.count) % targets.count
        let target = targets[next]
        DispatchQueue.main.async {
            focusedTarget = target
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        if agent.hasContext {
                            outputCard
                            if !agent.permissionRequests.isEmpty {
                                permissionCards
                            }
                            if agent.phase == .unconfigured {
                                unconfiguredCard
                            }
                            if let error = agent.errorMessage {
                                errorBanner(error)
                            }
                        } else if agent.phase == .error {
                            errorState
                        } else {
                            emptyState
                        }
                    }
                    .padding(16)
                }
                .scrollIndicators(.hidden)
                .onAppear {
                    if agent.autoFollow {
                        proxy.scrollTo("agent-output-bottom", anchor: .bottom)
                    }
                }
                .onChange(of: agent.output) { _ in
                    guard agent.autoFollow else { return }
                    withAnimation(.snappy(duration: 0.18)) {
                        proxy.scrollTo("agent-output-bottom", anchor: .bottom)
                    }
                }
                .background(KiteAgentScrollWheelMonitor {
                    agent.pauseAutoFollow()
                })
            }
            .frame(maxHeight: .infinity)
            composer
        }
    }

    private var composer: some View {
        VStack(spacing: 10) {
            contextCard
            promptCard
            statusCard
        }
        .padding(12)
        .background(.regularMaterial)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(Color.primary.opacity(0.06))
                .frame(height: 0.5)
        }
    }

    private var statusCard: some View {
        HStack(spacing: 7) {
            Circle()
                .fill(color(for: agent.phase))
                .frame(width: 6, height: 6)
            Text(agent.profileName)
                .font(.system(size: 9.5, weight: .medium))
            Text("· \(agent.modelLabel) · \(agent.permissionLabel)")
                .font(.system(size: 9.5))
                .foregroundStyle(.secondary)
            Spacer(minLength: 6)
            Text(agent.workingDirectoryDisplay)
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                .truncationMode(.middle)
            Text(agent.statusLabel)
                .font(.system(size: 9.5, weight: .medium))
                .foregroundStyle(color(for: agent.phase))
        }
        .padding(.horizontal, 4)
    }

    private var contextCard: some View {
        VStack(alignment: .leading, spacing: 7) {
            ScrollView(.horizontal) {
                HStack(spacing: 6) {
                    ForEach(Array(agent.contextPaths.enumerated()), id: \.element.id) { index, item in
                        contextChip(item, index: index + 1)
                    }
                }
            }
            .scrollIndicators(.hidden)

            HStack(spacing: 10) {
                Button {
                    let paths = KitePlatform.chooseContextPaths(prompt: "添加到 Agent 上下文")
                    agent.addPaths(paths)
                } label: {
                    Label("添加上下文", systemImage: "plus")
                        .font(.system(size: 9.5, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.accentColor)
                .focusable()
                .focused($focusedTarget, equals: .addContext)
                .accessibilityLabel("添加上下文路径")
                .accessibilityIdentifier("agent-context-add")

                Toggle(
                    isOn: Binding(
                        get: { agent.contextMode == .followSelection },
                        set: { follows in
                            agent.setFollowSelection(
                                follows,
                                entries: model.selectedColumnEntries
                            )
                        }
                    )
                ) {
                    Text(agent.contextMode.title)
                        .font(.system(size: 9.5, weight: .medium))
                }
                .toggleStyle(.switch)
                .controlSize(.mini)
                .focusable()
                .focused($focusedTarget, equals: .followSelection)
                .accessibilityIdentifier("agent-follow-selection")

                Spacer()
                Text("\(agent.contextPaths.count) 个上下文")
                    .font(.system(size: 9))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.24), in: RoundedRectangle(cornerRadius: 12))
    }

    private func contextChip(_ item: KiteAgentContextPath, index: Int) -> some View {
        let isPrimary = item.path == agent.primaryPath
        return HStack(spacing: 5) {
            Text("\(index)")
                .font(.system(size: 9, weight: .semibold))
                .frame(width: 18, height: 18)
                .background(
                    isPrimary ? Color.accentColor.opacity(0.18) : Color.primary.opacity(0.06),
                    in: RoundedRectangle(cornerRadius: 5)
                )
            Image(systemName: item.isDirectory ? "folder.fill" : "doc.text.fill")
                .font(.system(size: 9.5, weight: .semibold))
                .foregroundStyle(.secondary)
            Button {
                model.revealInBrowser(path: item.path)
            } label: {
                Text(item.displayName)
                    .font(.system(size: 10, weight: isPrimary ? .semibold : .regular))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .buttonStyle(.plain)
            .help(item.path)
            .focusable()
            .focused($focusedTarget, equals: .contextReveal(item.path))
            .accessibilityLabel("定位 \(item.path)")
            .accessibilityIdentifier("agent-context-reveal-\(item.path)")
            Button {
                agent.removePath(item.path)
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 7.5, weight: .bold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(.tertiary)
            .help("从上下文移除")
            .focusable()
            .focused($focusedTarget, equals: .contextRemove(item.path))
            .accessibilityLabel("从上下文移除 \(item.path)")
            .accessibilityIdentifier("agent-context-remove-\(item.path)")
        }
        .padding(.horizontal, 7)
        .frame(height: 27)
        .background(
            isPrimary ? Color.accentColor.opacity(0.10) : Color.primary.opacity(0.045),
            in: RoundedRectangle(cornerRadius: 8)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .strokeBorder(
                    isPrimary ? Color.accentColor.opacity(0.34) : Color.primary.opacity(0.06),
                    lineWidth: 0.6
                )
        }
        .frame(maxWidth: 240)
    }

    private var unconfiguredCard: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "info.circle.fill")
                .font(.system(size: 12))
                .foregroundStyle(Color.accentColor)
            VStack(alignment: .leading, spacing: 3) {
                Text("尚未配置 Agent")
                    .font(.system(size: 11.5, weight: .semibold))
                Text("上下文和提示已经准备好。接入 Agent 后才会启用发送和流式输出。")
                    .font(.system(size: 9.5))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(10)
        .background(Color.accentColor.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
    }

    private var promptCard: some View {
        VStack(spacing: 8) {
            ZStack(alignment: .topLeading) {
                if agent.prompt.isEmpty {
                    Text("让 Agent 修改代码，使用 @ 引用文件，或输入 / 命令")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                        .padding(8)
                        .allowsHitTesting(false)
                }
                promptEditor
            }
            .padding(2)
            .background(Color(nsColor: .controlBackgroundColor).opacity(0.35), in: RoundedRectangle(cornerRadius: 9))
            .onChange(of: focusedTarget) { keyboardFocus.target = $0 }
            .background(
                KiteAgentKeyboardMonitor { keyCode, modifiers in
                    switch keyCode {
                    case 48:
                        guard let target = keyboardFocus.target else { return false }
                        moveFocus(from: target, forward: !modifiers.contains(.shift))
                        return true
                    case 36, 49:
                        guard let target = keyboardFocus.target else { return false }
                        switch target {
                        case let .contextReveal(path):
                            model.revealInBrowser(path: path)
                            return true
                        case let .contextRemove(path):
                            agent.removePath(path)
                            return true
                        case let .permission(requestId, actionId):
                            guard let request = agent.permissionRequests.first(where: { $0.requestId == requestId }),
                                  let action = KiteAgentPermissionPresentation.actions(for: request)
                                      .first(where: { $0.id == actionId })
                            else {
                                return false
                            }
                            agent.respondPermission(request, decision: action.decision)
                            return true
                        default:
                            return false
                        }
                    default:
                        return false
                    }
                }
            )
            .accessibilityLabel("Agent 问题或指令")

            HStack(spacing: 8) {
                Button {
                    model.presentAgentReviewFile()
                } label: {
                    Image(systemName: "doc.badge.gearshape")
                        .font(.system(size: 11, weight: .medium))
                }
                .buttonStyle(.plain)
                .help("审阅结构化 Agent 结果并确认写入")
                .focusable()
                .focused($focusedTarget, equals: .reviewResult)
                .accessibilityLabel("审阅结果")
                .accessibilityIdentifier("agent-review-result")

                Button {
                    KiteSettingsWindowController.open(pane: .agent)
                } label: {
                    Image(systemName: "gearshape")
                        .font(.system(size: 11, weight: .medium))
                }
                .buttonStyle(.plain)
                .help("Agent 设置")
                .focusable()
                .focused($focusedTarget, equals: .settings)
                .accessibilityLabel("Agent 设置")
                .accessibilityIdentifier("agent-settings")

                Spacer()

                if agent.phase == .loading || agent.phase == .running {
                    Button("停止") {
                        agent.cancelTask()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .keyboardShortcut(".", modifiers: .command)
                    .focusable()
                    .focused($focusedTarget, equals: .stop)
                    .accessibilityIdentifier("agent-stop")
                } else if agent.canRetry {
                    Button("重试") {
                        agent.retryPrompt()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .focusable()
                    .focused($focusedTarget, equals: .retry)
                    .accessibilityIdentifier("agent-retry")
                }

                Button {
                    agent.sendPrompt()
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 11, weight: .bold))
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(!agent.canSend)
                .focusable()
                .focused($focusedTarget, equals: .send)
                .accessibilityLabel("发送")
                .accessibilityIdentifier("agent-send")
            }
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.24), in: RoundedRectangle(cornerRadius: 12))
    }

    @ViewBuilder
    private var promptEditor: some View {
        if #available(macOS 14.0, *) {
            TextEditor(text: $agent.prompt)
                .font(.system(size: 11))
                .scrollContentBackground(.hidden)
                .frame(minHeight: 72, maxHeight: 132)
                .focused($focusedTarget, equals: .prompt)
                .accessibilityIdentifier("agent-prompt-input")
                .onKeyPress(phases: .down) { keyPress in
                    guard keyPress.key == .return else { return .ignored }
                    if keyPress.modifiers.contains(.shift) {
                        return .ignored
                    }
                    agent.sendPrompt()
                    return .handled
                }
        } else {
            TextField(
                "让 Agent 修改代码，使用 @ 引用文件，或输入 / 命令",
                text: $agent.prompt,
                axis: .vertical
            )
            .textFieldStyle(.plain)
            .font(.system(size: 11))
            .lineLimit(3...6)
            .focused($focusedTarget, equals: .prompt)
            .accessibilityIdentifier("agent-prompt-input")
            .onSubmit {
                agent.sendPrompt()
            }
        }
    }

    private var showSubmittedPrompt: Bool {
        let prompt = agent.lastSubmittedPrompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty else { return false }
        return agent.transcriptMessages
            .last(where: { $0.role == .user })?
            .content
            .trimmingCharacters(in: .whitespacesAndNewlines) != prompt
    }

    private var showCurrentOutput: Bool {
        let output = agent.output.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !output.isEmpty else { return false }
        return agent.transcriptMessages
            .last(where: { $0.role == .assistant })?
            .content
            .trimmingCharacters(in: .whitespacesAndNewlines) != output
    }

    private var outputCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            if showSubmittedPrompt {
                userBubble(agent.lastSubmittedPrompt)
            }
            ForEach(agent.transcriptMessages, id: \.id) { message in
                messageBubble(message)
            }
            if showCurrentOutput {
                assistantBubble(agent.output)
            }
            if !showSubmittedPrompt && !showCurrentOutput && agent.transcriptMessages.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "sparkles")
                        .font(.system(size: 22, weight: .semibold))
                        .foregroundStyle(Color.accentColor)
                    Text("开始一段新的 Agent 对话")
                        .font(.system(size: 13, weight: .semibold))
                    Text("输入任务，或使用 @ 引用文件和 / 命令。")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 48)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .id("agent-output-bottom")
    }

    @ViewBuilder
    private func messageBubble(_ message: AgentMessage) -> some View {
        switch message.role {
        case .user:
            userBubble(message.content)
        case .assistant:
            assistantBubble(message.content)
        case .system:
            Text(message.content)
                .font(.system(size: 9.5))
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
        }
    }

    private func userBubble(_ content: String) -> some View {
        HStack {
            Spacer(minLength: 40)
            Text(content)
                .font(.system(size: 11))
                .textSelection(.enabled)
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
        }
    }

    private func assistantBubble(_ content: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "sparkles")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Color.accentColor)
                .frame(width: 25, height: 25)
                .background(Color.accentColor.opacity(0.12), in: Circle())
            Group {
                if let rendered = try? AttributedString(
                    markdown: content,
                    options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
                ) {
                    Text(rendered)
                } else {
                    Text(content)
                }
            }
            .font(.system(size: 11))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var permissionCards: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(agent.permissionRequests.enumerated()), id: \.offset) { _, request in
                VStack(alignment: .leading, spacing: 7) {
                    Text(request.title.isEmpty ? "Agent 请求权限" : request.title)
                        .font(.system(size: 11.5, weight: .semibold))
                    Text(request.description)
                        .font(.system(size: 9.5))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    if let notice = request.policyNotice, !notice.isEmpty {
                        Text(notice)
                            .font(.system(size: 9.5))
                            .foregroundStyle(.orange)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 82), spacing: 7)],
                        alignment: .leading,
                        spacing: 7
                    ) {
                        ForEach(KiteAgentPermissionPresentation.actions(for: request)) { action in
                            if action.isPrimary {
                                Button(action.title) {
                                    agent.respondPermission(request, decision: action.decision)
                                }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                .disabled(!action.enabled)
                                .focusable()
                                .focused($focusedTarget, equals: .permission(request.requestId, action.id))
                                .accessibilityIdentifier("agent-permission-\(action.id)")
                            } else {
                                Button(action.title) {
                                    agent.respondPermission(request, decision: action.decision)
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                                .disabled(!action.enabled)
                                .focusable()
                                .focused($focusedTarget, equals: .permission(request.requestId, action.id))
                                .accessibilityIdentifier("agent-permission-\(action.id)")
                            }
                        }
                    }
                }
                .padding(10)
                .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
                .overlay {
                    RoundedRectangle(cornerRadius: 12)
                        .strokeBorder(Color.orange.opacity(0.25), lineWidth: 0.6)
                }
            }
        }
    }

    private func errorBanner(_ error: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                Text(error)
                    .font(.system(size: 10))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 7) {
                if agent.canRetry {
                    Button("重试") {
                        agent.retryPrompt()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                Button("Agent 设置") {
                    KiteSettingsWindowController.open(pane: .agent)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.red.opacity(0.07), in: RoundedRectangle(cornerRadius: 10))
    }
    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "sparkles")
                .font(.system(size: 23, weight: .light))
                .foregroundStyle(Color.accentColor)
                .frame(width: 52, height: 52)
                .background(Color.accentColor.opacity(0.10), in: Circle())
            Text("从文件或文件夹开始")
                .font(.system(size: 12.5, weight: .semibold))
            Text("在任意项目上右键并选择“使用 Agent”。Kite 会先展示上下文，不会自动发送。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button {
                    model.presentAgentReviewFile()
                } label: {
                    Label("审阅已有结果", systemImage: "doc.badge.gearshape")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button {
                    KiteSettingsWindowController.open(pane: .agent)
                } label: {
                    Label("打开 Agent 设置", systemImage: "gearshape")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 14)
        .padding(.vertical, 42)
    }

    private var errorState: some View {
        VStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 23, weight: .light))
                .foregroundStyle(.red)
            Text("无法准备 Agent 上下文")
                .font(.system(size: 12.5, weight: .semibold))
            Text(agent.notice ?? "请刷新后重试。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 14)
        .padding(.vertical, 42)
    }

    private func color(for phase: KiteAgentSessionPhase) -> Color {
        switch phase {
        case .running: return .green
        case .error: return .red
        case .loading: return .accentColor
        case .empty, .unconfigured, .ended: return .secondary
        }
    }
}

struct KiteAgentWindowView: View {
    @ObservedObject var model: KiteModel
    @EnvironmentObject private var agent: KiteAgentPanelModel

    var body: some View {
        ZStack {
            KiteGlassBackdrop()
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Image(systemName: "sparkles")
                        .foregroundStyle(Color.accentColor)
                    Text("Kite Agent")
                        .font(.system(size: 13, weight: .semibold))
                    Text("与右侧 Agent 栏共享同一上下文")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                    Text("关闭窗口会保留会话")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.tertiary)
                    Spacer()
                }
                .padding(.horizontal, 14)
                .frame(height: 46)
                .kiteGlass(.panel, cornerRadius: KiteTheme.controlRadius)
                .padding(12)

                KiteAgentPanelContent(model: model)
            }
        }
        .frame(minWidth: 440, minHeight: 560)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Kite Agent 独立窗口")
    }
}

@MainActor
final class KiteAgentWindowController: NSWindowController, NSWindowDelegate {
    static let shared = KiteAgentWindowController()

    private weak var model: KiteModel?
    private var agent: KiteAgentPanelModel?

    var isVisible: Bool {
        window?.isVisible == true
    }

    func show(model: KiteModel, agent: KiteAgentPanelModel) {
        self.model = model
        self.agent = agent
        if window == nil {
            let root = KiteAgentWindowView(model: model).environmentObject(agent)
            let hosting = NSHostingController(rootView: root)
            let preferredSize = agent.preferredWindowSize
            let agentWindow = NSWindow(
                contentRect: NSRect(
                    x: 0,
                    y: 0,
                    width: preferredSize.width,
                    height: preferredSize.height
                ),
                styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                backing: .buffered,
                defer: false
            )
            agentWindow.title = "Kite Agent"
            agentWindow.titlebarAppearsTransparent = true
            agentWindow.minSize = NSSize(width: 440, height: 560)
            agentWindow.isReleasedWhenClosed = false
            agentWindow.contentViewController = hosting
            agentWindow.delegate = self
            agentWindow.center()
            agentWindow.setFrameAutosaveName("KiteAgentWindow")
            window = agentWindow
        }
        if window?.isMiniaturized == true {
            window?.deminiaturize(nil)
        }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow else { return }
        let width = UInt32(max(CGFloat(1), window.frame.width.rounded()))
        let height = UInt32(max(CGFloat(1), window.frame.height.rounded()))
        agent?.persistWindowState(width: width, height: height)
    }
}

private struct KiteAgentScrollWheelMonitor: NSViewRepresentable {
    let onScrollUp: () -> Void

    func makeNSView(context: Context) -> KiteAgentScrollWheelMonitorView {
        let view = KiteAgentScrollWheelMonitorView()
        view.onScrollUp = onScrollUp
        return view
    }

    func updateNSView(_ nsView: KiteAgentScrollWheelMonitorView, context: Context) {
        nsView.onScrollUp = onScrollUp
    }
}

private final class KiteAgentScrollWheelMonitorView: NSView {
    var onScrollUp: (() -> Void)?
    private var monitor: Any?

    override func hitTest(_ point: NSPoint) -> NSView? {
        nil
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        removeMonitor()
        guard window != nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] event in
            guard let self,
                  let window = self.window,
                  event.window === window,
                  event.scrollingDeltaY > 0.1 else {
                return event
            }
            let point = self.convert(event.locationInWindow, from: nil)
            if self.bounds.contains(point) {
                self.onScrollUp?()
            }
            return event
        }
    }

    deinit {
        removeMonitor()
    }

    private func removeMonitor() {
        if let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
    }
}
