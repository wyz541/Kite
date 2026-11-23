import AppKit
import SwiftUI

// MARK: - Toolbar branch control

struct KiteGitBranchButton: View {
    @ObservedObject var model: KiteModel
    @State private var summary: GitHeadSummaryDto?
    @State private var worktrees: [GitWorktreeDto] = []
    @State private var showingBranches = false

    var body: some View {
        Group {
            if let summary {
                Menu {
                    Button("分支、标签与 Worktree…") {
                        showingBranches = true
                    }

                    Divider()

                    if worktrees.isEmpty {
                        Text("正在读取 Worktree…")
                    } else {
                        ForEach(worktrees, id: \.path) { worktree in
                            Button {
                                guard !worktree.isCurrent else { return }
                                model.loadRoot(worktree.path)
                            } label: {
                                if worktree.isCurrent {
                                    Label(worktreeMenuTitle(worktree), systemImage: "checkmark")
                                } else {
                                    Text(worktreeMenuTitle(worktree))
                                }
                            }
                            .disabled(worktree.isCurrent || worktree.prunableReason != nil)
                        }
                    }

                    Divider()

                    Button("Worktree 与 Submodule…") {
                        KiteGitToolsWindowController.open(
                            repositoryPath: model.currentPath,
                            model: model
                        )
                    }
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "arrow.triangle.branch")
                            .font(.system(size: 10, weight: .semibold))
                        Text(branchLabel(summary))
                            .font(.system(size: 11, weight: .medium))
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: 150, alignment: .leading)
                        Image(systemName: "chevron.down")
                            .font(.system(size: 7.5, weight: .bold))
                            .foregroundStyle(.tertiary)
                    }
                    .padding(.horizontal, 9)
                    .frame(height: KiteTheme.toolbarControlHeight)
                    .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("切换 Worktree 或管理分支、标签与 Submodule")
                .kiteGlass(.panel, cornerRadius: KiteTheme.controlRadius)
                .sheet(isPresented: $showingBranches, onDismiss: {
                    Task { await refreshGitState() }
                }) {
                    KiteGitBranchesView(model: model, repositoryRoot: summary.repositoryRoot)
                        .frame(minWidth: 860, minHeight: 600)
                }
            }
        }
        .task(id: model.currentPath) {
            await refreshGitState()
        }
    }

    private func refreshGitState() async {
        async let head = model.gitHeadSummary(path: model.currentPath)
        async let list = model.gitWorktrees(path: model.currentPath)
        summary = await head
        worktrees = await list?.worktrees ?? []
    }

    private func worktreeMenuTitle(_ worktree: GitWorktreeDto) -> String {
        let branch = worktree.branch ?? "分离 HEAD \(worktree.headHash?.prefix(7) ?? "")"
        let marker = worktree.isPrimary
            ? "主工作区"
            : URL(fileURLWithPath: worktree.path).lastPathComponent
        return "\(branch) · \(marker)"
    }

    private func branchLabel(_ summary: GitHeadSummaryDto) -> String {
        var value = summary.branch
            ?? summary.shortHash.map { "HEAD \($0)" }
            ?? "分离 HEAD"
        if summary.ahead > 0 || summary.behind > 0 {
            value += "  ↑\(summary.ahead) ↓\(summary.behind)"
        }
        return value
    }
}

// MARK: - Branch manager

private enum KiteGitBranchFilter: String, CaseIterable, Identifiable {
    case all
    case local
    case remote
    case tags
    case favorites
    case merged
    case worktrees

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return "全部引用"
        case .local: return "本地分支"
        case .remote: return "远程分支"
        case .tags: return "标签"
        case .favorites: return "收藏"
        case .merged: return "已合并"
        case .worktrees: return "Worktree"
        }
    }

    var icon: String {
        switch self {
        case .all: return "square.stack.3d.up"
        case .local: return "arrow.triangle.branch"
        case .remote: return "network"
        case .tags: return "tag"
        case .favorites: return "star"
        case .merged: return "checkmark.circle"
        case .worktrees: return "folder.badge.gearshape"
        }
    }
}

private enum KiteGitReferenceKind: String, CaseIterable {
    case local
    case remote
    case tag
    case worktree

    var title: String {
        switch self {
        case .local: return "本地分支"
        case .remote: return "远程分支"
        case .tag: return "标签"
        case .worktree: return "Worktree"
        }
    }
}

private struct KiteGitReference: Identifiable {
    let id: String
    let kind: KiteGitReferenceKind
    let name: String
    let subtitle: String
    let commitHash: String
    let upstream: String?
    let ahead: UInt32
    let behind: UInt32
    let isHead: Bool
    let isMerged: Bool
    let isAnnotated: Bool
    let remote: String?
    let branchName: String?
    let path: String?
}

struct KiteGitBranchesView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: KiteModel
    let repositoryRoot: String

    @State private var snapshot: GitBranchSnapshotDto?
    @State private var filter: KiteGitBranchFilter = .all
    @State private var query = ""
    @State private var selectedID: String?
    @State private var loading = false
    @State private var message: String?
    @State private var isError = false
    @State private var compareResult: GitCompareResultDto?
    @State private var compareBase = ""
    @State private var compareHead = ""
    @State private var favorites: Set<String>
    @State private var protectedPatterns: Set<String>

    init(model: KiteModel, repositoryRoot: String) {
        self.model = model
        self.repositoryRoot = repositoryRoot
        let defaults = UserDefaults.standard
        _favorites = State(
            initialValue: Set(defaults.stringArray(forKey: "kite.macos.git-favorites.\(repositoryRoot)") ?? [])
        )
        _protectedPatterns = State(
            initialValue: Set(defaults.stringArray(forKey: "kite.macos.git-protected.\(repositoryRoot)") ?? [])
        )
    }

    private var references: [KiteGitReference] {
        guard let snapshot else { return [] }
        var values: [KiteGitReference] = snapshot.localBranches.map { branch in
            KiteGitReference(
                id: "local:\(branch.name)",
                kind: .local,
                name: branch.name,
                subtitle: "\(branch.upstream ?? "未设置上游") · \(branch.shortHash)",
                commitHash: branch.commitHash,
                upstream: branch.upstream,
                ahead: branch.ahead,
                behind: branch.behind,
                isHead: branch.isHead,
                isMerged: branch.isMerged,
                isAnnotated: false,
                remote: nil,
                branchName: branch.name,
                path: nil
            )
        }
        values += snapshot.remoteBranches.map { branch in
            KiteGitReference(
                id: "remote:\(branch.name)",
                kind: .remote,
                name: branch.name,
                subtitle: "\(branch.shortHash) · \(branch.subject ?? "")",
                commitHash: branch.commitHash,
                upstream: nil,
                ahead: branch.ahead,
                behind: branch.behind,
                isHead: false,
                isMerged: false,
                isAnnotated: false,
                remote: branch.remote,
                branchName: branch.name.split(separator: "/", maxSplits: 1).last.map(String.init),
                path: nil
            )
        }
        values += snapshot.tags.map { tag in
            KiteGitReference(
                id: "tag:\(tag.name)",
                kind: .tag,
                name: tag.name,
                subtitle: "\(tag.shortHash) · \(tag.isAnnotated ? "附注标签" : "轻量标签")",
                commitHash: tag.targetHash,
                upstream: nil,
                ahead: 0,
                behind: 0,
                isHead: false,
                isMerged: false,
                isAnnotated: tag.isAnnotated,
                remote: nil,
                branchName: tag.name,
                path: nil
            )
        }
        values += snapshot.worktrees.map { worktree in
            let url = URL(fileURLWithPath: worktree.path)
            return KiteGitReference(
                id: "worktree:\(worktree.path)",
                kind: .worktree,
                name: url.lastPathComponent.isEmpty ? worktree.path : url.lastPathComponent,
                subtitle: "\(worktree.branch ?? "分离 HEAD") · \(worktree.path)",
                commitHash: worktree.headHash ?? "",
                upstream: nil,
                ahead: 0,
                behind: 0,
                isHead: worktree.isCurrent,
                isMerged: false,
                isAnnotated: false,
                remote: nil,
                branchName: worktree.branch,
                path: worktree.path
            )
        }
        return values
    }

    private var filteredReferences: [KiteGitReference] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return references.filter { reference in
            let filterMatches: Bool
            switch filter {
            case .all: filterMatches = true
            case .local: filterMatches = reference.kind == .local
            case .remote: filterMatches = reference.kind == .remote
            case .tags: filterMatches = reference.kind == .tag
            case .favorites: filterMatches = favorites.contains(reference.id)
            case .merged: filterMatches = reference.kind == .local && !reference.isHead && reference.isMerged
            case .worktrees: filterMatches = reference.kind == .worktree
            }
            guard filterMatches else { return false }
            guard !needle.isEmpty else { return true }
            return reference.name.lowercased().contains(needle)
                || reference.subtitle.lowercased().contains(needle)
        }
    }

    private var selectedReference: KiteGitReference? {
        guard let selectedID else { return nil }
        return references.first { $0.id == selectedID }
    }
}
private extension KiteGitBranchesView {
    func count(for filter: KiteGitBranchFilter) -> Int {
        references.filter { reference in
            switch filter {
            case .all: return true
            case .local: return reference.kind == .local
            case .remote: return reference.kind == .remote
            case .tags: return reference.kind == .tag
            case .favorites: return favorites.contains(reference.id)
            case .merged: return reference.kind == .local && !reference.isHead && reference.isMerged
            case .worktrees: return reference.kind == .worktree
            }
        }.count
    }

    func isProtected(_ name: String) -> Bool {
        protectedPatterns.contains { wildcardMatch(pattern: $0, value: name) }
    }

    func defaultRemote() -> String? {
        snapshot?.remoteBranches.compactMap(\.remote).first
    }

    func defaultUpstream(for branch: GitBranchDto) -> String {
        if let upstream = branch.upstream { return upstream }
        return snapshot?.remoteBranches.first { $0.name.hasSuffix("/\(branch.name)") }?.name ?? ""
    }

    @MainActor
    func reload() async {
        loading = true
        do {
            let next = try await model.gitBranchSnapshot(path: repositoryRoot)
            snapshot = next
            if selectedID == nil, let head = next.headBranch {
                selectedID = "local:\(head)"
            }
            isError = false
        } catch {
            message = error.localizedDescription
            isError = true
        }
        loading = false
    }

    @MainActor
    func perform(_ operation: @escaping @Sendable () async throws -> String) {
        loading = true
        Task { @MainActor in
            do {
                let result = try await operation()
                await reload()
                message = result
                isError = false
            } catch {
                message = error.localizedDescription
                isError = true
            }
            loading = false
        }
    }

    func checkout(_ reference: KiteGitReference) {
        let kind: GitRefKindDto
        switch reference.kind {
        case .local: kind = .localBranch
        case .remote: kind = .remoteBranch
        case .tag: kind = .tag
        case .worktree: return
        }
        var selectedPolicy: GitDirtyPolicyDto = .cancel
        if snapshot?.dirty == true {
            guard confirm(
                title: "工作区有未提交的更改",
                message: "切换前是否先 stash，并在切换后自动恢复？",
                action: "Stash 并切换"
            ) else { return }
            selectedPolicy = .stash
        }
        let policy = selectedPolicy
        let root = repositoryRoot
        let target = reference.name
        let model = model
        perform {
            try await model.gitCheckout(
                path: root,
                target: target,
                kind: kind,
                dirtyPolicy: policy
            )
        }
    }

    func createBranch() {
        let start = selectedReference?.name ?? "HEAD"
        guard let values = promptValues(
            title: "新建分支",
            message: "从任意分支、标签或提交创建本地分支。",
            labels: ["分支名称", "起点"],
            defaults: ["", start]
        ) else { return }
        guard !values[0].isEmpty else { return }
        let root = repositoryRoot
        let model = model
        perform {
            try await model.gitCreateBranch(
                path: root,
                name: values[0],
                startPoint: values[1].isEmpty ? "HEAD" : values[1]
            )
        }
    }

    func rename(_ branch: KiteGitReference) {
        guard let values = promptValues(
            title: "重命名分支",
            message: "为 \(branch.name) 输入新名称。",
            labels: ["新分支名称"],
            defaults: [branch.name]
        ), !values[0].isEmpty else { return }
        let root = repositoryRoot
        let oldName = branch.name
        let model = model
        perform {
            try await model.gitRenameBranch(path: root, oldName: oldName, newName: values[0])
        }
    }

    func deleteLocal(_ branch: KiteGitReference) {
        let unmerged = !branch.isMerged
        guard confirm(
            title: unmerged ? "删除未合并的分支？" : "删除本地分支？",
            message: unmerged
                ? "\(branch.name) 尚未合并到当前 HEAD。删除会丢失该分支独有的提交。"
                : "确认删除 \(branch.name)？",
            action: "删除"
        ) else { return }
        let root = repositoryRoot
        let name = branch.name
        let model = model
        perform {
            try await model.gitDeleteLocalBranch(path: root, name: name, force: unmerged)
        }
    }

    func deleteRemote(_ branch: KiteGitReference) {
        guard let remote = branch.remote, let name = branch.branchName else { return }
        guard confirm(
            title: "删除远程分支？",
            message: "将从 \(remote) 删除 \(name)。本地分支不会改变。",
            action: "删除远程分支"
        ) else { return }
        let root = repositoryRoot
        let model = model
        perform {
            try await model.gitDeleteRemoteBranch(path: root, remote: remote, branch: name)
        }
    }

    func setUpstream(_ branch: KiteGitReference) {
        let current = snapshot?.localBranches.first { $0.name == branch.name }
        let defaultUpstream = current.map(defaultUpstream(for:)) ?? ""
        guard let values = promptValues(
            title: "设置上游",
            message: "输入远程分支，例如 origin/main。留空会清除上游。",
            labels: ["上游分支"],
            defaults: [defaultUpstream]
        ) else { return }
        let root = repositoryRoot
        let name = branch.name
        let upstream = values[0].isEmpty ? nil : values[0]
        let model = model
        perform {
            try await model.gitSetBranchUpstream(path: root, branch: name, upstream: upstream)
        }
    }

    func createWorktree(_ branch: KiteGitReference) {
        let rootURL = URL(fileURLWithPath: repositoryRoot)
        let defaultPath = rootURL.deletingLastPathComponent()
            .appendingPathComponent("\(rootURL.lastPathComponent)--\(branch.name.replacingOccurrences(of: "/", with: "-"))")
            .path
        guard let values = promptValues(
            title: "在新 worktree 中检出",
            message: "为 \(branch.name) 输入目标文件夹。",
            labels: ["目标路径"],
            defaults: [defaultPath]
        ), !values[0].isEmpty else { return }
        let root = repositoryRoot
        let name = branch.name
        let destination = values[0]
        let model = model
        perform {
            try await model.gitCreateWorktree(path: root, destination: destination, branch: name)
        }
    }

    func createTag() {
        let target = selectedReference?.name ?? "HEAD"
        guard let values = promptValues(
            title: "新建标签",
            message: "目标：\(target)。说明留空会创建轻量标签，填写说明会创建附注标签。",
            labels: ["标签名称", "说明"],
            defaults: ["", ""]
        ), !values[0].isEmpty else { return }
        let root = repositoryRoot
        let name = values[0]
        let message = values[1].trimmingCharacters(in: .whitespacesAndNewlines)
        let annotated = !message.isEmpty
        let model = model
        perform {
            try await model.gitCreateTag(
                path: root,
                name: name,
                target: target,
                annotated: annotated,
                message: annotated ? message : nil
            )
        }
    }

    func pushTag(_ tag: KiteGitReference) {
        guard let remote = defaultRemote() else {
            message = "仓库没有配置远程。"
            isError = true
            return
        }
        let root = repositoryRoot
        let name = tag.name
        let model = model
        perform {
            try await model.gitPushTag(path: root, remote: remote, name: name)
        }
    }

    func deleteTag(_ tag: KiteGitReference, remote: Bool) {
        guard confirm(
            title: remote ? "删除远程标签？" : "删除本地标签？",
            message: remote ? "将从 \(defaultRemote() ?? "远程") 删除标签 \(tag.name)。" : "确认删除标签 \(tag.name)？",
            action: "删除"
        ) else { return }
        let root = repositoryRoot
        let name = tag.name
        let model = model
        if remote {
            guard let remoteName = defaultRemote() else { return }
            perform {
                try await model.gitDeleteRemoteTag(path: root, remote: remoteName, name: name)
            }
        } else {
            perform {
                try await model.gitDeleteTag(path: root, name: name)
            }
        }
    }

    func cleanupMerged() {
        guard let snapshot else { return }
        let names = snapshot.localBranches
            .filter { !$0.isHead && $0.isMerged }
            .map(\.name)
        guard !names.isEmpty else {
            message = "没有可清理的已合并本地分支。"
            isError = false
            return
        }
        guard confirm(
            title: "删除已合并的本地分支？",
            message: names.joined(separator: "\n"),
            action: "全部删除"
        ) else { return }
        let root = repositoryRoot
        let model = model
        perform {
            try await model.gitDeleteLocalBranches(path: root, names: names, force: false)
        }
    }

    func compare(_ reference: KiteGitReference) {
        guard let head = snapshot?.headBranch ?? snapshot?.headShortHash else { return }
        let root = repositoryRoot
        let target = reference.name
        let model = model
        loading = true
        Task { @MainActor in
            do {
                compareResult = try await model.gitCompareBranches(path: root, base: head, head: target)
                compareBase = head
                compareHead = target
                message = "已加载分支比较。"
                isError = false
            } catch {
                message = error.localizedDescription
                isError = true
            }
            loading = false
        }
    }

    func toggleFavorite(_ reference: KiteGitReference) {
        if favorites.contains(reference.id) {
            favorites.remove(reference.id)
        } else {
            favorites.insert(reference.id)
        }
        UserDefaults.standard.set(
            favorites.sorted(),
            forKey: "kite.macos.git-favorites.\(repositoryRoot)"
        )
    }

    func toggleProtection(_ reference: KiteGitReference) {
        guard reference.kind == .local else { return }
        if protectedPatterns.contains(reference.name) {
            protectedPatterns.remove(reference.name)
        } else {
            protectedPatterns.insert(reference.name)
        }
        UserDefaults.standard.set(
            protectedPatterns.sorted(),
            forKey: "kite.macos.git-protected.\(repositoryRoot)"
        )
    }
}
extension KiteGitBranchesView {
    var body: some View {
        VStack(spacing: 0) {
            header
            toolbar
            Divider()
            HStack(spacing: 0) {
                sidebar
                Divider()
                VStack(spacing: 0) {
                    referenceList
                    if let compareResult {
                        comparePanel(compareResult)
                    }
                    Divider()
                    details
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            statusBar
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .task { await reload() }
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("分支与标签")
                    .font(.system(size: 16, weight: .semibold))
                Text(repositoryRoot)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            if loading {
                ProgressView().controlSize(.small)
            }
            Button("关闭") { dismiss() }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
            TextField("搜索分支、远程、标签或 worktree", text: $query)
                .textFieldStyle(.plain)
            if !query.isEmpty {
                Button { query = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.tertiary)
                }
                .buttonStyle(.plain)
            }
            Spacer(minLength: 12)
            Button { createBranch() } label: {
                Label("新建分支", systemImage: "arrow.triangle.branch")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            Button { createTag() } label: {
                Label("新建标签", systemImage: "tag")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            Button("清理已合并") { cleanupMerged() }
                .buttonStyle(.bordered)
                .controlSize(.small)
            Button { Task { await reload() } } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .help("刷新")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 9)
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(KiteGitBranchFilter.allCases) { item in
                Button {
                    filter = item
                    compareResult = nil
                } label: {
                    HStack(spacing: 7) {
                        Image(systemName: item.icon)
                            .frame(width: 14)
                        Text(item.title)
                        Spacer()
                        Text("\(count(for: item))")
                            .font(.system(size: 10, design: .rounded))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        filter == item ? Color.accentColor.opacity(0.12) : Color.clear,
                        in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                    )
                }
                .buttonStyle(.plain)
            }
            Spacer()
            if let snapshot {
                VStack(alignment: .leading, spacing: 3) {
                    Text(snapshot.headBranch ?? "分离 HEAD")
                        .font(.system(size: 10.5, weight: .medium))
                    if snapshot.dirty {
                        Text("工作区有未提交更改")
                            .foregroundStyle(.orange)
                    }
                    if snapshot.ahead > 0 || snapshot.behind > 0 {
                        Text("↑\(snapshot.ahead)  ↓\(snapshot.behind)")
                            .foregroundStyle(.secondary)
                    }
                }
                .font(.system(size: 10))
                .padding(8)
            }
        }
        .padding(10)
        .frame(width: 180)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.22))
    }

    private var referenceList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 8) {
                ForEach(KiteGitReferenceKind.allCases, id: \.rawValue) { kind in
                    let items = filteredReferences.filter { $0.kind == kind }
                    if !items.isEmpty {
                        Text(kind.title)
                            .font(.system(size: 10, weight: .semibold))
                            .textCase(.uppercase)
                            .tracking(0.6)
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 10)
                            .padding(.top, 4)
                        ForEach(items) { reference in
                            referenceRow(reference)
                        }
                    }
                }
                if filteredReferences.isEmpty {
                    VStack(spacing: 6) {
                        Image(systemName: "arrow.triangle.branch")
                            .font(.system(size: 22))
                            .foregroundStyle(.tertiary)
                        Text(loading ? "正在读取 Git 引用…" : "没有匹配的 Git 引用")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 60)
                }
            }
            .padding(8)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func referenceRow(_ reference: KiteGitReference) -> some View {
        let selected = selectedID == reference.id
        let favorite = favorites.contains(reference.id)
        let indentation = CGFloat(reference.name.filter { $0 == "/" }.count) * 10
        return HStack(spacing: 8) {
            Button {
                selectedID = reference.id
                compareResult = nil
            } label: {
                HStack(spacing: 9) {
                    Image(systemName: icon(for: reference))
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                        .frame(width: 14)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(reference.name)
                                .font(.system(size: 12, weight: reference.isHead ? .semibold : .regular))
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .padding(.leading, indentation)
                            if reference.isHead {
                                Text("HEAD")
                                    .font(.system(size: 8.5, weight: .bold))
                                    .foregroundStyle(Color.accentColor)
                            }
                            if reference.kind == .local && !reference.isMerged {
                                Text("未合并")
                                    .font(.system(size: 8.5, weight: .medium))
                                    .foregroundStyle(.orange)
                            }
                            if isProtected(reference.name), reference.kind == .local {
                                Image(systemName: "lock.fill")
                                    .font(.system(size: 8.5))
                                    .foregroundStyle(.orange)
                            }
                        }
                        HStack(spacing: 5) {
                            Text(reference.subtitle)
                            if reference.ahead > 0 || reference.behind > 0 {
                                Text("↑\(reference.ahead) ↓\(reference.behind)")
                            }
                        }
                        .font(.system(size: 9.5, design: .rounded))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    }
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if reference.kind != .worktree {
                Button { toggleFavorite(reference) } label: {
                    Image(systemName: favorite ? "star.fill" : "star")
                        .font(.system(size: 10.5))
                        .foregroundStyle(favorite ? Color.yellow : Color.secondary)
                }
                .buttonStyle(.plain)
                .help(favorite ? "取消收藏" : "收藏")
                .padding(.trailing, 8)
            }
        }
        .background(
            selected ? Color.accentColor.opacity(0.10) : Color(nsColor: .controlBackgroundColor).opacity(0.24),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(selected ? Color.accentColor.opacity(0.35) : Color.primary.opacity(0.05), lineWidth: 0.6)
        }
        .contextMenu {
            if reference.kind != .worktree {
                Button("检出") { checkout(reference) }
                Button("与 HEAD 比较") { compare(reference) }
            }
            if reference.kind == .local {
                Button("设置上游") { setUpstream(reference) }
                Button("在新 worktree 中检出") { createWorktree(reference) }
                Button("重命名") { rename(reference) }
                Button(isProtected(reference.name) ? "取消保护" : "保护") { toggleProtection(reference) }
                Divider()
                Button("删除") { deleteLocal(reference) }
            } else if reference.kind == .remote {
                Divider()
                Button("删除远程分支") { deleteRemote(reference) }
                    .disabled(isProtected(reference.branchName ?? reference.name))
            } else if reference.kind == .tag {
                Divider()
                Button("推送标签") { pushTag(reference) }
                Button("删除本地标签") { deleteTag(reference, remote: false) }
                Button("删除远程标签") { deleteTag(reference, remote: true) }
            }
        }
    }

    private func icon(for reference: KiteGitReference) -> String {
        switch reference.kind {
        case .local: return reference.isHead ? "arrow.triangle.branch.circle.fill" : "arrow.triangle.branch"
        case .remote: return "network"
        case .tag: return reference.isAnnotated ? "tag.fill" : "tag"
        case .worktree: return "folder"
        }
    }
}
extension KiteGitBranchesView {
    @ViewBuilder
    private var details: some View {
        if let reference = selectedReference {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(reference.name)
                            .font(.system(size: 13, weight: .semibold))
                        Text(reference.subtitle)
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer()
                    if reference.kind != .worktree {
                        Button { toggleFavorite(reference) } label: {
                            Label(
                                favorites.contains(reference.id) ? "取消收藏" : "收藏",
                                systemImage: favorites.contains(reference.id) ? "star.slash" : "star"
                            )
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }

                switch reference.kind {
                case .local:
                    HStack(spacing: 7) {
                        Button("检出") { checkout(reference) }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .disabled(reference.isHead)
                        Button("与 HEAD 比较") { compare(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Button("设置上游") { setUpstream(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Button("新 worktree") { createWorktree(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Button("重命名") { rename(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Button(isProtected(reference.name) ? "取消保护" : "保护") { toggleProtection(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Spacer()
                        Button("删除") { deleteLocal(reference) }
                            .buttonStyle(.bordered)
                            .tint(.red)
                            .controlSize(.small)
                            .disabled(reference.isHead)
                    }
                case .remote:
                    HStack(spacing: 7) {
                        Button("检出并跟踪") { checkout(reference) }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        Button("与 HEAD 比较") { compare(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Spacer()
                        Button("删除远程分支") { deleteRemote(reference) }
                            .buttonStyle(.bordered)
                            .tint(.red)
                            .controlSize(.small)
                            .disabled(isProtected(reference.branchName ?? reference.name))
                    }
                case .tag:
                    HStack(spacing: 7) {
                        Button("检出标签") { checkout(reference) }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        Button("与 HEAD 比较") { compare(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Button("推送") { pushTag(reference) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        Spacer()
                        Button("删除本地") { deleteTag(reference, remote: false) }
                            .buttonStyle(.bordered)
                            .tint(.red)
                            .controlSize(.small)
                        Button("删除远程") { deleteTag(reference, remote: true) }
                            .buttonStyle(.bordered)
                            .tint(.red)
                            .controlSize(.small)
                    }
                case .worktree:
                    HStack(spacing: 7) {
                        Button("在 Kite 中打开") {
                            if let path = reference.path {
                                model.loadRoot(path)
                                dismiss()
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        Spacer()
                        if reference.isHead {
                            Text("当前 worktree").font(.system(size: 10)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .padding(12)
        } else {
            Text("选择一个引用以查看可用操作。")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
        }
    }

    private func comparePanel(_ result: GitCompareResultDto) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text("\(compareBase) ↔ \(compareHead)")
                    .font(.system(size: 10.5, weight: .semibold))
                Spacer()
                Text("领先 \(result.ahead) · 落后 \(result.behind)")
                    .font(.system(size: 10, design: .rounded))
                    .foregroundStyle(.secondary)
                Button { compareResult = nil } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.tertiary)
                }
                .buttonStyle(.plain)
            }
            ForEach(result.commits.prefix(4), id: \.hash) { commit in
                Text("\(commit.shortHash)  \(commit.subject)")
                    .font(.system(size: 9.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            ForEach(result.files.prefix(6), id: \.path) { file in
                Text("\(file.path)  +\(file.additions) -\(file.deletions)")
                    .font(.system(size: 9.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(9)
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
    }

    private var statusBar: some View {
        HStack(spacing: 6) {
            Image(systemName: isError ? "exclamationmark.triangle.fill" : "info.circle")
                .foregroundStyle(isError ? Color.red : Color.secondary)
            Text(message ?? "Git 分支与标签")
                .font(.system(size: 10))
                .foregroundStyle(isError ? Color.red : Color.secondary)
                .lineLimit(2)
            Spacer()
            if loading {
                Text("正在执行…")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 30)
    }
}

// MARK: - Native prompts

@MainActor
private func promptValues(
    title: String,
    message: String,
    labels: [String],
    defaults: [String]
) -> [String]? {
    let alert = NSAlert()
    alert.alertStyle = .informational
    alert.messageText = title
    alert.informativeText = message
    alert.addButton(withTitle: "确定")
    alert.addButton(withTitle: "取消")

    let stack = NSStackView()
    stack.orientation = .vertical
    stack.alignment = .leading
    stack.spacing = 7
    var fields: [NSTextField] = []
    for (index, label) in labels.enumerated() {
        let labelView = NSTextField(labelWithString: label)
        labelView.font = .systemFont(ofSize: 11, weight: .medium)
        let field = NSTextField(string: index < defaults.count ? defaults[index] : "")
        field.font = .systemFont(ofSize: 12)
        field.frame.size.width = 340
        field.widthAnchor.constraint(equalToConstant: 340).isActive = true
        stack.addArrangedSubview(labelView)
        stack.addArrangedSubview(field)
        fields.append(field)
    }
    stack.frame = NSRect(x: 0, y: 0, width: 360, height: CGFloat(labels.count * 50))
    alert.accessoryView = stack
    if alert.runModal() == .alertFirstButtonReturn {
        return fields.map { $0.stringValue.trimmingCharacters(in: .whitespacesAndNewlines) }
    }
    return nil
}

@MainActor
private func confirm(title: String, message: String, action: String) -> Bool {
    let alert = NSAlert()
    alert.alertStyle = .warning
    alert.messageText = title
    alert.informativeText = message
    alert.addButton(withTitle: action)
    alert.addButton(withTitle: "取消")
    return alert.runModal() == .alertFirstButtonReturn
}

private func wildcardMatch(pattern: String, value: String) -> Bool {
    let pattern = Array(pattern)
    let value = Array(value)
    var patternIndex = 0
    var valueIndex = 0
    var star: Int?
    var matched = 0
    while valueIndex < value.count {
        if patternIndex < pattern.count,
           pattern[patternIndex] == "?" || pattern[patternIndex] == value[valueIndex] {
            patternIndex += 1
            valueIndex += 1
        } else if patternIndex < pattern.count, pattern[patternIndex] == "*" {
            star = patternIndex
            matched = valueIndex
            patternIndex += 1
        } else if let star {
            patternIndex = star + 1
            matched += 1
            valueIndex = matched
        } else {
            return false
        }
    }
    while patternIndex < pattern.count, pattern[patternIndex] == "*" {
        patternIndex += 1
    }
    return patternIndex == pattern.count
}
