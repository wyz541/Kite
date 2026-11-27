import Foundation

@main
struct OpenWithSmoke {
    static func main() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("kite-open-with-smoke-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let file = root.appendingPathComponent("sample.txt")
        try Data("kite".utf8).write(to: file)
        let directory = root.appendingPathComponent("folder", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        precondition(KiteOpenWithFiles.isEligibleFile(file.path))
        precondition(!KiteOpenWithFiles.isEligibleFile(directory.path))
        precondition(!KiteOpenWithFiles.isEligibleFile(root.appendingPathComponent("missing").path))

        let discovered = [
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.default",
                displayName: "Default",
                isDefault: true
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.recent-b",
                displayName: "Recent B",
                isDefault: false
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.recent-a",
                displayName: "Recent A",
                isDefault: false
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.recent-b",
                displayName: "Recent B Duplicate",
                isDefault: false
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.recent-c",
                displayName: "Recent C",
                isDefault: false
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.recent-d",
                displayName: "Recent D",
                isDefault: false
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.recent-e",
                displayName: "Recent E",
                isDefault: false
            ),
            KiteOpenWithDiscoveredApplication(
                applicationID: "com.example.remaining",
                displayName: "Remaining",
                isDefault: false
            ),
        ]
        let mru = [
            "com.example.uninstalled",
            "com.example.recent-b",
            "com.example.recent-a",
            "com.example.recent-c",
            "com.example.recent-d",
            "com.example.recent-e",
        ]
        let applications = KiteOpenWithCatalogBuilder.make(discovered: discovered, mru: mru)
        precondition(applications.map(\.displayName) == [
            "Default",
            "Recent B",
            "Recent A",
            "Recent C",
            "Recent D",
            "Recent E",
            "Remaining",
        ])
        precondition(applications.first?.isDefault == true)
        precondition(!applications.contains { $0.applicationID == "com.example.uninstalled" })
        precondition(applications.filter { $0.applicationID == "com.example.recent-b" }.count == 1)
        precondition(applications.filter(\.isRecent).count == 5)

        let catalog = KiteOpenWithCatalog(state: .ready, applications: applications)
        let dto = catalog.dto
        precondition(dto.status == .ready)
        precondition(dto.applications.first?.applicationId == "com.example.default")

        print("OpenWithSmoke passed")
    }
}
