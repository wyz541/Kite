import AppKit
import SwiftUI

@MainActor
private struct WorkspaceNode: Identifiable {
    let id: String
    let path: String
    let name: String
    let isDirectory: Bool
    let children: [WorkspaceNode]?
}

@MainActor
private final class WorkspaceEditorModel: ObservableObject {
    let root: String
    @Published var nodes: [WorkspaceNode] = []

    init(root: String) {
        self.root = root
        refresh()
    }

    func refresh() { nodes = makeNodes(at: root, depth: 0) }

    func open(_ node: WorkspaceNode) {
        guard !node.isDirectory else { return }
        try? TextEditorWindowController.open(path: node.path)
    }

    private func makeNodes(at path: String, depth: Int) -> [WorkspaceNode] {
        guard depth <= 4 else { return [] }
        let url = URL(fileURLWithPath: path)
        let keys: Set<URLResourceKey> = [.isDirectoryKey, .isRegularFileKey]
        guard let urls = try? FileManager.default.contentsOfDirectory(
            at: url, includingPropertiesForKeys: Array(keys), options: [.skipsHiddenFiles]
        ) else { return [] }
        return urls.compactMap { child in
            guard let values = try? child.resourceValues(forKeys: keys) else { return nil }
            let directory = values.isDirectory == true
            guard directory || values.isRegularFile == true else { return nil }
            return WorkspaceNode(
                id: child.path, path: child.path, name: child.lastPathComponent,
                isDirectory: directory,
                children: directory ? makeNodes(at: child.path, depth: depth + 1) : nil
            )
        }.sorted {
            if $0.isDirectory != $1.isDirectory { return $0.isDirectory }
            return $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }
}

@MainActor
private struct WorkspaceEditorView: View {
    @ObservedObject var model: WorkspaceEditorModel

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "folder.badge.gearshape")
                Text(URL(fileURLWithPath: model.root).lastPathComponent).font(.headline).lineLimit(1)
                Spacer()
                Button("刷新", systemImage: "arrow.clockwise") { model.refresh() }
                    .labelStyle(.iconOnly).help("刷新工作区目录树")
            }
            .padding(.horizontal, 14).frame(height: 42)
            Divider()
            OutlineGroup(model.nodes, children: \.children) { node in
                Button { model.open(node) } label: {
                    Label(node.name, systemImage: node.isDirectory ? "folder" : "doc.text")
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.plain).padding(.vertical, 2).help(node.path)
            }
            .listStyle(.sidebar)
        }
        .frame(minWidth: 360, minHeight: 500)
    }
}

@MainActor
final class WorkspaceEditorWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: WorkspaceEditorWindowController] = [:]
    private let model: WorkspaceEditorModel

    init(path: String) {
        let normalized = URL(fileURLWithPath: path).standardizedFileURL.path
        model = WorkspaceEditorModel(root: normalized)
        let controller = NSHostingController(rootView: WorkspaceEditorView(model: model))
        let window = NSWindow(contentViewController: controller)
        window.title = "Kite 工作区 — \(URL(fileURLWithPath: normalized).lastPathComponent)"
        window.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        window.setContentSize(NSSize(width: 520, height: 700))
        super.init(window: window)
        window.delegate = self
        window.center()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    static func open(path: String) throws {
        var directory = path
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory, isDirectory: &isDirectory) else {
            throw NSError(domain: "KiteWorkspace", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "目录不存在"])
        }
        if !isDirectory.boolValue {
            directory = URL(fileURLWithPath: directory).deletingLastPathComponent().path
        }
        let normalized = URL(fileURLWithPath: directory).standardizedFileURL.path
        if let existing = controllers[normalized] {
            existing.showWindow(nil); existing.window?.makeKeyAndOrderFront(nil); return
        }
        let controller = WorkspaceEditorWindowController(path: normalized)
        controllers[normalized] = controller
        controller.showWindow(nil); controller.window?.makeKeyAndOrderFront(nil)
    }

    func windowWillClose(_ notification: Notification) {
        Self.controllers.removeValue(forKey: model.root)
    }
}
