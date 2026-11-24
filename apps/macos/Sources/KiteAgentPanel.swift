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

@MainActor\nfinal class KiteAgentPanelModel: ObservableObject {
    @Published private(set) var contextPaths: [KiteAgentContextPath] = []
    @Published private(set) var primaryPath: String?
    @Published private(set) var workingDirectory = ""
    @Published var contextMode: KiteAgentContextMode = .fixed
    @Published private(set) var phase: KiteAgentSessionPhase = .empty
    @Published var prompt = ""
    @Published private(set) var presentationToken = 0
    @Published private(set) var notice: String?
    @Published private(set) var profile: AgentProfile?
    @Published private(set) var output = ""
    @Published private(set) var errorMessage: String?
    @Published private(set) var permissionRequests: [AgentPermissionRequest] = []
    @Published private(set) var canCancel = false

    private let core = KiteCore()
    private var runtime: AgentRuntime?
    private var pollTask: Task<Void, Never>?
    private var conversationId = ""
    private var restoredMessages: [AgentMessage] = []
    private var turnId: UInt64 = 0
    private var sentPrompt = ""
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
        profile != nil && hasContext && phase != .loading && phase != .running
    }

    var preferredWindowSize: NSSize {
        NSSize(width: CGFloat(windowWidth), height: CGFloat(windowHeight))
    }

    func present(
        action: KiteAgentAction,
        primary: String,
        entries: [BrowserEntry],
        workingDirectory: String
    ) {
        guard FileManager.default.fileExists(atPath: primary) else {
            contextPaths = []
            primaryPath = nil
            workingDirectory = ""
            phase = .error
            notice = "Agent 上下文目标已失效，请刷新后重试"
            presentationToken += 1
            return
        }
        let paths = normalizedPaths(primary: primary, entries: entries)
        guard let first = paths.first else { return }
        contextPaths = paths
        primaryPath = primary
        self.workingDirectory = resolvedWorkingDirectory(
            primary: first,
            fallback: workingDirectory
        )
        contextMode = .fixed
        refreshProfile()
        phase = profile == nil ? .unconfigured : .ended
        prompt = action.prompt
        output = ""
        errorMessage = nil
        permissionRequests = []
        savedTerminalSnapshot = false
        sentPrompt = ""
        conversationId = core.newAgentConversationId()
        restoredMessages = []
        turnId = 0
        notice = nil
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
        workingDirectory = path
        contextMode = .fixed
        refreshProfile()
        phase = profile == nil ? .unconfigured : .ended
        prompt = action.prompt
        output = ""
        errorMessage = nil
        permissionRequests = []
        savedTerminalSnapshot = false
        sentPrompt = ""
        conversationId = core.newAgentConversationId()
        restoredMessages = []
        turnId = 0
        notice = nil
        presentationToken += 1
    }

    func updateFollowedSelection(entries: [BrowserEntry], currentDirectory: String) {
        guard contextMode == .followSelection, let first = entries.first else { return }
        let paths = entries.map {
            KiteAgentContextPath(path: $0.path, isDirectory: $0.isDirectory)
        }
        contextPaths = paths
        primaryPath = first.path
        workingDirectory = resolvedWorkingDirectory(
            primary: paths[0],
            fallback: currentDirectory
        )
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
            workingDirectory = resolvedWorkingDirectory(primary: first, fallback: "")
            phase = .unconfigured
            notice = "已添加上下文路径"
        }
    }

    func removePath(_ path: String) {
        contextPaths.removeAll { $0.path == path }
        if primaryPath == path {
            primaryPath = contextPaths.first?.path
        }
        if contextPaths.isEmpty {
            primaryPath = nil
            workingDirectory = ""
            phase = .empty
        }
        notice = "已从 Agent 上下文移除"
    }

    func setFollowSelection(
        _ followsSelection: Bool,
        entries: [BrowserEntry],
        currentDirectory: String
    ) {
        contextMode = followsSelection ? .followSelection : .fixed
        if followsSelection {
            updateFollowedSelection(entries: entries, currentDirectory: currentDirectory)
        }
    }

    func sendPrompt() {
        guard let profile, hasContext else {
            phase = .unconfigured
            notice = "请先配置 Agent 并准备上下文"
            return
        }
        let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            notice = "提示不能为空"
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
            sentPrompt = trimmed
            output = ""
            errorMessage = nil
            permissionRequests = []
            savedTerminalSnapshot = false
            phase = .loading
            notice = "Agent 已启动"
            startPolling(runtime)
        } catch {
            phase = .error
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
                if self.phase == .ended || self.phase == .error {
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
        output = snapshot.output
        errorMessage = snapshot.error
        permissionRequests = snapshot.permissionRequests
        canCancel = snapshot.state == .running || snapshot.state == .waitingPermission
    }

    private func refreshProfile() {
        let loaded = core.agentConfig()
        if let id = loaded.store.defaultProfileId,
           let profile = loaded.store.profiles.first(where: { $0.id == id }) {
            self.profile = profile
        } else {
            profile = loaded.store.profiles.first
        }
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
        workingDirectory = conversation.workingDirectory
        contextMode = conversation.followSelection ? .followSelection : .fixed
        prompt = conversation.draft
        output = conversation.messages.last(where: { $0.role == .assistant })?.content ?? ""
        conversationId = conversation.id
        restoredMessages = conversation.messages
        turnId = 0
        windowWidth = conversation.windowWidth
        windowHeight = conversation.windowHeight
        if restore.workingDirectoryMissing || !restore.missingPaths.isEmpty {
            phase = .error
            errorMessage = "恢复的 Agent 上下文包含失效路径，请检查后重试"
            notice = errorMessage
        } else {
            phase = .ended
            notice = primaryPath == nil
                ? "已恢复上次 Agent 会话；请重新选择上下文"
                : "已恢复上次 Agent 会话"
        }
    }

    private func persistConversation() {
        guard let profile, hasContext else { return }
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
        guard let profile, hasContext else { return }
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

    private func resolvedWorkingDirectory(
        primary: KiteAgentContextPath,
        fallback: String
    ) -> String {
        if primary.isDirectory {
            return primary.path
        }
        let parent = URL(fileURLWithPath: primary.path).deletingLastPathComponent().path
        return fallback.isEmpty ? parent : fallback
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
            ScrollView {
                KiteAgentPanelContent(model: model)
                .padding(12)
            }
            .scrollIndicators(.hidden)
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

private struct KiteAgentPanelContent: View {
    @ObservedObject var model: KiteModel
    @EnvironmentObject private var agent: KiteAgentPanelModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if agent.hasContext {
                statusCard
                contextCard
                if agent.phase == .unconfigured {
                    unconfiguredCard
                }
                if let error = agent.errorMessage {
                    errorBanner(error)
                }
                if !agent.output.isEmpty {
                    outputCard
                }
                if !agent.permissionRequests.isEmpty {
                    permissionCards
                }
                promptCard
            } else if agent.phase == .error {
                errorState
            } else {
                emptyState
            }
        }
    }

    private var statusCard: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 9) {
                Image(systemName: "sparkles")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 30, height: 30)
                    .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))

                VStack(alignment: .leading, spacing: 2) {
                    Text(agent.profileName)
                        .font(.system(size: 12.5, weight: .semibold))
                    Text("\(agent.modelLabel) · \(agent.permissionLabel)")
                        .font(.system(size: 9.5))
                        .foregroundStyle(.secondary)
                }

                Spacer(minLength: 4)

                HStack(spacing: 4) {
                    Circle()
                        .fill(color(for: agent.phase))
                        .frame(width: 7, height: 7)
                    Text(agent.phase.label)
                        .font(.system(size: 9.5, weight: .medium))
                        .foregroundStyle(color(for: agent.phase))
                }
            }

            VStack(alignment: .leading, spacing: 3) {
                Text("工作目录")
                    .font(.system(size: 9.5))
                    .foregroundStyle(.tertiary)
                Text(agent.workingDirectory)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }

            if let notice = agent.notice {
                Text(notice)
                    .font(.system(size: 9.5))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.32), in: RoundedRectangle(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(Color.primary.opacity(0.06), lineWidth: 0.5)
        }
    }

    private var contextCard: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                Text("上下文")
                    .font(.system(size: 10, weight: .semibold))
                Text("\(agent.contextPaths.count) 个")
                    .font(.system(size: 9.5))
                    .foregroundStyle(.tertiary)
                Spacer()
                Button {
                    let paths = KitePlatform.chooseContextPaths(prompt: "添加到 Agent 上下文")
                    agent.addPaths(paths)
                } label: {
                    Label("添加", systemImage: "plus")
                        .font(.system(size: 10, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.accentColor)
                .accessibilityLabel("添加上下文路径")
            }

            ScrollView(.horizontal) {
                HStack(spacing: 5) {
                    ForEach(agent.contextPaths) { item in
                        contextChip(item)
                    }
                }
            }
            .scrollIndicators(.hidden)

            Toggle(
                isOn: Binding(
                    get: { agent.contextMode == .followSelection },
                    set: { follows in
                        agent.setFollowSelection(
                            follows,
                            entries: model.selectedColumnEntries,
                            currentDirectory: model.currentPath
                        )
                    }
                )
            ) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(agent.contextMode.title)
                        .font(.system(size: 10.5, weight: .medium))
                    Text(
                        agent.contextMode == .followSelection
                            ? "切换目录或选择时先更新上下文；发送前仍需确认。"
                            : "这份快照不会因切换目录而改变。"
                    )
                    .font(.system(size: 9.5))
                    .foregroundStyle(.secondary)
                }
            }
            .toggleStyle(.switch)
            .controlSize(.small)
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.25), in: RoundedRectangle(cornerRadius: 12))
    }

    private func contextChip(_ item: KiteAgentContextPath) -> some View {
        let isPrimary = item.path == agent.primaryPath
        return HStack(spacing: 4) {
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
            .accessibilityLabel("定位 \(item.path)")
            Button {
                agent.removePath(item.path)
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 7.5, weight: .bold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(.tertiary)
            .help("从上下文移除")
            .accessibilityLabel("从上下文移除 \(item.path)")
        }
        .padding(.horizontal, 8)
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
        .frame(maxWidth: 230)
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
        VStack(alignment: .leading, spacing: 8) {
            Text("发送准备")
                .font(.system(size: 10, weight: .semibold))
            TextField("添加问题或指令。内容会先停留在 Kite 中。", text: $agent.prompt, axis: .vertical)
                .textFieldStyle(.plain)
                .font(.system(size: 11))
                .lineLimit(3...6)
                .padding(8)
                .background(Color(nsColor: .controlBackgroundColor).opacity(0.35), in: RoundedRectangle(cornerRadius: 9))
                .accessibilityLabel("Agent 问题或指令")
            HStack {
                Button {
                    model.presentAgentReviewFile()
                } label: {
                    Label("审阅结果", systemImage: "doc.badge.gearshape")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("审阅结构化 Agent 结果并确认写入")
                Button {
                    KiteSettingsWindowController.open(pane: .agent)
                } label: {
                    Label("Agent 设置", systemImage: "gearshape")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Spacer()
                if agent.phase == .loading || agent.phase == .running {
                    Button("停止") {
                        agent.cancelTask()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                Button("发送") {
                    agent.sendPrompt()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(!agent.canSend)
            }
            Text("发送前请确认 Agent、模型、工作目录和权限。Kite 不会自动执行命令或应用修改。")
                .font(.system(size: 9.5))
                .foregroundStyle(.secondary)
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.25), in: RoundedRectangle(cornerRadius: 12))
    }

    private var outputCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("回答")
                    .font(.system(size: 10, weight: .semibold))
                Spacer()
                Button {
                    agent.copyOutput()
                } label: {
                    Label("复制", systemImage: "doc.on.doc")
                        .font(.system(size: 10, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.accentColor)
            }
            ScrollView {
                Group {
                    if let rendered = try? AttributedString(
                        markdown: agent.output,
                        options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
                    ) {
                        Text(rendered)
                    } else {
                        Text(agent.output)
                    }
                }
                .font(.system(size: 11))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            }
            .frame(minHeight: 80, maxHeight: 280)
            .padding(8)
            .background(
                Color(nsColor: .controlBackgroundColor).opacity(0.30),
                in: RoundedRectangle(cornerRadius: 9)
            )
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.25), in: RoundedRectangle(cornerRadius: 12))
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
                    HStack(spacing: 7) {
                        Button("仅本次允许") {
                            agent.respondPermission(request, decision: .allowOnce)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        if request.options.contains(where: {
                            $0.kind.contains("always") || $0.kind.contains("task")
                        }) {
                            Button("本任务允许") {
                                agent.respondPermission(request, decision: .allowTask)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }
                        Button("拒绝") {
                            agent.respondPermission(request, decision: .rejectOnce)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        Button("取消任务") {
                            agent.respondPermission(request, decision: .cancel)
                        }
                        .buttonStyle(.plain)
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(.secondary)
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
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
            Text(error)
                .font(.system(size: 10))
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
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

                ScrollView {
                    KiteAgentPanelContent(model: model)
                        .padding(.horizontal, 12)
                        .padding(.bottom, 12)
                }
                .scrollIndicators(.hidden)
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
