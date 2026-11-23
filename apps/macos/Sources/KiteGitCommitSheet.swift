import SwiftUI

/// Commit panel for Git. The Rust core performs every preflight check again, so
/// this view cannot bypass identity, conflict, or staged-file validation.
struct KiteGitCommitSheet: View {
    @ObservedObject var model: KiteModel
    let path: String

    @Environment(\.dismiss) private var dismiss
    @State private var state: GitCommitStateDto?
    @State private var message = ""
    @State private var author = ""
    @State private var amend = false
    @State private var signoff = false
    @State private var signing: GitSigningModeDto = .gitConfig
    @State private var skipHooks = false
    @State private var push = false
    @State private var loading = true
    @State private var committing = false
    @State private var error: String?
    @State private var output: String?
    @State private var successHash: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            blockers

            if loading && state == nil {
                Spacer()
                ProgressView("正在读取 Git 提交信息…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Spacer()
            } else if let state {
                stagedChanges(state)
                commitForm(state)
            } else {
                Spacer()
                Text(error ?? "无法读取 Git 仓库")
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Spacer()
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Color(nsColor: .windowBackgroundColor))
        .task { await load() }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "arrow.triangle.branch")
                .font(.system(size: 18, weight: .semibold))
                .foregroundColor(.accentColor)
            VStack(alignment: .leading, spacing: 2) {
                Text(repositoryName)
                    .font(.headline)
                Text(branchDescription)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("关闭") { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
    }

    @ViewBuilder
    private var blockers: some View {
        if let state, !state.identityConfigured {
            VStack(alignment: .leading, spacing: 5) {
                Label("尚未配置 Git 用户身份", systemImage: "person.crop.circle.badge.exclamationmark")
                    .font(.callout.weight(.semibold))
                Text("请先运行：\ngit config --global user.name \"Your Name\"\ngit config --global user.email \"you@example.com\"")
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.orange.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
        }
        if let state, !state.unresolvedConflicts.isEmpty {
            Label(
                "仍有未解决的冲突：\(state.unresolvedConflicts.joined(separator: "、"))",
                systemImage: "exclamationmark.triangle.fill"
            )
            .font(.callout)
            .foregroundStyle(.red)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.red.opacity(0.10), in: RoundedRectangle(cornerRadius: 10))
        }
    }

    private func stagedChanges(_ state: GitCommitStateDto) -> some View {
        let files = amend ? state.amendFiles : state.stagedFiles
        let added = amend ? state.amendLinesAdded : state.stagedLinesAdded
        let removed = amend ? state.amendLinesRemoved : state.stagedLinesRemoved
        return VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(amend ? "Amend 后的改动" : "待提交改动")
                    .font(.headline)
                Spacer()
                Text("\(files.count) 个文件 · +\(added) −\(removed)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if files.isEmpty {
                Text(amend ? "上一次提交中没有可显示的改动。" : "没有已暂存的文件。")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 5)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(Array(files.enumerated()), id: \.offset) { _, path in
                            HStack(spacing: 7) {
                                Image(systemName: "doc")
                                    .foregroundStyle(.secondary)
                                Text(path)
                                    .font(.system(.caption, design: .monospaced))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 120)
            }
        }
        .padding(12)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
    }

    private func commitForm(_ state: GitCommitStateDto) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("提交说明")
                    .font(.headline)
                Text("⌘↩ 提交")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                Spacer()
                if let warning = subjectWarning {
                    Text(warning)
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }
            }

            TextEditor(text: $message)
                .font(.system(.body, design: .monospaced))
                .scrollContentBackground(.hidden)
                .padding(6)
                .frame(minHeight: 190)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 10))
                .overlay {
                    RoundedRectangle(cornerRadius: 10)
                        .strokeBorder(Color.primary.opacity(0.08))
                }

            if !state.recentMessages.isEmpty {
                Menu("最近提交信息") {
                    ForEach(Array(state.recentMessages.prefix(10).enumerated()), id: \.offset) { _, value in
                        Button(value.components(separatedBy: "\n").first ?? value) {
                            message = value
                        }
                    }
                }
                .fixedSize()
            }

            HStack(alignment: .center, spacing: 18) {
                Toggle("Amend", isOn: $amend)
                    .onChange(of: amend) { enabled in
                        if enabled, let previous = state.previousMessage {
                            message = previous
                        }
                    }
                    .disabled(state.headShortHash == nil)
                Toggle("Signed-off-by", isOn: $signoff)
                Toggle("跳过 hooks", isOn: $skipHooks)
                Toggle("提交并推送", isOn: $push)
            }
            .toggleStyle(.switch)
            .font(.callout)

            HStack(spacing: 12) {
                Text("签名")
                    .font(.callout)
                Picker("签名", selection: $signing) {
                    Text("跟随 Git").tag(GitSigningModeDto.gitConfig)
                    Text("强制签名").tag(GitSigningModeDto.enabled)
                    Text("不签名").tag(GitSigningModeDto.disabled)
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                .frame(maxWidth: 330)
                Text(signingDescription(state))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            TextField("作者（可选），例如 Name <email>", text: $author)
                .textFieldStyle(.roundedBorder)

            if let error {
                Text(error)
                    .font(.callout)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .padding(9)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.red.opacity(0.10), in: RoundedRectangle(cornerRadius: 9))
            }

            if let successHash {
                HStack(spacing: 8) {
                    Label("已创建提交 \(successHash)", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Button("查看日志") {
                        Task { await model.openVcsLog(tool: .git, path: path) }
                    }
                    .buttonStyle(.link)
                }
                .font(.callout)
            }

            if let output {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Git / hooks 输出")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ScrollView {
                        Text(output)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxHeight: 110)
                    .padding(8)
                    .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
                }
            }

            HStack {
                Spacer()
                Button(committing ? "正在提交…" : commitButtonTitle(state)) {
                    Task { await submit() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: [.command])
                .disabled(commitDisabled(state))
            }
        }
        .padding(12)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var repositoryName: String {
        guard let root = state?.repositoryRoot else { return "Git" }
        return (root as NSString).lastPathComponent
    }

    private var branchDescription: String {
        guard let state else { return "正在读取…" }
        let branch = state.branch ?? "分离 HEAD"
        if let upstream = state.upstream {
            return "\(branch) → \(upstream)"
        }
        return branch
    }

    private var subjectWarning: String? {
        let subject = message.components(separatedBy: "\n").first ?? ""
        if subject.isEmpty, !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return "建议在第一行写提交标题"
        }
        if subject.count > 72 {
            return "标题 \(subject.count) 个字符，建议不超过 72"
        }
        return nil
    }

    private func signingDescription(_ state: GitCommitStateDto) -> String {
        switch signing {
        case .gitConfig:
            return state.signingDefault ? "Git 当前默认签名" : "Git 当前默认不签名"
        case .enabled:
            return state.signingFormat == "ssh" ? "使用 SSH 签名" : "使用已配置的签名密钥"
        case .disabled:
            return "即使 Git 默认开启签名也不会签名"
        }
    }

    private func commitButtonTitle(_ state: GitCommitStateDto) -> String {
        let count = amend ? state.amendFiles.count : state.stagedFiles.count
        if push {
            return "提交并推送 · \(count) 个文件"
        }
        if amend {
            return "Amend · \(count) 个文件"
        }
        return "提交 · \(count) 个文件"
    }

    private func commitDisabled(_ state: GitCommitStateDto) -> Bool {
        committing
            || !state.identityConfigured
            || !state.unresolvedConflicts.isEmpty
            || (!amend && state.stagedFiles.isEmpty)
            || (amend && state.headShortHash == nil)
            || message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func load() async {
        loading = true
        error = nil
        do {
            let loaded = try await model.gitCommitState(path: path)
            state = loaded
            if message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                message = loaded.commitTemplate ?? ""
            }
        } catch {
            state = nil
            self.error = error.localizedDescription
        }
        loading = false
    }

    private func submit() async {
        guard let state, !commitDisabled(state) else { return }
        committing = true
        error = nil
        output = nil
        successHash = nil
        do {
            let options = GitCommitOptionsDto(
                repositoryPath: state.repositoryRoot,
                message: message,
                amend: amend,
                signoff: signoff,
                signing: signing,
                skipHooks: skipHooks,
                author: author.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : author,
                push: push
            )
            let result = try await model.gitCommit(options: options)
            successHash = result.shortHash
            output = [result.commandOutput, result.pushOutput]
                .compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
                .joined(separator: "\n\n")
            if output?.isEmpty == true {
                output = nil
            }
            if let pushError = result.pushError {
                error = pushError
            }
            message = result.amended ? result.subject : ""
            amend = false
            self.state = try await model.gitCommitState(path: path)
        } catch {
            self.error = error.localizedDescription
        }
        committing = false
    }
}
