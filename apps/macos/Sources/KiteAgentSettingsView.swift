import AppKit
import Combine
import SwiftUI

extension Notification.Name {
    static let kiteAgentProfilesDidChange = Notification.Name("kite.agent.profiles-did-change")
}

@MainActor
final class KiteAgentSettingsStore: ObservableObject {
    @Published private(set) var profiles: [AgentProfile] = []
    @Published private(set) var defaultProfileID: String?
    @Published private(set) var warning: String?
    @Published private(set) var statusMessage: String?
    @Published private(set) var tests: [String: AgentConnectionResult] = [:]
    @Published private(set) var testingIDs: Set<String> = []
    @Published private(set) var error: String?
    @Published private(set) var transportCapabilities: [AgentTransportCapabilities] = []

    private var core: KiteCore?

    func attach(core: KiteCore) {
        self.core = core
        reload()
    }

    func reload() {
        guard let core else { return }
        transportCapabilities = core.agentTransportCapabilities()
        apply(core.agentConfig())
    }

    func clearError() { error = nil }
    func clearStatus() { statusMessage = nil }
    func showError(_ message: String) { error = message }

    func save(_ draft: AgentProfileDraft) throws {
        guard let core else { throw AgentDraftError.message("Agent Core 尚未准备好") }
        let profile = try draft.makeProfile()
        let credential = draft.credential.agentTrimmed.agentNilIfEmpty
        apply(try core.saveAgentProfile(
            profile: profile,
            credential: credential,
            clearCredential: draft.clearCredential && credential == nil
        ))
        statusMessage = "已保存 Agent 配置"
        NotificationCenter.default.post(name: .kiteAgentProfilesDidChange, object: nil)
    }

    func setDefault(_ profile: AgentProfile) {
        guard let core else { return }
        do {
            apply(try core.setDefaultAgentProfile(profileId: profile.id))
            statusMessage = "默认 Agent 已更新"
            NotificationCenter.default.post(name: .kiteAgentProfilesDidChange, object: nil)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func delete(_ profile: AgentProfile, replacementDefaultID: String? = nil) {
        guard let core else { return }
        do {
            apply(try core.deleteAgentProfile(
                profileId: profile.id,
                replacementDefaultId: replacementDefaultID
            ))
            tests[profile.id] = nil
            statusMessage = "已删除 \(profile.name)"
            NotificationCenter.default.post(name: .kiteAgentProfilesDidChange, object: nil)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func isTesting(_ profile: AgentProfile) -> Bool {
        testingIDs.contains(profile.id)
    }

    func testAndRecord(_ profile: AgentProfile) async {
        guard !testingIDs.contains(profile.id) else { return }
        testingIDs.insert(profile.id)
        let result = await test(profile: profile, credential: nil)
        tests[profile.id] = result
        testingIDs.remove(profile.id)
    }

    func test(profile: AgentProfile, credential: String?) async -> AgentConnectionResult {
        guard let core else {
            return AgentConnectionResult(
                success: false,
                status: .other,
                message: "Agent Core 尚未准备好",
                agentName: nil,
                agentVersion: nil,
                protocolVersion: nil,
                capabilities: [],
                authMethods: [],
                elapsedMs: 0,
                sessionSupported: false,
                profileSaveSupported: false
            )
        }
        return await Task.detached(priority: .userInitiated) {
            core.testAgentConnection(profile: profile, credential: credential)
        }.value
    }

    var defaultProfile: AgentProfile? {
        guard let defaultProfileID else { return nil }
        return profiles.first { $0.id == defaultProfileID }
    }

    func capability(for transport: AgentTransport) -> AgentTransportCapabilities? {
        transportCapabilities.first { $0.transport == transport }
    }

    func canStartSession(_ transport: AgentTransport) -> Bool {
        capability(for: transport)?.sessionSupported == true
    }

    func canStartSession(_ profile: AgentProfile) -> Bool {
        canStartSession(profile.transport)
    }

    func canSaveProfile(_ transport: AgentTransport) -> Bool {
        capability(for: transport)?.profileSaveSupported == true
    }

    func canTestConnection(_ transport: AgentTransport) -> Bool {
        capability(for: transport)?.connectionTestSupported == true
    }

    func transportLabel(for transport: AgentTransport, includeDiagnosticSuffix: Bool = true) -> String {
        guard let capability = capability(for: transport) else {
            return transport.label
        }
        if includeDiagnosticSuffix && !capability.profileSaveSupported {
            return "\(capability.label)（仅测试）"
        }
        return capability.label
    }

    private func apply(_ snapshot: AgentProfileSnapshot) {
        profiles = snapshot.profiles
        defaultProfileID = snapshot.defaultProfileId
        warning = snapshot.warning?.agentMessage
    }
}

private enum AgentDraftError: LocalizedError {
    case message(String)

    var errorDescription: String? {
        switch self {
        case .message(let message): return message
        }
    }
}

enum AgentWorkingDirectoryChoice: String, CaseIterable, Identifiable {
    case selectedItem
    case fileParent
    case repositoryRoot
    case fixed
    case ask

    var id: String { rawValue }

    var label: String {
        switch self {
        case .selectedItem: return "所选项目"
        case .fileParent: return "文件所在目录"
        case .repositoryRoot: return "Git 仓库根目录"
        case .fixed: return "固定目录"
        case .ask: return "每次询问"
        }
    }

    init(policy: WorkingDirectoryPolicy) {
        switch policy {
        case .selectedItem: self = .selectedItem
        case .fileParent: self = .fileParent
        case .repositoryRoot: self = .repositoryRoot
        case .fixed(_): self = .fixed
        case .ask: self = .ask
        }
    }

    func policy(fixedPath: String?) -> WorkingDirectoryPolicy {
        switch self {
        case .selectedItem: return .selectedItem
        case .fileParent: return .fileParent
        case .repositoryRoot: return .repositoryRoot
        case .fixed: return .fixed(path: fixedPath ?? "")
        case .ask: return .ask
        }
    }
}

struct AgentProfileDraft: Identifiable, Equatable {
    var id: String
    var name: String
    var transport: AgentTransport
    var executable: String
    var endpoint: String
    var argumentsText: String
    var environmentText: String
    var model: String
    var workingDirectoryChoice: AgentWorkingDirectoryChoice
    var workingDirectory: String
    var permission: PermissionPolicy
    var capabilities: AgentCapabilities
    var timeoutText: String
    var outputLimitKiBText: String
    var credentialEnvVar: String
    var credential: String
    var clearCredential: Bool
    var existingCredentialRef: String?
    var isNew: Bool

    var hasStoredCredential: Bool { existingCredentialRef != nil }

    init(profile: AgentProfile, isNew: Bool = false) {
        id = profile.id
        name = profile.name
        transport = profile.transport
        executable = profile.executable ?? ""
        endpoint = profile.endpoint ?? ""
        argumentsText = profile.arguments.joined(separator: "\n")
        environmentText = profile.environment.map { "\($0.name)=\($0.value)" }.joined(separator: "\n")
        model = profile.model ?? ""
        workingDirectoryChoice = AgentWorkingDirectoryChoice(policy: profile.workingDirectory)
        if case .fixed(let path) = profile.workingDirectory {
            workingDirectory = path
        } else {
            workingDirectory = ""
        }
        permission = profile.permission
        capabilities = profile.capabilities
        timeoutText = String(profile.timeoutSeconds)
        outputLimitKiBText = String(profile.outputLimitBytes / 1024)
        credentialEnvVar = profile.credentialEnvVar ?? ""
        credential = ""
        clearCredential = false
        existingCredentialRef = profile.credentialRef
        self.isNew = isNew
    }

    static func new(core: KiteCore, transport: AgentTransport) -> AgentProfileDraft {
        AgentProfileDraft(
            profile: AgentProfile(
                id: core.newAgentProfileId(),
                name: transport == .acpStdio ? "ACP Agent" : (transport == .acpHttp ? "ACP HTTP Agent" : "CLI Agent"),
                transport: transport,
                executable: transport == .acpHttp ? nil : "",
                endpoint: nil,
                arguments: [],
                environment: [],
                model: nil,
                workingDirectory: .ask,
                permission: transport == .cli ? .agentManaged : .readOnly,
                timeoutSeconds: 30,
                outputLimitBytes: 512 * 1024,
                credentialRef: nil,
                credentialEnvVar: nil,
                capabilities: AgentCapabilities.defaultCapabilities
            ),
            isNew: true
        )
    }

    static func duplicate(_ profile: AgentProfile, core: KiteCore) -> AgentProfileDraft {
        var draft = AgentProfileDraft(profile: profile, isNew: true)
        draft.id = core.newAgentProfileId()
        draft.name = "\(profile.name) 副本"
        draft.existingCredentialRef = nil
        draft.credential = ""
        draft.clearCredential = false
        return draft
    }

    func makeProfile() throws -> AgentProfile {
        guard let timeout = UInt32(timeoutText.agentTrimmed), timeout > 0 else {
            throw AgentDraftError.message("超时必须是大于 0 的整数秒")
        }
        guard let outputKiB = UInt64(outputLimitKiBText.agentTrimmed), outputKiB > 0 else {
            throw AgentDraftError.message("输出上限必须是大于 0 的整数 KiB")
        }
        let (outputBytes, overflow) = outputKiB.multipliedReportingOverflow(by: 1024)
        guard !overflow else { throw AgentDraftError.message("输出上限过大") }
        let isHTTP = transport == .acpHttp
        let environment: [AgentEnvironmentVariable] = isHTTP ? [] : try parseEnvironment(environmentText)
        let arguments: [String] = isHTTP ? [] : argumentsText.components(separatedBy: .newlines)
            .map { $0.agentTrimmed }
            .filter { !$0.isEmpty }
        let fixedPath = workingDirectory.agentTrimmed.agentNilIfEmpty
        if workingDirectoryChoice == .fixed && fixedPath == nil {
            throw AgentDraftError.message("固定工作目录策略需要选择目录")
        }
        return AgentProfile(
            id: id,
            name: name.agentTrimmed,
            transport: transport,
            executable: isHTTP ? nil : executable.agentTrimmed.agentNilIfEmpty,
            endpoint: isHTTP ? endpoint.agentTrimmed.agentNilIfEmpty : nil,
            arguments: isHTTP ? [] : arguments,
            environment: isHTTP ? [] : environment,
            model: isHTTP ? nil : model.agentTrimmed.agentNilIfEmpty,
            workingDirectory: workingDirectoryChoice.policy(fixedPath: fixedPath),
            permission: permission,
            timeoutSeconds: timeout,
            outputLimitBytes: outputBytes,
            credentialRef: clearCredential && credential.agentTrimmed.isEmpty
                ? nil
                : existingCredentialRef,
            credentialEnvVar: credentialEnvVar.agentTrimmed.agentNilIfEmpty,
            capabilities: capabilities
        )
    }

    private func parseEnvironment(_ text: String) throws -> [AgentEnvironmentVariable] {
        var entries: [AgentEnvironmentVariable] = []
        for (index, rawLine) in text.components(separatedBy: .newlines).enumerated() {
            let line = rawLine.agentTrimmed
            if line.isEmpty || line.hasPrefix("#") { continue }
            guard let separator = line.firstIndex(of: "=") else {
                throw AgentDraftError.message("环境变量第 \(index + 1) 行缺少 =")
            }
            let key = String(line[..<separator]).agentTrimmed
            let value = String(line[line.index(after: separator)...])
            entries.append(AgentEnvironmentVariable(name: key, value: value))
        }
        return entries
    }
}

private extension AgentCapabilities {
    static let defaultCapabilities = AgentCapabilities(
        readSelection: true,
        readWorkspace: false,
        writeFiles: false,
        executeCommands: false,
        network: false,
        permissionRequests: true
    )
}

private extension AgentConfigWarning {
    var agentMessage: String {
        switch self {
        case .corruptConfig(let message):
            return "Agent 配置损坏：\(message)"
        case .unknownSchemaVersion(let found, let supported):
            return "不支持的 Agent 配置版本：\(found)（当前为 \(supported)）"
        case .configTooLarge(let bytes, let maxBytes):
            return "Agent 配置过大：\(bytes) 字节（上限 \(maxBytes) 字节）"
        case .invalidProfile(let field, let message):
            return "Agent 配置字段 \(field) 无效：\(message)"
        case .storageUnavailable(let message):
            return "Agent 配置存储不可用：\(message)"
        case .io(let message):
            return "Agent 配置读写失败：\(message)"
        case .migrationApplied(let fromVersion, let toVersion):
            return "Agent 配置已从版本 \(fromVersion) 迁移到 \(toVersion)"
        }
    }
}

@MainActor
struct KiteAgentSettingsView: View {
    let core: KiteCore
    @StateObject private var store = KiteAgentSettingsStore()
    @State private var editorDraft: AgentProfileDraft?
    @State private var deleteTarget: AgentProfile?

    init(model: KiteModel) { core = model.core }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            header
            if let warning = store.warning {
                noticeRow(warning, systemImage: "exclamationmark.triangle.fill", color: .orange)
            }
            if let status = store.statusMessage {
                noticeRow(status, systemImage: "checkmark.circle.fill", color: .green)
                    .onTapGesture { store.clearStatus() }
            }
            if store.profiles.isEmpty {
                emptyState
            } else {
                defaultAgentSection
                profilesSection
            }
            presetsSection
            diagnosticsSection
            Text("Token 或密码只写入 macOS 钥匙串；agent.json 仅保存 keychain:// 引用。只有 ACP / JSON-RPC stdio 可保存为可运行配置。")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .onAppear { store.attach(core: core) }
        .sheet(item: $editorDraft) { draft in
            KiteAgentEditorView(store: store, initialDraft: draft) { editorDraft = nil }
        }
        .confirmationDialog(
            deleteTarget.map { "删除 \($0.name)？" } ?? "删除 Agent？",
            isPresented: Binding(
                get: { deleteTarget != nil },
                set: { if !$0 { deleteTarget = nil } }
            ),
            titleVisibility: .visible,
            presenting: deleteTarget
        ) { profile in
            deleteButtons(for: profile)
            Button("取消", role: .cancel) { deleteTarget = nil }
        } message: { profile in
            Text(deleteMessage(for: profile))
        }
        .alert(
            "Agent 配置错误",
            isPresented: Binding(
                get: { store.error != nil },
                set: { if !$0 { store.clearError() } }
            )
        ) {
            Button("好", role: .cancel) { store.clearError() }
        } message: {
            Text(store.error ?? "")
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Agent 由外部进程提供。")
                .font(.system(size: 12, weight: .medium))
            Text("仅 ACP / JSON-RPC stdio 可保存并启动会话；ACP HTTP 和通用 CLI 只用于连接测试。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Spacer()
                Button("测试通用 CLI") { openNew(.cli) }
                    .controlSize(.small)
                Button("添加 Agent") { openNew(.acpStdio) }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 14) {
            Image(systemName: "cpu")
                .font(.system(size: 28, weight: .medium))
                .foregroundStyle(.purple)
                .frame(width: 56, height: 56)
                .background(Color.purple.opacity(0.12), in: RoundedRectangle(cornerRadius: 15))
            Text("尚未配置 Agent")
                .font(.system(size: 16, weight: .semibold))
            Text("添加一个 ACP / JSON-RPC stdio Agent。ACP HTTP 和通用 CLI 仅用于连接测试。")
                .font(.system(size: 11.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            HStack(spacing: 8) {
                Button("添加 Agent") { openNew(.acpStdio) }
                    .buttonStyle(.borderedProminent)
                Button("测试通用 CLI") { openNew(.cli) }
            }
            .controlSize(.small)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 38)
        .padding(.horizontal, 24)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.52))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Color.primary.opacity(0.075), lineWidth: 1)
        }
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private var defaultAgentSection: some View {
        VStack(alignment: .leading, spacing: 9) {
            agentSectionTitle("默认 Agent")
            if let profile = store.defaultProfile {
                VStack(alignment: .leading, spacing: 12) {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: "cpu")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.purple)
                            .frame(width: 36, height: 36)
                            .background(Color.purple.opacity(0.12), in: RoundedRectangle(cornerRadius: 9))
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 6) {
                                Text(profile.name).font(.system(size: 13, weight: .semibold))
                                agentBadge("默认", color: .accentColor)
                                agentBadge(store.transportLabel(for: profile.transport, includeDiagnosticSuffix: false), color: .purple)
                            }
                            Text(profile.connectionTarget)
                                .font(.system(size: 10.5, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Text("工作目录：\(profile.workingDirectorySummary) · 权限：\(profile.permission.label)")
                                .font(.system(size: 10.5))
                                .foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 12)
                        Button(store.isTesting(profile) ? "测试中…" : "测试连接") {
                            Task { await store.testAndRecord(profile) }
                        }
                        .controlSize(.small)
                        .disabled(store.isTesting(profile) || !store.canTestConnection(profile.transport))
                    }
                    if let result = store.tests[profile.id] {
                        KiteAgentConnectionResultView(result: result)
                    }
                }
                .padding(16)
                .background(Color.purple.opacity(0.055))
                .overlay {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .stroke(Color.purple.opacity(0.28), lineWidth: 1)
                }
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            } else {
                Text("当前没有可运行的默认 Agent。请编辑一个配置并将接入方式改为 ACP / JSON-RPC stdio。")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.orange.opacity(0.07), in: RoundedRectangle(cornerRadius: 10))
            }
        }
    }

    private var profilesSection: some View {
        VStack(alignment: .leading, spacing: 9) {
            agentSectionTitle("Agent 配置")
            VStack(spacing: 0) {
                ForEach(Array(store.profiles.enumerated()), id: \.element.id) { index, profile in
                    if index > 0 { Divider().padding(.leading, 16) }
                    agentProfileRow(profile)
                }
            }
            .background(Color(nsColor: .controlBackgroundColor).opacity(0.52))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .stroke(Color.primary.opacity(0.075), lineWidth: 1)
            }
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }

    private func agentProfileRow(_ profile: AgentProfile) -> some View {
        let isDefault = store.defaultProfileID == profile.id
        let canRun = store.canStartSession(profile)
        let canSave = store.canSaveProfile(profile.transport)
        return VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(profile.name).font(.system(size: 13, weight: .medium))
                        if isDefault { agentBadge("默认", color: .accentColor) }
                        agentBadge(store.transportLabel(for: profile.transport, includeDiagnosticSuffix: false), color: .purple)
                        if !canRun { agentBadge("仅测试", color: .orange) }
                    }
                    Text(profile.connectionTarget)
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text("\(profile.permission.label) · \(profile.workingDirectorySummary) · \(profile.credentialRef == nil ? "无保存凭据" : "凭据已保存")")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 12)
                HStack(spacing: 6) {
                    Button(store.isTesting(profile) ? "测试中…" : "测试") {
                        Task { await store.testAndRecord(profile) }
                    }
                    .disabled(store.isTesting(profile) || !store.canTestConnection(profile.transport))
                    Button("编辑") { editorDraft = AgentProfileDraft(profile: profile) }
                    if canSave {
                        Button("复制") {
                            editorDraft = AgentProfileDraft.duplicate(profile, core: core)
                        }
                    }
                    if !isDefault && canRun { Button("设为默认") { store.setDefault(profile) } }
                    Button("删除", role: .destructive) { deleteTarget = profile }
                }
                .controlSize(.small)
            }
            if let result = store.tests[profile.id] {
                KiteAgentConnectionResultView(result: result)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

        private var presetsSection: some View {
        VStack(alignment: .leading, spacing: 9) {
            agentSectionTitle("Agent 预设")
            VStack(spacing: 0) {
                ForEach(Array(core.agentPresets().enumerated()), id: \.element.id) { index, preset in
                    if index > 0 { Divider().padding(.leading, 16) }
                    HStack(alignment: .top, spacing: 12) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(preset.name).font(.system(size: 12.5, weight: .medium))
                            Text(preset.description)
                                .font(.system(size: 10.5))
                                .foregroundStyle(.secondary)
                            Text(preset.installHint)
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer(minLength: 12)
                        Button("添加") { addPreset(preset) }
                            .controlSize(.small)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                }
            }
            .background(Color(nsColor: .controlBackgroundColor).opacity(0.52))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .stroke(Color.primary.opacity(0.075), lineWidth: 1)
            }
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            Text("预设只填写接入元数据，不覆盖 Agent 原生认证或用户配置。")
                .font(.system(size: 10.5))
                .foregroundStyle(.tertiary)
        }
    }

    private var diagnosticsSection: some View {
        let diagnostics = core.agentDiagnostics()
        return VStack(alignment: .leading, spacing: 9) {
            agentSectionTitle("诊断")
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Agent 诊断日志").font(.system(size: 12.5, weight: .medium))
                        Text(diagnostics.logPath)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 12)
                    Button("显示") {
                        NSWorkspace.shared.activateFileViewerSelecting([
                            URL(fileURLWithPath: diagnostics.logPath)
                        ])
                    }
                    .controlSize(.small)
                    Button("清除") {
                        do {
                            try core.clearAgentDiagnostics()
                            store.reload()
                        } catch {
                            store.showError(error.localizedDescription)
                        }
                    }
                    .controlSize(.small)
                }
                ForEach(diagnostics.profiles, id: \.profileId) { profile in
                    Divider()
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(profile.profileName) · \(profile.status)")
                            .font(.system(size: 11.5, weight: .medium))
                        Text("\(profile.protocol) · \(profile.version ?? "版本待测试") · \(profile.capabilities.count) 项能力")
                            .font(.system(size: 10.5))
                            .foregroundStyle(.secondary)
                        Text(profile.resolvedCommand ?? profile.command)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                ForEach(Array(diagnostics.recentEntries.reversed().filter {
                    $0.level == "error" || $0.level == "warning"
                }.prefix(8).enumerated()), id: \.offset) { _, entry in
                    Divider()
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(entry.event) · \(entry.level)")
                            .font(.system(size: 10.5, weight: .semibold))
                            .foregroundStyle(.red)
                        Text(entry.message)
                            .font(.system(size: 10.5))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
            }
            .padding(16)
            .background(Color(nsColor: .controlBackgroundColor).opacity(0.52))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .stroke(Color.primary.opacity(0.075), lineWidth: 1)
            }
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }

    private func addPreset(_ preset: AgentPreset) {
        guard let profile = core.agentPresetProfile(presetId: preset.id) else {
            store.showError("未知的 Agent 预设")
            return
        }
        var draft = AgentProfileDraft(profile: profile, isNew: true)
        let baseName = profile.name
        var suffix = 2
        while store.profiles.contains(where: {
            $0.name.compare(draft.name, options: .caseInsensitive) == .orderedSame
        }) {
            draft.name = "\(baseName) (\(suffix))"
            suffix += 1
        }
        editorDraft = draft
    }

    @ViewBuilder
    private func deleteButtons(for profile: AgentProfile) -> some View {
        let runnableReplacements = store.profiles.filter {
            $0.id != profile.id && store.canStartSession($0)
        }
        if store.defaultProfileID == profile.id && !runnableReplacements.isEmpty {
            ForEach(runnableReplacements, id: \.id) { candidate in
                Button("删除并设为 \(candidate.name)", role: .destructive) {
                    store.delete(profile, replacementDefaultID: candidate.id)
                    deleteTarget = nil
                }
            }
        } else {
            Button("删除", role: .destructive) {
                store.delete(profile)
                deleteTarget = nil
            }
        }
    }

    private func deleteMessage(for profile: AgentProfile) -> String {
        let hasRunnableReplacement = store.profiles.contains {
            $0.id != profile.id && store.canStartSession($0)
        }
        if store.defaultProfileID == profile.id && hasRunnableReplacement {
            return "这是当前默认 Agent。请选择一个可运行的新默认 Agent，然后删除。"
        }
        if store.defaultProfileID == profile.id && store.profiles.count > 1 {
            return "删除后将没有可运行的默认 Agent；其余配置只能用于连接测试。"
        }
        if store.profiles.count == 1 { return "这会让 Agent 列表变为空。" }
        return "此操作会从 agent.json 删除配置，并清理对应钥匙串条目。"
    }

    private func openNew(_ transport: AgentTransport) {
        editorDraft = AgentProfileDraft.new(core: core, transport: transport)
    }

    private func agentSectionTitle(_ title: String) -> some View {
        Text(title)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(.secondary)
            .textCase(.uppercase)
            .tracking(0.5)
    }

    private func agentBadge(_ text: String, color: Color) -> some View {
        Text(text)
            .font(.system(size: 9.5, weight: .semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.12), in: Capsule())
    }

    private func noticeRow(_ text: String, systemImage: String, color: Color) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: systemImage).foregroundStyle(color)
            Text(text)
                .font(.system(size: 10.5))
                .foregroundStyle(color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(color.opacity(0.075), in: RoundedRectangle(cornerRadius: 10))
    }
}


@MainActor
private struct KiteAgentEditorView: View {
    @ObservedObject var store: KiteAgentSettingsStore
    @State private var draft: AgentProfileDraft
    @State private var testResult: AgentConnectionResult?
    @State private var testing = false
    @State private var error: String?
    @State private var showingDiscardConfirmation = false

    private let initialDraft: AgentProfileDraft
    private let onClose: () -> Void

    init(store: KiteAgentSettingsStore, initialDraft: AgentProfileDraft, onClose: @escaping () -> Void) {
        self.store = store
        self.initialDraft = initialDraft
        _draft = State(initialValue: initialDraft)
        self.onClose = onClose
    }

    private var isDirty: Bool { draft != initialDraft }

    var body: some View {
        VStack(spacing: 0) {
            editorHeader
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    basicSection
                    workspaceSection
                    credentialSection
                    advancedSection
                    if let capability = store.capability(for: draft.transport), !capability.sessionSupported {
                        warningBox("\(capability.summary) \(capability.limitation ?? "")")
                    }
                    if let testResult { KiteAgentConnectionResultView(result: testResult) }
                    if let error { notice(error, systemImage: "exclamationmark.triangle.fill", color: .red) }
                }
                .padding(20)
            }
            Divider()
            editorFooter
        }
        .frame(minWidth: 680, idealWidth: 720, minHeight: 540, idealHeight: 620)
        .interactiveDismissDisabled(isDirty)
        .confirmationDialog(
            "放弃未保存的 Agent 更改？",
            isPresented: $showingDiscardConfirmation,
            titleVisibility: .visible
        ) {
            Button("放弃更改", role: .destructive) { onClose() }
            Button("继续编辑", role: .cancel) {}
        } message: {
            Text("这些更改尚未写入 agent.json，也没有更新系统钥匙串。")
        }
    }

    private var editorHeader: some View {
        HStack(alignment: .center, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(draft.isNew ? "添加 Agent" : "编辑 Agent")
                    .font(.system(size: 18, weight: .semibold))
                Text("测试使用当前表单值，但不会保存；只有 ACP / JSON-RPC stdio 可以保存为可运行配置。")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button { requestClose() } label: {
                Image(systemName: "xmark").font(.system(size: 12, weight: .semibold))
            }
            .buttonStyle(.borderless)
            .accessibilityLabel("关闭 Agent 表单")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 15)
    }

    private var basicSection: some View {
        editorSection("基本") {
            editorField("名称", detail: "在设置、菜单和 Agent 栏中显示；名称必须唯一。") {
                TextField("例如：本地 Agent", text: $draft.name)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Agent 名称")
            }
            AgentDivider()
            editorField("接入方式", detail: "只有 ACP / JSON-RPC stdio 可保存并启动会话；ACP HTTP 和通用 CLI 仅执行连接测试。") {
                Picker("接入方式", selection: $draft.transport) {
                    ForEach(Array(store.transportCapabilities.enumerated()), id: \.offset) { _, capability in
                        Text(store.transportLabel(for: capability.transport))
                            .tag(capability.transport)
                    }
                }
                .labelsHidden()
                .pickerStyle(.segmented)
            }
            if draft.transport == .acpHttp {
                AgentDivider()
                editorField("ACP HTTP 端点", detail: "使用绝对 HTTP 或 HTTPS URL。凭据通过系统钥匙串和 Authorization 头注入。") {
                    TextField("https://localhost:7777/acp", text: $draft.endpoint)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityLabel("ACP HTTP 端点")
                }
            } else {
                AgentDivider()
                editorField("可执行文件", detail: "使用绝对路径，或只填写 PATH 中的命令名称。参数不会经过 shell。") {
                    TextField("/usr/local/bin/agent", text: $draft.executable)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityLabel("Agent 可执行文件")
                }
                AgentDivider()
                editorField("启动参数", detail: "每行一个参数。禁止在参数中直接填写 token、密码或带凭据的 URL。") {
                    multilineEditor($draft.argumentsText, height: 64)
                        .accessibilityLabel("Agent 启动参数")
                }
                AgentDivider()
                editorField("环境变量", detail: "每行 KEY=VALUE。敏感环境变量会被拒绝，请使用凭据栏。") {
                    multilineEditor($draft.environmentText, height: 64)
                        .accessibilityLabel("Agent 环境变量")
                }
                AgentDivider()
                editorField("模型", detail: "可选。留空时由 Agent 自身配置决定。") {
                    TextField("留空时使用 Agent 自身配置", text: $draft.model)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityLabel("Agent 模型")
                }
            }
        }
    }

    private var workspaceSection: some View {
        VStack(alignment: .leading, spacing: 22) {
            editorSection("工作目录与权限") {
                editorField("工作目录策略", detail: "设置页无法获知本次选中项；固定目录会在保存时校验。") {
                    Picker("工作目录策略", selection: $draft.workingDirectoryChoice) {
                        ForEach(AgentWorkingDirectoryChoice.allCases) { choice in
                            Text(choice.label).tag(choice)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(maxWidth: 260, alignment: .leading)
                }
                if draft.workingDirectoryChoice == .fixed {
                    AgentDivider()
                    editorField("固定工作目录", detail: "保存前会确认目录存在。") {
                        HStack(spacing: 8) {
                            TextField("/path/to/project", text: $draft.workingDirectory)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityLabel("固定工作目录")
                            Button("选择…") { chooseWorkingDirectory() }
                        }
                    }
                }
                AgentDivider()
                editorField("权限摘要", detail: draft.permission.summary) {
                    Picker("权限摘要", selection: $draft.permission) {
                        Text(PermissionPolicy.readOnly.label).tag(PermissionPolicy.readOnly)
                        Text(PermissionPolicy.confirmWrites.label).tag(PermissionPolicy.confirmWrites)
                        Text(PermissionPolicy.agentManaged.label).tag(PermissionPolicy.agentManaged)
                    }
                    .labelsHidden()
                    .pickerStyle(.segmented)
                }
            }

            editorSection("能力声明") {
                editorField("上下文", detail: "这些声明用于权限摘要；普通 CLI 的实际权限仍由 Agent 自身管理。") {
                    VStack(alignment: .leading, spacing: 7) {
                        Toggle("读取选中项", isOn: $draft.capabilities.readSelection)
                        Toggle("读取工作区", isOn: $draft.capabilities.readWorkspace)
                        Toggle("请求权限确认", isOn: $draft.capabilities.permissionRequests)
                    }
                    .toggleStyle(.switch)
                    .controlSize(.small)
                }
                AgentDivider()
                editorField("写入与执行", detail: "Kite 不会因为这里的声明而绕过确认。") {
                    VStack(alignment: .leading, spacing: 7) {
                        Toggle("写入文件", isOn: $draft.capabilities.writeFiles)
                        Toggle("执行命令", isOn: $draft.capabilities.executeCommands)
                        Toggle("网络访问", isOn: $draft.capabilities.network)
                    }
                    .toggleStyle(.switch)
                    .controlSize(.small)
                }
            }
        }
    }

    private var credentialSection: some View {
        editorSection("凭据边界") {
            editorField("Token / 密码", detail: "仅用于本次测试或保存到系统钥匙串。测试不会保存表单值。") {
                SecureField("Token 或密码", text: $draft.credential)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Agent 凭据")
                credentialStatus
            }
            AgentDivider()
            editorField("凭据环境变量", detail: "启动 Agent 时把钥匙串中的凭据注入此环境变量；默认 KITE_AGENT_TOKEN。") {
                TextField("KITE_AGENT_TOKEN", text: $draft.credentialEnvVar)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("凭据环境变量")
            }
        }
    }

    private var advancedSection: some View {
        editorSection("高级") {
            editorField("超时与输出上限", detail: "超时单位为秒；输出上限单位为 KiB。ACP 响应超过上限会中止测试。") {
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("超时").font(.system(size: 10.5)).foregroundStyle(.secondary)
                        TextField("30", text: $draft.timeoutText)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("连接超时秒数")
                    }
                    VStack(alignment: .leading, spacing: 5) {
                        Text("输出上限").font(.system(size: 10.5)).foregroundStyle(.secondary)
                        TextField("512", text: $draft.outputLimitKiBText)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("输出上限 KiB")
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var credentialStatus: some View {
        if !draft.credential.agentTrimmed.isEmpty {
            Text("保存后会写入 macOS 钥匙串并替换现有凭据。")
                .font(.system(size: 10.5)).foregroundStyle(.green)
        } else if draft.clearCredential {
            HStack(spacing: 8) {
                Text("保存时会从钥匙串删除凭据。")
                    .font(.system(size: 10.5)).foregroundStyle(.orange)
                Button("撤销") { draft.clearCredential = false }
                    .buttonStyle(.link).font(.system(size: 10.5))
            }
        } else if draft.hasStoredCredential {
            HStack(spacing: 8) {
                Text("已保存到 macOS 钥匙串。留空会保留现有凭据。")
                    .font(.system(size: 10.5)).foregroundStyle(.green)
                Button("清除已保存凭据") { draft.clearCredential = true }
                    .buttonStyle(.link).font(.system(size: 10.5))
            }
        } else {
            Text("尚未保存凭据。输入后会写入 macOS 钥匙串，不会进入 agent.json。")
                .font(.system(size: 10.5)).foregroundStyle(.secondary)
        }
    }

    private var editorFooter: some View {
        HStack(spacing: 10) {
            Text("仅 ACP / JSON-RPC stdio 可保存为可运行配置；其他接入方式只执行连接测试。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
            Spacer()
            Button("取消") { requestClose() }.keyboardShortcut(.cancelAction)
            Button(testing ? "测试中…" : "测试连接") { testConnection() }
                .disabled(testing || !store.canTestConnection(draft.transport))
            Button("保存") { save() }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!store.canSaveProfile(draft.transport))
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    private func requestClose() {
        if isDirty { showingDiscardConfirmation = true } else { onClose() }
    }

    private func testConnection() {
        guard !testing else { return }
        testing = true
        testResult = nil
        error = nil
        Task {
            do {
                let profile = try draft.makeProfile()
                let credential = draft.credential.agentTrimmed.agentNilIfEmpty
                testResult = await store.test(profile: profile, credential: credential)
            } catch {
                self.error = error.localizedDescription
            }
            testing = false
        }
    }

    private func save() {
        do {
            try store.save(draft)
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func chooseWorkingDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        if panel.runModal() == .OK, let url = panel.url { draft.workingDirectory = url.path }
    }

    private func editorSection<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
                .tracking(0.5)
            VStack(spacing: 0) { content() }
                .background(Color(nsColor: .controlBackgroundColor).opacity(0.52))
                .overlay {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .stroke(Color.primary.opacity(0.075), lineWidth: 1)
                }
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }

    private func editorField<Content: View>(_ title: String, detail: String?, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.system(size: 12.5, weight: .medium))
            if let detail {
                Text(detail)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            content()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private func multilineEditor(_ text: Binding<String>, height: CGFloat) -> some View {
        TextEditor(text: text)
            .font(.system(size: 11, design: .monospaced))
            .frame(minHeight: height)
            .padding(4)
            .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
    }

    private func warningBox(_ text: String) -> some View {
        notice(text, systemImage: "exclamationmark.triangle.fill", color: .orange)
    }

    private func notice(_ text: String, systemImage: String, color: Color) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: systemImage).foregroundStyle(color)
            Text(text)
                .font(.system(size: 10.5))
                .foregroundStyle(color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(color.opacity(0.075), in: RoundedRectangle(cornerRadius: 9))
    }
}


private struct KiteAgentConnectionResultView: View {
    let result: AgentConnectionResult

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: result.status.systemImage).foregroundStyle(result.status.tint)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 7) {
                    Text(result.status.label)
                        .font(.system(size: 10.5, weight: .semibold))
                        .foregroundStyle(result.status.tint)
                    Text("\(result.elapsedMs) ms")
                        .font(.system(size: 9.5, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Text(result.detailText)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                if !result.sessionSupported {
                    Text("此接入方式仅用于连接测试，无论结果如何都不能启动 Agent 会话；请改用 ACP / JSON-RPC stdio。")
                        .font(.system(size: 9.5, weight: .medium))
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(result.status.tint.opacity(0.075), in: RoundedRectangle(cornerRadius: 8))
    }
}

private struct AgentDivider: View {
    var body: some View { Divider().padding(.leading, 16) }
}

private extension AgentTransport {
    var label: String {
        switch self {
        case .acpStdio: return "ACP / JSON-RPC"
        case .acpHttp: return "ACP HTTP"
        case .cli: return "通用 CLI"
        }
    }
}

private extension PermissionPolicy {
    var label: String {
        switch self {
        case .readOnly: return "只读"
        case .confirmWrites: return "写入前确认"
        case .agentManaged: return "由 Agent 自身管理"
        }
    }

    var summary: String {
        switch self {
        case .readOnly: return "Kite 仅提供文件上下文；不自动写入或执行命令。"
        case .confirmWrites: return "写入和命令执行需要用户确认。"
        case .agentManaged: return "权限由 Agent 自身管理；Kite 不声称已沙箱化。"
        }
    }
}

private extension WorkingDirectoryPolicy {
    var label: String {
        switch self {
        case .selectedItem: return "所选项目"
        case .fileParent: return "文件所在目录"
        case .repositoryRoot: return "Git 仓库根目录"
        case .fixed: return "固定目录"
        case .ask: return "每次询问"
        }
    }

    var fixedPath: String? {
        if case .fixed(let path) = self { return path }
        return nil
    }
}

private extension AgentProfile {
    var connectionTarget: String {
        if transport == .acpHttp {
            return endpoint ?? "未设置端点"
        }
        return executable ?? "未设置命令"
    }

    var workingDirectorySummary: String {
        workingDirectory.fixedPath ?? workingDirectory.label
    }
}

private extension AgentConnectionStatus {
    var label: String {
        switch self {
        case .connected: return "已连接"
        case .authenticationRequired: return "需要认证"
        case .notInstalled: return "未安装"
        case .incompatibleVersion: return "版本不兼容"
        case .authenticationFailed: return "认证失败"
        case .protocolError: return "协议错误"
        case .permissionDenied: return "权限不足"
        case .timeout: return "连接超时"
        case .outputTooLarge: return "输出超过限制"
        case .invalidProfile: return "配置无效"
        case .credentialStore: return "钥匙串错误"
        case .io: return "启动失败"
        case .other: return "连接失败"
        }
    }

    var tint: Color {
        switch self {
        case .connected:
            return .green
        case .authenticationRequired, .authenticationFailed, .incompatibleVersion:
            return .orange
        case .notInstalled, .protocolError, .permissionDenied, .timeout,
             .outputTooLarge, .invalidProfile, .credentialStore, .io, .other:
            return .red
        }
    }

    var systemImage: String {
        switch self {
        case .connected: return "checkmark.circle.fill"
        case .authenticationRequired, .authenticationFailed, .incompatibleVersion:
            return "exclamationmark.triangle.fill"
        default: return "xmark.octagon.fill"
        }
    }
}

private extension AgentConnectionResult {
    var detailText: String {
        var details: [String] = []
        if let agentName { details.append(agentName) }
        if let agentVersion { details.append("版本 \(agentVersion)") }
        if let protocolVersion { details.append("ACP \(protocolVersion)") }
        if !capabilities.isEmpty { details.append("能力：\(capabilities.joined(separator: "、"))") }
        if !authMethods.isEmpty { details.append("认证：\(authMethods.joined(separator: "、"))") }
        if details.isEmpty { return message }
        return "\(message) · \(details.joined(separator: " · "))"
    }
}

private extension String {
    var agentTrimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
    var agentNilIfEmpty: String? {
        let value = agentTrimmed
        return value.isEmpty ? nil : value
    }
}
