import AppKit
import SwiftUI

enum KiteContextMenuTarget {
    case entry(BrowserEntry)
    case searchEntry(BrowserEntry)
    case directory(BrowserColumn)
}

struct KiteContextMenuView: View {
    @ObservedObject var model: KiteModel
    @ObservedObject private var openWithCatalog = KiteOpenWithCatalogCache.shared
    @EnvironmentObject private var agent: KiteAgentPanelModel
    let target: KiteContextMenuTarget

    var body: some View {
        let _ = openWithCatalog.revision
        let plan = makePlan()
        KiteContextMenuPlanView(groups: plan.groups, submenus: plan.submenus) { action in
            KiteContextMenuRunner(model: model, agent: agent, target: target)
                .perform(action)
        }
    }

    private func makePlan() -> ContextMenuPlanDto {
        let request = ContextMenuRequestDto(
            surface: surface,
            platform: "macos",
            primaryPath: primaryPath,
            targets: menuTargets,
            workingDirectory: workingDirectory,
            containerPath: containerPath,
            containerIsPrimaryTarget: isDirectoryContext,
            loading: isLoading,
            canPaste: true,
            canCreate: KitePlatform.canModify(containerPath),
            supportsTransferOps: true,
            supportsCut: menuPaths.allSatisfy(KitePlatform.canModify),
            supportsRename: menuPaths.count == 1 && KitePlatform.canModify(primaryPath),
            supportsDelete: menuPaths.allSatisfy(KitePlatform.canModify),
            supportsFavorite: true,
            isFavorite: menuPaths.count == 1 && model.isFavorite(path: primaryPath),
            gitAvailable: gitAvailable,
            svnAvailable: svnAvailable,
            agentAvailable: agent.hasConfiguredAgent,
            quickLookAvailable: true,
            revealAvailable: true,
            shareAvailable: true,
            propertiesAvailable: true,
            textEditorAvailable: menuPaths.count == 1
                && !isDirectoryContext
                && KitePlatform.isEditableTextFile(primaryPath),
            mindmapAvailable: menuPaths.count == 1
                && !isDirectoryContext
                && KitePlatform.isMindmapFile(primaryPath),
            mindmapImportAvailable: menuPaths.count == 1
                && !isDirectoryContext
                && KitePlatform.isMindmapImportFile(primaryPath),
            isShellNamespace: false,
            nativePlaneAvailable: false,
            openWith: openWithCatalog.catalog(for: openWithPath).dto,
            compact: true,
            hideUnavailable: true,
            confirmHighRisk: true
        )
        return composeContextMenu(request: request)
    }

    private var primaryPath: String {
        switch target {
        case .entry(let entry), .searchEntry(let entry): return entry.path
        case .directory(let column): return column.path
        }
    }

    private var isDirectoryContext: Bool {
        switch target {
        case .entry(let entry), .searchEntry(let entry): return entry.isDirectory
        case .directory: return true
        }
    }

    private var surface: String {
        switch target {
        case .entry: return "browser.list"
        case .searchEntry: return "search.results"
        case .directory: return "browser.columns"
        }
    }

    private var menuPaths: [String] {
        switch target {
        case .directory:
            return [primaryPath]
        case .entry, .searchEntry:
            return menuEntries.map(\.path)
        }
    }

    private var menuEntries: [BrowserEntry] {
        switch target {
        case .directory:
            return []
        case .entry(let entry), .searchEntry(let entry):
            let selected = model.selectedColumnEntries
            if case .entry = target,
               selected.count > 1,
               selected.contains(where: { $0.path == entry.path }) {
                return selected
            }
            return [entry]
        }
    }

    private var workingDirectory: String {
        switch target {
        case .entry(let entry), .searchEntry(let entry):
            return entry.isDirectory ? entry.path : (entry.path as NSString).deletingLastPathComponent
        case .directory(let column):
            return column.path
        }
    }

    private var containerPath: String {
        switch target {
        case .entry(let entry), .searchEntry(let entry):
            return entry.isDirectory ? entry.path : (entry.path as NSString).deletingLastPathComponent
        case .directory(let column):
            return column.path
        }
    }

    private var isLoading: Bool {
        switch target {
        case .entry, .searchEntry: return false
        case .directory(let column): return model.columns.last?.path == column.path && column.entries.isEmpty
        }
    }

    private var gitAvailable: Bool {
        switch target {
        case .directory(let column):
            return column.gitAvailable
        case .entry, .searchEntry:
            return menuEntries.allSatisfy {
                $0.gitStatus != nil || model.isGitAvailable(forEntryPath: $0.path)
            }
        }
    }

    private var svnAvailable: Bool {
        switch target {
        case .directory(let column):
            return column.svnAvailable
        case .entry, .searchEntry:
            return menuEntries.allSatisfy { $0.svnStatus != nil }
                || model.columns.last?.svnAvailable == true
        }
    }

    private var menuTargets: [ContextMenuTargetDto] {
        switch target {
        case .entry, .searchEntry:
            return menuEntries.map { entry in
                ContextMenuTargetDto(
                    path: entry.path,
                    displayName: entry.name,
                    kind: targetKind(for: entry),
                    exists: FileManager.default.fileExists(atPath: entry.path),
                    readable: FileManager.default.isReadableFile(atPath: entry.path)
                )
            }
        case .directory(let column):
            return [
                ContextMenuTargetDto(
                    path: column.path,
                    displayName: column.title,
                    kind: "directory",
                    exists: FileManager.default.fileExists(atPath: column.path),
                    readable: FileManager.default.isReadableFile(atPath: column.path)
                )
            ]
        }
    }

    private var openWithPath: String? {
        guard !isDirectoryContext, menuPaths.count == 1 else { return nil }
        return menuPaths[0]
    }

    private func targetKind(for entry: BrowserEntry) -> String {
        if KitePlatform.archiveFormat(entry.path) != nil {
            return "archive"
        }
        return entry.isDirectory ? "directory" : "file"
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
    let target: KiteContextMenuTarget

    func perform(_ action: ContextMenuActionDto) {
        let paths = action.targetPaths
        let primaryPath = paths.first ?? target.primaryPath
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
            model.pasteFiles(into: target.containerPath)
        case "file.new-folder":
            model.createFolder(in: target.containerPath)
        case "file.new-file":
            model.createFile(in: target.containerPath)
        case "favorite.add", "favorite.remove":
            model.toggleFavorite(
                path: primaryPath,
                name: target.displayName(for: primaryPath),
                isDirectory: target.isDirectory(path: primaryPath)
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
            KiteGitWindows.shared.showClone(model: model, directory: target.containerPath)
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
        case "system.native-menu":
            break
        default:
            model.errorMessage = "未实现的上下文菜单动作：\(action.actionId)"
        }
    }

    private func openDefault(_ path: String) {
        if target.isDirectory(path: path) {
            model.loadRoot(path)
        } else {
            KitePlatform.open(path)
        }
    }

    private func presentAgent(_ action: KiteAgentAction) {
        switch target {
        case .directory(let column):
            agent.presentDirectory(column.path, action: action)
        case .entry(let entry), .searchEntry(let entry):
            agent.present(action: action, primary: entry.path, entries: target.entries(for: model, clicked: entry))
        }
    }

    private func repositoryPath(for path: String) -> String {
        target.isDirectory(path: path) ? path : (path as NSString).deletingLastPathComponent
    }

    private func logScope(for path: String) -> GitGraphScopeDto {
        target.isDirectory(path: path) ? .directory(path: path) : .file(path: path)
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

private extension KiteContextMenuTarget {
    var primaryPath: String {
        switch self {
        case .entry(let entry), .searchEntry(let entry): return entry.path
        case .directory(let column): return column.path
        }
    }

    var containerPath: String {
        switch self {
        case .entry(let entry), .searchEntry(let entry):
            return entry.isDirectory ? entry.path : (entry.path as NSString).deletingLastPathComponent
        case .directory(let column):
            return column.path
        }
    }

    @MainActor
    func entries(for model: KiteModel, clicked entry: BrowserEntry) -> [BrowserEntry] {
        let selected = model.selectedColumnEntries
        if selected.count > 1, selected.contains(where: { $0.path == entry.path }) {
            return selected
        }
        return [entry]
    }

    func isDirectory(path: String) -> Bool {
        var isDirectory: ObjCBool = false
        if FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) {
            return isDirectory.boolValue
        }
        switch self {
        case .directory:
            return true
        case .entry(let entry), .searchEntry(let entry):
            return entry.path == path && entry.isDirectory
        }
    }

    func displayName(for path: String) -> String {
        switch self {
        case .directory(let column):
            return column.title
        case .entry(let entry) where entry.path == path:
            return entry.name
        case .searchEntry(let entry) where entry.path == path:
            return entry.name
        default:
            return (path as NSString).lastPathComponent
        }
    }
}
