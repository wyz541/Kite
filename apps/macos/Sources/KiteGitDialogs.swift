import AppKit
import SwiftUI

@MainActor
final class KiteGitWindows {
    static let shared = KiteGitWindows()

    private var controllers: [NSWindowController] = []

    private init() {}

    func showPush(model: KiteModel, path: String) {
        let viewModel = KiteGitPushViewModel(model: model, path: path)
        let controller = makeController(
            title: "推送提交",
            size: NSSize(width: 980, height: 660),
            view: KiteGitPushView(viewModel: viewModel)
        )
        viewModel.close = { [weak controller] in controller?.close() }
        present(controller)
    }

    func showClone(model: KiteModel, directory: String) {
        let viewModel = KiteGitCloneViewModel(model: model, directory: directory)
        let controller = makeController(
            title: "克隆 Git 仓库",
            size: NSSize(width: 580, height: 420),
            view: KiteGitCloneView(viewModel: viewModel)
        )
        viewModel.close = { [weak controller] in controller?.close() }
        present(controller)
    }

    func showRemotes(model: KiteModel, path: String) {
        let viewModel = KiteGitRemotesViewModel(model: model, path: path)
        let controller = makeController(
            title: "管理 Git 远程",
            size: NSSize(width: 820, height: 560),
            view: KiteGitRemotesView(viewModel: viewModel)
        )
        viewModel.close = { [weak controller] in controller?.close() }
        present(controller)
    }

    private func makeController<V: View>(
        title: String,
        size: NSSize,
        view: V
    ) -> NSWindowController {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = title
        window.minSize = NSSize(width: min(size.width, 520), height: min(size.height, 360))
        window.contentViewController = NSHostingController(rootView: view)
        window.center()
        return NSWindowController(window: window)
    }

    private func present(_ controller: NSWindowController) {
        controllers.append(controller)
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification,
            object: controller.window,
            queue: .main
        ) { [weak self, weak controller] _ in
            Task { @MainActor in
                self?.controllers.removeAll { $0 === controller }
            }
        }
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
    }
}

struct KiteGitMenu: View {
    @ObservedObject var model: KiteModel
    let path: String

    var body: some View {
        Menu {
            Button("拉取…") {
                presentGitPullChoice(model: model, path: path)
            }
            Button("推送…") {
                KiteGitWindows.shared.showPush(model: model, path: path)
            }
            Menu("获取") {
                Button("获取当前远程") {
                    KiteGitService.shared.fetch(model: model, path: path, allRemotes: false)
                }
                Button("获取全部远程") {
                    KiteGitService.shared.fetch(model: model, path: path, allRemotes: true)
                }
            }
            Button("管理远程…") {
                KiteGitWindows.shared.showRemotes(model: model, path: path)
            }

            Divider()

            Button("克隆仓库…") {
                KiteGitWindows.shared.showClone(model: model, directory: model.currentPath)
            }
            Button("初始化仓库") {
                initializeRepository()
            }
        } label: {
            Label("Git", systemImage: "arrow.triangle.branch")
        }
    }

    private func initializeRepository() {
        let directory: String
        var isDirectory: ObjCBool = false
        if FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory),
           isDirectory.boolValue {
            directory = path
        } else {
            directory = (path as NSString).deletingLastPathComponent
        }
        Task {
            do {
                let initialized = try await KiteGitService.shared.initRepository(path: directory)
                model.updateGitOperationStatus("Git 仓库已初始化：\(initialized)")
                model.refresh()
            } catch {
                model.errorMessage = error.localizedDescription
            }
        }
    }
}

@MainActor
func presentGitPullChoice(model: KiteModel, path: String) {
    let alert = NSAlert()
    alert.messageText = "选择拉取方式"
    alert.informativeText = "工作区有未提交更改时会先自动暂存，并在拉取后恢复。"
    alert.addButton(withTitle: "合并")
    alert.addButton(withTitle: "变基")
    alert.addButton(withTitle: "仅快进")
    alert.addButton(withTitle: "取消")
    let response = alert.runModal()
    let strategy: GitPullStrategyDto
    switch response {
    case .alertFirstButtonReturn:
        strategy = .merge
    case .alertSecondButtonReturn:
        strategy = .rebase
    case .alertThirdButtonReturn:
        strategy = .fastForwardOnly
    default:
        return
    }
    KiteGitService.shared.pull(model: model, path: path, strategy: strategy)
}

@MainActor
private final class KiteGitPushViewModel: ObservableObject {
    @Published var remote = ""
    @Published var branch = ""
    @Published var includeTags = false
    @Published var forceWithLease = false
    @Published private(set) var preview: GitPushPreviewDto?
    @Published private(set) var files: [GitFileChangeDto] = []
    @Published var selectedIndex: Int?
    @Published private(set) var loading = false
    @Published private(set) var loadingFiles = false
    @Published var error: String?

    let model: KiteModel
    let path: String
    var close: (() -> Void)?

    private let service = KiteGitService.shared

    init(model: KiteModel, path: String) {
        self.model = model
        self.path = path
    }

    var protectedBranch: Bool {
        preview?.protectedBranch == true
    }

    func loadPreview(selectLatest: Bool = true) async {
        loading = true
        error = nil
        do {
            let result = try await service.pushPreview(path: path, options: options())
            preview = result
            if selectLatest || selectedIndex == nil {
                selectedIndex = result.commits.indices.last
            }
            loading = false
            await loadSelectedFiles()
        } catch {
            preview = nil
            files = []
            loading = false
            self.error = error.localizedDescription
        }
    }

    func selectCommit(_ index: Int) async {
        guard preview?.commits.indices.contains(index) == true else { return }
        selectedIndex = index
        await loadSelectedFiles()
    }

    func refreshForOptionsChange() async {
        await loadPreview(selectLatest: selectedIndex == nil)
    }

    func push(upToSelected: Bool) {
        if forceWithLease && protectedBranch {
            error = "受保护分支不能强制推送"
            return
        }
        let oid: String?
        if upToSelected {
            guard let selectedIndex,
                  let commit = preview?.commits[safe: selectedIndex] else {
                error = "请先选择要推送到的提交"
                return
            }
            oid = commit.hash
        } else {
            oid = nil
        }
        let baseOptions = options()
        let pushOptions = GitPushOptionsDto(
            remote: baseOptions.remote,
            branch: baseOptions.branch,
            includeTags: baseOptions.includeTags,
            forceWithLease: baseOptions.forceWithLease,
            upToCommit: oid
        )
        KiteGitService.shared.push(model: model, path: path, options: pushOptions)
        close?()
    }

    private func options() -> GitPushOptionsDto {
        GitPushOptionsDto(
            remote: remote.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
            branch: branch.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty,
            includeTags: includeTags,
            forceWithLease: forceWithLease,
            upToCommit: nil
        )
    }

    private func loadSelectedFiles() async {
        guard let selectedIndex,
              let commit = preview?.commits[safe: selectedIndex] else {
            files = []
            return
        }
        loadingFiles = true
        do {
            files = try await service.commitFiles(path: path, oid: commit.hash)
        } catch {
            files = []
            self.error = error.localizedDescription
        }
        loadingFiles = false
    }
}

private struct KiteGitPushView: View {
    @ObservedObject var viewModel: KiteGitPushViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("推送提交")
                    .font(.system(size: 20, weight: .semibold))
                Text("检查待推送提交和文件，选择目标远程与分支。强制推送始终使用 --force-with-lease。")
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
            }

            HStack(spacing: 12) {
                LabeledContent("远程") {
                    TextField("自动选择（通常为 origin）", text: $viewModel.remote)
                        .frame(width: 180)
                }
                LabeledContent("目标分支") {
                    TextField("自动选择当前分支", text: $viewModel.branch)
                        .frame(width: 180)
                }
                Spacer()
                Button("刷新预览") {
                    Task { await viewModel.loadPreview() }
                }
                .disabled(viewModel.loading)
            }

            HStack(spacing: 12) {
                Toggle("同时推送标签", isOn: $viewModel.includeTags)
                    .onChange(of: viewModel.includeTags) { _ in
                        Task { await viewModel.refreshForOptionsChange() }
                    }
                Toggle("force-with-lease", isOn: $viewModel.forceWithLease)
                    .disabled(viewModel.protectedBranch)
                    .onChange(of: viewModel.forceWithLease) { _ in
                        Task { await viewModel.refreshForOptionsChange() }
                    }
                if viewModel.protectedBranch {
                    Text("受保护分支禁止强制推送")
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(.red)
                }
                Spacer()
            }

            HStack(spacing: 14) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("待推送提交").font(.system(size: 12, weight: .semibold))
                        Spacer()
                        Text("\(viewModel.preview?.commits.count ?? 0) 个")
                            .font(.system(size: 10.5, design: .rounded))
                            .foregroundStyle(.secondary)
                    }
                    ScrollView {
                        VStack(alignment: .leading, spacing: 4) {
                            if viewModel.loading {
                                ProgressView("正在读取推送预览…")
                                    .controlSize(.small)
                                    .padding()
                            }
                            ForEach(Array((viewModel.preview?.commits ?? []).enumerated()), id: \.element.hash) { index, commit in
                                Button {
                                    Task { await viewModel.selectCommit(index) }
                                } label: {
                                    HStack(spacing: 8) {
                                        Text(commit.shortHash)
                                            .font(.system(size: 10.5, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                        Text(commit.subject)
                                            .font(.system(size: 11.5))
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                        Spacer()
                                    }
                                    .padding(.horizontal, 8)
                                    .frame(height: 28)
                                    .contentShape(Rectangle())
                                    .background {
                                        if viewModel.selectedIndex == index {
                                            RoundedRectangle(cornerRadius: 6)
                                                .fill(Color.accentColor.opacity(0.13))
                                        }
                                    }
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .frame(maxHeight: .infinity)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
                }
                .frame(maxWidth: .infinity)

                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("选中提交更改的文件").font(.system(size: 12, weight: .semibold))
                        Spacer()
                        if viewModel.loadingFiles {
                            ProgressView().controlSize(.mini)
                        }
                    }
                    ScrollView {
                        VStack(alignment: .leading, spacing: 5) {
                            ForEach(viewModel.files, id: \.path) { file in
                                HStack(spacing: 8) {
                                    Text(file.status)
                                        .font(.system(size: 10.5, weight: .semibold, design: .monospaced))
                                        .foregroundStyle(Color.accentColor)
                                        .frame(width: 18)
                                    Text(file.path)
                                        .font(.system(size: 11, design: .monospaced))
                                        .lineLimit(1)
                                        .truncationMode(.middle)
                                    Spacer()
                                }
                                .padding(.horizontal, 8)
                                .frame(height: 24)
                            }
                            if viewModel.files.isEmpty && !viewModel.loadingFiles {
                                Text("此提交没有文件变更")
                                    .font(.system(size: 11))
                                    .foregroundStyle(.tertiary)
                                    .padding(8)
                            }
                        }
                    }
                    .frame(maxHeight: .infinity)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
                }
                .frame(maxWidth: .infinity)
            }
            .frame(maxHeight: .infinity)

            if let error = viewModel.error {
                Text(error)
                    .font(.system(size: 11))
                    .foregroundStyle(.red)
            }

            HStack {
                Spacer()
                Button("取消") { viewModel.close?() }
                Button("只推送到所选提交") {
                    viewModel.push(upToSelected: true)
                }
                .disabled(viewModel.selectedIndex == nil || viewModel.loading || (viewModel.forceWithLease && viewModel.protectedBranch))
                Button("推送") {
                    viewModel.push(upToSelected: false)
                }
                .buttonStyle(.borderedProminent)
                .disabled(viewModel.loading || (viewModel.forceWithLease && viewModel.protectedBranch))
            }
        }
        .padding(20)
        .task { await viewModel.loadPreview() }
    }
}

@MainActor
private final class KiteGitCloneViewModel: ObservableObject {
    @Published var url = ""
    @Published var target = ""
    @Published var shallow = false
    @Published var depth = "1"
    @Published var error: String?

    let directory: String
    let model: KiteModel
    var close: (() -> Void)?

    init(model: KiteModel, directory: String) {
        self.model = model
        self.directory = directory
    }

    func clone() {
        let trimmedURL = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedURL.isEmpty else {
            error = "请输入仓库 URL"
            return
        }
        let parsedDepth: UInt32?
        if shallow {
            guard let value = UInt32(depth), value > 0 else {
                error = "浅克隆深度必须是大于 0 的整数"
                return
            }
            parsedDepth = value
        } else {
            parsedDepth = nil
        }
        let targetValue = target.trimmingCharacters(in: .whitespacesAndNewlines)
        let finalTarget: String
        if targetValue.isEmpty {
            finalTarget = URL(fileURLWithPath: directory)
                .appendingPathComponent(Self.repositoryName(from: trimmedURL), isDirectory: true)
                .path
        } else if targetValue.hasPrefix("/") || targetValue.hasPrefix("~") {
            finalTarget = (targetValue as NSString).expandingTildeInPath
        } else {
            finalTarget = URL(fileURLWithPath: directory)
                .appendingPathComponent(targetValue, isDirectory: true)
                .path
        }
        error = nil
        KiteGitService.shared.clone(
            model: model,
            url: trimmedURL,
            target: finalTarget,
            depth: parsedDepth
        )
        close?()
    }

    private static func repositoryName(from url: String) -> String {
        let trimmed = url.trimmingCharacters(in: CharacterSet(charactersIn: "/\\"))
        let component = trimmed.split(whereSeparator: { $0 == "/" || $0 == "\\" || $0 == ":" }).last
        let name = component.map(String.init) ?? "repository"
        return name.hasSuffix(".git") ? String(name.dropLast(4)) : name
    }
}

private struct KiteGitCloneView: View {
    @ObservedObject var viewModel: KiteGitCloneViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("克隆 Git 仓库")
                    .font(.system(size: 20, weight: .semibold))
                Text("克隆在后台进行，可随时取消。完成后 Kite 会打开目标目录。")
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
            }

            LabeledContent("仓库 URL") {
                TextField("https://example.com/repository.git 或 SSH URL", text: $viewModel.url)
                    .frame(maxWidth: .infinity)
            }
            LabeledContent("目标目录") {
                VStack(alignment: .leading, spacing: 4) {
                    TextField("留空则使用仓库名称", text: $viewModel.target)
                    Text(viewModel.directory)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .frame(maxWidth: .infinity)
            }
            HStack {
                Toggle("浅克隆", isOn: $viewModel.shallow)
                if viewModel.shallow {
                    TextField("深度", text: $viewModel.depth)
                        .frame(width: 80)
                }
                Spacer()
            }
            if let error = viewModel.error {
                Text(error).font(.system(size: 11)).foregroundStyle(.red)
            }
            Spacer()
            HStack {
                Spacer()
                Button("取消") { viewModel.close?() }
                Button("克隆") { viewModel.clone() }
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(20)
        .frame(minWidth: 520, minHeight: 360)
    }
}

@MainActor
private final class KiteGitRemotesViewModel: ObservableObject {
    @Published private(set) var remotes: [GitRemoteDto] = []
    @Published var selectedName: String?
    @Published var name = ""
    @Published var fetchURL = ""
    @Published var pushURL = ""
    @Published var loading = false
    @Published var error: String?

    let path: String
    let model: KiteModel
    var close: (() -> Void)?
    private let service = KiteGitService.shared

    init(model: KiteModel, path: String) {
        self.model = model
        self.path = path
    }

    func load() async {
        loading = true
        do {
            remotes = try await service.listRemotes(path: path)
            if let selectedName,
               let remote = remotes.first(where: { $0.name == selectedName }) {
                select(remote)
            }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }

    func select(_ remote: GitRemoteDto) {
        selectedName = remote.name
        name = remote.name
        fetchURL = remote.fetchUrl
        pushURL = remote.pushUrl ?? ""
    }

    func newRemote() {
        selectedName = nil
        name = ""
        fetchURL = ""
        pushURL = ""
    }

    func save() async {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedFetch = fetchURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedPush = pushURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty, !trimmedFetch.isEmpty else {
            error = "远程名称和 Fetch URL 不能为空"
            return
        }
        loading = true
        do {
            if let oldName = selectedName {
                if oldName != trimmedName {
                    try await service.renameRemote(path: path, oldName: oldName, newName: trimmedName)
                }
                try await service.setRemoteURL(
                    path: path,
                    name: trimmedName,
                    fetchURL: trimmedFetch,
                    pushURL: trimmedPush.nilIfEmpty
                )
            } else {
                try await service.addRemote(path: path, name: trimmedName, url: trimmedFetch)
                if !trimmedPush.isEmpty {
                    try await service.setRemoteURL(
                        path: path,
                        name: trimmedName,
                        fetchURL: trimmedFetch,
                        pushURL: trimmedPush
                    )
                }
            }
            selectedName = trimmedName
            await load()
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }

    func deleteSelected() {
        guard let selectedName else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "删除远程 \(selectedName)？"
        alert.informativeText = "本地远程跟踪分支会一并移除。"
        alert.addButton(withTitle: "删除")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task {
            loading = true
            do {
                try await service.removeRemote(path: path, name: selectedName)
                newRemote()
                await load()
            } catch {
                self.error = error.localizedDescription
            }
            loading = false
        }
    }
}

private struct KiteGitRemotesView: View {
    @ObservedObject var viewModel: KiteGitRemotesViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("管理 Git 远程")
                    .font(.system(size: 20, weight: .semibold))
                Text("添加、改名、删除或修改 Fetch/Push URL。")
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
            }

            HStack(alignment: .top, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("远程").font(.system(size: 12, weight: .semibold))
                        Spacer()
                        Button("新增") { viewModel.newRemote() }
                            .controlSize(.small)
                    }
                    ScrollView {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(viewModel.remotes, id: \.name) { remote in
                                Button {
                                    viewModel.select(remote)
                                } label: {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(remote.name).font(.system(size: 11.5, weight: .semibold))
                                        Text(remote.fetchUrl)
                                            .font(.system(size: 10, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(8)
                                    .background {
                                        if viewModel.selectedName == remote.name {
                                            RoundedRectangle(cornerRadius: 7)
                                                .fill(Color.accentColor.opacity(0.13))
                                        }
                                    }
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
                }
                .frame(width: 300)

                VStack(alignment: .leading, spacing: 10) {
                    LabeledContent("名称") {
                        TextField("例如 origin", text: $viewModel.name)
                    }
                    LabeledContent("Fetch URL") {
                        TextField("https://…", text: $viewModel.fetchURL)
                    }
                    LabeledContent("Push URL") {
                        TextField("可选", text: $viewModel.pushURL)
                    }
                    if let error = viewModel.error {
                        Text(error).font(.system(size: 11)).foregroundStyle(.red)
                    }
                    Spacer()
                    HStack {
                        Button("删除所选远程") {
                            viewModel.deleteSelected()
                        }
                        .disabled(viewModel.selectedName == nil || viewModel.loading)
                        Spacer()
                        Button(viewModel.selectedName == nil ? "添加远程" : "保存远程") {
                            Task { await viewModel.save() }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(viewModel.loading)
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .frame(maxHeight: .infinity)

            HStack {
                if viewModel.loading {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                Button("关闭") { viewModel.close?() }
            }
        }
        .padding(20)
        .frame(minWidth: 760, minHeight: 500)
        .task { await viewModel.load() }
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
