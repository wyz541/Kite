import AppKit
import SwiftUI

enum KiteAppearance: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: return "跟随系统"
        case .light: return "浅色"
        case .dark: return "深色"
        }
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}

enum KiteSettingsPane: String, CaseIterable, Identifiable {
    case general
    case appearance
    case layout
    case fileHandling
    case agent
    case updates
    case about

    var id: String { rawValue }

    var title: String {
        switch self {
        case .general: return "通用"
        case .appearance: return "外观"
        case .layout: return "布局"
        case .fileHandling: return "文件处理"
        case .agent: return "Agent"
        case .updates: return "更新"
        case .about: return "关于"
        }
    }

    var subtitle: String {
        switch self {
        case .general: return "启动和窗口恢复"
        case .appearance: return "主题和界面外观"
        case .layout: return "侧边栏、预览和栏位"
        case .fileHandling: return "默认文件查看器"
        case .agent: return "连接、权限与默认 Agent"
        case .updates: return "版本、更新与诊断"
        case .about: return "Kite 版本信息"
        }
    }

    var systemImage: String {
        switch self {
        case .general: return "gearshape.fill"
        case .appearance: return "paintpalette.fill"
        case .layout: return "rectangle.split.3x1.fill"
        case .fileHandling: return "folder.fill"
        case .agent: return "sparkles"
        case .updates: return "arrow.triangle.2.circlepath"
        case .about: return "info.circle.fill"
        }
    }

    var tint: Color {
        switch self {
        case .general: return .secondary
        case .appearance: return KiteTheme.brandOrange
        case .layout: return .blue
        case .fileHandling: return .cyan
        case .agent: return KiteTheme.brandOrange
        case .updates: return .green
        case .about: return .purple
        }
    }
}

extension Notification.Name {
    static let kiteSelectSettingsPane = Notification.Name("KiteSelectSettingsPane")
}

enum KiteSettingsWindowController {
    private static var pendingPane: KiteSettingsPane?

    static func open(pane: KiteSettingsPane? = nil) {
        if let pane {
            pendingPane = pane
            NotificationCenter.default.post(name: .kiteSelectSettingsPane, object: nil)
        }
        NSApp.activate(ignoringOtherApps: true)

        if let appMenu = NSApp.mainMenu?.items.first?.submenu,
           let item = appMenu.items.first(where: {
               $0.title == "设置…" || $0.title == "Settings…" || $0.title == "Preferences…"
           }),
           let action = item.action {
            NSApp.sendAction(action, to: item.target, from: item)
            return
        }

        if NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil) {
            return
        }
        _ = NSApp.sendAction(Selector(("showPreferencesWindow:")), to: nil, from: nil)
    }

    static func consumePendingPane() -> KiteSettingsPane? {
        defer { pendingPane = nil }
        return pendingPane
    }
}

struct KiteSettingsView: View {
    @ObservedObject var model: KiteModel
    @State private var selection: KiteSettingsPane = .general
    @State private var agentStore: AgentStoreLoad?
    @State private var agentDiagnostics: AgentDiagnostics?
    @State private var agentTestResult: AgentConnectionInfo?
    @State private var agentTestingId: String?
    @State private var agentStatus: String?

    var body: some View {
        HStack(spacing: 0) {
            settingsSidebar
            Divider()
            settingsDetail
        }
        .frame(minWidth: 760, idealWidth: 760, minHeight: 560, idealHeight: 560)
        .background(.regularMaterial)
        .onAppear {
            applyPendingPane()
        }
        .onReceive(NotificationCenter.default.publisher(for: .kiteSelectSettingsPane)) { _ in
            applyPendingPane()
        }
    }

    private var settingsSidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 11) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .interpolation(.high)
                    .frame(width: 34, height: 34)

                VStack(alignment: .leading, spacing: 1) {
                    Text("Kite")
                        .font(.system(size: 14, weight: .semibold))
                    Text("设置")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 18)
            .padding(.top, 18)
            .padding(.bottom, 16)

            VStack(spacing: 4) {
                ForEach(KiteSettingsPane.allCases) { pane in
                    settingsSidebarRow(pane)
                }
            }
            .padding(.horizontal, 10)

            Spacer(minLength: 20)

            Text("Kite \(KiteSettingsView.appVersion)")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
                .padding(.horizontal, 18)
                .padding(.bottom, 14)
        }
        .frame(width: 220)
        .frame(maxHeight: .infinity)
        .background(Color(nsColor: .windowBackgroundColor).opacity(0.22))
    }

    private func settingsSidebarRow(_ pane: KiteSettingsPane) -> some View {
        Button {
            selection = pane
        } label: {
            HStack(spacing: 10) {
                ZStack {
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(pane.tint.opacity(selection == pane ? 0.22 : 0.13))
                    Image(systemName: pane.systemImage)
                        .font(.system(size: 12.5, weight: .semibold))
                        .symbolRenderingMode(.hierarchical)
                        .foregroundStyle(pane.tint)
                }
                .frame(width: 24, height: 24)

                Text(pane.title)
                    .font(.system(size: 13, weight: selection == pane ? .semibold : .medium))
                    .foregroundStyle(.primary)

                Spacer(minLength: 0)
            }
            .padding(.horizontal, 9)
            .frame(height: 36)
            .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .background {
                if selection == pane {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(Color.accentColor.opacity(0.13))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .stroke(Color.accentColor.opacity(0.12), lineWidth: 1)
                        }
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(pane.title)
        .accessibilityAddTraits(selection == pane ? .isSelected : [])
    }

    @ViewBuilder
    private var settingsDetail: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                settingsHeader

                switch selection {
                case .general:
                    generalSettings
                case .appearance:
                    appearanceSettings
                case .layout:
                    layoutSettings
                case .fileHandling:
                    fileHandlingSettings
                case .agent:
                    agentSettings
                case .updates:
                    updateSettings
                case .about:
                    aboutSettings
                }
            }
            .padding(.horizontal, 30)
            .padding(.top, 26)
            .padding(.bottom, 30)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.08))
    }

    private var settingsHeader: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(selection.title)
                .font(.system(size: 22, weight: .semibold))
            Text(selection.subtitle)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
        }
    }

    private func applyPendingPane() {
        if let pane = KiteSettingsWindowController.consumePendingPane() {
            selection = pane
        }
    }

    private var generalSettings: some View {
        settingsSection("启动") {
            SettingsCard {
                SettingsRow(
                    title: "恢复上次会话",
                    detail: "重新打开 Kite 时恢复页签和目录。"
                ) {
                    Toggle("", isOn: restoreSessionBinding)
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .controlSize(.small)
                }
                SettingsDivider()
                SettingsRow(
                    title: "定时获取 Git 更新",
                    detail: "每 15 分钟在后台获取当前仓库，不会自动合并。"
                ) {
                    Toggle("", isOn: gitAutoFetchBinding)
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .controlSize(.small)
                }
            }
        } footer: {
            Text("关闭会话恢复后，Kite 下次启动时打开个人文件夹。定时获取默认关闭。")
        }
    }

    private var gitAutoFetchBinding: Binding<Bool> {
        Binding(
            get: { model.gitAutoFetchEnabled },
            set: { model.setGitAutoFetchEnabled($0) }
        )
    }

    private var appearanceSettings: some View {
        settingsSection("主题") {
            SettingsCard {
                SettingsRow(
                    title: "外观",
                    detail: "选择 Kite 窗口使用的明暗外观。"
                ) {
                    Picker("", selection: appearanceBinding) {
                        ForEach(KiteAppearance.allCases) { appearance in
                            Text(appearance.title).tag(appearance)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.segmented)
                    .frame(width: 230)
                }
            }
        } footer: {
            Text("Kite 的所有窗口会立即应用相同外观。")
        }
    }

    private var layoutSettings: some View {
        VStack(alignment: .leading, spacing: 24) {
            settingsSection("窗口组件") {
                SettingsCard {
                    SettingsRow(
                        title: "显示侧边栏",
                        detail: "显示位置、收藏和磁盘。"
                    ) {
                        Toggle("", isOn: sidebarBinding)
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .controlSize(.small)
                    }
                    SettingsDivider()
                    SettingsRow(
                        title: "显示右侧栏",
                        detail: "显示预览或 Agent 上下文表面。"
                    ) {
                        Toggle("", isOn: previewBinding)
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .controlSize(.small)
                    }
                    SettingsDivider()
                    SettingsRow(
                        title: "显示详细信息列",
                        detail: "在活动栏显示日期、类型和大小。"
                    ) {
                        Toggle("", isOn: columnDetailsBinding)
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .controlSize(.small)
                    }
                }
            }

            settingsSection("导航") {
                SettingsCard {
                    SettingsRow(
                        title: "路径栏",
                        detail: "使用 Command+L 编辑当前路径。"
                    ) {
                        Text("⌘L")
                            .font(.system(size: 11, weight: .medium, design: .rounded))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(Color.primary.opacity(0.06), in: RoundedRectangle(cornerRadius: 6))
                    }
                }
            }
        }
    }

    private var fileHandlingSettings: some View {
        VStack(alignment: .leading, spacing: 24) {
            settingsSection("默认文件查看器") {
                SettingsCard {
                    SettingsRow(
                        title: KitePlatform.canSetDefaultFolderHandler
                            ? "使用 Kite 打开文件夹"
                            : "使用 Kite 处理 Finder 显示请求",
                        detail: KitePlatform.canSetDefaultFolderHandler
                            ? "Kite 成为文件夹的默认应用。"
                            : "macOS 26 及更高版本保留 Finder 作为默认文件夹应用。"
                    ) {
                        Button("设为默认…") {
                            model.setAsDefaultFileViewer()
                        }
                        .controlSize(.small)
                    }
                    SettingsDivider()
                    SettingsRow(
                        title: "恢复 Finder",
                        detail: "撤销 Kite 写入的默认文件查看器设置。"
                    ) {
                        Button("恢复…") {
                            model.restoreFinderAsDefaultFileViewer()
                        }
                        .controlSize(.small)
                    }
                }
            } footer: {
                Text("更改后，Finder 和 Kite 会继续处理各自负责的系统请求。")
            }
        }
    }

    private var agentSettings: some View {
        VStack(alignment: .leading, spacing: 24) {
            settingsSection("已配置 Agent") {
                SettingsCard {
                    let profiles = agentStore?.store.profiles ?? []
                    if profiles.isEmpty {
                        SettingsRow(
                            title: "尚未配置 Agent",
                            detail: "从预设添加，或通过配置文件接入自定义 ACP Agent。"
                        ) {
                            EmptyView()
                        }
                    } else {
                        ForEach(Array(profiles.enumerated()), id: \.element.id) { index, profile in
                            if index > 0 {
                                SettingsDivider()
                            }
                            agentProfileRow(profile)
                        }
                    }
                }
            } footer: {
                Text("测试连接会启动一次 ACP 握手。认证与模型权限仍由外部 Agent 自身管理。")
            }

            settingsSection("添加预设") {
                SettingsCard {
                    let presets = model.core.agentPresets()
                    ForEach(Array(presets.enumerated()), id: \.element.id) { index, preset in
                        if index > 0 {
                            SettingsDivider()
                        }
                        SettingsRow(
                            title: preset.name,
                            detail: "\(preset.description) · \(preset.installHint)"
                        ) {
                            Button("添加") {
                                addAgentPreset(preset.id)
                            }
                            .controlSize(.small)
                        }
                    }
                }
            }

            if let result = agentTestResult {
                settingsSection("连接测试") {
                    SettingsCard {
                        VStack(alignment: .leading, spacing: 5) {
                            Text(result.success ? "连接成功" : "连接失败")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(result.success ? .green : .red)
                            Text(
                                result.success
                                    ? "\(result.agentName) \(result.agentVersion) · 协议 v\(result.protocolVersion) · \(result.latencyMs) ms"
                                    : (result.error ?? "未知错误")
                            )
                            .font(.system(size: 10.5))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 12)
                    }
                }
            }

            settingsSection("诊断") {
                SettingsCard {
                    SettingsRow(
                        title: "Agent 配置",
                        detail: agentStore?.configPath ?? "无法读取配置路径"
                    ) {
                        Button("在 Finder 中显示") {
                            revealAgentPath(agentStore?.configPath)
                        }
                        .controlSize(.small)
                    }
                    SettingsDivider()
                    SettingsRow(
                        title: "Agent 诊断日志",
                        detail: agentDiagnostics?.logPath ?? "尚未生成日志"
                    ) {
                        HStack(spacing: 7) {
                            Button("显示") {
                                revealAgentPath(agentDiagnostics?.logPath)
                            }
                            .controlSize(.small)
                            Button("清除") {
                                clearAgentLogs()
                            }
                            .controlSize(.small)
                        }
                    }
                    if let profiles = agentDiagnostics?.profiles, !profiles.isEmpty {
                        SettingsDivider()
                        VStack(alignment: .leading, spacing: 7) {
                            Text("Agent 状态")
                                .font(.system(size: 11.5, weight: .medium))
                            ForEach(profiles, id: \.profileId) { profile in
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("\(profile.profileName) · \(profile.status)")
                                        .font(.system(size: 11))
                                    Text(
                                        "\(profile.protocol) · \(profile.version ?? "版本待测试") · \(profile.capabilities.count) 项能力"
                                    )
                                    .font(.system(size: 10))
                                    .foregroundStyle(.secondary)
                                    Text(profile.resolvedCommand ?? profile.command)
                                        .font(.system(size: 10, design: .monospaced))
                                        .foregroundStyle(.secondary)
                                        .textSelection(.enabled)
                                }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                    }
                    let recentErrors = (agentDiagnostics?.recentEntries ?? [])
                        .filter { $0.level == "error" || $0.level == "warning" }
                    if !recentErrors.isEmpty {
                        SettingsDivider()
                        VStack(alignment: .leading, spacing: 7) {
                            Text("最近错误")
                                .font(.system(size: 11.5, weight: .medium))
                                .foregroundStyle(.red)
                            ForEach(
                                Array(recentErrors.suffix(8).enumerated()),
                                id: \.offset
                            ) { _, entry in
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("\(entry.event) · \(entry.level)")
                                        .font(.system(size: 10.5, weight: .semibold))
                                        .foregroundStyle(.red)
                                    Text(entry.message)
                                        .font(.system(size: 10))
                                        .foregroundStyle(.secondary)
                                        .textSelection(.enabled)
                                }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                    }
                    if let warning = agentStore?.warning {
                        SettingsDivider()
                        VStack(alignment: .leading, spacing: 4) {
                            Text("配置警告")
                                .font(.system(size: 11.5, weight: .medium))
                                .foregroundStyle(.red)
                            Text(warning)
                                .font(.system(size: 10.5))
                                .foregroundStyle(.red)
                                .textSelection(.enabled)
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                    }
                }
            } footer: {
                Text("日志会遮盖常见 token、密码和 URL 内嵌凭据，不记录提示词正文。")
            }

            if let status = agentStatus {
                Text(status)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
    }

    private func agentProfileRow(_ profile: AgentProfile) -> some View {
        let isDefault = agentStore?.store.defaultProfileId == profile.id
        let isTesting = agentTestingId == profile.id
        return SettingsRow(
            title: profile.name + (isDefault ? " · 默认" : ""),
            detail: "\(profile.executable ?? "未设置命令") · \(profile.model ?? "Agent 默认模型") · \(agentPermissionLabel(profile.permission))"
        ) {
            HStack(spacing: 7) {
                if !isDefault {
                    Button("设为默认") {
                        setAgentDefault(profile.id)
                    }
                    .controlSize(.small)
                }
                Button {
                    testAgentProfile(profile)
                } label: {
                    if isTesting {
                        ProgressView()
                            .controlSize(.small)
                    } else {
                        Text("测试连接")
                    }
                }
                .controlSize(.small)
                .disabled(agentTestingId != nil)
                if !isDefault, (agentStore?.store.profiles.count ?? 0) > 1 {
                    Button(role: .destructive) {
                        deleteAgentProfile(profile.id)
                    } label: {
                        Image(systemName: "trash")
                    }
                    .controlSize(.small)
                    .help("删除 Agent")
                }
            }
        }
    }

    private func reloadAgentSettings() {
        agentStore = model.core.agentConfig()
        agentDiagnostics = model.core.agentDiagnostics()
    }

    private func saveAgentStore(_ store: AgentProfileSnapshot) {
        do {
            let loaded = try model.core.saveAgentConfig(store: store)
            agentStore = loaded
            agentDiagnostics = model.core.agentDiagnostics()
            agentStatus = "Agent 配置已保存"
        } catch {
            agentStatus = error.localizedDescription
        }
    }

    private func addAgentPreset(_ presetId: String) {
        guard var profile = model.core.agentPresetProfile(presetId: presetId),
              var store = agentStore?.store else { return }
        let existingNames = Set(store.profiles.map { $0.name.lowercased() })
        if existingNames.contains(profile.name.lowercased()) {
            for index in 2...100 {
                let candidate = "\(profile.name) \(index)"
                if !existingNames.contains(candidate.lowercased()) {
                    profile.name = candidate
                    break
                }
            }
            profile.id = "\(profile.id)-\(Int(Date().timeIntervalSince1970 * 1_000))"
        }
        store.profiles.append(profile)
        if store.defaultProfileId == nil {
            store.defaultProfileId = profile.id
        }
        saveAgentStore(store)
    }

    private func setAgentDefault(_ profileId: String) {
        guard var store = agentStore?.store else { return }
        store.defaultProfileId = profileId
        saveAgentStore(store)
    }

    private func deleteAgentProfile(_ profileId: String) {
        guard var store = agentStore?.store else { return }
        store.profiles.removeAll { $0.id == profileId }
        if store.defaultProfileId == profileId {
            store.defaultProfileId = store.profiles.first?.id
        }
        saveAgentStore(store)
    }

    private func testAgentProfile(_ profile: AgentProfile) {
        agentTestingId = profile.id
        agentTestResult = nil
        agentStatus = "正在测试 \(profile.name)…"
        let core = model.core
        Task { @MainActor in
            let result = await Task.detached(priority: .userInitiated) {
                core.testAgentConnection(profile: profile)
            }.value
            agentTestingId = nil
            agentTestResult = result
            agentDiagnostics = core.agentDiagnostics()
            agentStatus = result.success
                ? "连接成功：\(result.agentName) \(result.agentVersion)"
                : (result.error ?? "Agent 连接测试失败")
        }
    }

    private func clearAgentLogs() {
        do {
            try model.core.clearAgentDiagnostics()
            agentDiagnostics = model.core.agentDiagnostics()
            agentStatus = "Agent 诊断日志已清除"
        } catch {
            agentStatus = error.localizedDescription
        }
    }

    private func revealAgentPath(_ path: String?) {
        guard let path, !path.isEmpty else { return }
        let url = URL(fileURLWithPath: path)
        let target = FileManager.default.fileExists(atPath: path) ? url : url.deletingLastPathComponent()
        NSWorkspace.shared.activateFileViewerSelecting([target])
    }

    private func agentPermissionLabel(_ mode: PermissionPolicy) -> String {
        switch mode {
        case .readOnly: return "只读"
        case .confirmWrites: return "写入前确认"
        case .agentManaged: return "由 Agent 自身管理"
        }
    }
    private var updateSettings: some View {
        VStack(alignment: .leading, spacing: 24) {
            settingsSection("版本") {
                SettingsCard {
                    SettingsRow(
                        title: "当前版本",
                        detail: "Kite \(KiteSettingsView.appVersion)"
                    ) {
                        Button {
                            model.checkForUpdates()
                        } label: {
                            HStack(spacing: 6) {
                                if model.updateInProgress {
                                    ProgressView()
                                        .controlSize(.small)
                                } else {
                                    Image(systemName: "arrow.triangle.2.circlepath")
                                }
                                Text("检查更新…")
                            }
                        }
                        .controlSize(.small)
                        .disabled(model.updateInProgress)
                    }

                    if let latestVersion = model.updateLatestVersion {
                        SettingsDivider()
                        SettingsRow(
                            title: "Kite \(latestVersion) 可用",
                            detail: "当前版本：\(KiteSettingsView.appVersion)"
                        ) {
                            HStack(spacing: 8) {
                                Button("查看发布说明") {
                                    model.openAvailableUpdateReleaseNotes()
                                }
                                .controlSize(.small)

                                Button("下载并安装") {
                                    model.installAvailableUpdate()
                                }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                .disabled(model.updateInProgress)
                            }
                        }

                        if let notes = model.updateReleaseNotes, !notes.isEmpty {
                            SettingsDivider()
                            VStack(alignment: .leading, spacing: 6) {
                                Text("更新内容")
                                    .font(.system(size: 12, weight: .medium))
                                Text(notes)
                                    .font(.system(size: 10.5))
                                    .foregroundStyle(.secondary)
                                    .textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .padding(.horizontal, 16)
                            .padding(.vertical, 12)
                        }
                    }

                    if model.updateDownload != nil
                        || model.updateLatestVersion == nil
                        || model.errorMessage != nil,
                       let message = model.updateStatusMessage {
                        SettingsDivider()
                        if let download = model.updateDownload,
                           let fraction = download.fraction {
                            SettingsRow(title: message, detail: nil) {
                                VStack(alignment: .trailing, spacing: 4) {
                                    ProgressView(value: fraction)
                                        .frame(width: 150)
                                    Text(download.summaryText)
                                        .font(.system(size: 10.5, design: .monospaced))
                                        .foregroundStyle(.secondary)
                                }
                            }
                        } else if let error = model.errorMessage,
                                  model.updateLatestVersion != nil {
                            SettingsRow(title: message, detail: error) {
                                EmptyView()
                            }
                        } else {
                            SettingsRow(title: message, detail: nil) {
                                if model.updateInProgress {
                                    ProgressView()
                                        .controlSize(.small)
                                }
                            }
                        }
                    }
                }
            }

            settingsSection("诊断") {
                SettingsCard {
                    SettingsRow(
                        title: "更新诊断日志",
                        detail: "查看下载、校验和安装记录。"
                    ) {
                        Button("在 Finder 中显示") {
                            revealUpdateLog()
                        }
                        .controlSize(.small)
                    }
                }
            } footer: {
                Text("Kite 只在用户选择“检查更新…”后访问更新服务。")
            }
        }
    }

    private var aboutSettings: some View {
        VStack(alignment: .leading, spacing: 24) {
            HStack(spacing: 18) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .interpolation(.high)
                    .frame(width: 76, height: 76)

                VStack(alignment: .leading, spacing: 5) {
                    Text("Kite")
                        .font(.system(size: 24, weight: .semibold))
                    Text("版本 \(KiteSettingsView.appVersion)")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                    Text("原生 macOS 文件导航器")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.vertical, 4)

            settingsSection("产品") {
                SettingsCard {
                    SettingsRow(
                        title: "导航先于保管",
                        detail: "Kite 把文件系统显示为可牵引的层级路径。"
                    ) {
                        EmptyView()
                    }
                    SettingsDivider()
                    SettingsRow(
                        title: "系统集成",
                        detail: "Kite 支持 Finder 文件剪贴板、快速预览和分享菜单。"
                    ) {
                        EmptyView()
                    }
                    SettingsDivider()
                    SettingsRow(
                        title: "本地优先",
                        detail: "Kite 不上传文件内容，也不收集浏览记录。"
                    ) {
                        EmptyView()
                    }
                }
            }

            Text("MIT License · Kite \(KiteSettingsView.appVersion)")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
        }
    }

    private func settingsSection<Content: View, Footer: View>(
        _ title: String,
        @ViewBuilder content: () -> Content,
        @ViewBuilder footer: () -> Footer
    ) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
                .tracking(0.5)
            content()
            footer()
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func settingsSection<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        settingsSection(title, content: content, footer: { EmptyView() })
    }

    private var restoreSessionBinding: Binding<Bool> {
        Binding(
            get: {
                UserDefaults.standard.object(forKey: KiteModel.restoreSessionDefaultsKey) as? Bool ?? true
            },
            set: {
                UserDefaults.standard.set($0, forKey: KiteModel.restoreSessionDefaultsKey)
            }
        )
    }

    private var appearanceBinding: Binding<KiteAppearance> {
        Binding(
            get: {
                KiteAppearance(rawValue: UserDefaults.standard.string(forKey: KiteModel.appearanceDefaultsKey) ?? "")
                    ?? .system
            },
            set: {
                UserDefaults.standard.set($0.rawValue, forKey: KiteModel.appearanceDefaultsKey)
            }
        )
    }

    private var sidebarBinding: Binding<Bool> {
        Binding(
            get: { UserDefaults.standard.object(forKey: "kite.macos.sidebar-visible") as? Bool ?? true },
            set: { UserDefaults.standard.set($0, forKey: "kite.macos.sidebar-visible") }
        )
    }

    private var previewBinding: Binding<Bool> {
        Binding(
            get: { UserDefaults.standard.object(forKey: "kite.macos.preview-visible") as? Bool ?? true },
            set: { UserDefaults.standard.set($0, forKey: "kite.macos.preview-visible") }
        )
    }

    private var columnDetailsBinding: Binding<Bool> {
        Binding(
            get: { UserDefaults.standard.object(forKey: "kite.macos.column-details-visible") as? Bool ?? false },
            set: { UserDefaults.standard.set($0, forKey: "kite.macos.column-details-visible") }
        )
    }

    private func revealUpdateLog() {
        let fileManager = FileManager.default
        guard let support = try? fileManager.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return
        }
        let updates = support
            .appendingPathComponent("Kite", isDirectory: true)
            .appendingPathComponent("updates", isDirectory: true)
        try? fileManager.createDirectory(at: updates, withIntermediateDirectories: true)
        let log = updates.appendingPathComponent("update.log")
        if fileManager.fileExists(atPath: log.path) {
            NSWorkspace.shared.activateFileViewerSelecting([log])
        } else {
            NSWorkspace.shared.open(updates)
        }
    }

    static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0"
    }
}

private struct SettingsCard<Content: View>: View {
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(spacing: 0) {
            content()
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.52))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Color.primary.opacity(0.075), lineWidth: 1)
        }
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

private struct SettingsDivider: View {
    var body: some View {
        Divider()
            .padding(.leading, 16)
    }
}

private struct SettingsRow<Trailing: View>: View {
    let title: String
    let detail: String?
    @ViewBuilder let trailing: () -> Trailing

    init(
        title: String,
        detail: String?,
        @ViewBuilder trailing: @escaping () -> Trailing
    ) {
        self.title = title
        self.detail = detail
        self.trailing = trailing
    }

    var body: some View {
        HStack(alignment: .center, spacing: 18) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail {
                    Text(detail)
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 12)
            trailing()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(minHeight: 54)
    }
}
