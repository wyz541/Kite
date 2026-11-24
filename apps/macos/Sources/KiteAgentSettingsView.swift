import AppKit
import Combine
import SwiftUI

extension Notification.Name {
    static let kiteAgentProfilesDidChange = Notification.Name("kite.agent.profiles-did-change")
}

@MainActor
final class KiteAgentSettingsStore: ObservableObject {
    @Published private(set) var config = AgentConfig(
        schemaVersion: 1,
        defaultProfileId: nil,
        profiles: []
    )
    @Published private(set) var warning: String?
    @Published private(set) var statusMessage: String?
    @Published private(set) var tests: [String: AgentConnectionResult] = [:]
    @Published private(set) var testingIDs: Set<String> = []
    @Published private(set) var error: String?

    private var core: KiteCore?

    func attach(core: KiteCore) {
        self.core = core
        reload()
    }

    func reload() {
        guard let core else { return }
        let snapshot = core.agentConfig()
        config = snapshot.config
        warning = snapshot.warning
    }

    func clearError() { error = nil }
    func clearStatus() { statusMessage = nil }

    func save(_ draft: AgentProfileDraft) throws {
        guard let core else { throw AgentDraftError.message("Agent Core 尚未准备好") }
        let profile = try draft.makeProfile()
        let credential = draft.credential.agentTrimmed.agentNilIfEmpty
        config = try core.saveAgentProfile(
            profile: profile,
            credential: credential,
            clearCredential: draft.clearCredential && credential == nil
        )
        warning = nil
        statusMessage = "已保存 Agent 配置"
        NotificationCenter.default.post(name: .kiteAgentProfilesDidChange, object: nil)
    }

    func setDefault(_ profile: AgentProfile) {
        guard let core else { return }
        do {
            config = try core.setDefaultAgentProfile(profileId: profile.id)
            warning = nil
            statusMessage = "默认 Agent 已更新"
            NotificationCenter.default.post(name: .kiteAgentProfilesDidChange, object: nil)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func delete(_ profile: AgentProfile, replacementDefaultID: String? = nil) {
        guard let core else { return }
        do {
            config = try core.deleteAgentProfile(
                profileId: profile.id,
                replacementDefaultId: replacementDefaultID
            )
            tests[profile.id] = nil
            warning = nil
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
                elapsedMs: 0
            )
        }
        return await Task.detached(priority: .userInitiated) {
            core.testAgentConnection(profile: profile, credential: credential)
        }.value
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

struct AgentProfileDraft: Identifiable, Equatable {
    var id: String
    var name: String
    var transport: AgentTransport
    var executable: String
    var argumentsText: String
    var environmentText: String
    var model: String
    var workingDirectoryPolicy: WorkingDirectoryPolicy
    var workingDirectory: String
    var permissionPolicy: PermissionPolicy
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
        executable = profile.executable
        argumentsText = profile.arguments.joined(separator: "\n")
        environmentText = profile.environment.map { "\($0.key)=\($0.value)" }.joined(separator: "\n")
        model = profile.model ?? ""
        workingDirectoryPolicy = profile.workingDirectoryPolicy
        workingDirectory = profile.workingDirectory ?? ""
        permissionPolicy = profile.permissionPolicy
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
                name: transport == .acpStdio ? "ACP Agent" : "CLI Agent",
                transport: transport,
                executable: "",
                endpoint: nil,
                arguments: [],
                environment: [],
                model: nil,
                workingDirectoryPolicy: .ask,
                workingDirectory: nil,
                permissionPolicy: transport == .genericCli ? .agentManaged : .readOnly,
                timeoutSeconds: 30,
                outputLimitBytes: 512 * 1024,
                credentialEnvVar: nil,
                credentialRef: nil
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
        let environment = try parseEnvironment(environmentText)
        let arguments = argumentsText.components(separatedBy: .newlines)
            .map { $0.agentTrimmed }
            .filter { !$0.isEmpty }
        let fixedDirectory = workingDirectoryPolicy == .fixed
            ? workingDirectory.agentTrimmed.agentNilIfEmpty
            : nil
        if workingDirectoryPolicy == .fixed && fixedDirectory == nil {
            throw AgentDraftError.message("固定工作目录策略需要选择目录")
        }
        return AgentProfile(
            id: id,
            name: name.agentTrimmed,
            transport: transport,
            executable: executable.agentTrimmed,
            endpoint: nil,
            arguments: arguments,
            environment: environment,
            model: model.agentTrimmed.agentNilIfEmpty,
            workingDirectoryPolicy: workingDirectoryPolicy,
            workingDirectory: fixedDirectory,
            permissionPolicy: permissionPolicy,
            timeoutSeconds: timeout,
            outputLimitBytes: outputBytes,
            credentialEnvVar: credentialEnvVar.agentTrimmed.agentNilIfEmpty,
            credentialRef: clearCredential && credential.agentTrimmed.isEmpty
                ? nil
                : existingCredentialRef
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
            entries.append(AgentEnvironmentVariable(key: key, value: value))
        }
        return entries
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
            if store.config.profiles.isEmpty {
                emptyState
            } else {
                defaultAgentSection
                profilesSection
            }
            Text("Token 或密码只写入 macOS 钥匙串；agent.json 仅保存 keychain:// 引用。通用 CLI 的权限由 Agent 自身管理，Kite 不声称完成沙箱隔离。")
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
            Text("Kite 只保存最小接入信息、工作目录策略和权限摘要，不扫描或导入其他工具配置。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Spacer()
                Button("使用 CLI 预设") { openNew(.genericCli) }
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
            Text("添加一个 ACP Agent，或先用通用 CLI 预设验证启动命令。")
                .font(.system(size: 11.5))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            HStack(spacing: 8) {
                Button("添加 Agent") { openNew(.acpStdio) }
                    .buttonStyle(.borderedProminent)
                Button("使用预设") { openNew(.genericCli) }
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
            if let profile = defaultProfile {
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
                                agentBadge(profile.transport.label, color: .purple)
                            }
                            Text(profile.executable)
                                .font(.system(size: 10.5, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Text("工作目录：\(profile.workingDirectorySummary) · 权限：\(profile.permissionPolicy.label)")
                                .font(.system(size: 10.5))
                                .foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 12)
                        Button(store.isTesting(profile) ? "测试中…" : "测试连接") {
                            Task { await store.testAndRecord(profile) }
                        }
                        .controlSize(.small)
                        .disabled(store.isTesting(profile))
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
            }
        }
    }

    private var profilesSection: some View {
        VStack(alignment: .leading, spacing: 9) {
            agentSectionTitle("Agent 配置")
            VStack(spacing: 0) {
                ForEach(Array(store.config.profiles.enumerated()), id: \.element.id) { index, profile in
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
        let isDefault = store.config.defaultProfileId == profile.id
        return VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(profile.name).font(.system(size: 13, weight: .medium))
                        if isDefault { agentBadge("默认", color: .accentColor) }
                        agentBadge(profile.transport.label, color: .purple)
                    }
                    Text(profile.executable)
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text("\(profile.permissionPolicy.label) · \(profile.workingDirectorySummary) · \(profile.credentialRef == nil ? "无保存凭据" : "凭据已保存")")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 12)
                HStack(spacing: 6) {
                    Button(store.isTesting(profile) ? "测试中…" : "测试") {
                        Task { await store.testAndRecord(profile) }
                    }
                    .disabled(store.isTesting(profile))
                    Button("编辑") { editorDraft = AgentProfileDraft(profile: profile) }
                    Button("复制") {
                        editorDraft = AgentProfileDraft.duplicate(profile, core: core)
                    }
                    if !isDefault { Button("设为默认") { store.setDefault(profile) } }
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

    private var defaultProfile: AgentProfile? {
        guard let id = store.config.defaultProfileId else { return nil }
        return store.config.profiles.first { $0.id == id }
    }

    @ViewBuilder
    private func deleteButtons(for profile: AgentProfile) -> some View {
        if store.config.defaultProfileId == profile.id && store.config.profiles.count > 1 {
            ForEach(store.config.profiles.filter { $0.id != profile.id }, id: \.id) { candidate in
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
        if store.config.defaultProfileId == profile.id && store.config.profiles.count > 1 {
            return "这是当前默认 Agent。请选择一个新的默认 Agent，然后删除。"
        }
        if store.config.profiles.count == 1 { return "这会让 Agent 列表变为空。" }
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
                    if draft.transport == .genericCli {
                        warningBox("通用 CLI 不是 ACP 协议会话。Kite 只能验证可执行文件能否启动；文件写入、命令和网络权限均由 Agent 自身管理。")
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
                Text("测试使用当前表单值，但不会保存；只有“保存”会写入 agent.json。")
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
            editorField("接入方式", detail: "ACP 会执行 initialize 握手；通用 CLI 只验证进程能否启动。") {
                Picker("接入方式", selection: $draft.transport) {
                    Text("ACP / JSON-RPC").tag(AgentTransport.acpStdio)
                    Text("通用 CLI").tag(AgentTransport.genericCli)
                }
                .labelsHidden()
                .pickerStyle(.segmented)
            }
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
            if draft.transport == .acpStdio {
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
        editorSection("工作目录与权限") {
            editorField("工作目录策略", detail: "设置页无法获知本次选中项；固定目录会在保存时校验。") {
                Picker("工作目录策略", selection: $draft.workingDirectoryPolicy) {
                    ForEach(WorkingDirectoryPolicy.settingsCases, id: \.self) { policy in
                        Text(policy.label).tag(policy)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(maxWidth: 260, alignment: .leading)
            }
            if draft.workingDirectoryPolicy == .fixed {
                AgentDivider()
                editorField("固定工作目录", detail: "保存前会确认目录存在。") {
                    HStack(spacing: 8) {
                        TextField("/Users/me/project", text: $draft.workingDirectory)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("固定工作目录")
                        Button("选择…") { chooseWorkingDirectory() }
                    }
                }
            }
            AgentDivider()
            editorField("权限摘要", detail: draft.permissionPolicy.summary) {
                Picker("权限摘要", selection: $draft.permissionPolicy) {
                    Text(PermissionPolicy.readOnly.label).tag(PermissionPolicy.readOnly)
                    Text(PermissionPolicy.confirmWrites.label).tag(PermissionPolicy.confirmWrites)
                    Text(PermissionPolicy.agentManaged.label).tag(PermissionPolicy.agentManaged)
                }
                .labelsHidden()
                .pickerStyle(.segmented)
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
            Text("失败测试不会修改已保存配置。关闭或取消时若有未保存修改，Kite 会先确认。")
                .font(.system(size: 10.5))
                .foregroundStyle(.secondary)
            Spacer()
            Button("取消") { requestClose() }.keyboardShortcut(.cancelAction)
            Button(testing ? "测试中…" : "测试连接") { testConnection() }.disabled(testing)
            Button("保存") { save() }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
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
        case .genericCli: return "通用 CLI"
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
    static let settingsCases: [WorkingDirectoryPolicy] = [
        .selectedItem, .fileParent, .repositoryRoot, .fixed, .ask
    ]

    var label: String {
        switch self {
        case .selectedItem: return "所选项目"
        case .fileParent: return "文件所在目录"
        case .repositoryRoot: return "Git 仓库根目录"
        case .fixed: return "固定目录"
        case .ask: return "每次询问"
        }
    }
}

private extension AgentProfile {
    var workingDirectorySummary: String {
        if workingDirectoryPolicy == .fixed {
            return workingDirectory ?? "固定目录未设置"
        }
        return workingDirectoryPolicy.label
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
