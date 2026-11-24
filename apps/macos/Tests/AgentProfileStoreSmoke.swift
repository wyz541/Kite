import Foundation

@main
struct AgentProfileStoreSmoke {
    static func main() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("kite-agent-profile-store-\(UUID().uuidString)")
        let config = root.appendingPathComponent("agent.json")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let store = AgentProfileStore.withPath(path: config.path)
        let initial = store.snapshot()
        precondition(initial.profiles.isEmpty)
        precondition(initial.defaultProfileId == nil)
        precondition(initial.warning == nil)

        let capabilities = AgentCapabilities(
            readSelection: true,
            readWorkspace: false,
            writeFiles: false,
            executeCommands: false,
            network: false,
            permissionRequests: true
        )
        let profile = AgentProfile(
            id: "",
            name: "Smoke Agent",
            transport: .acpStdio,
            executable: "agent",
            endpoint: nil,
            arguments: ["--read-only"],
            environment: [AgentEnvironmentVariable(name: "LANG", value: "en_US.UTF-8")],
            model: nil,
            workingDirectory: .selectedItem,
            permission: .readOnly,
            timeoutSeconds: 30,
            outputLimitBytes: 65_536,
            credentialRef: "keychain:kite/agent/smoke",
            capabilities: capabilities
        )
        let created = try store.createProfile(profile: profile)
        precondition(!created.id.isEmpty)

        let afterCreate = store.snapshot()
        precondition(afterCreate.profiles.count == 1)
        precondition(afterCreate.defaultProfileId == created.id)

        let unsafe = AgentProfile(
            id: "",
            name: "Unsafe Agent",
            transport: .acpStdio,
            executable: "agent",
            endpoint: nil,
            arguments: [],
            environment: [AgentEnvironmentVariable(name: "OPENAI_API_KEY", value: "not-stored-secret")],
            model: nil,
            workingDirectory: .selectedItem,
            permission: .readOnly,
            timeoutSeconds: 30,
            outputLimitBytes: 65_536,
            credentialRef: "keychain:kite/agent/smoke",
            capabilities: capabilities
        )
        do {
            _ = try store.createProfile(profile: unsafe)
            preconditionFailure("Sensitive environment variable was accepted")
        } catch {
            // Expected: the Rust Core rejects sensitive environment values.
        }

        let persisted = try String(contentsOf: config, encoding: .utf8)
        precondition(persisted.contains("\"credential_ref\": \"keychain:kite/agent/smoke\""))
        precondition(!persisted.contains("not-stored-secret"))
        precondition(!persisted.contains("OPENAI_API_KEY"))

        let deleted = try store.deleteProfile(profileId: created.id)
        precondition(deleted)
        let afterDelete = store.snapshot()
        precondition(afterDelete.profiles.isEmpty)
        precondition(afterDelete.defaultProfileId == nil)

        print("AgentProfileStoreSmoke passed")
    }
}
