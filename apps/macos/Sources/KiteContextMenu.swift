import AppKit
import SwiftUI

enum KiteNativeMenuBridge {
    static var isAvailable: Bool {
        guard let services = NSApp?.servicesMenu else { return false }
        return !services.items.isEmpty
    }

    @discardableResult
    static func showServices() -> Bool {
        guard let services = NSApp?.servicesMenu, !services.items.isEmpty else {
            return false
        }
        services.popUp(positioning: nil, at: NSEvent.mouseLocation, in: nil)
        return true
    }
}

enum KiteContextMenuSurface: String {
    case browserList = "browser.list"
    case browserColumns = "browser.columns"
    case preview = "preview"
    case searchResults = "search.results"
    case searchColumn = "search.column"
    case searchGlobal = "search.global"
    case searchContent = "search.content"
    case sidebarLocations = "sidebar.locations"
    case sidebarFavorites = "sidebar.favorites"
    case sidebarVolumes = "sidebar.volumes"
    case background = "background"
}

enum KiteContextMenuTarget {
    case entries(BrowserEntry, columnIndex: Int?, surface: KiteContextMenuSurface)
    case background(path: String, title: String)
    case sidebar(
        path: String,
        name: String,
        isDirectory: Bool,
        surface: KiteContextMenuSurface
    )
    case preview(BrowserEntry)
}

struct KiteContextMenuEntrySnapshot {
    let path: String
    let name: String
    let isDirectory: Bool
    let kind: String
}

/// Captured when the menu is created. Dispatch never reads the live selection,
/// preview target, or navigation history again.
struct KiteFrozenContextMenuContext {
    let surface: KiteContextMenuSurface
    let targets: [KiteContextMenuEntrySnapshot]
    let entries: [BrowserEntry]
    let primaryPath: String?
    let containerPath: String
    let workingDirectory: String
    let containerIsPrimary: Bool
    let isFavorite: Bool

    @MainActor
    init(model: KiteModel, target: KiteContextMenuTarget) {
        switch target {
        case .entries(let clicked, let columnIndex, let surface):
            let column = columnIndex.flatMap { index in
                model.columns.indices.contains(index) ? model.columns[index] : nil
            }
            let selected = column?.entries.filter {
                column?.selectedPaths.contains($0.path) == true
            } ?? []
            let frozenEntries = if selected.count > 1,
                                   selected.contains(where: { $0.path == clicked.path }) {
                selected
            } else {
                [clicked]
            }
            self.surface = surface
            self.entries = frozenEntries
            self.targets = frozenEntries.map(Self.snapshot)
            self.primaryPath = clicked.path
            self.containerPath = clicked.isDirectory
                ? clicked.path
                : (clicked.path as NSString).deletingLastPathComponent
            self.workingDirectory = self.containerPath
            self.containerIsPrimary = false
            self.isFavorite = false

        case .background(let path, _):
            self.surface = .background
            self.entries = []
            self.targets = []
            self.primaryPath = nil
            self.containerPath = path
            self.workingDirectory = path
            self.containerIsPrimary = true
            self.isFavorite = false

        case .sidebar(let path, let name, let isDirectory, let surface):
            let snapshot = KiteContextMenuEntrySnapshot(
                path: path,
                name: name,
                isDirectory: isDirectory,
                kind: Self.targetKind(path: path, isDirectory: isDirectory)
            )
            self.surface = surface
            self.entries = []
            self.targets = [snapshot]
            self.primaryPath = path
            self.containerPath = isDirectory
                ? path
                : (path as NSString).deletingLastPathComponent
            self.workingDirectory = self.containerPath
            self.containerIsPrimary = false
            self.isFavorite = surface == .sidebarFavorites

        case .preview(let entry):
            self.surface = .preview
            self.entries = [entry]
            self.targets = [Self.snapshot(entry)]
            self.primaryPath = entry.path
            self.containerPath = entry.isDirectory
                ? entry.path
                : (entry.path as NSString).deletingLastPathComponent
            self.workingDirectory = self.containerPath
            self.containerIsPrimary = false
            self.isFavorite = false
        }
    }

    var menuPaths: [String] { targets.map(\.path) }
    var toolPaths: [String] { menuPaths.isEmpty ? [containerPath] : menuPaths }
    var primaryTarget: KiteContextMenuEntrySnapshot? { targets.first }

    private static func snapshot(_ entry: BrowserEntry) -> KiteContextMenuEntrySnapshot {
        KiteContextMenuEntrySnapshot(
            path: entry.path,
            name: entry.name,
            isDirectory: entry.isDirectory,
            kind: targetKind(path: entry.path, isDirectory: entry.isDirectory)
        )
    }

    private static func targetKind(path: String, isDirectory: Bool) -> String {
        if KitePlatform.archiveFormat(path) != nil {
            return "archive"
        }
        return isDirectory ? "directory" : "file"
    }
}

struct KiteContextMenuView: View {
    @ObservedObject var model: KiteModel
    @ObservedObject private var openWithCatalog = KiteOpenWithCatalogCache.shared
    @ObservedObject private var toolCatalog = KiteToolCatalogCache.shared
    @ObservedObject private var toolPolicy = KiteToolPolicyStore.shared
    @EnvironmentObject private var agent: KiteAgentPanelModel
    @State private var context: KiteFrozenContextMenuContext
    @State private var vcsMenu: VcsMenuSnapshotDto

    @MainActor
    init(model: KiteModel, target: KiteContextMenuTarget) {
        self.model = model
        let context = KiteFrozenContextMenuContext(model: model, target: target)
        _context = State(initialValue: context)
        // VCS state is part of the menu snapshot. Keep one snapshot for the
        // lifetime of the menu and revalidate it again before any write.
        _vcsMenu = State(initialValue: loadVcsMenuSnapshot(
            paths: context.toolPaths,
            workingDirectory: context.workingDirectory
        ))
    }

    var body: some View {
        let _ = openWithCatalog.revision
        let _ = toolCatalog.revision
        let plan = makePlan()
        KiteContextMenuPlanView(groups: plan.groups, submenus: plan.submenus) { action in
            KiteContextMenuRunner(model: model, agent: agent, context: context)
                .perform(action)
        }
    }

    private func makePlan() -> ContextMenuPlanDto {
        let textEditorAvailable = context.targets.count == 1
            && !(context.primaryTarget?.isDirectory ?? false)
            && KitePlatform.isEditableTextFile(context.primaryTarget?.path ?? "")
        let mindmapAvailable = context.targets.count == 1
            && !(context.primaryTarget?.isDirectory ?? false)
            && KitePlatform.isMindmapFile(context.primaryTarget?.path ?? "")
        let request = ContextMenuRequestDto(
            surface: context.surface.rawValue,
            platform: "macos",
            primaryPath: context.primaryPath,
            targets: context.targets.map { target in
                ContextMenuTargetDto(
                    path: target.path,
                    displayName: target.name,
                    kind: target.kind,
                    exists: FileManager.default.fileExists(atPath: target.path),
                    readable: FileManager.default.isReadableFile(atPath: target.path)
                )
            },
            workingDirectory: context.workingDirectory,
            containerPath: context.containerPath,
            containerIsPrimaryTarget: context.containerIsPrimary,
            loading: false,
            canPaste: true,
            canCreate: KitePlatform.canModify(context.containerPath),
            supportsTransferOps: context.targets.allSatisfy {
                KitePlatform.canModify($0.path)
            },
            supportsCut: context.targets.allSatisfy { KitePlatform.canModify($0.path) },
            supportsRename: context.targets.count == 1
                && KitePlatform.canModify(context.primaryPath ?? context.containerPath),
            supportsDelete: !context.targets.isEmpty
                && context.targets.allSatisfy { KitePlatform.canModify($0.path) },
            supportsFavorite: !context.targets.isEmpty,
            isFavorite: context.isFavorite,
            gitAvailable: vcsMenu.gitAvailable,
            svnAvailable: vcsMenu.svnAvailable,
            agentAvailable: agent.hasConfiguredAgent,
            quickLookAvailable: !context.targets.isEmpty,
            revealAvailable: !context.targets.isEmpty,
            shareAvailable: !context.targets.isEmpty,
            propertiesAvailable: context.targets.count == 1,
            textEditorAvailable: textEditorAvailable,
            mindmapAvailable: mindmapAvailable,
            mindmapImportAvailable: context.targets.count == 1
                && !(context.primaryTarget?.isDirectory ?? false)
                && KitePlatform.isMindmapImportFile(context.primaryTarget?.path ?? ""),
            isShellNamespace: false,
            nativePlaneAvailable: KiteNativeMenuBridge.isAvailable,
            openWith: openWithCatalog.catalog(
                for: context.targets.count == 1 && !(context.primaryTarget?.isDirectory ?? false)
                    ? context.primaryTarget?.path
                    : nil
            ).dto,
            tools: toolCatalog.catalog(
                paths: context.toolPaths,
                textEditorEligible: textEditorAvailable,
                mindmapEligible: mindmapAvailable,
                gitAvailable: vcsMenu.gitAvailable
            ).dto,
            toolPolicy: toolPolicy.policy.dto,
            vcsMenu: vcsMenu,
            compact: true,
            hideUnavailable: true,
            confirmHighRisk: true
        )
        return composeContextMenu(request: request)
    }
}

private struct KiteContextMenuPlanView: View {
    let groups: [ContextMenuGroupDto]
    let submenus: [ContextMenuSubmenuDto]
    let perform: (ContextMenuActionDto) -> Void

    var body: some View {
        ForEach(Array(groups.enumerated()), id: \.offset) { groupIndex, group in
            if groupIndex > 0 {
                Divider()
            }
            ForEach(Array(group.entries.enumerated()), id: \.offset) { _, entry in
                KiteContextMenuEntryView(
                    entry: entry,
                    submenus: submenus,
                    perform: perform
                )
            }
        }
    }
}

private struct KiteContextMenuEntryView: View {
    let entry: ContextMenuEntryDto
    let submenus: [ContextMenuSubmenuDto]
    let perform: (ContextMenuActionDto) -> Void

    var body: some View {
        if let action = entry.action {
            if action.risk == "high" {
                Button(role: .destructive) {
                    perform(action)
                } label: {
                    Label(action.label, systemImage: iconName(action.iconKey))
                }
                .disabled(!action.enabled)
                .help(action.disabledReason ?? "")
            } else {
                Button {
                    perform(action)
                } label: {
                    Label(action.label, systemImage: iconName(action.iconKey))
                }
                .disabled(!action.enabled)
                .help(action.disabledReason ?? "")
            }
        } else if let submenuID = entry.submenuId,
                  let submenu = submenus.first(where: { $0.id == submenuID }) {
            Menu {
                KiteContextMenuPlanView(
                    groups: submenu.groups,
                    submenus: submenus,
                    perform: perform
                )
            } label: {
                Label(submenu.label, systemImage: iconName(submenu.iconKey))
            }
        }
    }

    private func iconName(_ key: String) -> String {
        switch key {
        case "refresh": return "arrow.clockwise"
        case "open": return "arrow.up.forward.app"
        case "open-with": return "square.grid.2x2"
        case "open-in": return "arrow.up.forward.app"
        case "compare-with": return "rectangle.split.2x1"
        case "quick-look": return "eye"
        case "copy": return "doc.on.doc"
        case "cut": return "scissors"
        case "copy-to": return "folder.badge.plus"
        case "copy-path": return "point.topleft.down.to.point.bottomright.curvepath"
        case "paste": return "doc.on.clipboard"
        case "new-folder": return "folder.badge.plus"
        case "new-file": return "doc.badge.plus"
        case "favorite": return "star.badge.plus"
        case "rename": return "pencil"
        case "trash": return "trash"
        case "archive", "extract": return "doc.zipper"
        case "agent": return "sparkles"
        case "vcs": return "arrow.triangle.branch"
        case "system", "properties": return "info.circle"
        case "reveal": return "folder"
        case "share": return "square.and.arrow.up"
        case "editor": return "square.and.pencil"
        case "mindmap": return "point.3.connected.trianglepath.dotted"
        case "native": return "ellipsis.circle"
        default: return "circle"
        }
    }
}

@MainActor
private struct KiteContextMenuRunner {
    let model: KiteModel
    let agent: KiteAgentPanelModel
    let context: KiteFrozenContextMenuContext

    func perform(_ action: ContextMenuActionDto) {
        guard validateTargets(action) else { return }
        let paths = action.targetPaths
        let primaryPath = paths.first ?? context.primaryPath ?? context.containerPath
        if isVcsWriteAction(action.actionId) {
            guard validateVcsRevision(action, path: primaryPath) else { return }
        }
        switch action.actionId {
        case "navigation.refresh":
            model.refresh()
        case "open.default":
            openDefault(primaryPath)
        case "open.with":
            guard action.payloadKind == "application",
                  let applicationID = action.payloadValue else {
                model.errorMessage = "打开方式缺少应用标识。"
                return
            }
            KiteOpenWithLauncher.launch(path: primaryPath, applicationID: applicationID) { error in
                if let error {
                    model.errorMessage = error.localizedDescription
                }
            }
        case "open.with.choose-other":
            KiteOpenWithLauncher.chooseAndLaunch(path: primaryPath) { error in
                if let error {
                    model.errorMessage = error.localizedDescription
                }
            }
        case "open.in":
            guard action.payloadKind == "tool",
                  let toolID = action.payloadValue else {
                model.errorMessage = "在工具中打开缺少工具标识。"
                return
            }
            openTool(toolID: toolID, role: "open", paths: paths)
        case "compare.with":
            guard action.payloadKind == "tool",
                  let toolID = action.payloadValue else {
                model.errorMessage = "比较动作缺少工具标识。"
                return
            }
            openTool(toolID: toolID, role: "compare", paths: paths)
        case "open.quick-look":
            KitePlatform.quickLook(primaryPath)
        case "clipboard.copy":
            model.copyFiles(paths)
        case "clipboard.cut":
            model.cutFiles(paths)
        case "clipboard.copy-to":
            model.copyTo(paths: paths)
        case "clipboard.copy-path":
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            pasteboard.setString(paths.joined(separator: "\n"), forType: .string)
        case "clipboard.paste":
            model.pasteFiles(into: context.containerPath)
        case "file.new-folder":
            model.createFolder(in: context.containerPath)
        case "file.new-file":
            model.createFile(in: context.containerPath)
        case "favorite.add", "favorite.remove":
            model.toggleFavorite(
                path: primaryPath,
                name: context.targets.first(where: { $0.path == primaryPath })?.name
                    ?? (primaryPath as NSString).lastPathComponent,
                isDirectory: context.targets.first(where: { $0.path == primaryPath })?.isDirectory ?? false
            )
        case "file.rename":
            model.rename(primaryPath)
        case "file.move-to-trash", "device.delete":
            model.moveToTrash(paths: paths)
        case "archive.compress.zip":
            model.compress(primaryPath, format: "zip")
        case "archive.compress.tar-gz":
            model.compress(primaryPath, format: "tar.gz")
        case "archive.extract-here":
            model.extract(primaryPath)
        case "archive.extract-to":
            model.extractTo(primaryPath)
        case "agent.ask":
            presentAgent(.ask)
        case "agent.explain-file":
            presentAgent(.explainFile)
        case "agent.summarize-folder":
            presentAgent(.summarizeFolder)
        case "agent.review-changes":
            presentAgent(.reviewChanges)
        case "agent.generate-commit-message":
            presentAgent(.generateCommitMessage)
        case "agent.configure":
            KiteSettingsWindowController.open(pane: .agent)
        case "system.reveal":
            KitePlatform.revealInFinder(paths)
        case "system.share":
            KitePlatform.share(paths)
        case "system.properties":
            KitePlatform.showInfo(primaryPath)
        case "tool.kite.text-editor":
            KitePlatform.openTextEditor(primaryPath)
        case "tool.kite.mindmap":
            KiteMindmapWindowController.open(path: primaryPath)
        case "tool.kite.mindmap-import":
            KitePlatform.openMindmapConversion(primaryPath)
        case "git.open-client", "git.commit", "git.diff":
            KiteGitClientWindowController.show(
                model: model,
                repositoryPath: repositoryPath(for: primaryPath)
            )
        case "git.clone":
            KiteGitWindows.shared.showClone(model: model, directory: context.containerPath)
        case "git.init":
            initializeRepository(at: repositoryPath(for: primaryPath))
        case "git.pull":
            presentGitPullChoice(model: model, path: repositoryPath(for: primaryPath))
        case "git.push":
            KiteGitWindows.shared.showPush(model: model, path: repositoryPath(for: primaryPath))
        case "git.fetch-current":
            KiteGitService.shared.fetch(
                model: model,
                path: repositoryPath(for: primaryPath),
                allRemotes: false
            )
        case "git.fetch-all":
            KiteGitService.shared.fetch(
                model: model,
                path: repositoryPath(for: primaryPath),
                allRemotes: true
            )
        case "git.remotes.manage":
            KiteGitWindows.shared.showRemotes(
                model: model,
                path: repositoryPath(for: primaryPath)
            )
        case "git.add":
            runVcsOperation(.git, .add, path: primaryPath)
        case "git.unstage":
            runVcsOperation(.git, .unstage, path: primaryPath)
        case "git.log":
            model.openGitLog(path: primaryPath, scope: logScope(for: primaryPath))
        case "git.rebase.interactive":
            KiteGitRebaseWindowController.show(
                path: repositoryPath(for: primaryPath),
                model: model
            )
        case "git.conflicts.resolve":
            KiteGitClientWindowController.show(
                model: model,
                repositoryPath: repositoryPath(for: primaryPath)
            )
        case "git.operation.continue":
            executeGitMenuControl(.continueOperation, path: primaryPath)
        case "git.operation.skip":
            executeGitMenuControl(.skip, path: primaryPath)
        case "git.operation.abort":
            executeGitMenuControl(.abort, path: primaryPath)
        case "git.discard":
            if confirm(
                title: "放弃更改？",
                message: "将丢弃所选目标的工作区更改。此操作不可撤销。"
            ) {
                runVcsOperation(.git, .revert, path: primaryPath)
            }
        case "git.clean-untracked":
            if confirm(
                title: "清理未跟踪文件？",
                message: "将删除仓库中的未跟踪文件。此操作不可撤销。"
            ) {
                runVcsOperation(.git, .clean, path: repositoryPath(for: primaryPath))
            }
        case "svn.update":
            runVcsOperation(.svn, .update, path: primaryPath)
        case "svn.add":
            runVcsOperation(.svn, .add, path: primaryPath)
        case "svn.commit":
            promptForCommitMessage(tool: .svn, path: primaryPath)
        case "svn.lock":
            runVcsOperation(.svn, .lock, path: primaryPath)
        case "svn.unlock":
            runVcsOperation(.svn, .unlock, path: primaryPath)
        case "svn.diff":
            Task { await model.openVcsDiff(tool: .svn, path: primaryPath) }
        case "svn.log":
            Task { await model.openVcsLog(tool: .svn, path: primaryPath) }
        case "svn.revert":
            if confirm(
                title: "还原 SVN 更改？",
                message: "将放弃所选目标的工作副本更改。此操作不可撤销。"
            ) {
                runVcsOperation(.svn, .revert, path: primaryPath)
            }
        case "svn.cleanup":
            runVcsOperation(.svn, .cleanup, path: repositoryPath(for: primaryPath))
        case "svn.resolve-conflicts":
            Task { await model.openVcsDiff(tool: .svn, path: primaryPath) }
        case "system.native-menu":
            if !KiteNativeMenuBridge.showServices() {
                model.errorMessage = "系统原生菜单当前不可用。"
            }
        default:
            model.errorMessage = "未实现的上下文菜单动作：\(action.actionId)"
        }
    }

    private func openTool(toolID: String, role: String, paths: [String]) {
        switch toolID {
        case "kite.text-editor":
            if let first = paths.first {
                KitePlatform.openTextEditor(first)
            }
        case "kite.mindmap":
            if let first = paths.first {
                KiteMindmapWindowController.open(path: first)
            }
        case "kite.git-client":
            if let first = paths.first {
                KiteGitClientWindowController.show(
                    model: model,
                    repositoryPath: repositoryPath(for: first)
                )
            }
        default:
            KiteToolLauncher.launch(
                toolID: toolID,
                role: role,
                paths: paths,
                workingDirectory: context.workingDirectory,
                repositoryRoot: paths.first.map(repositoryPath(for:))
            ) { error in
                if let error {
                    model.errorMessage = error.localizedDescription
                }
            }
        }
    }

    private func openDefault(_ path: String) {
        if targetIsDirectory(path) {
            model.loadRoot(path)
        } else {
            KitePlatform.open(path)
        }
    }

    private func presentAgent(_ action: KiteAgentAction) {
        let primary = context.primaryPath ?? context.containerPath
        if context.entries.isEmpty {
            agent.presentDirectory(primary, action: action)
        } else {
            agent.present(action: action, primary: primary, entries: context.entries)
        }
    }

    private func repositoryPath(for path: String) -> String {
        targetIsDirectory(path) ? path : (path as NSString).deletingLastPathComponent
    }

    private func logScope(for path: String) -> GitGraphScopeDto {
        targetIsDirectory(path) ? .directory(path: path) : .file(path: path)
    }

    private func targetIsDirectory(_ path: String) -> Bool {
        if let snapshot = context.targets.first(where: { $0.path == path }) {
            return snapshot.isDirectory
        }
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory)
            && isDirectory.boolValue
    }

    private func validateTargets(_ action: ContextMenuActionDto) -> Bool {
        guard action.targetKinds.count == action.targetPaths.count else {
            if action.targetKinds.isEmpty { return true }
            model.errorMessage = "右键目标快照已失效，请重新选择目标。"
            return false
        }
        for (path, expectedKind) in zip(action.targetPaths, action.targetKinds) {
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) else {
                model.errorMessage = "右键目标已不存在，请重新选择目标。"
                return false
            }
            let kindMatches: Bool
            switch expectedKind {
            case "directory", "repository-root", "workspace-root", "device-container":
                kindMatches = isDirectory.boolValue
            case "file", "archive", "device-item":
                kindMatches = !isDirectory.boolValue
            default:
                kindMatches = true
            }
            if !kindMatches {
                model.errorMessage = "右键目标类型已变化，请重新选择目标。"
                return false
            }
        }
        return true
    }

    private func initializeRepository(at path: String) {
        Task {
            do {
                let initialized = try await KiteGitService.shared.initRepository(path: path)
                model.updateGitOperationStatus("Git 仓库已初始化：\(initialized)")
                model.refresh()
            } catch {
                model.errorMessage = error.localizedDescription
            }
        }
    }

    private func runVcsOperation(
        _ tool: VcsToolDto,
        _ operation: VcsOperationDto,
        path: String
    ) {
        Task { await model.runVcsOperation(tool: tool, operation: operation, path: path) }
    }

    private func executeGitMenuControl(
        _ control: VcsOperationControlDto,
        path: String
    ) {
        Task {
            do {
                let message = try await Task.detached(priority: .userInitiated) {
                    try runGitMenuControl(path: path, control: control)
                }.value
                model.errorMessage = nil
                model.refresh()
                model.updateGitOperationStatus(message)
            } catch {
                model.errorMessage = error.localizedDescription
            }
        }
    }

    private func validateVcsRevision(
        _ action: ContextMenuActionDto,
        path: String
    ) -> Bool {
        guard let revision = action.payloadStateRevision else { return true }
        let tool: VcsMenuToolDto = action.actionId.hasPrefix("svn.") ? .svn : .git
        do {
            try revalidateVcsRevision(
                path: path,
                tool: tool,
                expectedRevision: revision
            )
            return true
        } catch {
            model.errorMessage = error.localizedDescription
            return false
        }
    }

    private func isVcsWriteAction(_ actionId: String) -> Bool {
        switch actionId {
        case "git.add", "git.unstage", "git.commit", "git.pull", "git.push",
             "git.fetch-current", "git.fetch-all", "git.remotes.manage",
             "git.rebase.interactive", "git.conflicts.resolve", "git.discard",
             "git.clean-untracked", "git.operation.continue", "git.operation.skip",
             "git.operation.abort", "svn.update", "svn.add", "svn.commit",
             "svn.lock", "svn.unlock", "svn.revert", "svn.cleanup":
            return true
        default:
            return false
        }
    }

    private func promptForCommitMessage(tool: VcsToolDto, path: String) {
        let alert = NSAlert()
        alert.messageText = tool == .git ? "Git 提交" : "SVN 提交"
        alert.informativeText = "输入提交说明。"
        alert.addButton(withTitle: "提交")
        alert.addButton(withTitle: "取消")
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 360, height: 24))
        field.placeholderString = "提交说明"
        alert.accessoryView = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let message = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else {
            model.errorMessage = "提交说明不能为空。"
            return
        }
        runVcsOperation(tool, .commit(message: message), path: path)
    }

    private func confirm(title: String, message: String) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: "继续")
        alert.addButton(withTitle: "取消")
        return alert.runModal() == .alertFirstButtonReturn
    }
}
