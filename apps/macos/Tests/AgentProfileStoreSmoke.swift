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

        let transportCapabilities = KiteCore().agentTransportCapabilities()
        precondition(transportCapabilities.count == 3)
        precondition(transportCapabilities.filter { $0.sessionSupported }.count == 1)
        precondition(transportCapabilities.first { $0.transport == .acpStdio }?.sessionSupported == true)
        precondition(transportCapabilities.first { $0.transport == .acpHttp }?.sessionSupported == false)
        precondition(transportCapabilities.first { $0.transport == .cli }?.sessionSupported == false)

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
            credentialEnvVar: nil,
            capabilities: capabilities
        )
        let created = try store.createProfile(profile: profile)
        precondition(!created.id.isEmpty)

        let afterCreate = store.snapshot()
        precondition(afterCreate.profiles.count == 1)
        precondition(afterCreate.defaultProfileId == created.id)

        let httpProbe = AgentProfile(
            id: "",
            name: "HTTP Probe",
            transport: .acpHttp,
            executable: nil,
            endpoint: "http://127.0.0.1:9/acp",
            arguments: [],
            environment: [],
            model: nil,
            workingDirectory: .ask,
            permission: .readOnly,
            timeoutSeconds: 30,
            outputLimitBytes: 65_536,
            credentialRef: nil,
            credentialEnvVar: nil,
            capabilities: capabilities
        )
        do {
            _ = try store.createProfile(profile: httpProbe)
            preconditionFailure("Connection-test-only HTTP profile was saved")
        } catch {
            // Expected: Core rejects profiles that cannot start a session.
        }

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
            credentialEnvVar: nil,
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
