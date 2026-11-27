import Foundation

@main
struct ContextMenuPlanSmoke {
    static func main() {
        let path = "/tmp/main.rs"
        let gitRepository = VcsMenuRepositoryDto(
            id: "git:/tmp",
            tool: .git,
            repositoryRoot: "/tmp",
            worktreeRoot: "/tmp",
            metadataDir: "/tmp/.git",
            commonDir: "/tmp/.git",
            displayName: "tmp",
            head: VcsHeadStateDto(kind: .branch, name: "main", oid: "abc", shortOid: "abc"),
            upstream: "origin/main",
            ahead: 0,
            behind: 0,
            changes: VcsChangeSummaryDto(
                staged: 1,
                unstaged: 1,
                untracked: 0,
                ignored: 0,
                conflicted: 0
            ),
            conflicts: [],
            operation: nil,
            svn: nil,
            stateRevision: "git-revision"
        )
        let svnRepository = VcsMenuRepositoryDto(
            id: "svn:/tmp",
            tool: .svn,
            repositoryRoot: "/tmp",
            worktreeRoot: "/tmp",
            metadataDir: nil,
            commonDir: nil,
            displayName: "tmp",
            head: VcsHeadStateDto(kind: .none, name: nil, oid: nil, shortOid: nil),
            upstream: nil,
            ahead: 0,
            behind: 0,
            changes: VcsChangeSummaryDto(
                staged: nil,
                unstaged: 1,
                untracked: 0,
                ignored: 0,
                conflicted: 0
            ),
            conflicts: [],
            operation: nil,
            svn: SvnWorkingCopyMenuStateDto(
                wcRoot: "/tmp",
                url: "https://example.test/trunk",
                revision: 1,
                switched: false,
                propertyModified: false,
                missing: 0,
                conflicted: 0,
                unversioned: 0,
                cleanupRequired: false
            ),
            stateRevision: "svn-revision"
        )
        let vcsMenu = VcsMenuSnapshotDto(
            schemaVersion: 1,
            observedAtUnixMs: 0,
            gitAvailable: true,
            svnAvailable: true,
            repositories: [gitRepository, svnRepository],
            targets: [
                VcsTargetMenuStateDto(
                    path: path,
                    links: [
                        VcsTargetLinkDto(
                            repositoryId: "git:/tmp",
                            tool: .git,
                            relativePath: "main.rs",
                            relation: .tracked,
                            staged: true,
                            unstaged: true,
                            conflicted: false,
                            switched: false,
                            propertyModified: false,
                            missing: false
                        ),
                        VcsTargetLinkDto(
                            repositoryId: "svn:/tmp",
                            tool: .svn,
                            relativePath: "main.rs",
                            relation: .svnVersioned,
                            staged: nil,
                            unstaged: true,
                            conflicted: false,
                            switched: false,
                            propertyModified: false,
                            missing: false
                        ),
                    ]
                )
            ],
            stateRevision: "snapshot-revision"
        )
        let request = ContextMenuRequestDto(
            surface: "browser.list",
            platform: "macos",
            primaryPath: "/tmp/main.rs",
            targets: [
                ContextMenuTargetDto(
                    path: "/tmp/main.rs",
                    displayName: "main.rs",
                    kind: "file",
                    exists: true,
                    readable: true
                )
            ],
            workingDirectory: "/tmp",
            containerPath: "/tmp",
            containerIsPrimaryTarget: false,
            loading: false,
            canPaste: true,
            canCreate: true,
            supportsTransferOps: true,
            supportsCut: true,
            supportsRename: true,
            supportsDelete: true,
            supportsFavorite: true,
            isFavorite: false,
            gitAvailable: true,
            svnAvailable: true,
            agentAvailable: true,
            quickLookAvailable: true,
            revealAvailable: true,
            shareAvailable: true,
            propertiesAvailable: true,
            textEditorAvailable: true,
            mindmapAvailable: false,
            mindmapImportAvailable: false,
            isShellNamespace: false,
            nativePlaneAvailable: false,
            openWith: OpenWithCatalogDto(
                status: .loading,
                applications: []
            ),
            tools: ToolCatalogDto(
                status: "unavailable",
                tools: []
            ),
            toolPolicy: ToolPolicyDto(
                pinnedToolIds: [],
                hiddenToolIds: [],
                userOrder: [],
                recentToolIds: []
            ),
            vcsMenu: vcsMenu,
            compact: true,
            hideUnavailable: true,
            confirmHighRisk: true
        )

        let plan = composeContextMenu(request: request)
        precondition(plan.schemaVersion == 1)
        precondition(plan.title == "main.rs")
        precondition(plan.groups.last?.id == "G99")
        precondition(plan.submenus.contains { $0.id == "file-actions" })
        precondition(plan.submenus.contains { $0.id == "git" })
        precondition(plan.submenus.contains { $0.id == "svn" })
        precondition(plan.submenus.contains { $0.id == "open-with" })

        let native = plan.groups.last?.entries.last?.action
        precondition(native?.actionId == "system.native-menu")
        precondition(native?.enabled == true)
        precondition(native?.targetKinds == ["file"])

        let gitActions = plan.submenus.first { $0.id == "git" }?
            .groups.flatMap(\.entries)
            .compactMap(\.action) ?? []
        precondition(gitActions.contains { $0.actionId == "git.commit" })
        precondition(gitActions.contains { $0.actionId == "git.log" })

        print("ContextMenuPlanSmoke passed")
    }
}
