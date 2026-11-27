import AppKit
import Combine
import Foundation
import UniformTypeIdentifiers

enum KiteOpenWithCatalogState: Equatable, Sendable {
    case ready
    case loading
    case unavailable

    var dto: OpenWithCatalogStatusDto {
        switch self {
        case .ready: return .ready
        case .loading: return .loading
        case .unavailable: return .unavailable
        }
    }
}

struct KiteOpenWithDiscoveredApplication: Equatable, Sendable {
    let applicationID: String
    let displayName: String
    let isDefault: Bool
}

struct KiteOpenWithApplication: Equatable, Sendable {
    let applicationID: String
    let displayName: String
    let isDefault: Bool
    let isRecent: Bool
    let isAvailable: Bool
    let unavailableReason: String?
}

struct KiteOpenWithCatalog: Equatable, Sendable {
    let state: KiteOpenWithCatalogState
    let applications: [KiteOpenWithApplication]

    static let loading = KiteOpenWithCatalog(state: .loading, applications: [])
    static let unavailable = KiteOpenWithCatalog(state: .unavailable, applications: [])

    var dto: OpenWithCatalogDto {
        OpenWithCatalogDto(
            status: state.dto,
            applications: applications.map { application in
                OpenWithApplicationDto(
                    applicationId: application.applicationID,
                    displayName: application.displayName,
                    isDefault: application.isDefault,
                    isRecent: application.isRecent,
                    isAvailable: application.isAvailable,
                    unavailableReason: application.unavailableReason
                )
            }
        )
    }
}

enum KiteOpenWithCatalogBuilder {
    static let mruLimit = 5

    static func make(
        discovered: [KiteOpenWithDiscoveredApplication],
        mru: [String],
        mruLimit: Int = mruLimit
    ) -> [KiteOpenWithApplication] {
        var unique: [KiteOpenWithDiscoveredApplication] = []
        for application in discovered {
            let applicationID = application.applicationID.trimmingCharacters(
                in: .whitespacesAndNewlines
            )
            let displayName = application.displayName.trimmingCharacters(
                in: .whitespacesAndNewlines
            )
            guard !applicationID.isEmpty, !displayName.isEmpty else { continue }
            if let index = unique.firstIndex(where: {
                $0.applicationID.caseInsensitiveCompare(applicationID) == .orderedSame
            }) {
                if application.isDefault {
                    unique[index] = KiteOpenWithDiscoveredApplication(
                        applicationID: unique[index].applicationID,
                        displayName: unique[index].displayName,
                        isDefault: true
                    )
                }
                continue
            }
            unique.append(
                KiteOpenWithDiscoveredApplication(
                    applicationID: applicationID,
                    displayName: displayName,
                    isDefault: application.isDefault
                )
            )
        }

        let defaultApplication = unique.first(where: \.isDefault)
        let remaining = unique.filter { !$0.isDefault }
        var ordered: [KiteOpenWithApplication] = []
        if let defaultApplication {
            ordered.append(
                KiteOpenWithApplication(
                    applicationID: defaultApplication.applicationID,
                    displayName: defaultApplication.displayName,
                    isDefault: true,
                    isRecent: false,
                    isAvailable: true,
                    unavailableReason: nil
                )
            )
        }

        var used = Set(ordered.map { $0.applicationID.lowercased() })
        var recentCount = 0
        for recentID in mru {
            guard recentCount < max(0, mruLimit) else { break }
            guard let application = remaining.first(where: {
                $0.applicationID.caseInsensitiveCompare(recentID) == .orderedSame
            }) else { continue }
            let key = application.applicationID.lowercased()
            guard used.insert(key).inserted else { continue }
            ordered.append(
                KiteOpenWithApplication(
                    applicationID: application.applicationID,
                    displayName: application.displayName,
                    isDefault: false,
                    isRecent: true,
                    isAvailable: true,
                    unavailableReason: nil
                )
            )
            recentCount += 1
        }

        for application in remaining {
            let key = application.applicationID.lowercased()
            guard used.insert(key).inserted else { continue }
            ordered.append(
                KiteOpenWithApplication(
                    applicationID: application.applicationID,
                    displayName: application.displayName,
                    isDefault: false,
                    isRecent: false,
                    isAvailable: true,
                    unavailableReason: nil
                )
            )
        }
        return ordered
    }
}

enum KiteOpenWithDiscovery {
    static func discover(path: String, mru: [String]) -> KiteOpenWithCatalog {
        guard KiteOpenWithFiles.isEligibleFile(path) else {
            return .unavailable
        }
        let url = URL(fileURLWithPath: path)
        let workspace = NSWorkspace.shared
        let defaultURL = workspace.urlForApplication(toOpen: url)
        let compatibleURLs = workspace.urlsForApplications(toOpen: url)

        var discovered: [KiteOpenWithDiscoveredApplication] = []
        if let defaultURL,
           let application = application(at: defaultURL, isDefault: true) {
            discovered.append(application)
        }
        for applicationURL in compatibleURLs {
            if let application = application(at: applicationURL, isDefault: false) {
                discovered.append(application)
            }
        }

        let applications = KiteOpenWithCatalogBuilder.make(
            discovered: discovered,
            mru: mru
        )
        let currentIDs = Set(applications.map { $0.applicationID.lowercased() })
        let sanitizedMRU = mru.filter { currentIDs.contains($0.lowercased()) }
        if sanitizedMRU != mru {
            KiteOpenWithMRUStore.save(sanitizedMRU)
        }
        guard !applications.isEmpty else {
            return .unavailable
        }
        return KiteOpenWithCatalog(state: .ready, applications: applications)
    }

    private static func application(
        at url: URL,
        isDefault: Bool
    ) -> KiteOpenWithDiscoveredApplication? {
        guard let bundle = Bundle(url: url),
              let bundleID = bundle.bundleIdentifier?.trimmingCharacters(
                in: .whitespacesAndNewlines
              ),
              !bundleID.isEmpty else {
            return nil
        }
        let displayName = (
            bundle.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String
        )?.trimmingCharacters(in: .whitespacesAndNewlines)
        let fallbackName = (
            bundle.object(forInfoDictionaryKey: "CFBundleName") as? String
        )?.trimmingCharacters(in: .whitespacesAndNewlines)
        let fileName = url.deletingPathExtension().lastPathComponent
        let name = [displayName, fallbackName, fileName]
            .compactMap { $0 }
            .first { !$0.isEmpty } ?? bundleID
        return KiteOpenWithDiscoveredApplication(
            applicationID: bundleID,
            displayName: name,
            isDefault: isDefault
        )
    }
}

enum KiteOpenWithFiles {
    static var supportedApplicationTypes: [UTType] {
        [.application]
    }

    static func isEligibleFile(_ path: String) -> Bool {
        guard !path.isEmpty else { return false }
        let fileManager = FileManager.default
        var isDirectory: ObjCBool = false
        guard fileManager.fileExists(atPath: path, isDirectory: &isDirectory),
              !isDirectory.boolValue,
              fileManager.isReadableFile(atPath: path) else {
            return false
        }
        return URL(fileURLWithPath: path).isFileURL
    }

    static func cacheKey(for path: String) -> String {
        let url = URL(fileURLWithPath: path).standardizedFileURL
        if let contentType = try? url.resourceValues(forKeys: [.contentTypeKey]).contentType {
            return "uti:\(contentType.identifier)"
        }
        let extensionName = url.pathExtension.lowercased()
        return extensionName.isEmpty ? "url:\(url.path)" : "ext:\(extensionName)"
    }
}

enum KiteOpenWithMRUStore {
    private static let defaultsKey = "kite.macos.open-with.mru.v1"

    static func load() -> [String] {
        UserDefaults.standard.stringArray(forKey: defaultsKey) ?? []
    }

    static func save(_ applicationIDs: [String]) {
        UserDefaults.standard.set(
            Array(applicationIDs.prefix(KiteOpenWithCatalogBuilder.mruLimit)),
            forKey: defaultsKey
        )
    }

    static func record(_ applicationID: String) {
        var applicationIDs = load().filter {
            $0.caseInsensitiveCompare(applicationID) != .orderedSame
        }
        applicationIDs.insert(applicationID, at: 0)
        save(applicationIDs)
    }
}

@MainActor
final class KiteOpenWithCatalogCache: ObservableObject {
    static let shared = KiteOpenWithCatalogCache()

    @Published private(set) var revision = 0
    private var catalogs: [String: KiteOpenWithCatalog] = [:]
    private var inFlight = Set<String>()
    private init() {}

    func catalog(for path: String?) -> KiteOpenWithCatalog {
        guard let path, KiteOpenWithFiles.isEligibleFile(path) else {
            return .unavailable
        }
        let key = KiteOpenWithFiles.cacheKey(for: path)
        if let catalog = catalogs[key] {
            return catalog
        }
        catalogs[key] = .loading
        if inFlight.insert(key).inserted {
            let mru = KiteOpenWithMRUStore.load()
            Task {
                let catalog = await Task.detached(priority: .userInitiated) {
                    KiteOpenWithDiscovery.discover(path: path, mru: mru)
                }.value
                self.inFlight.remove(key)
                self.catalogs[key] = catalog
                self.revision &+= 1
            }
        }
        return .loading
    }

    func recordUse(path: String, applicationID: String) {
        guard KiteOpenWithFiles.isEligibleFile(path) else { return }
        KiteOpenWithMRUStore.record(applicationID)
        let key = KiteOpenWithFiles.cacheKey(for: path)
        catalogs.removeValue(forKey: key)
        revision &+= 1
    }
}

enum KiteOpenWithLauncher {
    @MainActor
    static func launch(
        path: String,
        applicationID: String,
        completion: @escaping (Error?) -> Void
    ) {
        guard KiteOpenWithFiles.isEligibleFile(path) else {
            completion(KiteOpenWithError.notAFile)
            return
        }
        guard let applicationURL = NSWorkspace.shared.urlForApplication(
            withBundleIdentifier: applicationID
        ) else {
            completion(KiteOpenWithError.applicationUnavailable)
            return
        }
        launch(path: path, applicationURL: applicationURL, completion: completion)
    }

    @MainActor
    static func chooseAndLaunch(
        path: String,
        completion: @escaping (Error?) -> Void
    ) {
        guard KiteOpenWithFiles.isEligibleFile(path) else {
            completion(KiteOpenWithError.notAFile)
            return
        }
        let panel = NSOpenPanel()
        panel.title = "选择其他应用"
        panel.message = "选择用于打开“\((path as NSString).lastPathComponent)”的应用。"
        panel.prompt = "打开"
        panel.allowedContentTypes = KiteOpenWithFiles.supportedApplicationTypes
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        panel.resolvesAliases = true
        guard panel.runModal() == .OK, let applicationURL = panel.url else {
            return
        }
        launch(path: path, applicationURL: applicationURL, completion: completion)
    }

    @MainActor
    private static func launch(
        path: String,
        applicationURL: URL,
        completion: @escaping (Error?) -> Void
    ) {
        let configuration = NSWorkspace.OpenConfiguration()
        NSWorkspace.shared.open(
            [URL(fileURLWithPath: path)],
            withApplicationAt: applicationURL,
            configuration: configuration
        ) { _, error in
            DispatchQueue.main.async {
                if error == nil,
                   let applicationID = Bundle(url: applicationURL)?.bundleIdentifier {
                    KiteOpenWithCatalogCache.shared.recordUse(
                        path: path,
                        applicationID: applicationID
                    )
                }
                completion(error)
            }
        }
    }
}

enum KiteOpenWithError: LocalizedError {
    case notAFile
    case applicationUnavailable

    var errorDescription: String? {
        switch self {
        case .notAFile:
            return "“打开方式”仅适用于存在且可读的普通文件。"
        case .applicationUnavailable:
            return "所选应用已卸载或不再处理此文件类型，请重新选择应用。"
        }
    }
}
