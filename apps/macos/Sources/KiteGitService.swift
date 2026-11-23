import AppKit
import Combine
import Foundation

struct KiteGitOperationSnapshot: Identifiable {
    let id = UUID()
    let label: String
    let monitor: GitProgressMonitor
    var progress: GitOperationProgressDto
}

private enum KiteGitNetworkRequest: Sendable {
    case fetch(path: String, allRemotes: Bool)
    case pull(path: String, options: GitPullOptionsDto)
    case push(path: String, options: GitPushOptionsDto)
    case clone(url: String, target: String, depth: UInt32?)
    case pullThenPush(path: String, pull: GitPullOptionsDto, push: GitPushOptionsDto)

    var label: String {
        switch self {
        case .fetch: return "获取远程更新"
        case .pull, .pullThenPush: return "拉取远程更新"
        case .push: return "推送提交"
        case .clone: return "克隆仓库"
        }
    }
}

private enum KiteGitNetworkOutcome: Sendable {
    case message(String)
    case cloned(String)
}

private enum KiteGitOperationError: Error, Sendable {
    case busy
}

@MainActor
final class KiteGitService: ObservableObject {
    static let shared = KiteGitService()

    @Published private(set) var operation: KiteGitOperationSnapshot?

    private let core = KiteCore()
    private var task: Task<Void, Never>?

    private init() {}

    func fetch(model: KiteModel, path: String, allRemotes: Bool) {
        start(.fetch(path: path, allRemotes: allRemotes), model: model)
    }

    func pull(model: KiteModel, path: String, strategy: GitPullStrategyDto) {
        start(
            .pull(
                path: path,
                options: GitPullOptionsDto(
                    strategy: strategy,
                    autoStash: true,
                    remote: nil,
                    branch: nil
                )
            ),
            model: model
        )
    }

    func push(
        model: KiteModel,
        path: String,
        options: GitPushOptionsDto
    ) {
        start(.push(path: path, options: options), model: model)
    }

    func clone(
        model: KiteModel,
        url: String,
        target: String,
        depth: UInt32?
    ) {
        start(.clone(url: url, target: target, depth: depth), model: model)
    }

    func cancel() {
        operation?.monitor.cancel()
    }

    func pushPreview(path: String, options: GitPushOptionsDto) async throws -> GitPushPreviewDto {
        let core = self.core
        return try await Task.detached(priority: .userInitiated) {
            try core.gitPushPreview(path: path, options: options)
        }.value
    }

    func commitFiles(path: String, oid: String) async throws -> [GitFileChangeDto] {
        let core = self.core
        return try await Task.detached(priority: .userInitiated) {
            try core.gitCommitFiles(path: path, oid: oid)
        }.value
    }

    func repository(path: String) async throws -> GitRepositoryDto {
        let core = self.core
        return try await Task.detached(priority: .utility) {
            try core.gitRepository(path: path)
        }.value
    }

    func listRemotes(path: String) async throws -> [GitRemoteDto] {
        let core = self.core
        return try await Task.detached(priority: .userInitiated) {
            try core.gitListRemotes(path: path)
        }.value
    }

    func addRemote(path: String, name: String, url: String) async throws {
        let core = self.core
        try await Task.detached(priority: .userInitiated) {
            try core.gitAddRemote(path: path, name: name, url: url)
        }.value
    }

    func renameRemote(path: String, oldName: String, newName: String) async throws {
        let core = self.core
        try await Task.detached(priority: .userInitiated) {
            try core.gitRenameRemote(path: path, oldName: oldName, newName: newName)
        }.value
    }

    func setRemoteURL(
        path: String,
        name: String,
        fetchURL: String,
        pushURL: String?
    ) async throws {
        let core = self.core
        try await Task.detached(priority: .userInitiated) {
            try core.gitSetRemoteUrl(
                path: path,
                name: name,
                fetchUrl: fetchURL,
                pushUrl: pushURL
            )
        }.value
    }

    func removeRemote(path: String, name: String) async throws {
        let core = self.core
        try await Task.detached(priority: .userInitiated) {
            try core.gitRemoveRemote(path: path, name: name)
        }.value
    }

    func initRepository(path: String) async throws -> String {
        let core = self.core
        return try await Task.detached(priority: .userInitiated) {
            try core.gitInit(path: path)
        }.value
    }

    private func start(_ request: KiteGitNetworkRequest, model: KiteModel) {
        guard operation == nil else {
            model.errorMessage = "已有 Git 网络操作正在进行"
            return
        }
        let monitor = GitProgressMonitor()
        operation = KiteGitOperationSnapshot(
            label: request.label,
            monitor: monitor,
            progress: monitor.snapshot()
        )
        model.errorMessage = nil
        let operationID = operation?.id
        let finished = LockedFlag()
        task = Task { [weak self, weak model] in
            let progressTask = Task { @MainActor [weak self] in
                while !Task.isCancelled && !finished.value {
                    guard let self, self.operation?.id == operationID else { break }
                    self.operation?.progress = monitor.snapshot()
                    try? await Task.sleep(nanoseconds: 100_000_000)
                }
            }

            let request = request
            let core = self?.core
            let result: Result<KiteGitNetworkOutcome, Error> = await Task.detached(priority: .userInitiated) {
                Result {
                    guard let core else {
                        throw KiteGitOperationError.busy
                    }
                    return try Self.execute(
                        request,
                        core: core,
                        credentials: nil,
                        monitor: monitor
                    )
                }
            }.value
            finished.value = true
            progressTask.cancel()
            _ = await progressTask.value

            guard let self else { return }
            self.operation = nil
            guard let model else { return }
            self.finish(result, request: request, model: model)
        }
    }

    private func finish(
        _ result: Result<KiteGitNetworkOutcome, Error>,
        request: KiteGitNetworkRequest,
        model: KiteModel
    ) {
        switch result {
        case .success(.message(let message)):
            model.errorMessage = nil
            model.updateGitOperationStatus(message)
            model.refresh()
        case .success(.cloned(let path)):
            model.errorMessage = nil
            model.updateGitOperationStatus("已克隆到 \(path)")
            model.loadRoot(path)
        case .failure(let error):
            let message = Self.gitErrorMessage(error)
            guard error is KiteError else {
                model.errorMessage = message
                return
            }
            let reflected = String(reflecting: error).lowercased()
            if reflected.contains("authenticationrequired") {
                guard let credentials = Self.promptCredentials(message: message) else {
                    model.errorMessage = message
                    return
                }
                retry(request, credentials: credentials, model: model)
            } else if reflected.contains("pushrejected") {
                recoverRejectedPush(request, message: message, model: model)
            } else if reflected.contains("cancelled") {
                model.updateGitOperationStatus(message)
                if case .clone(_, let target, _) = request,
                   let partialPath = Self.partialClonePath(from: message) {
                    promptCloneCleanup(
                        model: model,
                        partialPath: partialPath,
                        target: target
                    )
                }
            } else {
                model.errorMessage = message
            }
        }
    }

    private func retry(
        _ request: KiteGitNetworkRequest,
        credentials: GitCredentialsDto,
        model: KiteModel
    ) {
        guard operation == nil else { return }
        let monitor = GitProgressMonitor()
        operation = KiteGitOperationSnapshot(
            label: request.label,
            monitor: monitor,
            progress: monitor.snapshot()
        )
        let operationID = operation?.id
        let finished = LockedFlag()
        task = Task { [weak self, weak model] in
            let progressTask = Task { @MainActor [weak self] in
                while !Task.isCancelled && !finished.value {
                    guard let self, self.operation?.id == operationID else { break }
                    self.operation?.progress = monitor.snapshot()
                    try? await Task.sleep(nanoseconds: 100_000_000)
                }
            }
            let request = request
            let core = self?.core
            let result: Result<KiteGitNetworkOutcome, Error> = await Task.detached(priority: .userInitiated) {
                Result {
                    guard let core else {
                        throw KiteGitOperationError.busy
                    }
                    return try Self.execute(
                        request,
                        core: core,
                        credentials: credentials,
                        monitor: monitor
                    )
                }
            }.value
            finished.value = true
            progressTask.cancel()
            _ = await progressTask.value
            guard let self else { return }
            self.operation = nil
            guard let model else { return }
            self.finish(result, request: request, model: model)
        }
    }

    private func recoverRejectedPush(
        _ request: KiteGitNetworkRequest,
        message: String,
        model: KiteModel
    ) {
        guard case .push(let path, let pushOptions) = request else {
            model.errorMessage = message
            return
        }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "推送被拒绝"
        alert.informativeText = "远程包含本地没有的提交。先整合远程历史，然后自动重试推送。"
        alert.addButton(withTitle: "合并后重试")
        alert.addButton(withTitle: "变基后重试")
        alert.addButton(withTitle: "取消")
        let response = alert.runModal()
        let strategy: GitPullStrategyDto
        if response == .alertFirstButtonReturn {
            strategy = .merge
        } else if response == .alertSecondButtonReturn {
            strategy = .rebase
        } else {
            model.errorMessage = message
            return
        }
        start(
            .pullThenPush(
                path: path,
                pull: GitPullOptionsDto(
                    strategy: strategy,
                    autoStash: true,
                    remote: pushOptions.remote,
                    branch: pushOptions.branch
                ),
                push: pushOptions
            ),
            model: model
        )
    }

    private func promptCloneCleanup(model: KiteModel, partialPath: String, target: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "克隆已取消"
        alert.informativeText = "未完成的目录位于：\n\(partialPath)\n\n是否删除这个未完成目录？"
        alert.addButton(withTitle: "删除")
        alert.addButton(withTitle: "保留")
        guard alert.runModal() == .alertFirstButtonReturn else {
            Task { [weak self] in
                guard let self else { return }
                do {
                    let core = self.core
                    try await Task.detached(priority: .userInitiated) {
                        try core.gitAdoptPartialClone(partialPath: partialPath, target: target)
                    }.value
                    model.updateGitOperationStatus("已保留克隆目录：\(target)")
                } catch {
                    model.errorMessage = error.localizedDescription
                }
            }
            return
        }
        Task { [weak self] in
            guard let self else { return }
            do {
                let core = self.core
                try await Task.detached(priority: .userInitiated) {
                    try core.gitDeletePartialClone(path: partialPath)
                }.value
                model.updateGitOperationStatus("未完成的克隆目录已删除")
            } catch {
                model.errorMessage = error.localizedDescription
            }
        }
    }

    private static func promptCredentials(message: String) -> GitCredentialsDto? {
        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.messageText = "需要 Git 凭据"
        alert.informativeText = "\(message)\n\n凭据只用于本次操作，不会保存。"

        let username = NSTextField(frame: NSRect(x: 0, y: 28, width: 360, height: 24))
        username.placeholderString = "用户名"
        let password = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 360, height: 24))
        password.placeholderString = "密码或个人访问令牌"
        let fields = NSStackView(views: [username, password])
        fields.orientation = .vertical
        fields.spacing = 8
        fields.frame = NSRect(x: 0, y: 0, width: 360, height: 56)
        alert.accessoryView = fields
        alert.addButton(withTitle: "连接")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn,
              !username.stringValue.isEmpty,
              !password.stringValue.isEmpty else {
            return nil
        }
        return GitCredentialsDto(username: username.stringValue, password: password.stringValue)
    }

    private static func gitErrorMessage(_ error: Error) -> String {
        guard error is KiteError else { return error.localizedDescription }
        let reflected = String(reflecting: error)
        guard let start = reflected.firstIndex(of: "\""),
              let end = reflected.lastIndex(of: "\""),
              start < end else {
            return reflected
        }
        let valueStart = reflected.index(after: start)
        return String(reflected[valueStart..<end])
            .replacingOccurrences(of: "\\\"", with: "\"")
            .replacingOccurrences(of: "\\\\", with: "\\")
    }

    private static func partialClonePath(from message: String) -> String? {
        let marker = "未完成目录保留在 "
        guard let range = message.range(of: marker) else { return nil }
        let path = message[range.upperBound...]
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return path.isEmpty ? nil : path
    }

    private nonisolated static func execute(
        _ request: KiteGitNetworkRequest,
        core: KiteCore,
        credentials: GitCredentialsDto?,
        monitor: GitProgressMonitor
    ) throws -> KiteGitNetworkOutcome {
        switch request {
        case .fetch(let path, let allRemotes):
            let message = try core.gitFetch(
                path: path,
                options: GitFetchOptionsDto(remote: nil, allRemotes: allRemotes, prune: true),
                credentials: credentials,
                monitor: monitor
            )
            return .message(message)
        case .pull(let path, let options):
            let message = try core.gitPull(
                path: path,
                options: options,
                credentials: credentials,
                monitor: monitor
            )
            return .message(message)
        case .push(let path, let options):
            let message = try core.gitPush(
                path: path,
                options: options,
                credentials: credentials,
                monitor: monitor
            )
            return .message(message)
        case .clone(let url, let target, let depth):
            let result = try core.gitClone(
                url: url,
                target: target,
                options: GitCloneOptionsDto(shallowDepth: depth),
                credentials: credentials,
                monitor: monitor
            )
            return .cloned(result.path)
        case .pullThenPush(let path, let pull, let push):
            _ = try core.gitPull(
                path: path,
                options: pull,
                credentials: credentials,
                monitor: monitor
            )
            let message = try core.gitPush(
                path: path,
                options: push,
                credentials: credentials,
                monitor: monitor
            )
            return .message(message)
        }
    }
}

private final class LockedFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var stored = false

    var value: Bool {
        get {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }
        set {
            lock.lock()
            stored = newValue
            lock.unlock()
        }
    }
}
