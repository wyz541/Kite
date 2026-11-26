import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
private struct MindmapTopic: Identifiable {
    let id: String
    var title: String
    let parentID: String?
    let depth: Int
    let childIDs: [String]
    let colorHex: String?
    let fontSize: Double
    let shapeClass: String?

    var hasChildren: Bool { !childIDs.isEmpty }
}

@MainActor
private struct MindmapNodeLayout {
    let id: String
    let rect: CGRect
    let parentID: String?
}

@MainActor
private struct MindmapLayout {
    let nodes: [String: MindmapNodeLayout]
    let order: [String]
    let bounds: CGRect
}

@MainActor
private struct MindmapOutlineRow: Identifiable {
    let id: String
    let title: String
    let depth: Int
    let childCount: Int
    let isCollapsed: Bool
}

@MainActor
private struct MindmapSnapshot {
    let contentJson: String
    let sourceSha256: String
}

@MainActor
private enum MindmapMoveDirection {
    case up
    case down
    case left
    case right
}

@MainActor
private final class MindmapEditorModel: ObservableObject {
    private(set) var path: String
    @Published private(set) var document: MindmapDocumentDto
    @Published private(set) var topics: [String: MindmapTopic] = [:]
    @Published private(set) var rootID: String?
    @Published private(set) var layout = MindmapLayout(nodes: [:], order: [], bounds: .zero)
    @Published var selectedID: String?
    @Published var collapsedIDs: Set<String> = []
    @Published var titleDraft = ""
    @Published var editingID: String?
    @Published var inlineTitle = ""
    @Published var textInputFocused = false
    @Published var zoom: CGFloat = 0.82
    @Published var pan = CGSize.zero
    @Published var status = "正在加载…"
    @Published var errorMessage: String?
    @Published var isWorking = false
    @Published var searchText = ""

    private var viewportSize = CGSize(width: 1_000, height: 700)
    private var didFit = false
    private var undoStack: [MindmapSnapshot] = []
    private var redoStack: [MindmapSnapshot] = []

    init(path: String) throws {
        self.path = URL(fileURLWithPath: path).standardizedFileURL.path
        self.document = try openMindmapDocument(path: self.path)
        rebuildFromDocument(initial: true)
        status = "Rust Core 已读取 \(topics.count) 个主题"
    }

    var selectedTopic: MindmapTopic? {
        selectedID.flatMap { topics[$0] }
    }

    var documentName: String {
        URL(fileURLWithPath: path).lastPathComponent
    }

    var canUndo: Bool { !undoStack.isEmpty }
    var canRedo: Bool { !redoStack.isEmpty }
    var undoDepth: Int { undoStack.count }
    var redoDepth: Int { redoStack.count }

    var outlineRows: [MindmapOutlineRow] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        if !query.isEmpty {
            return topics.values
                .filter { topic in
                    topic.title.localizedCaseInsensitiveContains(query)
                        || topic.id.localizedCaseInsensitiveContains(query)
                }
                .sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
                .prefix(40)
                .map {
                    MindmapOutlineRow(
                        id: $0.id,
                        title: $0.title,
                        depth: $0.depth,
                        childCount: $0.childIDs.count,
                        isCollapsed: collapsedIDs.contains($0.id)
                    )
                }
        }

        guard let rootID else { return [] }
        var rows: [MindmapOutlineRow] = []
        func append(_ id: String, depth: Int) {
            guard let topic = topics[id] else { return }
            rows.append(
                MindmapOutlineRow(
                    id: id,
                    title: topic.title,
                    depth: depth,
                    childCount: topic.childIDs.count,
                    isCollapsed: collapsedIDs.contains(id)
                )
            )
            guard !collapsedIDs.contains(id) else { return }
            for childID in topic.childIDs {
                append(childID, depth: depth + 1)
            }
        }
        append(rootID, depth: 0)
        return rows
    }

    func select(_ id: String?) {
        selectedID = id
        titleDraft = id.flatMap { topics[$0]?.title } ?? ""
    }

    func beginEditingSelected() {
        guard let id = selectedID else { return }
        beginEditing(id)
    }

    func beginEditing(_ id: String) {
        guard let topic = topics[id] else { return }
        select(id)
        inlineTitle = topic.title
        editingID = id
    }

    func commitInlineEdit() {
        guard let id = editingID else { return }
        let title = inlineTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        editingID = nil
        guard !title.isEmpty, title != topics[id]?.title else { return }
        rename(topicID: id, title: title)
    }

    func cancelInlineEdit() {
        editingID = nil
        titleDraft = selectedID.flatMap { topics[$0]?.title } ?? ""
    }

    func rename(topicID: String, title: String) {
        let normalized = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalized.isEmpty else { return }
        performEdit(.rename(topicId: topicID, title: normalized), selecting: .keep(topicID))
    }

    func undo() {
        guard let snapshot = undoStack.popLast() else { return }
        let current = MindmapSnapshot(
            contentJson: document.contentJson,
            sourceSha256: document.sourceSha256
        )
        if restore(snapshot) {
            redoStack.append(current)
        } else {
            undoStack.append(snapshot)
        }
    }

    func redo() {
        guard let snapshot = redoStack.popLast() else { return }
        let current = MindmapSnapshot(
            contentJson: document.contentJson,
            sourceSha256: document.sourceSha256
        )
        if restore(snapshot) {
            undoStack.append(current)
        } else {
            redoStack.append(snapshot)
        }
    }

    func moveSelection(_ direction: MindmapMoveDirection) {
        guard let selectedID, let topic = topics[selectedID] else { return }
        switch direction {
        case .left:
            select(topic.parentID)
        case .right:
            if !collapsedIDs.contains(selectedID),
               let child = topic.childIDs.first(where: { topics[$0] != nil }) {
                select(child)
            } else {
                selectAdjacentSibling(selectedID, offset: 1)
            }
        case .up:
            selectAdjacentSibling(selectedID, offset: -1)
        case .down:
            selectAdjacentSibling(selectedID, offset: 1)
        }
    }

    func renameSelected() {
        guard let id = selectedID, let topic = topics[id] else { return }
        let title = titleDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, title != topic.title else { return }
        performEdit(.rename(topicId: id, title: title), selecting: .keep(id))
    }

    func addChild() {
        guard let parentID = selectedID ?? rootID else { return }
        performEdit(.addChild(parentId: parentID, title: "新主题"), selecting: .newest)
    }

    func addSibling() {
        guard let topicID = selectedID ?? rootID else { return }
        performEdit(.addSibling(topicId: topicID, title: "新主题"), selecting: .newest)
    }

    func deleteSelected() {
        guard let id = selectedID, let topic = topics[id] else { return }
        guard let parentID = topic.parentID else {
            status = "根主题不能删除"
            return
        }
        performEdit(.delete(topicId: id), selecting: .specific(parentID))
    }

    func toggleCollapse(_ id: String) {
        guard let topic = topics[id], topic.hasChildren else { return }
        if collapsedIDs.contains(id) {
            collapsedIDs.remove(id)
        } else {
            collapsedIDs.insert(id)
        }
        rebuildLayout()
    }

    func expandSelected() {
        guard let id = selectedID else { return }
        collapsedIDs.remove(id)
        rebuildLayout()
    }

    func collapseSelected() {
        guard let id = selectedID, topics[id]?.hasChildren == true else { return }
        collapsedIDs.insert(id)
        rebuildLayout()
    }

    func zoomIn() {
        zoom = min(2.2, zoom * 1.18)
    }

    func zoomOut() {
        zoom = max(0.18, zoom / 1.18)
    }

    func updateViewport(_ size: CGSize) {
        guard size.width > 1, size.height > 1 else { return }
        viewportSize = size
        if !didFit {
            didFit = true
            zoom = 0.88
            let rootMidY = rootID.flatMap { layout.nodes[$0]?.rect.midY } ?? 0
            pan = CGSize(
                width: -size.width * 0.32,
                height: -rootMidY * zoom
            )
        }
    }

    func fit() {
        let bounds = layout.bounds
        guard bounds.width > 0, bounds.height > 0 else { return }
        let available = CGSize(
            width: max(200, viewportSize.width - 120),
            height: max(200, viewportSize.height - 120)
        )
        zoom = max(0.18, min(1.15, min(available.width / bounds.width, available.height / bounds.height)))
        pan = CGSize(
            width: -bounds.midX * zoom,
            height: -bounds.midY * zoom
        )
    }

    func saveCurrentContent() {
        isWorking = true
        defer { isWorking = false }
        do {
            let result = try saveMindmapContent(
                path: path,
                expectedSourceSha256: document.sourceSha256,
                contentJson: document.contentJson
            )
            status = result.byteIdentical
                ? "文件已是最新状态"
                : "已保存 \(result.bytesWritten) 字节"
            document = try openMindmapDocument(path: path)
        } catch {
            errorMessage = error.localizedDescription
            status = "保存失败"
        }
    }

    func revealInFinder() {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    private enum SelectionRule {
        case keep(String)
        case specific(String)
        case newest
    }

    private func performEdit(_ edit: MindmapEditDto, selecting rule: SelectionRule) {
        isWorking = true
        defer { isWorking = false }
        let previousIDs = Set(topics.keys)
        let snapshot = MindmapSnapshot(
            contentJson: document.contentJson,
            sourceSha256: document.sourceSha256
        )
        do {
            document = try editMindmapDocument(
                path: path,
                expectedSourceSha256: document.sourceSha256,
                edit: edit
            )
            undoStack.append(snapshot)
            redoStack.removeAll()
            rebuildFromDocument(initial: false, previousIDs: previousIDs, selectionRule: rule)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
            status = "编辑未保存"
        }
    }

    private func restore(_ snapshot: MindmapSnapshot) -> Bool {
        isWorking = true
        defer { isWorking = false }
        do {
            _ = try saveMindmapContent(
                path: path,
                expectedSourceSha256: document.sourceSha256,
                contentJson: snapshot.contentJson
            )
            document = try openMindmapDocument(path: path)
            rebuildFromDocument(
                initial: false,
                previousIDs: Set(topics.keys),
                selectionRule: .keep(selectedID ?? rootID ?? "")
            )
            status = "已恢复历史版本"
            errorMessage = nil
            return true
        } catch {
            errorMessage = error.localizedDescription
            status = "撤销/重做失败"
            return false
        }
    }

    private func selectAdjacentSibling(_ id: String, offset: Int) {
        guard let parentID = topics[id]?.parentID,
              let siblings = topics[parentID]?.childIDs,
              let index = siblings.firstIndex(of: id) else { return }
        let target = index + offset
        guard siblings.indices.contains(target) else { return }
        select(siblings[target])
    }

    private func rebuildFromDocument(
        initial: Bool,
        previousIDs: Set<String> = [],
        selectionRule: SelectionRule = .specific("")
    ) {
        do {
            let parsed = try Self.parse(contentJSON: document.contentJson)
            topics = parsed.topics
            rootID = parsed.rootID
            if initial {
                let collapsed = parsed.topics.values
                    .filter { $0.depth >= 1 }
                    .map(\.id)
                collapsedIDs = Set(collapsed)
                select(parsed.rootID)
            } else {
                switch selectionRule {
                case .keep(let id):
                    select(topics[id] == nil ? parsed.rootID : id)
                case .specific(let id):
                    select(topics[id] == nil ? parsed.rootID : id)
                case .newest:
                    let added = Set(parsed.topics.keys).subtracting(previousIDs)
                    select(added.first ?? parsed.rootID)
                }
            }
            rebuildLayout()
            status = "已自动保存 · \(topics.count) 个主题"
        } catch {
            errorMessage = error.localizedDescription
            status = "无法解析 Mindmap 内容"
        }
    }

    private func rebuildLayout() {
        guard let rootID else {
            layout = MindmapLayout(nodes: [:], order: [], bounds: .zero)
            return
        }
        var heights: [String: CGFloat] = [:]
        let gap: CGFloat = 12
        let column: CGFloat = 320

        func visibleChildren(_ id: String) -> [String] {
            guard !collapsedIDs.contains(id), let topic = topics[id] else { return [] }
            return topic.childIDs.filter { topics[$0] != nil }
        }

        func nodeSize(_ topic: MindmapTopic) -> CGSize {
            let characters = max(6, topic.title.count)
            let width = min(344, max(112, 58 + CGFloat(characters) * 7.3))
            let height: CGFloat = topic.title.count > 34 ? 54 : 36
            return CGSize(width: width, height: height)
        }

        func measure(_ id: String) -> CGFloat {
            if let cached = heights[id] { return cached }
            guard let topic = topics[id] else { return 0 }
            let own = nodeSize(topic).height
            let children = visibleChildren(id)
            let childTotal = children.isEmpty
                ? 0
                : children.map(measure).reduce(0, +) + gap * CGFloat(children.count - 1)
            let value = max(own, childTotal)
            heights[id] = value
            return value
        }

        var nodes: [String: MindmapNodeLayout] = [:]
        var order: [String] = []
        let measureRoot = measure(rootID)

        func place(_ id: String, depth: Int, top: CGFloat) {
            guard let topic = topics[id] else { return }
            let subtreeHeight = heights[id] ?? measure(id)
            let size = nodeSize(topic)
            let rect = CGRect(
                x: CGFloat(depth) * column,
                y: top + (subtreeHeight - size.height) / 2,
                width: size.width,
                height: size.height
            )
            nodes[id] = MindmapNodeLayout(id: id, rect: rect, parentID: topic.parentID)
            order.append(id)

            let children = visibleChildren(id)
            guard !children.isEmpty else { return }
            let childHeights = children.map { heights[$0] ?? measure($0) }
            let total = childHeights.reduce(0, +) + gap * CGFloat(children.count - 1)
            var cursor = top + (subtreeHeight - total) / 2
            for (index, child) in children.enumerated() {
                place(child, depth: depth + 1, top: cursor)
                cursor += childHeights[index] + gap
            }
        }

        place(rootID, depth: 0, top: 0)
        let rects = nodes.values.map(\.rect)
        let bounds = rects.reduce(CGRect.null) { $0.union($1) }.insetBy(dx: -80, dy: -80)
        layout = MindmapLayout(nodes: nodes, order: order, bounds: bounds)
        _ = measureRoot
    }

    private static func parse(contentJSON: String) throws -> (topics: [String: MindmapTopic], rootID: String?) {
        let data = Data(contentJSON.utf8)
        let rootObject = try JSONSerialization.jsonObject(with: data)
        guard let sheets = rootObject as? [[String: Any]],
              let rootTopic = sheets.first?["rootTopic"] as? [String: Any] else {
            throw NSError(
                domain: "KiteMindmap",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "content.json 不包含 rootTopic"]
            )
        }

        var topics: [String: MindmapTopic] = [:]
        var rootID: String?

        func walk(_ object: [String: Any], parentID: String?, depth: Int) {
            let id = object["id"] as? String ?? UUID().uuidString
            if depth == 0 { rootID = id }
            let title = (object["title"] as? String) ?? "未命名主题"
            let properties = (object["style"] as? [String: Any])?["properties"] as? [String: Any]
            let color = properties?["svg:fill"] as? String
                ?? properties?["fo:color"] as? String
                ?? properties?["fill-color"] as? String
            let font = parseFontSize(properties?["fo:font-size"])
            let shape = (object["shape-class"] as? String) ?? (properties?["shape-class"] as? String)
            let childrenObject = object["children"] as? [String: Any]
            let childObjects = childrenObject?["attached"] as? [[String: Any]] ?? []
            let childIDs = childObjects.compactMap { $0["id"] as? String }
            topics[id] = MindmapTopic(
                id: id,
                title: title,
                parentID: parentID,
                depth: depth,
                childIDs: childIDs,
                colorHex: color,
                fontSize: font,
                shapeClass: shape
            )
            for child in childObjects {
                walk(child, parentID: id, depth: depth + 1)
            }
        }

        walk(rootTopic, parentID: nil, depth: 0)
        return (topics, rootID)
    }

    private static func parseFontSize(_ value: Any?) -> Double {
        guard let string = value as? String else { return 12.5 }
        let numeric = string.drop(while: { !$0.isNumber && $0 != "." })
        return Double(numeric.trimmingCharacters(in: CharacterSet(charactersIn: "ptpx "))) ?? 12.5
    }
}

@MainActor
private struct KiteMindmapEditorView: View {
    @ObservedObject var model: MindmapEditorModel
    @State private var dragOrigin: CGSize?
    @State private var magnificationOrigin: CGFloat?
    @FocusState private var inlineEditorFocused: Bool
    @FocusState private var searchFocused: Bool
    @FocusState private var inspectorTitleFocused: Bool

    var body: some View {
        ZStack {
            Color(red: 0.075, green: 0.078, blue: 0.082)
                .ignoresSafeArea()

            VStack(spacing: 0) {
                header
                Divider().overlay(Color.white.opacity(0.08))
                HStack(spacing: 0) {
                    sidebar
                    Rectangle()
                        .fill(Color.white.opacity(0.08))
                        .frame(width: 1)
                    canvas
                }
                Divider().overlay(Color.white.opacity(0.08))
                statusBar
            }
        }
        .preferredColorScheme(.dark)
        .onAppear {
            searchFocused = false
            inspectorTitleFocused = false
            inlineEditorFocused = false
            model.textInputFocused = false
        }
        .onChange(of: model.editingID) { id in
            if id != nil {
                DispatchQueue.main.async { inlineEditorFocused = true }
            } else {
                inlineEditorFocused = false
                updateTextInputFocus()
            }
        }
        .onChange(of: inlineEditorFocused) { _ in updateTextInputFocus() }
        .onChange(of: searchFocused) { _ in updateTextInputFocus() }
        .onChange(of: inspectorTitleFocused) { _ in updateTextInputFocus() }
        .alert("Mindmap 错误", isPresented: Binding(
            get: { model.errorMessage != nil },
            set: { if !$0 { model.errorMessage = nil } }
        )) {
            Button("好") { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(Color(red: 0.72, green: 0.34, blue: 0.15))
                Image(systemName: "point.3.connected.trianglepath.dotted")
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(.white)
            }
            .frame(width: 28, height: 28)

            VStack(alignment: .leading, spacing: 1) {
                Text(model.documentName)
                    .font(.system(size: 12.5, weight: .semibold, design: .rounded))
                    .lineLimit(1)
                Text("XMind · Rust Core · 自动保存")
                    .font(.system(size: 9.5, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 20)

            HStack(spacing: 5) {
                headerButton("arrow.uturn.backward", "撤销 (Command+Z)", enabled: model.canUndo) {
                    model.undo()
                }
                headerButton("arrow.uturn.forward", "重做 (Command+Shift+Z)", enabled: model.canRedo) {
                    model.redo()
                }
                headerButton("folder", "打开") { KiteMindmapWindowController.chooseAndOpen() }
                headerButton("arrow.clockwise", "重新载入") { KiteMindmapWindowController.open(path: model.path) }
                headerButton("minus.magnifyingglass", "缩小") { model.zoomOut() }
                headerButton("plus.magnifyingglass", "放大") { model.zoomIn() }
                headerButton("arrow.up.left.and.arrow.down.right", "适应画布") { model.fit() }
                headerButton("square.and.arrow.up", "在 Finder 中显示") { model.revealInFinder() }
            }

            if model.isWorking {
                ProgressView().controlSize(.small)
            }
        }
        .padding(.horizontal, 14)
        .frame(height: 54)
        .background(.black.opacity(0.32))
    }

    private var sidebar: some View {
        GeometryReader { proxy in
            VStack(spacing: 0) {
                outlinePanel
                    .frame(height: max(180, proxy.size.height * 0.43))
                Rectangle()
                    .fill(Color.white.opacity(0.08))
                    .frame(height: 1)
                ScrollView {
                    inspectorContent
                        .padding(14)
                }
            }
        }
        .frame(width: 292)
        .background(Color.black.opacity(0.24))
    }

    private var outlinePanel: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                Text("OUTLINE")
                    .font(.system(size: 9, weight: .bold, design: .monospaced))
                    .tracking(1.3)
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(model.outlineRows.count)")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }

            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.tertiary)
                TextField("搜索主题", text: $model.searchText)
                    .textFieldStyle(.plain)
                    .font(.system(size: 11))
                    .focused($searchFocused)
            }
            .padding(.horizontal, 8)
            .frame(height: 27)
            .background(Color.white.opacity(0.065), in: RoundedRectangle(cornerRadius: 6))

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    ForEach(model.outlineRows) { row in
                        outlineRow(row)
                    }
                }
            }
            .scrollIndicators(.hidden)
        }
        .padding(12)
    }

    private func outlineRow(_ row: MindmapOutlineRow) -> some View {
        HStack(spacing: 3) {
            Spacer()
                .frame(width: CGFloat(min(row.depth, 7)) * 10)
            Button {
                clearTextInputFocus()
                model.toggleCollapse(row.id)
            } label: {
                Image(systemName: row.isCollapsed ? "chevron.right" : "chevron.down")
                    .font(.system(size: 8, weight: .bold))
                    .frame(width: 13, height: 20)
                    .foregroundStyle(.secondary)
                    .opacity(row.childCount > 0 ? 1 : 0.2)
            }
            .buttonStyle(.plain)
            .disabled(row.childCount == 0)

            Button {
                clearTextInputFocus()
                model.select(row.id)
            } label: {
                HStack(spacing: 6) {
                    Text(row.title)
                        .font(.system(size: 10.5, weight: model.selectedID == row.id ? .semibold : .regular))
                        .lineLimit(1)
                    Spacer(minLength: 2)
                    if row.childCount > 0 {
                        Text("\(row.childCount)")
                            .font(.system(size: 8.5, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                }
                .padding(.horizontal, 6)
                .frame(height: 22)
                .background(
                    model.selectedID == row.id
                        ? Color(red: 0.72, green: 0.34, blue: 0.15).opacity(0.18)
                        : Color.clear,
                    in: RoundedRectangle(cornerRadius: 4)
                )
            }
            .buttonStyle(.plain)
        }
    }

    private var inspectorContent: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("INSPECTOR")
                    .font(.system(size: 9, weight: .bold, design: .monospaced))
                    .tracking(1.3)
                    .foregroundStyle(.secondary)
                Text("节点检查器")
                    .font(.system(size: 16, weight: .semibold, design: .rounded))
            }

            if let topic = model.selectedTopic {
                VStack(alignment: .leading, spacing: 7) {
                    Text("标题")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                    TextField("主题标题", text: $model.titleDraft)
                        .textFieldStyle(.plain)
                        .font(.system(size: 12.5, weight: .medium))
                        .focused($inspectorTitleFocused)
                        .padding(.horizontal, 9)
                        .frame(height: 30)
                        .background(Color.white.opacity(0.075), in: RoundedRectangle(cornerRadius: 6))
                        .onSubmit { model.renameSelected() }
                    HStack(spacing: 6) {
                        Button("应用") { model.renameSelected() }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        Button("添加子主题") { model.addChild() }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                }

                Divider().overlay(Color.white.opacity(0.08))

                Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 7) {
                    metaRow("深度", "\(topic.depth)")
                    metaRow("子主题", "\(topic.childIDs.count)")
                    metaRow("形状", topic.shapeClass ?? "默认")
                    metaRow("字号", String(format: "%.1f pt", topic.fontSize))
                }

                HStack(spacing: 6) {
                    Button("添加同级") { model.addSibling() }
                        .controlSize(.small)
                        .disabled(topic.parentID == nil)
                    Button(topic.childIDs.isEmpty ? "无子级" : "展开") { model.expandSelected() }
                        .controlSize(.small)
                        .disabled(topic.childIDs.isEmpty)
                    Button(topic.childIDs.isEmpty ? "无子级" : "折叠") { model.collapseSelected() }
                        .controlSize(.small)
                        .disabled(topic.childIDs.isEmpty)
                }

                Button(role: .destructive) {
                    model.deleteSelected()
                } label: {
                    Label("删除主题", systemImage: "trash")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(topic.parentID == nil)

                Text(topic.id)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
            } else {
                Text("在画布上选择一个主题")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }

            Divider().overlay(Color.white.opacity(0.08))

            VStack(alignment: .leading, spacing: 6) {
                Text("能力基线")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.secondary)
                Label("\(model.topics.count) 个主题", systemImage: "circle.grid.3x3")
                Label("\(model.document.sheetCount) 个 sheet", systemImage: "square.stack.3d.up")
                Label("\(model.document.entryCount) 个 ZIP 条目", systemImage: "archivebox")
            }
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
        }
    }

    private var canvas: some View {
        GeometryReader { proxy in
            ZStack(alignment: .bottomTrailing) {
                industrialCanvas(size: proxy.size)
                    .onAppear { model.updateViewport(proxy.size) }
                    .onChange(of: proxy.size) { size in model.updateViewport(size) }
                if let editingID = model.editingID,
                   let node = model.layout.nodes[editingID],
                   let topic = model.topics[editingID] {
                    inlineEditor(topic: topic, node: node, size: proxy.size)
                }
                zoomBadge
                    .padding(14)
            }
        }
    }

    private func industrialCanvas(size: CGSize) -> some View {
        Canvas { context, canvasSize in
            drawGrid(context: &context, size: canvasSize)
            var scene = context
            scene.translateBy(
                x: canvasSize.width / 2 + model.pan.width,
                y: canvasSize.height / 2 + model.pan.height
            )
            scene.scaleBy(x: model.zoom, y: model.zoom)

            for id in model.layout.order {
                guard let node = model.layout.nodes[id],
                      let topic = model.topics[id],
                      let parentID = node.parentID,
                      let parent = model.layout.nodes[parentID] else { continue }
                drawEdge(context: &scene, parent: parent, child: node, colorHex: topic.colorHex)
            }
            for id in model.layout.order {
                guard let node = model.layout.nodes[id],
                      let topic = model.topics[id] else { continue }
                drawNode(context: &scene, topic: topic, layout: node)
            }
        }
        .background(Color(red: 0.92, green: 0.91, blue: 0.875))
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 3)
                .onChanged { value in
                    if dragOrigin == nil { dragOrigin = model.pan }
                    let origin = dragOrigin ?? .zero
                    model.pan = CGSize(
                        width: origin.width + value.translation.width,
                        height: origin.height + value.translation.height
                    )
                }
                .onEnded { _ in dragOrigin = nil }
        )
        .gesture(
            MagnificationGesture()
                .onChanged { value in
                    if magnificationOrigin == nil { magnificationOrigin = model.zoom }
                    model.zoom = max(0.18, min(2.2, (magnificationOrigin ?? 1) * value))
                }
                .onEnded { _ in magnificationOrigin = nil }
        )
        .simultaneousGesture(
            SpatialTapGesture()
                .onEnded { value in
                    clearTextInputFocus()
                    let point = worldPoint(value.location, size: size)
                    if let hit = model.layout.order.reversed().first(where: {
                        model.layout.nodes[$0]?.rect.contains(point) == true
                    }), let node = model.layout.nodes[hit] {
                        if toggleBadgeRect(for: node).contains(point) {
                            model.toggleCollapse(hit)
                        } else {
                            model.select(hit)
                        }
                    }
                }
        )
        .simultaneousGesture(
            SpatialTapGesture(count: 2)
                .onEnded { value in
                    clearTextInputFocus()
                    let point = worldPoint(value.location, size: size)
                    if let hit = model.layout.order.reversed().first(where: {
                        model.layout.nodes[$0]?.rect.contains(point) == true
                    }), let node = model.layout.nodes[hit] {
                        if toggleBadgeRect(for: node).contains(point) {
                            model.select(hit)
                            model.toggleCollapse(hit)
                        } else {
                            model.beginEditing(hit)
                        }
                    }
                }
        )
    }

    private func updateTextInputFocus() {
        model.textInputFocused = searchFocused || inspectorTitleFocused || inlineEditorFocused
    }

    private func clearTextInputFocus() {
        searchFocused = false
        inspectorTitleFocused = false
        model.textInputFocused = false
        NSApp.keyWindow?.makeFirstResponder(nil)
    }

    private func inlineEditor(topic: MindmapTopic, node: MindmapNodeLayout, size: CGSize) -> some View {
        let rect = screenRect(node.rect, size: size)
        return TextField("主题标题", text: $model.inlineTitle)
            .textFieldStyle(.plain)
            .font(.system(size: max(10, min(14, topic.fontSize)), weight: .medium, design: .rounded))
            .padding(.horizontal, 9)
            .frame(width: max(110, rect.width), height: max(30, rect.height))
            .background(
                Color(red: 0.99, green: 0.985, blue: 0.96),
                in: RoundedRectangle(cornerRadius: 6, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .stroke(Color(red: 0.72, green: 0.34, blue: 0.15), lineWidth: 2)
            )
            .focused($inlineEditorFocused)
            .position(x: rect.midX, y: rect.midY)
            .onSubmit { model.commitInlineEdit() }
            .onExitCommand { model.cancelInlineEdit() }
    }

    private func screenRect(_ rect: CGRect, size: CGSize) -> CGRect {
        CGRect(
            x: size.width / 2 + model.pan.width + rect.minX * model.zoom,
            y: size.height / 2 + model.pan.height + rect.minY * model.zoom,
            width: rect.width * model.zoom,
            height: rect.height * model.zoom
        )
    }

    private func toggleBadgeRect(for node: MindmapNodeLayout) -> CGRect {
        CGRect(
            x: node.rect.maxX - 26,
            y: node.rect.midY - 12,
            width: 24,
            height: 24
        )
    }

    private var zoomBadge: some View {
        Text("\(Int(model.zoom * 100))%")
            .font(.system(size: 9.5, weight: .semibold, design: .monospaced))
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(.black.opacity(0.72), in: Capsule())
            .foregroundStyle(.white.opacity(0.8))
    }

    private func drawGrid(context: inout GraphicsContext, size: CGSize) {
        let spacing = max(18, 36 * model.zoom)
        let offsetX = model.pan.width.truncatingRemainder(dividingBy: spacing)
        let offsetY = model.pan.height.truncatingRemainder(dividingBy: spacing)
        var path = Path()
        var x = offsetX - spacing
        while x < size.width {
            path.move(to: CGPoint(x: x, y: 0))
            path.addLine(to: CGPoint(x: x, y: size.height))
            x += spacing
        }
        var y = offsetY - spacing
        while y < size.height {
            path.move(to: CGPoint(x: 0, y: y))
            path.addLine(to: CGPoint(x: size.width, y: y))
            y += spacing
        }
        context.stroke(path, with: .color(.black.opacity(0.045)), lineWidth: 0.5)
    }

    private func drawEdge(
        context: inout GraphicsContext,
        parent: MindmapNodeLayout,
        child: MindmapNodeLayout,
        colorHex: String?
    ) {
        let start = CGPoint(x: parent.rect.maxX, y: parent.rect.midY)
        let end = CGPoint(x: child.rect.minX, y: child.rect.midY)
        let midX = (start.x + end.x) / 2
        var path = Path()
        path.move(to: start)
        path.addCurve(
            to: end,
            control1: CGPoint(x: midX, y: start.y),
            control2: CGPoint(x: midX, y: end.y)
        )
        let color = Color(hex: colorHex) ?? Color(red: 0.24, green: 0.25, blue: 0.24)
        context.stroke(path, with: .color(color.opacity(0.55)), lineWidth: 1.25)
    }

    private func drawNode(context: inout GraphicsContext, topic: MindmapTopic, layout: MindmapNodeLayout) {
        let rect = layout.rect
        let selected = model.selectedID == topic.id
        let isRoot = topic.parentID == nil
        let accent = Color(hex: topic.colorHex) ?? Color(red: 0.72, green: 0.34, blue: 0.15)
        let fill = isRoot
            ? Color(red: 0.10, green: 0.105, blue: 0.11)
            : Color(red: 0.985, green: 0.98, blue: 0.95)
        let textColor = isRoot ? Color.white : Color(red: 0.12, green: 0.125, blue: 0.12)
        let radius = topic.shapeClass?.contains("rounded") == true ? min(10, rect.height / 2) : 3
        let nodePath = Path(roundedRect: rect, cornerRadius: radius)
        context.fill(nodePath, with: .color(fill))
        context.stroke(
            nodePath,
            with: .color(selected ? accent : .black.opacity(isRoot ? 0.65 : 0.16)),
            lineWidth: selected ? 2.2 : 1
        )

        if !isRoot {
            let bar = CGRect(x: rect.minX, y: rect.minY + 5, width: 3, height: rect.height - 10)
            context.fill(Path(roundedRect: bar, cornerRadius: 1.5), with: .color(accent))
        }

        let font = Font.system(
            size: max(9.5, min(13.5, topic.fontSize * 0.82)),
            weight: isRoot ? .semibold : .medium,
            design: .rounded
        )
        let text = Text(topic.title)
            .font(font)
            .foregroundColor(textColor)
        var textContext = context
        textContext.clip(to: nodePath)
        textContext.draw(text, at: CGPoint(x: rect.minX + 12, y: rect.midY), anchor: .leading)

        if topic.hasChildren {
            let badge = CGRect(x: rect.maxX - 18, y: rect.midY - 6, width: 12, height: 12)
            context.fill(Path(ellipseIn: badge), with: .color(accent.opacity(0.9)))
            let sign = model.collapsedIDs.contains(topic.id) ? "+" : "−"
            context.draw(
                Text(sign).font(.system(size: 8, weight: .bold)).foregroundColor(.white),
                at: CGPoint(x: badge.midX, y: badge.midY - 0.5),
                anchor: .center
            )
        }
    }

    private func worldPoint(_ point: CGPoint, size: CGSize) -> CGPoint {
        CGPoint(
            x: (point.x - size.width / 2 - model.pan.width) / model.zoom,
            y: (point.y - size.height / 2 - model.pan.height) / model.zoom
        )
    }

    private var statusBar: some View {
        HStack(spacing: 12) {
            Circle()
                .fill(model.errorMessage == nil ? Color.green.opacity(0.85) : Color.orange)
                .frame(width: 6, height: 6)
            Text(model.status)
                .lineLimit(1)
            Spacer()
            Text(model.document.sourceSha256.prefix(10))
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text("Tab 子主题 · Enter 同级 · Space 编辑")
                .foregroundStyle(.tertiary)
            Text("\(model.layout.order.count) visible")
            Text("Rust Core")
        }
        .font(.system(size: 10, weight: .medium, design: .monospaced))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 14)
        .frame(height: 30)
        .background(.black.opacity(0.32))
    }

    private func headerButton(
        _ symbol: String,
        _ help: String,
        enabled: Bool = true,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .semibold))
                .frame(width: 28, height: 26)
                .background(Color.white.opacity(0.07), in: RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.36)
        .help(help)
    }

    private func metaRow(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label)
                .foregroundStyle(.secondary)
            Text(value)
                .foregroundStyle(.primary.opacity(0.9))
                .lineLimit(1)
        }
        .font(.system(size: 10.5, design: .monospaced))
    }
}

@MainActor
final class KiteMindmapWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: KiteMindmapWindowController] = [:]
    private let model: MindmapEditorModel
    private var keyMonitor: Any?

    init(path: String) throws {
        let normalized = URL(fileURLWithPath: path).standardizedFileURL.path
        self.model = try MindmapEditorModel(path: normalized)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1_360, height: 860),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Mindmap — \(URL(fileURLWithPath: normalized).lastPathComponent)"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isMovableByWindowBackground = true
        window.minSize = NSSize(width: 900, height: 620)
        window.contentViewController = NSHostingController(
            rootView: KiteMindmapEditorView(model: model)
        )
        window.setFrameAutosaveName("KiteMindmapEditorWindow")
        super.init(window: window)
        window.delegate = self
        window.center()
        installKeyboardMonitor()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func open(path: String) {
        let normalized = URL(fileURLWithPath: path).standardizedFileURL.path
        if let existing = controllers[normalized] {
            existing.showWindow(nil)
            existing.window?.makeKeyAndOrderFront(nil)
            existing.window?.makeFirstResponder(existing.window?.contentView)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        do {
            let controller = try KiteMindmapWindowController(path: normalized)
            controllers[normalized] = controller
            controller.showWindow(nil)
            controller.window?.makeKeyAndOrderFront(nil)
            controller.window?.makeFirstResponder(controller.window?.contentView)
            NSApp.activate(ignoringOtherApps: true)
        } catch {
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = "无法打开 Mindmap"
            alert.informativeText = error.localizedDescription
            alert.runModal()
        }
    }

    static func chooseAndOpen() {
        let panel = NSOpenPanel()
        panel.title = "打开 XMind"
        panel.prompt = "打开"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if let type = UTType(filenameExtension: "xmind") {
            panel.allowedContentTypes = [type]
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        open(path: url.path)
    }

    static func openSample() {
        guard let bundled = Bundle.main.url(
            forResource: "kite-rust-gpui-capability",
            withExtension: "xmind"
        ) else {
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = "找不到 Mindmap 示例"
            alert.informativeText = "应用包中缺少 kite-rust-gpui-capability.xmind。"
            alert.runModal()
            return
        }

        let support = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first!.appendingPathComponent("Kite/Mindmaps", isDirectory: true)
        try? FileManager.default.createDirectory(
            at: support,
            withIntermediateDirectories: true
        )
        let destination = support.appendingPathComponent("Kite Rust GPUI 能力地图.xmind")
        if !FileManager.default.fileExists(atPath: destination.path) {
            try? FileManager.default.copyItem(at: bundled, to: destination)
        }
        open(path: destination.path)
    }

    func windowWillClose(_ notification: Notification) {
        if let keyMonitor {
            NSEvent.removeMonitor(keyMonitor)
        }
        keyMonitor = nil
        KiteMindmapWindowController.controllers.removeValue(forKey: model.path)
    }

    private func installKeyboardMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, self.window?.isKeyWindow == true else { return event }
            if self.model.textInputFocused { return event }

            let command = event.modifierFlags.contains(.command)
            let shift = event.modifierFlags.contains(.shift)
            let characters = event.charactersIgnoringModifiers?.lowercased()

            if command, characters == "z" {
                if shift {
                    self.model.redo()
                } else {
                    self.model.undo()
                }
                return nil
            }

            switch event.keyCode {
            case 48:
                self.model.addChild()
                return nil
            case 36:
                self.model.addSibling()
                return nil
            case 49, 120:
                self.model.beginEditingSelected()
                return nil
            case 51, 117:
                self.model.deleteSelected()
                return nil
            case 53:
                self.model.cancelInlineEdit()
                return nil
            case 123:
                self.model.moveSelection(.left)
                return nil
            case 124:
                self.model.moveSelection(.right)
                return nil
            case 125:
                self.model.moveSelection(.down)
                return nil
            case 126:
                self.model.moveSelection(.up)
                return nil
            default:
                return event
            }
        }
    }
}

private extension Color {
    init?(hex: String?) {
        guard let hex, hex.hasPrefix("#") else { return nil }
        let value = String(hex.dropFirst())
        guard value.count == 6, let number = UInt32(value, radix: 16) else { return nil }
        self.init(
            red: Double((number >> 16) & 0xFF) / 255,
            green: Double((number >> 8) & 0xFF) / 255,
            blue: Double(number & 0xFF) / 255
        )
    }
}
