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

final class KiteAgentPanelModel: ObservableObject {
    @Published private(set) var contextPaths: [KiteAgentContextPath] = []
    @Published private(set) var primaryPath: String?
    @Published private(set) var workingDirectory = ""
    @Published var contextMode: KiteAgentContextMode = .fixed
    @Published private(set) var phase: KiteAgentSessionPhase = .empty
    @Published var prompt = ""
    @Published private(set) var presentationToken = 0
    @Published private(set) var notice: String?

    var hasContext: Bool {
        primaryPath != nil && !contextPaths.isEmpty
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
            self.workingDirectory = ""
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
        phase = .unconfigured
        prompt = action.prompt
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
        phase = .unconfigured
        prompt = action.prompt
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
            var existing = Set(contextPaths.map(\.path))
            let unique = added.filter { existing.insert($0.path).inserted }
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
                if agent.phase != .unconfigured {
                    phaseNoticeCard
                }
                promptCard
            } else if agent.phase == .error {
                errorState
            } else {
                emptyState
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
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
                    Text(statusTitle)
                        .font(.system(size: 12.5, weight: .semibold))
                    Text(statusDetail)
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

    @ViewBuilder
    private var phaseNoticeCard: some View {
        if agent.phase == .error || agent.phase == .loading || agent.phase == .ended {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: agent.phase == .error ? "exclamationmark.triangle.fill" : "info.circle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(agent.phase == .error ? Color.red : Color.accentColor)
                VStack(alignment: .leading, spacing: 3) {
                    Text(agent.phase.label)
                        .font(.system(size: 11.5, weight: .semibold))
                    if let notice = agent.notice {
                        Text(notice)
                            .font(.system(size: 9.5))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .padding(10)
            .background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 12))
        }
    }

    private var statusTitle: String {
        switch agent.phase {
        case .empty: return "等待上下文"
        case .unconfigured: return "未配置 Agent"
        case .loading: return "正在连接 Agent"
        case .running: return "Agent"
        case .error: return "Agent 连接失败"
        case .ended: return "Agent 已结束"
        }
    }

    private var statusDetail: String {
        switch agent.phase {
        case .unconfigured, .empty:
            return "模型 — · 只读 · Kite 宿主"
        case .loading:
            return "模型 — · 只读 · 正在握手"
        case .running:
            return "模型 — · 只读 · 会话运行中"
        case .error:
            return "模型 — · 只读 · 需要处理"
        case .ended:
            return "模型 — · 只读 · 会话已结束"
        }
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
                Button("发送") {}
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(true)
                    .help("发送和流式输出由下一阶段接入")
            }
            Text("当前阶段只准备上下文和提示，不会启动外部 Agent。")
                .font(.system(size: 9.5))
                .foregroundStyle(.secondary)
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.25), in: RoundedRectangle(cornerRadius: 12))
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
        if let current = self.agent, current !== agent {
            window?.close()
            window = nil
        }
        self.model = model
        self.agent = agent
        if window == nil {
            let root = KiteAgentWindowView(model: model).environmentObject(agent)
            let hosting = NSHostingController(rootView: root)
            let agentWindow = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 520, height: 760),
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
}
