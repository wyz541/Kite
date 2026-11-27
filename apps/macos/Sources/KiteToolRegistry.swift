import AppKit
import Combine
import Foundation

struct KiteToolCatalogModel: Equatable, Sendable {
    let state: String
    let discoveries: [KiteToolDiscoveryRecord]

    static let loading = KiteToolCatalogModel(state: "loading", discoveries: [])
    static let unavailable = KiteToolCatalogModel(state: "unavailable", discoveries: [])

    var dto: ToolCatalogDto {
        ToolCatalogDto(
            status: state,
            tools: discoveries.map(\.dto)
        )
    }
}

struct KiteToolDiscoveryRecord: Equatable, Sendable {
    let toolID: String
    let status: String
    let executableOrBundle: String?
    let version: String?

    var isAvailable: Bool { status == "available" }

    var dto: ToolDiscoveryDto {
        ToolDiscoveryDto(
            toolId: toolID,
            status: status,
            executableOrBundle: executableOrBundle,
            version: version
        )
    }
}

enum KiteToolArgumentKind: String, Equatable, Sendable {
    case literal
    case primaryPath = "primary_path"
    case targetPaths = "target_paths"
    case workingDirectory = "working_directory"
    case repositoryRoot = "repository_root"
}

struct KiteToolArgument: Equatable, Sendable {
    let kind: KiteToolArgumentKind
    let value: String?
}

struct KiteToolRecipe: Equatable, Sendable {
    let role: String
    let arguments: [KiteToolArgument]
    let workingDirectory: String?
}

struct KiteToolCapability: Equatable, Sendable {
    let targetShapes: [String]
    let roles: [String]
    let minPaths: Int
    let maxPaths: Int?
    let allTargetsRequired: Bool
    let pathFilter: String

    init(
        targetShapes: [String],
        roles: [String],
        minPaths: Int,
        maxPaths: Int?,
        allTargetsRequired: Bool,
        pathFilter: String
    ) {
        self.targetShapes = targetShapes
        self.roles = roles
        self.minPaths = minPaths
        self.maxPaths = maxPaths
        self.allTargetsRequired = allTargetsRequired
        self.pathFilter = pathFilter
    }

    init(dto: ToolCapabilityDto) {
        self.init(
            targetShapes: dto.targetShapes,
            roles: dto.roles,
            minPaths: Int(dto.minPaths),
            maxPaths: dto.maxPaths.map(Int.init),
            allTargetsRequired: dto.allTargetsRequired,
            pathFilter: dto.pathFilter
        )
    }

    func matches(role: String, pathCount: Int) -> Bool {
        roles.contains(role)
            && pathCount >= minPaths
            && (maxPaths.map { pathCount <= $0 } ?? true)
    }
}

struct KiteToolDefinition: Identifiable, Equatable, Sendable {
    let toolID: String
    let category: String
    let displayName: String
    let description: String
    let platform: String
    let launchKind: String
    let executableIdentity: String?
    let bundleIdentity: String?
    let capabilities: [KiteToolCapability]
    let recipes: [KiteToolRecipe]

    var id: String { toolID }

    init(
        toolID: String,
        category: String,
        displayName: String,
        description: String,
        platform: String,
        launchKind: String,
        executableIdentity: String?,
        bundleIdentity: String?,
        capabilities: [KiteToolCapability],
        recipes: [KiteToolRecipe]
    ) {
        self.toolID = toolID
        self.category = category
        self.displayName = displayName
        self.description = description
        self.platform = platform
        self.launchKind = launchKind
        self.executableIdentity = executableIdentity
        self.bundleIdentity = bundleIdentity
        self.capabilities = capabilities
        self.recipes = recipes
    }

    init(dto: ToolDefinitionDto) {
        self.init(
            toolID: dto.toolId,
            category: dto.category,
            displayName: dto.displayName,
            description: dto.description,
            platform: dto.platform,
            launchKind: dto.launchKind,
            executableIdentity: dto.executableIdentity,
            bundleIdentity: dto.bundleIdentity,
            capabilities: dto.capabilities.map(KiteToolCapability.init),
            recipes: dto.recipes.map { recipe in
                KiteToolRecipe(
                    role: recipe.role,
                    arguments: recipe.arguments.map { argument in
                        KiteToolArgument(
                            kind: KiteToolArgumentKind(rawValue: argument.kind) ?? .literal,
                            value: argument.value
                        )
                    },
                    workingDirectory: recipe.workingDirectory
                )
            }
        )
    }

    var isKiteInternal: Bool { launchKind == "kite_internal" }

    func recipe(for role: String) -> KiteToolRecipe? {
        recipes.first { $0.role == role }
    }

    func expandedInvocation(
        role: String,
        paths: [String],
        workingDirectory: String,
        repositoryRoot: String?
    ) -> KiteToolInvocation? {
        guard capabilities.contains(where: { $0.matches(role: role, pathCount: paths.count) }),
              let recipe = recipe(for: role) else {
            return nil
        }
        guard let executable = executableIdentity ?? bundleIdentity else { return nil }

        var arguments: [String] = []
        for argument in recipe.arguments {
            switch argument.kind {
            case .literal:
                guard let value = argument.value else { return nil }
                arguments.append(value)
            case .primaryPath:
                guard let first = paths.first else { return nil }
                arguments.append(first)
            case .targetPaths:
                arguments.append(contentsOf: paths)
            case .workingDirectory:
                arguments.append(workingDirectory)
            case .repositoryRoot:
                arguments.append(repositoryRoot ?? workingDirectory)
            }
        }

        let resolvedWorkingDirectory: String?
        switch recipe.workingDirectory {
        case "context":
            resolvedWorkingDirectory = workingDirectory
        case "primary_target":
            resolvedWorkingDirectory = paths.first ?? workingDirectory
        case "repository_root":
            resolvedWorkingDirectory = repositoryRoot ?? workingDirectory
        default:
            resolvedWorkingDirectory = nil
        }
        return KiteToolInvocation(
            toolID: toolID,
            executable: executable,
            arguments: arguments,
            workingDirectory: resolvedWorkingDirectory
        )
    }
}

struct KiteToolInvocation: Equatable, Sendable {
    let toolID: String
    let executable: String
    let arguments: [String]
    let workingDirectory: String?
}

struct KiteToolPolicy: Codable, Equatable, Sendable {
    var pinnedToolIDs: [String] = []
    var hiddenToolIDs: [String] = []
    var userOrder: [String] = []
    var recentToolIDs: [String] = []

    var dto: ToolPolicyDto {
        ToolPolicyDto(
            pinnedToolIds: pinnedToolIDs,
            hiddenToolIds: hiddenToolIDs,
            userOrder: userOrder,
            recentToolIds: recentToolIDs
        )
    }

    func isPinned(_ toolID: String) -> Bool {
        pinnedToolIDs.contains { $0.caseInsensitiveCompare(toolID) == .orderedSame }
    }

    func isHidden(_ toolID: String) -> Bool {
        hiddenToolIDs.contains { $0.caseInsensitiveCompare(toolID) == .orderedSame }
    }
}

@MainActor
final class KiteToolPolicyStore: ObservableObject {
    static let shared = KiteToolPolicyStore()

    @Published private(set) var policy: KiteToolPolicy
    private let defaults: UserDefaults
    private let defaultsKey = "kite.macos.tool-policy.v1"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let data = defaults.data(forKey: defaultsKey),
           let decoded = try? JSONDecoder().decode(KiteToolPolicy.self, from: data) {
            policy = decoded
        } else {
            policy = KiteToolPolicy()
        }
    }

    func setPinned(_ toolID: String, pinned: Bool) {
        policy.pinnedToolIDs.removeAll { $0.caseInsensitiveCompare(toolID) == .orderedSame }
        if pinned {
            policy.pinnedToolIDs.append(toolID)
        }
        persist()
    }

    func setHidden(_ toolID: String, hidden: Bool) {
        policy.hiddenToolIDs.removeAll { $0.caseInsensitiveCompare(toolID) == .orderedSame }
        if hidden {
            policy.hiddenToolIDs.append(toolID)
        }
        persist()
    }

    func recordUse(_ toolID: String) {
        policy.recentToolIDs.removeAll { $0.caseInsensitiveCompare(toolID) == .orderedSame }
        policy.recentToolIDs.insert(toolID, at: 0)
        policy.recentToolIDs = Array(policy.recentToolIDs.prefix(5))
        persist()
    }

    func clearRecent() {
        policy.recentToolIDs = []
        persist()
    }

    func move(_ toolID: String, delta: Int, considering toolIDs: [String]) {
        guard delta != 0 else { return }
        var order = mergedOrder(toolIDs: toolIDs)
        guard let index = order.firstIndex(where: {
            $0.caseInsensitiveCompare(toolID) == .orderedSame
        }) else { return }
        let destination = index + delta
        guard order.indices.contains(destination) else { return }
        order.swapAt(index, destination)
        policy.userOrder = order
        persist()
    }

    func mergedOrder(toolIDs: [String]) -> [String] {
        var order = policy.userOrder
        for toolID in toolIDs where !order.contains(where: {
            $0.caseInsensitiveCompare(toolID) == .orderedSame
        }) {
            order.append(toolID)
        }
        return order
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(policy) else { return }
        defaults.set(data, forKey: defaultsKey)
        objectWillChange.send()
    }
}

struct KiteToolCatalogBuilder {
    static func make(
        definitions: [KiteToolDefinition],
        discovered: [KiteToolDiscoveryRecord],
        textEditorEligible: Bool,
        mindmapEligible: Bool,
        gitAvailable: Bool
    ) -> KiteToolCatalogModel {
        let known = Dictionary(uniqueKeysWithValues: definitions.map {
            ($0.toolID.lowercased(), $0)
        })
        var unique: [String: KiteToolDiscoveryRecord] = [:]
        for record in discovered where known[record.toolID.lowercased()] != nil {
            let key = record.toolID.lowercased()
            if unique[key]?.isAvailable == true { continue }
            unique[key] = record
        }
        let filtered = unique.values
            .filter { record in
                guard record.isAvailable else { return false }
                switch record.toolID {
                case "kite.text-editor":
                    return textEditorEligible
                case "kite.mindmap":
                    return mindmapEligible
                case "kite.git-client":
                    return gitAvailable
                default:
                    return true
                }
            }
            .sorted { $0.toolID.localizedCaseInsensitiveCompare($1.toolID) == .orderedAscending }
        return KiteToolCatalogModel(state: "ready", discoveries: filtered)
    }
}

enum KiteToolDiscovery {
    static let applicationBundleIDs: Set<String> = [
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "com.microsoft.VSCode",
        "com.todesktop.230313mzl4w4u92",
        "dev.zed.Zed",
        "com.sublimetext.4",
        "com.apple.dt.Xcode",
        "com.ScooterSoftware.BeyondCompare",
        "com.blackpixel.kaleidoscope",
    ]

    static func discover(
        definitions: [KiteToolDefinition],
        textEditorEligible: Bool,
        mindmapEligible: Bool,
        gitAvailable: Bool
    ) -> [KiteToolDiscoveryRecord] {
        definitions.map { definition in
            if definition.isKiteInternal {
                let available: Bool
                switch definition.toolID {
                case "kite.text-editor":
                    available = textEditorEligible
                case "kite.mindmap":
                    available = mindmapEligible
                case "kite.git-client":
                    available = gitAvailable
                default:
                    available = false
                }
                return discovery(
                    definition,
                    available: available,
                    identity: definition.executableIdentity
                )
            }

            if definition.launchKind == "workspace_application" {
                guard let bundleID = definition.bundleIdentity,
                      let url = NSWorkspace.shared.urlForApplication(
                        withBundleIdentifier: bundleID
                      ) else {
                    return discovery(
                        definition,
                        status: "not_installed",
                        identity: nil
                    )
                }
                return discovery(definition, available: true, identity: url.path)
            }

            if let identity = definition.executableIdentity,
               let executable = resolveExecutable(identity) {
                return discovery(definition, available: true, identity: executable)
            }
            if let bundleID = definition.bundleIdentity,
               let url = NSWorkspace.shared.urlForApplication(
                withBundleIdentifier: bundleID
               ) {
                return discovery(definition, available: true, identity: url.path)
            }
            return discovery(definition, status: "not_installed", identity: nil)
        }
    }

    static func resolveExecutable(_ identity: String) -> String? {
        let url = URL(fileURLWithPath: identity)
        if url.isFileURL, url.path.hasPrefix("/"), FileManager.default.isExecutableFile(atPath: url.path) {
            return url.path
        }
        let pathDirectories = executableSearchDirectories()
        for directory in pathDirectories {
            let candidate = URL(fileURLWithPath: directory).appendingPathComponent(identity)
            if FileManager.default.isExecutableFile(atPath: candidate.path) {
                return candidate.path
            }
        }
        return nil
    }

    private static func executableSearchDirectories() -> [String] {
        var directories = (ProcessInfo.processInfo.environment["PATH"] ?? "")
            .split(separator: ":")
            .map(String.init)
        directories.append(contentsOf: [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "\(NSHomeDirectory())/.local/bin",
            "\(NSHomeDirectory())/.cargo/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ])
        var seen = Set<String>()
        return directories.filter { seen.insert($0).inserted }
    }

    private static func discovery(
        _ definition: KiteToolDefinition,
        available: Bool,
        identity: String?
    ) -> KiteToolDiscoveryRecord {
        discovery(
            definition,
            status: available ? "available" : "unsupported",
            identity: identity
        )
    }

    private static func discovery(
        _ definition: KiteToolDefinition,
        status: String,
        identity: String?
    ) -> KiteToolDiscoveryRecord {
        KiteToolDiscoveryRecord(
            toolID: definition.toolID,
            status: status,
            executableOrBundle: identity,
            version: nil
        )
    }
}

@MainActor
final class KiteToolCatalogCache: ObservableObject {
    static let shared = KiteToolCatalogCache()

    @Published private(set) var revision = 0
    private var catalogs: [String: KiteToolCatalogModel] = [:]
    private var inFlight = Set<String>()
    private let registry = KiteToolRegistry.shared

    private init() {}

    func catalog(
        paths: [String],
        textEditorEligible: Bool,
        mindmapEligible: Bool,
        gitAvailable: Bool
    ) -> KiteToolCatalogModel {
        guard !paths.isEmpty else { return .unavailable }
        let key = [
            paths.joined(separator: "\u{1f}"),
            String(textEditorEligible),
            String(mindmapEligible),
            String(gitAvailable),
        ].joined(separator: "\u{1e}")
        if let catalog = catalogs[key] {
            return catalog
        }
        catalogs[key] = .loading
        if inFlight.insert(key).inserted {
            let definitions = registry.definitions
            Task {
                let discovered = await Task.detached(priority: .userInitiated) {
                    KiteToolDiscovery.discover(
                        definitions: definitions,
                        textEditorEligible: textEditorEligible,
                        mindmapEligible: mindmapEligible,
                        gitAvailable: gitAvailable
                    )
                }.value
                self.inFlight.remove(key)
                self.catalogs[key] = KiteToolCatalogBuilder.make(
                    definitions: definitions,
                    discovered: discovered,
                    textEditorEligible: textEditorEligible,
                    mindmapEligible: mindmapEligible,
                    gitAvailable: gitAvailable
                )
                self.revision &+= 1
            }
        }
        return .loading
    }

    func refresh() {
        catalogs.removeAll()
        inFlight.removeAll()
        revision &+= 1
    }

    func discovery(for toolID: String) -> KiteToolDiscoveryRecord? {
        catalogs.values
            .lazy
            .flatMap(\.discoveries)
            .first { $0.toolID.caseInsensitiveCompare(toolID) == .orderedSame }
    }
}

@MainActor
final class KiteToolRegistry {
    static let shared = KiteToolRegistry()

    let definitions: [KiteToolDefinition]

    private init() {
        definitions = contextMenuToolDefinitions(platform: "macos").map(KiteToolDefinition.init)
    }

    func definition(for toolID: String) -> KiteToolDefinition? {
        definitions.first { $0.toolID.caseInsensitiveCompare(toolID) == .orderedSame }
    }

    func resolvedIdentity(for definition: KiteToolDefinition) -> String? {
        if definition.isKiteInternal {
            return definition.executableIdentity
        }
        if let bundleID = definition.bundleIdentity,
           definition.launchKind == "workspace_application",
           let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) {
            return url.path
        }
        if let identity = definition.executableIdentity,
           let executable = KiteToolDiscovery.resolveExecutable(identity) {
            return executable
        }
        if let bundleID = definition.bundleIdentity,
           let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) {
            return url.path
        }
        return nil
    }
}

enum KiteToolLauncher {
    @MainActor
    static func launch(
        toolID: String,
        role: String,
        paths: [String],
        workingDirectory: String,
        repositoryRoot: String? = nil,
        completion: @escaping (Error?) -> Void
    ) {
        guard !paths.isEmpty else {
            completion(KiteToolLaunchError.noTargets)
            return
        }
        guard let definition = KiteToolRegistry.shared.definition(for: toolID) else {
            completion(KiteToolLaunchError.unknownTool(toolID))
            return
        }
        guard let invocation = definition.expandedInvocation(
            role: role,
            paths: paths,
            workingDirectory: workingDirectory,
            repositoryRoot: repositoryRoot
        ) else {
            completion(KiteToolLaunchError.recipeUnavailable)
            return
        }
        guard !definition.isKiteInternal else {
            completion(KiteToolLaunchError.internalTool(toolID))
            return
        }

        if definition.launchKind == "workspace_application" {
            guard let resolved = KiteToolRegistry.shared.resolvedIdentity(for: definition),
                  let applicationURL = applicationURL(for: resolved, definition: definition)
            else {
                completion(KiteToolLaunchError.toolUnavailable)
                return
            }
            let configuration = NSWorkspace.OpenConfiguration()
            NSWorkspace.shared.open(
                paths.map { URL(fileURLWithPath: $0) },
                withApplicationAt: applicationURL,
                configuration: configuration
            ) { _, error in
                DispatchQueue.main.async {
                    if error == nil {
                        KiteToolPolicyStore.shared.recordUse(toolID)
                    }
                    completion(error)
                }
            }
            return
        }

        guard let executable = resolvedProcessExecutable(
            invocation: invocation,
            definition: definition
        ) else {
            completion(KiteToolLaunchError.toolUnavailable)
            return
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = invocation.arguments
        if let directory = invocation.workingDirectory {
            process.currentDirectoryURL = URL(fileURLWithPath: directory)
        }
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            KiteToolPolicyStore.shared.recordUse(toolID)
            completion(nil)
        } catch {
            completion(error)
        }
    }

    @MainActor
    private static func resolvedProcessExecutable(
        invocation: KiteToolInvocation,
        definition: KiteToolDefinition
    ) -> String? {
        if let resolved = KiteToolDiscovery.resolveExecutable(invocation.executable) {
            return resolved
        }
        guard let bundleID = definition.bundleIdentity,
              let appURL = NSWorkspace.shared.urlForApplication(
                withBundleIdentifier: bundleID
              ) else {
            return nil
        }
        let candidates: [String]
        switch definition.toolID {
        case "editor.vscode":
            candidates = ["Contents/Resources/app/bin/code"]
        case "editor.cursor":
            candidates = ["Contents/Resources/app/bin/cursor"]
        case "editor.sublime":
            candidates = ["Contents/SharedSupport/bin/subl"]
        case "diff.beyond-compare":
            candidates = ["Contents/MacOS/bcomp"]
        default:
            candidates = ["Contents/MacOS/\(definition.toolID.split(separator: ".").last ?? "tool")"]
        }
        return candidates
            .map { appURL.appendingPathComponent($0).path }
            .first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    @MainActor
    private static func applicationURL(
        for identity: String,
        definition: KiteToolDefinition
    ) -> URL? {
        let url = URL(fileURLWithPath: identity)
        if url.path.hasPrefix("/"), FileManager.default.fileExists(atPath: url.path) {
            return url
        }
        guard let bundleID = definition.bundleIdentity else { return nil }
        return NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    }
}

enum KiteToolLaunchError: LocalizedError {
    case noTargets
    case unknownTool(String)
    case internalTool(String)
    case recipeUnavailable
    case toolUnavailable

    var errorDescription: String? {
        switch self {
        case .noTargets:
            return "“在工具中打开”缺少目标路径。"
        case .unknownTool(let toolID):
            return "未知工具：\(toolID)"
        case .internalTool(let toolID):
            return "内建工具必须由 Kite 分发：\(toolID)"
        case .recipeUnavailable:
            return "该工具不支持当前目标或操作。"
        case .toolUnavailable:
            return "所选工具已卸载或无法启动。"
        }
    }
}
