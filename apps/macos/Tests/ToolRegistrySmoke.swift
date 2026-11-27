import Foundation

@main
struct ToolRegistrySmoke {
    static func main() throws {
        let editor = KiteToolDefinition(
            toolID: "editor.synthetic",
            category: "editor_ide",
            displayName: "Synthetic Editor",
            description: "test editor",
            platform: "macos",
            launchKind: "process",
            executableIdentity: "/bin/echo",
            bundleIdentity: nil,
            capabilities: [
                KiteToolCapability(
                    targetShapes: ["file", "multi"],
                    roles: ["open"],
                    minPaths: 1,
                    maxPaths: nil,
                    allTargetsRequired: true,
                    pathFilter: "any"
                )
            ],
            recipes: [
                KiteToolRecipe(
                    role: "open",
                    arguments: [
                        KiteToolArgument(kind: .literal, value: "--new-window"),
                        KiteToolArgument(kind: .targetPaths, value: nil),
                    ],
                    workingDirectory: "context"
                )
            ]
        )
        let diff = KiteToolDefinition(
            toolID: "diff.synthetic",
            category: "diff_merge",
            displayName: "Synthetic Diff",
            description: "test diff",
            platform: "macos",
            launchKind: "process",
            executableIdentity: "/usr/bin/diff",
            bundleIdentity: nil,
            capabilities: [
                KiteToolCapability(
                    targetShapes: ["file", "multi"],
                    roles: ["compare"],
                    minPaths: 2,
                    maxPaths: 2,
                    allTargetsRequired: true,
                    pathFilter: "any"
                )
            ],
            recipes: [
                KiteToolRecipe(
                    role: "compare",
                    arguments: [
                        KiteToolArgument(kind: .targetPaths, value: nil),
                    ],
                    workingDirectory: nil
                )
            ]
        )

        let unsafePath = "/tmp/a file; echo not-a-shell.txt"
        let editorInvocation = editor.expandedInvocation(
            role: "open",
            paths: [unsafePath],
            workingDirectory: "/tmp",
            repositoryRoot: nil
        )
        precondition(editorInvocation?.arguments == ["--new-window", unsafePath])
        precondition(editorInvocation?.workingDirectory == "/tmp")

        precondition(
            editor.expandedInvocation(
                role: "open",
                paths: [],
                workingDirectory: "/tmp",
                repositoryRoot: nil
            ) == nil
        )
        let diffInvocation = diff.expandedInvocation(
            role: "compare",
            paths: ["/tmp/a.txt", "/tmp/b.txt"],
            workingDirectory: "/tmp",
            repositoryRoot: nil
        )
        precondition(diffInvocation?.arguments == ["/tmp/a.txt", "/tmp/b.txt"])
        precondition(
            diff.expandedInvocation(
                role: "compare",
                paths: ["/tmp/a.txt"],
                workingDirectory: "/tmp",
                repositoryRoot: nil
            ) == nil
        )

        let policy = KiteToolPolicy(
            pinnedToolIDs: ["diff.synthetic"],
            hiddenToolIDs: ["editor.hidden"],
            userOrder: ["editor.synthetic", "diff.synthetic"],
            recentToolIDs: ["diff.synthetic"]
        )
        precondition(policy.isPinned("diff.synthetic"))
        let catalog = KiteToolCatalogBuilder.make(
            definitions: [editor, diff],
            discovered: [
                KiteToolDiscoveryRecord(
                    toolID: "editor.synthetic",
                    status: "available",
                    executableOrBundle: "/bin/echo",
                    version: nil
                ),
                KiteToolDiscoveryRecord(
                    toolID: "editor.synthetic",
                    status: "available",
                    executableOrBundle: "/bin/echo",
                    version: nil
                ),
                KiteToolDiscoveryRecord(
                    toolID: "diff.synthetic",
                    status: "not_installed",
                    executableOrBundle: nil,
                    version: nil
                ),
                KiteToolDiscoveryRecord(
                    toolID: "stale.unknown",
                    status: "available",
                    executableOrBundle: "/tmp/stale",
                    version: nil
                ),
            ],
            textEditorEligible: true,
            mindmapEligible: false,
            gitAvailable: false
        )
        precondition(catalog.state == "ready")
        precondition(catalog.discoveries.map(\.toolID) == ["editor.synthetic"])
        precondition(catalog.dto.tools.count == 1)
        precondition(catalog.dto.tools.first?.executableOrBundle == "/bin/echo")

        print("ToolRegistrySmoke passed")
    }
}
