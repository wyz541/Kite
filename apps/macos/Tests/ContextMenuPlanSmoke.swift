import Foundation

@main
struct ContextMenuPlanSmoke {
    static func main() {
        let request = ContextMenuRequestDto(
            surface: "browser.list",
            platform: "macos",
            primaryPath: "/tmp/main.rs",
            targets: [
                ContextMenuTargetDto(
                    path: "/tmp/main.rs",
                    displayName: "main.rs",
                    kind: "file"
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

        let native = plan.groups.last?.entries.last?.action
        precondition(native?.actionId == "system.native-menu")
        precondition(native?.enabled == false)
        precondition(native?.disabledReasonCode == "NATIVE_PLANE_UNAVAILABLE")

        let gitActions = plan.submenus.first { $0.id == "git" }?
            .groups.flatMap(\.entries)
            .compactMap(\.action) ?? []
        precondition(gitActions.contains { $0.actionId == "git.commit" })
        precondition(gitActions.contains { $0.actionId == "git.log" })

        print("ContextMenuPlanSmoke passed")
    }
}
