import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
private struct MindmapMarkerChip: Identifiable, Equatable {
    let id: String
    let family: String
    let label: String
}

@MainActor
private struct MindmapTopicVisualStyle: Equatable {
    var fontFamily: String?
    var fontSize: Double = 12.5
    var fontWeight: String?
    var italic = false
    var underline = false
    var strike = false
    var textColorHex: String?
    var fillColorHex: String?
    var borderColorHex: String?
    var borderWidth: Double = 1
    var borderPattern: String?
    var cornerRadius: Double?
    var alignment: String?

    var accentHex: String? { borderColorHex ?? fillColorHex ?? textColorHex }
}

@MainActor
private struct MindmapTopic: Identifiable {
    let id: String
    var title: String
    let parentID: String?
    let depth: Int
    let childIDs: [String]
    let style: MindmapTopicVisualStyle
    let shapeClass: String?
    let markers: [MindmapMarkerChip]
    let isComplete: Bool

    var hasChildren: Bool { !childIDs.isEmpty }
    var hasTodo: Bool { markers.contains { $0.family == "task" } }
    var headerMarkers: [MindmapMarkerChip] { markers }
}

@MainActor
private struct MindmapTopicFormatDraft: Equatable {
    var fontFamily = ""
    var fontSize = 12.5
    var fontWeight = "normal"
    var italic = false
    var underline = false
    var strike = false
    var textColorHex = "#1F2328"
    var fillColorHex = "#F6F8FA"
    var borderColorHex = "#D0D7DE"
    var borderWidth = 1.0
    var borderPattern = "solid"
    var cornerRadius = 4.0
}

@MainActor
private struct MindmapGraphDraft: Equatable {
    var structure: MindmapGraphStructureDto = .logicRight
    var connectorKind: MindmapConnectorKindDto = .curved
    var cornerRadius = 12.0
}

@MainActor
private struct MindmapDateDraft: Equatable {
    var date = Date()
    var style: MindmapDateStyleDto = .iso
    var precision: MindmapDatePrecisionDto = .dateOnly
    var target: MindmapDateTargetDto = .title
    var placement: MindmapDatePlacementDto = .end
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
    @Published var showSidebar = true
    @Published var zenMode = false
    @Published var searchFocusRequest = 0
    @Published var zoom: CGFloat = 0.82
    @Published var pan = CGSize.zero
    @Published var status = "正在加载…"
    @Published var errorMessage: String?
    @Published var isWorking = false
    @Published var searchText = ""
    @Published var graphDraft = MindmapGraphDraft()
    @Published var topicFormat = MindmapTopicFormatDraft()
    @Published var topicMarkers: [MindmapMarkerChip] = []
    @Published var priorityLevel = 0
    @Published var taskProgressMarkerID = ""
    @Published var topicIsComplete = false
    @Published var dateDraft = MindmapDateDraft()

    private var viewportSize = CGSize(width: 1_000, height: 700)
    private var didFit = false
    private var copiedTopicJSON: String?
    private var undoStack: [MindmapSnapshot] = []
    private var redoStack: [MindmapSnapshot] = []

    init(path: String) throws {
        self.path = URL(fileURLWithPath: path).standardizedFileURL.path
        self.document = try openMindmapDocument(path: self.path)
        if let style = try? readMindmapDocumentStyle(path: self.path, sheetIndex: 0) {
            graphDraft = Self.graphDraft(from: style)
        }
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
        seedInspector(from: id.flatMap { topics[$0] })
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

    func addSiblingBefore() {
        guard let topicID = selectedID ?? rootID else { return }
        performEdit(
            .insertSiblingBefore(topicId: topicID, title: "新主题"),
            selecting: .newest
        )
    }

    func addParent() {
        guard let topicID = selectedID ?? rootID else { return }
        performEdit(.newParent(topicId: topicID, title: "新父主题"), selecting: .newest)
    }

    func moveSelectedUp() {
        guard let topicID = selectedID else { return }
        performEdit(.moveUp(topicId: topicID), selecting: .keep(topicID))
    }

    func moveSelectedDown() {
        guard let topicID = selectedID else { return }
        performEdit(.moveDown(topicId: topicID), selecting: .keep(topicID))
    }

    func copySelected() {
        guard let topicID = selectedID,
              let json = Self.topicJSON(in: document.contentJson, topicID: topicID) else {
            return
        }
        copiedTopicJSON = json
        status = "已复制主题及其子级"
    }

    func cutSelected() {
        guard selectedID != nil else { return }
        copySelected()
        guard copiedTopicJSON != nil else { return }
        deleteSelected()
    }

    func pasteCopiedTopic() {
        guard let topicJSON = copiedTopicJSON,
              let parentID = selectedID ?? rootID else { return }
        performEdit(
            .paste(parentId: parentID, topicJson: topicJSON),
            selecting: .newest
        )
    }

    func pasteTextAsTopic() {
        guard let value = NSPasteboard.general.string(forType: .string),
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              let parentID = selectedID ?? rootID else {
            return
        }
        performEdit(.addChild(parentId: parentID, title: value), selecting: .newest)
    }

    func requestSearchFocus() {
        searchFocusRequest &+= 1
    }

    func toggleSidebar() {
        showSidebar.toggle()
    }

    func applyDocumentStyle() {
        guard let structure = graphDraft.structure.supportedStructure,
              let connectorKind = graphDraft.connectorKind.supportedConnector else {
            errorMessage = "当前文档的图结构或连线样式未知，Kite 不会覆盖原值"
            return
        }
        performDocumentOperation(status: "已更新图结构与连线", fitAfter: true) {
            try updateMindmapDocumentStyle(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                patch: MindmapDocumentStylePatchDto(
                    structure: structure,
                    connectorKind: connectorKind,
                    connectorCornerRadius: UInt16(max(0, min(256, self.graphDraft.cornerRadius)))
                )
            )
        }
    }

    func applyPriority(_ level: Int) {
        guard let topicID = selectedID else { return }
        performDocumentOperation(status: level == 0 ? "已清除优先级" : "已设置优先级 P\(level)") {
            try setMindmapPriority(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID,
                level: UInt8(max(0, min(9, level)))
            )
        }
    }

    func applyTaskProgress() {
        guard let topicID = selectedID else { return }
        performDocumentOperation(status: taskProgressMarkerID.isEmpty ? "已清除任务进度" : "已更新任务进度") {
            try setMindmapTaskProgress(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID,
                markerId: self.taskProgressMarkerID
            )
        }
    }

    func addMarker(_ markerID: String) {
        guard let topicID = selectedID else { return }
        performDocumentOperation(status: "已添加标记 \(markerID)") {
            try addMindmapMarker(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID,
                markerId: markerID
            )
        }
    }

    func removeMarker(_ markerID: String) {
        guard let topicID = selectedID else { return }
        performDocumentOperation(status: "已移除标记 \(markerID)") {
            try removeMindmapMarker(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID,
                markerId: markerID
            )
        }
    }

    func setTodoCompletion(_ completed: Bool) {
        guard completed != topicIsComplete else { return }
        toggleTodo()
    }

    func toggleTodo() {
        guard let topicID = selectedID else { return }
        if taskProgressMarkerID.isEmpty {
            taskProgressMarkerID = topicIsComplete ? "task-start" : "task-done"
            applyTaskProgress()
            return
        }
        performDocumentOperation(status: topicIsComplete ? "已恢复为未完成" : "已标记完成") {
            try toggleMindmapTodo(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID
            )
        }
    }

    func insertDate() {
        guard let topicID = selectedID else { return }
        let calendar = Calendar.current
        let components = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: dateDraft.date)
        guard let year = components.year,
              let month = components.month,
              let day = components.day else { return }
        let date = String(format: "%04d-%02d-%02d", year, month, day)
        let time = String(
            format: "%02d:%02d:%02d",
            components.hour ?? 0,
            components.minute ?? 0,
            components.second ?? 0
        )
        let systemPattern = Self.systemDatePattern()
        let request = MindmapDateRequestDto(
            date: date,
            time: dateDraft.precision == .dateOnly ? nil : time,
            style: dateDraft.style,
            precision: dateDraft.precision,
            target: dateDraft.target,
            placement: dateDraft.placement,
            systemDateOrder: systemPattern?.0,
            systemDateSeparator: systemPattern?.1
        )
        performDocumentOperation(status: "已插入日期") {
            try insertMindmapDate(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID,
                request: request
            ).document
        }
    }

    func applyTopicFormat() {
        guard let topicID = selectedID else { return }
        let style = MindmapTopicStyleDto(
            fontFamily: topicFormat.fontFamily.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? nil
                : topicFormat.fontFamily,
            fontSize: topicFormat.fontSize,
            fontWeight: topicFormat.fontWeight,
            italic: topicFormat.italic,
            underline: topicFormat.underline,
            strike: topicFormat.strike,
            textColor: topicFormat.textColorHex,
            alignment: "left",
            nodeFill: topicFormat.fillColorHex,
            borderColor: topicFormat.borderColorHex,
            borderWidth: topicFormat.borderWidth,
            borderPattern: topicFormat.borderPattern,
            cornerRadius: UInt16(max(0, min(512, topicFormat.cornerRadius)))
        )
        performDocumentOperation(status: "已更新文字与节点格式") {
            try updateMindmapTopicStyle(
                path: self.path,
                expectedSourceSha256: self.document.sourceSha256,
                sheetIndex: 0,
                topicId: topicID,
                style: style
            )
        }
    }

    func toggleZenMode() {
        zenMode.toggle()
    }

    func zoomReset() {
        zoom = 1
    }

    func centerRoot() {
        guard let rootID, let root = layout.nodes[rootID] else { return }
        pan = CGSize(
            width: -viewportSize.width * 0.32,
            height: -root.rect.midY * zoom
        )
    }

    func toggleBranch() {
        guard let selectedID, topics[selectedID]?.hasChildren == true else { return }
        toggleCollapse(selectedID)
    }

    func expandAllDescendants() {
        guard let selectedID else { return }
        var ids = Set<String>()
        func collect(_ id: String) {
            guard let topic = topics[id] else { return }
            ids.insert(id)
            for child in topic.childIDs {
                collect(child)
            }
        }
        collect(selectedID)
        collapsedIDs.subtract(ids)
        rebuildLayout()
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

    private func performDocumentOperation(
        status successStatus: String,
        fitAfter: Bool = false,
        operation: () throws -> MindmapDocumentDto
    ) {
        isWorking = true
        defer { isWorking = false }
        let previousIDs = Set(topics.keys)
        let snapshot = MindmapSnapshot(
            contentJson: document.contentJson,
            sourceSha256: document.sourceSha256
        )
        do {
            document = try operation()
            undoStack.append(snapshot)
            redoStack.removeAll()
            rebuildFromDocument(
                initial: false,
                previousIDs: previousIDs,
                selectionRule: .keep(selectedID ?? rootID ?? "")
            )
            if fitAfter { fit() }
            status = "\(successStatus) · 已保存"
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
            status = "格式编辑未保存"
        }
    }

    private func seedInspector(from topic: MindmapTopic?) {
        guard let topic else {
            topicFormat = MindmapTopicFormatDraft()
            topicMarkers = []
            priorityLevel = 0
            taskProgressMarkerID = ""
            topicIsComplete = false
            return
        }
        topicMarkers = topic.markers
        topicIsComplete = topic.isComplete
        priorityLevel = topic.markers.first(where: { $0.family == "priority" })
            .flatMap { Int($0.id.split(separator: "-").last ?? "") } ?? 0
        taskProgressMarkerID = topic.markers.first(where: { $0.family == "task" })?.id ?? ""
        topicFormat = MindmapTopicFormatDraft(
            fontFamily: topic.style.fontFamily ?? "",
            fontSize: topic.style.fontSize,
            fontWeight: topic.style.fontWeight ?? "normal",
            italic: topic.style.italic,
            underline: topic.style.underline,
            strike: topic.style.strike,
            textColorHex: topic.style.textColorHex ?? "#1F2328",
            fillColorHex: topic.style.fillColorHex ?? (topic.parentID == nil ? "#1B1D20" : "#F6F8FA"),
            borderColorHex: topic.style.borderColorHex ?? "#D0D7DE",
            borderWidth: topic.style.borderWidth,
            borderPattern: topic.style.borderPattern ?? "solid",
            cornerRadius: topic.style.cornerRadius ?? (topic.shapeClass?.contains("rounded") == true ? 8 : 2)
        )
    }

    private func refreshGraphDraft() {
        if let style = try? readMindmapDocumentStyle(path: path, sheetIndex: 0) {
            graphDraft = Self.graphDraft(from: style)
        }
    }

    private static func graphDraft(from style: MindmapDocumentStyleDto) -> MindmapGraphDraft {
        MindmapGraphDraft(
            structure: style.structure == .unknown ? .logicRight : style.structure,
            connectorKind: style.connectorKind == .unknown ? .curved : style.connectorKind,
            cornerRadius: Double(style.connectorCornerRadius ?? 12)
        )
    }

    private static func systemDatePattern() -> (MindmapDatePartOrderDto, String)? {
        let pattern = DateFormatter.dateFormat(
            fromTemplate: "yMd",
            options: 0,
            locale: Locale.current
        ) ?? "y/M/d"
        let separator = pattern.first(where: { !$0.isLetter && !$0.isNumber }).map(String.init) ?? "/"
        let stripped = pattern.filter { $0 == "y" || $0 == "M" || $0 == "d" }
        if stripped.hasPrefix("y") { return (.yearMonthDay, separator) }
        if stripped.hasPrefix("M") { return (.monthDayYear, separator) }
        return (.dayMonthYear, separator)
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
            let previousGraphDraft = graphDraft
            refreshGraphDraft()
            rebuildFromDocument(
                initial: false,
                previousIDs: Set(topics.keys),
                selectionRule: .keep(selectedID ?? rootID ?? "")
            )
            if previousGraphDraft != graphDraft { fit() }
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

        func visibleChildren(_ id: String) -> [String] {
            guard !collapsedIDs.contains(id), let topic = topics[id] else { return [] }
            return topic.childIDs.filter { topics[$0] != nil }
        }

        func nodeSize(_ topic: MindmapTopic) -> CGSize {
            let newlineCount = topic.title.split(separator: "\n", omittingEmptySubsequences: false).count
            let characters = max(6, topic.title.count)
            let markerAllowance = topic.markers.isEmpty ? 0 : 28
            let width = min(390, max(112, 58 + CGFloat(characters) * 7.3 + CGFloat(markerAllowance)))
            var height: CGFloat = newlineCount > 2 ? 68 : newlineCount > 1 ? 52 : 38
            if !topic.markers.isEmpty { height += 16 }
            return CGSize(width: width, height: height)
        }

        var nodes: [String: MindmapNodeLayout] = [:]
        var order: [String] = []
        let gap: CGFloat = 12
        let column: CGFloat = 320

        var heights: [String: CGFloat] = [:]
        func measureHeight(_ id: String) -> CGFloat {
            if let cached = heights[id] { return cached }
            guard let topic = topics[id] else { return 0 }
            let own = nodeSize(topic).height
            let children = visibleChildren(id)
            let childTotal = children.isEmpty
                ? 0
                : children.map(measureHeight).reduce(0, +) + gap * CGFloat(children.count - 1)
            let value = max(own, childTotal)
            heights[id] = value
            return value
        }

        func placeHorizontal(_ id: String, depth: Int, top: CGFloat, direction: CGFloat) {
            guard let topic = topics[id] else { return }
            let subtreeHeight = heights[id] ?? measureHeight(id)
            let size = nodeSize(topic)
            let x: CGFloat
            if depth == 0 {
                x = 0
            } else if direction > 0 {
                x = CGFloat(depth) * column
            } else {
                x = -CGFloat(depth) * column - size.width
            }
            let rect = CGRect(
                x: x,
                y: top + (subtreeHeight - size.height) / 2,
                width: size.width,
                height: size.height
            )
            nodes[id] = MindmapNodeLayout(id: id, rect: rect, parentID: topic.parentID)
            order.append(id)

            let children = visibleChildren(id)
            guard !children.isEmpty else { return }
            let childHeights = children.map { heights[$0] ?? measureHeight($0) }
            let total = childHeights.reduce(0, +) + gap * CGFloat(children.count - 1)
            var cursor = top + (subtreeHeight - total) / 2
            for (index, child) in children.enumerated() {
                placeHorizontal(child, depth: depth + 1, top: cursor, direction: direction)
                cursor += childHeights[index] + gap
            }
        }

        switch graphDraft.structure {
        case .logicLeft:
            _ = measureHeight(rootID)
            placeHorizontal(rootID, depth: 0, top: 0, direction: -1)
        case .mindMap:
            _ = measureHeight(rootID)
            guard let rootTopic = topics[rootID] else { break }
            let rootSize = nodeSize(rootTopic)
            nodes[rootID] = MindmapNodeLayout(
                id: rootID,
                rect: CGRect(x: 0, y: -rootSize.height / 2, width: rootSize.width, height: rootSize.height),
                parentID: nil
            )
            order.append(rootID)
            let children = visibleChildren(rootID)
            let rightChildren = children.enumerated().filter { $0.offset.isMultiple(of: 2) }.map(\.element)
            let leftChildren = children.enumerated().filter { !$0.offset.isMultiple(of: 2) }.map(\.element)
            placeForest(rightChildren, direction: 1, depth: 1)
            placeForest(leftChildren, direction: -1, depth: 1)

            func placeForest(_ ids: [String], direction: CGFloat, depth: Int) {
                guard !ids.isEmpty else { return }
                let childHeights = ids.map { heights[$0] ?? measureHeight($0) }
                let total = childHeights.reduce(0, +) + gap * CGFloat(ids.count - 1)
                var cursor = -total / 2
                for (index, child) in ids.enumerated() {
                    placeHorizontal(child, depth: depth, top: cursor, direction: direction)
                    cursor += childHeights[index] + gap
                }
            }
        case .orgChart:
            layoutOrgChart(rootID: rootID, nodeSize: nodeSize, visibleChildren: visibleChildren)
            return
        case .logicRight, .tree, .unknown:
            _ = measureHeight(rootID)
            placeHorizontal(rootID, depth: 0, top: 0, direction: 1)
        }

        let rects = nodes.values.map(\.rect)
        let bounds = rects.reduce(CGRect.null) { $0.union($1) }.insetBy(dx: -80, dy: -80)
        layout = MindmapLayout(nodes: nodes, order: order, bounds: bounds)
    }

    private func layoutOrgChart(
        rootID: String,
        nodeSize: (MindmapTopic) -> CGSize,
        visibleChildren: (String) -> [String]
    ) {
        let gap: CGFloat = 22
        let rowHeight: CGFloat = 92
        var widths: [String: CGFloat] = [:]
        var nodes: [String: MindmapNodeLayout] = [:]
        var order: [String] = []

        func measureWidth(_ id: String) -> CGFloat {
            if let cached = widths[id] { return cached }
            guard let topic = topics[id] else { return 0 }
            let own = nodeSize(topic).width
            let children = visibleChildren(id)
            let childTotal = children.isEmpty
                ? 0
                : children.map(measureWidth).reduce(0, +) + gap * CGFloat(children.count - 1)
            let value = max(own, childTotal)
            widths[id] = value
            return value
        }

        func place(_ id: String, depth: Int, left: CGFloat) {
            guard let topic = topics[id] else { return }
            let subtreeWidth = widths[id] ?? measureWidth(id)
            let size = nodeSize(topic)
            let rect = CGRect(
                x: left + (subtreeWidth - size.width) / 2,
                y: CGFloat(depth) * rowHeight,
                width: size.width,
                height: size.height
            )
            nodes[id] = MindmapNodeLayout(id: id, rect: rect, parentID: topic.parentID)
            order.append(id)

            let children = visibleChildren(id)
            guard !children.isEmpty else { return }
            let childWidths = children.map { widths[$0] ?? measureWidth($0) }
            let total = childWidths.reduce(0, +) + gap * CGFloat(children.count - 1)
            var cursor = left + (subtreeWidth - total) / 2
            for (index, child) in children.enumerated() {
                place(child, depth: depth + 1, left: cursor)
                cursor += childWidths[index] + gap
            }
        }

        _ = measureWidth(rootID)
        place(rootID, depth: 0, left: 0)
        let rects = nodes.values.map(\.rect)
        let bounds = rects.reduce(CGRect.null) { $0.union($1) }.insetBy(dx: -80, dy: -80)
        layout = MindmapLayout(nodes: nodes, order: order, bounds: bounds)
    }

    private static func topicJSON(in contentJSON: String, topicID: String) -> String? {
        guard let data = contentJSON.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) else { return nil }

        func find(_ value: Any) -> [String: Any]? {
            if let object = value as? [String: Any] {
                if object["id"] as? String == topicID {
                    return object
                }
                if let children = (object["children"] as? [String: Any])?["attached"] as? [Any] {
                    for child in children {
                        if let found = find(child) { return found }
                    }
                }
            }
            if let array = value as? [Any] {
                for child in array {
                    if let found = find(child) { return found }
                }
            }
            return nil
        }

        guard let topic = find(root),
              let topicData = try? JSONSerialization.data(withJSONObject: topic),
              let json = String(data: topicData, encoding: .utf8) else { return nil }
        return json
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
            let style = parseVisualStyle(properties)
            let shape = (object["shape-class"] as? String) ?? (properties?["shape-class"] as? String)
            let markerObjects = object["markers"] as? [[String: Any]] ?? []
            let markers = markerObjects.compactMap { marker -> MindmapMarkerChip? in
                guard let markerID = marker["markerId"] as? String else { return nil }
                return MindmapMarkerChip(
                    id: markerID,
                    family: markerFamily(markerID),
                    label: markerLabel(markerID)
                )
            }
            let isComplete = markers.contains { $0.id == "task-done" }
            let childrenObject = object["children"] as? [String: Any]
            let childObjects = childrenObject?["attached"] as? [[String: Any]] ?? []
            let childIDs = childObjects.compactMap { $0["id"] as? String }
            topics[id] = MindmapTopic(
                id: id,
                title: title,
                parentID: parentID,
                depth: depth,
                childIDs: childIDs,
                style: style,
                shapeClass: shape,
                markers: markers,
                isComplete: isComplete
            )
            for child in childObjects {
                walk(child, parentID: id, depth: depth + 1)
            }
        }

        walk(rootTopic, parentID: nil, depth: 0)
        return (topics, rootID)
    }

    private static func parseVisualStyle(_ properties: [String: Any]?) -> MindmapTopicVisualStyle {
        var style = MindmapTopicVisualStyle()
        style.fontFamily = properties?["fo:font-family"] as? String
        style.fontSize = parseFontSize(properties?["fo:font-size"])
        style.fontWeight = properties?["fo:font-weight"] as? String
        let fontStyle = (properties?["fo:font-style"] as? String)?.lowercased()
        let decoration = (properties?["fo:text-decoration"] as? String)?.lowercased() ?? ""
        style.italic = fontStyle == "italic" || fontStyle == "oblique"
        style.underline = decoration.split(whereSeparator: \.isWhitespace).contains("underline")
        style.strike = decoration.split(whereSeparator: \.isWhitespace).contains("line-through")
        style.textColorHex = properties?["fo:color"] as? String
        let fill = properties?["svg:fill"] as? String ?? properties?["fill-color"] as? String
        style.fillColorHex = fill?.lowercased() == "none" ? nil : fill
        style.borderColorHex = properties?["border-line-color"] as? String
        style.borderWidth = parseMeasurement(properties?["border-line-width"]) ?? 1
        style.borderPattern = properties?["border-line-pattern"] as? String
        style.cornerRadius = parseMeasurement(properties?["shape-corner"])
        style.alignment = properties?["fo:text-align"] as? String
        return style
    }

    private static func parseFontSize(_ value: Any?) -> Double {
        parseMeasurement(value) ?? 12.5
    }

    private static func parseMeasurement(_ value: Any?) -> Double? {
        if let number = value as? NSNumber { return number.doubleValue }
        guard let string = value as? String else { return nil }
        let numeric = string.drop(while: { !$0.isNumber && $0 != "." })
        return Double(numeric.trimmingCharacters(in: CharacterSet(charactersIn: "ptpx%em ")))
    }

    private static func markerFamily(_ markerID: String) -> String {
        if markerID.hasPrefix("priority-") { return "priority" }
        if markerID.hasPrefix("task-") { return "task" }
        if markerID.hasPrefix("tag-") { return "tag" }
        if markerID.hasPrefix("flag-") { return "flag" }
        if markerID.hasPrefix("star-") { return "star" }
        if markerID.hasPrefix("people-") { return "people" }
        if markerID.hasPrefix("smiley-") { return "smiley" }
        if markerID.hasPrefix("symbol-") || markerID.hasPrefix("symbol_") { return "symbol" }
        return "other"
    }

    private static func markerLabel(_ markerID: String) -> String {
        if markerID.hasPrefix("priority-") {
            return "P\(markerID.dropFirst("priority-".count))"
        }
        switch markerID {
        case "task-start": return "○"
        case "task-oct": return "12%"
        case "task-quarter": return "25%"
        case "task-3oct": return "38%"
        case "task-half": return "50%"
        case "task-5oct": return "62%"
        case "task-3quar": return "75%"
        case "task-7oct": return "88%"
        case "task-done": return "✓"
        case "task-pause": return "Ⅱ"
        default: break
        }
        if markerID.hasPrefix("flag-") { return "⚑" }
        if markerID.hasPrefix("star-") { return "★" }
        if markerID.hasPrefix("tag-") { return "#" }
        if markerID.hasPrefix("people-") { return "●" }
        if markerID.hasPrefix("smiley-") { return "☺" }
        if markerID.hasPrefix("symbol-") || markerID.hasPrefix("symbol_") { return "!" }
        return (markerID.split(separator: "-").last.map(String.init) ?? markerID).prefix(2).uppercased()
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
                if !model.zenMode {
                    header
                    Divider().overlay(Color.white.opacity(0.08))
                }
                HStack(spacing: 0) {
                    if model.showSidebar && !model.zenMode {
                        sidebar
                        Rectangle()
                            .fill(Color.white.opacity(0.08))
                            .frame(width: 1)
                    }
                    canvas
                }
                if !model.zenMode {
                    Divider().overlay(Color.white.opacity(0.08))
                    statusBar
                }
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
        .onChange(of: model.searchFocusRequest) { _ in
            NSApp.keyWindow?.makeFirstResponder(nil)
            searchFocused = true
        }
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
                Text("格式与语义")
                    .font(.system(size: 16, weight: .semibold, design: .rounded))
            }

            documentStylePanel
            Divider().overlay(Color.white.opacity(0.08))

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
                topicSemanticPanel
                Divider().overlay(Color.white.opacity(0.08))
                topicFormatPanel
                Divider().overlay(Color.white.opacity(0.08))

                Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 7) {
                    metaRow("深度", "\(topic.depth)")
                    metaRow("子主题", "\(topic.childIDs.count)")
                    metaRow("形状", topic.shapeClass ?? "默认")
                    metaRow("字体", topic.style.fontFamily ?? "系统")
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

    private var documentStylePanel: some View {
        VStack(alignment: .leading, spacing: 7) {
            sectionHeader("DOCUMENT GRAPH")
            Picker("结构", selection: $model.graphDraft.structure) {
                Text("思维导图").tag(MindmapGraphStructureDto.mindMap)
                Text("右向逻辑图").tag(MindmapGraphStructureDto.logicRight)
                Text("左向逻辑图").tag(MindmapGraphStructureDto.logicLeft)
                Text("组织架构图").tag(MindmapGraphStructureDto.orgChart)
                Text("树形图").tag(MindmapGraphStructureDto.tree)
            }
            .pickerStyle(.menu)
            .controlSize(.small)
            Picker("连线", selection: $model.graphDraft.connectorKind) {
                Text("曲线").tag(MindmapConnectorKindDto.curved)
                Text("直线").tag(MindmapConnectorKindDto.straight)
                Text("直角折线").tag(MindmapConnectorKindDto.elbow)
                Text("圆角折线").tag(MindmapConnectorKindDto.roundedElbow)
            }
            .pickerStyle(.menu)
            .controlSize(.small)
            if model.graphDraft.connectorKind == .roundedElbow {
                HStack {
                    Text("拐角半径")
                    Slider(value: $model.graphDraft.cornerRadius, in: 0...48, step: 1)
                    Text("\(Int(model.graphDraft.cornerRadius))")
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 24, alignment: .trailing)
                }
                .font(.system(size: 10))
            }
            Button("应用图结构") { model.applyDocumentStyle() }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
    }

    private var topicSemanticPanel: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader("MARKERS · TODO · DATE")
            if !model.topicMarkers.isEmpty {
                FlowMarkerRow(markers: model.topicMarkers) { markerID in
                    model.removeMarker(markerID)
                }
            }

            HStack(spacing: 6) {
                Toggle(isOn: Binding(
                    get: { model.topicIsComplete },
                    set: { model.setTodoCompletion($0) }
                )) {
                    Text("TODO 完成")
                        .font(.system(size: 10))
                }
                .toggleStyle(.checkbox)
                Spacer()
                Button(model.topicIsComplete ? "设为未完成" : "设为完成") { model.toggleTodo() }
                    .controlSize(.mini)
            }

            HStack(spacing: 5) {
                Picker("优先级", selection: $model.priorityLevel) {
                    Text("无").tag(0)
                    ForEach(1...5, id: \.self) { level in
                        Text("P\(level)").tag(level)
                    }
                }
                .pickerStyle(.menu)
                .controlSize(.small)
                Button("应用") { model.applyPriority(model.priorityLevel) }
                    .controlSize(.mini)
            }

            HStack(spacing: 5) {
                Picker("任务进度", selection: $model.taskProgressMarkerID) {
                    Text("无").tag("")
                    Text("未开始").tag("task-start")
                    Text("25%").tag("task-quarter")
                    Text("50%").tag("task-half")
                    Text("75%").tag("task-3quar")
                    Text("完成").tag("task-done")
                    Text("暂停").tag("task-pause")
                }
                .pickerStyle(.menu)
                .controlSize(.small)
                Button("应用") { model.applyTaskProgress() }
                    .controlSize(.mini)
            }

            Menu("添加通用标记") {
                Button("红旗") { model.addMarker("flag-red") }
                Button("黄旗") { model.addMarker("flag-yellow") }
                Button("星标") { model.addMarker("star-yellow") }
                Button("警示") { model.addMarker("symbol-attention") }
                Button("标签") { model.addMarker("tag-red") }
            }
            .menuStyle(.borderlessButton)
            .controlSize(.small)

            HStack(spacing: 6) {
                DatePicker(
                    "",
                    selection: $model.dateDraft.date,
                    displayedComponents: model.dateDraft.precision == .dateOnly ? [.date] : [.date, .hourAndMinute]
                )
                .labelsHidden()
                Picker("", selection: $model.dateDraft.style) {
                    Text("ISO").tag(MindmapDateStyleDto.iso)
                    Text("中文").tag(MindmapDateStyleDto.chinese)
                    Text("English").tag(MindmapDateStyleDto.englishUs)
                    Text("系统").tag(MindmapDateStyleDto.system)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)
                Button("插入") { model.insertDate() }
                    .controlSize(.mini)
            }
            HStack(spacing: 6) {
                Picker("位置", selection: $model.dateDraft.target) {
                    Text("标题").tag(MindmapDateTargetDto.title)
                    Text("备注").tag(MindmapDateTargetDto.note)
                }
                .pickerStyle(.segmented)
                .controlSize(.mini)
                Picker("精度", selection: $model.dateDraft.precision) {
                    Text("日期").tag(MindmapDatePrecisionDto.dateOnly)
                    Text("分钟").tag(MindmapDatePrecisionDto.minutes)
                    Text("秒").tag(MindmapDatePrecisionDto.seconds)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)
            }
        }
    }

    private var topicFormatPanel: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader("TEXT · NODE")
            HStack(spacing: 6) {
                TextField("字体族", text: $model.topicFormat.fontFamily)
                    .textFieldStyle(.plain)
                    .padding(.horizontal, 7)
                    .frame(height: 26)
                    .background(Color.white.opacity(0.07), in: RoundedRectangle(cornerRadius: 5))
                Stepper(value: $model.topicFormat.fontSize, in: 8...72, step: 1) {
                    Text("\(Int(model.topicFormat.fontSize)) pt")
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 42, alignment: .trailing)
                }
                .controlSize(.mini)
            }
            HStack(spacing: 6) {
                Picker("", selection: $model.topicFormat.fontWeight) {
                    Text("Regular").tag("normal")
                    Text("Medium").tag("500")
                    Text("Semibold").tag("600")
                    Text("Bold").tag("bold")
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)
                Toggle("I", isOn: $model.topicFormat.italic).toggleStyle(.button)
                Toggle("U", isOn: $model.topicFormat.underline).toggleStyle(.button)
                Toggle("S", isOn: $model.topicFormat.strike).toggleStyle(.button)
            }
            ColorPicker("文字", selection: colorBinding(\.textColorHex), supportsOpacity: false)
            ColorPicker("节点背景", selection: colorBinding(\.fillColorHex), supportsOpacity: false)
            ColorPicker("节点边框", selection: colorBinding(\.borderColorHex), supportsOpacity: false)
            HStack(spacing: 8) {
                Stepper(value: $model.topicFormat.borderWidth, in: 0...8, step: 0.5) {
                    Text("边框 \(model.topicFormat.borderWidth, specifier: "%.1f")")
                }
                Stepper(value: $model.topicFormat.cornerRadius, in: 0...32, step: 1) {
                    Text("圆角 \(Int(model.topicFormat.cornerRadius))")
                }
            }
            .font(.system(size: 9.5))
            .controlSize(.mini)
            Button("应用文字与节点格式") { model.applyTopicFormat() }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
    }

    private func sectionHeader(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 9, weight: .bold, design: .monospaced))
            .tracking(1.1)
            .foregroundStyle(.secondary)
    }

    private func colorBinding(_ keyPath: WritableKeyPath<MindmapTopicFormatDraft, String>) -> Binding<Color> {
        Binding(
            get: { Color(hex: model.topicFormat[keyPath: keyPath]) ?? .black },
            set: { color in
                if let hex = color.hexString {
                    model.topicFormat[keyPath: keyPath] = hex
                }
            }
        )
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
                drawEdge(context: &scene, parent: parent, child: node, colorHex: topic.style.accentHex, isComplete: topic.isComplete)
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
                    }), let node = model.layout.nodes[hit], let topic = model.topics[hit] {
                        if toggleBadgeRect(for: node).contains(point) {
                            model.toggleCollapse(hit)
                        } else if topic.hasTodo && todoCheckboxRect(for: node).contains(point) {
                            model.select(hit)
                            model.toggleTodo()
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
                    }), let node = model.layout.nodes[hit], let topic = model.topics[hit] {
                        if toggleBadgeRect(for: node).contains(point) {
                            model.select(hit)
                            model.toggleCollapse(hit)
                        } else if topic.hasTodo && todoCheckboxRect(for: node).contains(point) {
                            model.select(hit)
                            model.toggleTodo()
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
        let lineCount = max(1, model.inlineTitle.split(separator: "\n", omittingEmptySubsequences: false).count)
        let editorHeight = max(rect.height, CGFloat(min(4, lineCount)) * 18 + 14)
        return TextEditor(text: $model.inlineTitle)
            .font(.system(size: max(10, min(14, topic.style.fontSize)), weight: .medium, design: .rounded))
            .scrollContentBackground(.hidden)
            .padding(.horizontal, 5)
            .padding(.vertical, 4)
            .frame(width: max(130, rect.width), height: editorHeight)
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
        colorHex: String?,
        isComplete: Bool
    ) {
        let isVertical = model.graphDraft.structure == .orgChart
        let start: CGPoint
        let end: CGPoint
        if isVertical {
            start = CGPoint(x: parent.rect.midX, y: parent.rect.maxY)
            end = CGPoint(x: child.rect.midX, y: child.rect.minY)
        } else if child.rect.midX >= parent.rect.midX {
            start = CGPoint(x: parent.rect.maxX, y: parent.rect.midY)
            end = CGPoint(x: child.rect.minX, y: child.rect.midY)
        } else {
            start = CGPoint(x: parent.rect.minX, y: parent.rect.midY)
            end = CGPoint(x: child.rect.maxX, y: child.rect.midY)
        }

        var path = Path()
        path.move(to: start)
        switch model.graphDraft.connectorKind {
        case .straight:
            path.addLine(to: end)
        case .elbow:
            if isVertical {
                let midY = (start.y + end.y) / 2
                path.addLine(to: CGPoint(x: start.x, y: midY))
                path.addLine(to: CGPoint(x: end.x, y: midY))
            } else {
                let midX = (start.x + end.x) / 2
                path.addLine(to: CGPoint(x: midX, y: start.y))
                path.addLine(to: CGPoint(x: midX, y: end.y))
            }
            path.addLine(to: end)
        case .roundedElbow:
            appendRoundedElbow(to: &path, start: start, end: end, vertical: isVertical)
        case .curved, .unknown:
            if isVertical {
                let midY = (start.y + end.y) / 2
                path.addCurve(
                    to: end,
                    control1: CGPoint(x: start.x, y: midY),
                    control2: CGPoint(x: end.x, y: midY)
                )
            } else {
                let midX = (start.x + end.x) / 2
                path.addCurve(
                    to: end,
                    control1: CGPoint(x: midX, y: start.y),
                    control2: CGPoint(x: midX, y: end.y)
                )
            }
        }
        let color = Color(hex: colorHex) ?? Color(red: 0.24, green: 0.25, blue: 0.24)
        context.stroke(
            path,
            with: .color(color.opacity(isComplete ? 0.28 : 0.58)),
            lineWidth: 1.25
        )
    }

    private func appendRoundedElbow(to path: inout Path, start: CGPoint, end: CGPoint, vertical: Bool) {
        let requested = CGFloat(max(0, min(64, model.graphDraft.cornerRadius)))
        if vertical {
            let midY = (start.y + end.y) / 2
            let dx = end.x - start.x
            let dy = end.y - start.y
            let radius = min(requested, abs(dx) / 2, abs(dy) / 2)
            let xSign: CGFloat = dx >= 0 ? 1 : -1
            let ySign: CGFloat = dy >= 0 ? 1 : -1
            path.addLine(to: CGPoint(x: start.x, y: midY - ySign * radius))
            path.addQuadCurve(
                to: CGPoint(x: start.x + xSign * radius, y: midY),
                control: CGPoint(x: start.x, y: midY)
            )
            path.addLine(to: CGPoint(x: end.x - xSign * radius, y: midY))
            path.addQuadCurve(
                to: CGPoint(x: end.x, y: midY + ySign * radius),
                control: CGPoint(x: end.x, y: midY)
            )
            path.addLine(to: end)
        } else {
            let midX = (start.x + end.x) / 2
            let dx = end.x - start.x
            let dy = end.y - start.y
            let radius = min(requested, abs(dx) / 2, abs(dy) / 2)
            let xSign: CGFloat = dx >= 0 ? 1 : -1
            let ySign: CGFloat = dy >= 0 ? 1 : -1
            path.addLine(to: CGPoint(x: midX - xSign * radius, y: start.y))
            path.addQuadCurve(
                to: CGPoint(x: midX, y: start.y + ySign * radius),
                control: CGPoint(x: midX, y: start.y)
            )
            path.addLine(to: CGPoint(x: midX, y: end.y - ySign * radius))
            path.addQuadCurve(
                to: CGPoint(x: midX + xSign * radius, y: end.y),
                control: CGPoint(x: midX, y: end.y)
            )
            path.addLine(to: end)
        }
    }

    private func drawNode(context: inout GraphicsContext, topic: MindmapTopic, layout: MindmapNodeLayout) {
        let rect = layout.rect
        let style = topic.style
        let selected = model.selectedID == topic.id
        let isRoot = topic.parentID == nil
        let accent = Color(hex: style.accentHex) ?? Color(red: 0.72, green: 0.34, blue: 0.15)
        let fill = Color(hex: style.fillColorHex)
            ?? (isRoot
                ? Color(red: 0.10, green: 0.105, blue: 0.11)
                : Color(red: 0.985, green: 0.98, blue: 0.95))
        let textColor = Color(hex: style.textColorHex)
            ?? (isRoot ? .white : Color(red: 0.12, green: 0.125, blue: 0.12))
        let borderColor = Color(hex: style.borderColorHex)
            ?? (isRoot ? .black.opacity(0.65) : .black.opacity(0.16))
        let radius = CGFloat(style.cornerRadius ?? (topic.shapeClass?.contains("rounded") == true ? 8 : 3))
        let nodePath = Path(roundedRect: rect, cornerRadius: max(0, min(radius, rect.height / 2)))

        context.fill(nodePath, with: .color(fill.opacity(topic.isComplete ? 0.68 : 1)))
        if style.borderWidth > 0 {
            context.stroke(
                nodePath,
                with: .color(selected ? accent : borderColor.opacity(topic.isComplete ? 0.55 : 0.9)),
                lineWidth: selected ? 2.2 : max(0.6, min(4, style.borderWidth))
            )
        } else if selected {
            context.stroke(nodePath, with: .color(accent), lineWidth: 2.2)
        }

        if !isRoot && style.borderWidth <= 0 {
            context.fill(
                Path(roundedRect: CGRect(x: rect.minX, y: rect.minY + 5, width: 3, height: rect.height - 10), cornerRadius: 1.5),
                with: .color(accent.opacity(topic.isComplete ? 0.45 : 0.9))
            )
        }

        var markerX = rect.minX + 9
        if !topic.markers.isEmpty {
            let markerY = rect.minY + 4
            for marker in topic.markers.prefix(8) {
                let markerRect = CGRect(x: markerX, y: markerY, width: markerWidth(marker), height: 13)
                let markerColor = markerColor(marker)
                context.fill(Path(roundedRect: markerRect, cornerRadius: 4), with: .color(markerColor.opacity(0.92)))
                context.draw(
                    Text(marker.label)
                        .font(.system(size: 7.5, weight: .bold, design: .rounded))
                        .foregroundColor(.white),
                    at: CGPoint(x: markerRect.midX, y: markerRect.midY - 0.3),
                    anchor: .center
                )
                markerX += markerRect.width + 4
            }
        }

        let textStartX: CGFloat
        if topic.hasTodo {
            let checkboxRect = todoCheckboxRect(for: layout)
            context.fill(Path(roundedRect: checkboxRect, cornerRadius: 3), with: .color(topic.isComplete ? accent : fill))
            context.stroke(Path(roundedRect: checkboxRect, cornerRadius: 3), with: .color(accent), lineWidth: 1)
            if topic.isComplete {
                context.draw(
                    Text("✓").font(.system(size: 9, weight: .bold)).foregroundColor(.white),
                    at: CGPoint(x: checkboxRect.midX, y: checkboxRect.midY - 0.5),
                    anchor: .center
                )
            }
            textStartX = checkboxRect.maxX + 6
        } else {
            textStartX = rect.minX + 11
        }

        let font = topicFont(topic, isRoot: isRoot)
        var text = Text(topic.title)
            .font(font)
            .foregroundColor(textColor.opacity(topic.isComplete ? 0.58 : 1))
            .underline(style.underline)
            .strikethrough(style.strike || topic.isComplete)
        if topic.markers.isEmpty && topic.hasTodo {
            text = text.font(.system(size: max(9.5, min(14, style.fontSize * 0.9)), weight: fontWeight(style.fontWeight, root: isRoot)))
        }
        var textContext = context
        textContext.clip(to: nodePath)
        let textY: CGFloat = topic.markers.isEmpty ? rect.midY : rect.minY + 30
        let resolvedText = context.resolve(text)
        let alignment = style.alignment ?? "left"
        let textX: CGFloat
        switch alignment {
        case "center": textX = rect.midX
        case "right": textX = rect.maxX - 22
        default: textX = textStartX
        }
        context.draw(
            resolvedText,
            at: CGPoint(x: textX, y: textY),
            anchor: alignment == "center" ? .center : alignment == "right" ? .trailing : .leading
        )

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

    private func topicFont(_ topic: MindmapTopic, isRoot: Bool) -> Font {
        let size = max(9.5, min(24, topic.style.fontSize * 0.86))
        let weight = fontWeight(topic.style.fontWeight, root: isRoot)
        let family = topic.style.fontFamily?.trimmingCharacters(in: .whitespacesAndNewlines)
        var font = family?.isEmpty == false
            ? Font.custom(family!, size: size).weight(weight)
            : Font.system(size: size, weight: weight, design: .rounded)
        if topic.style.italic {
            font = font.italic()
        }
        return font
    }

    private func fontWeight(_ value: String?, root: Bool) -> Font.Weight {
        switch value?.lowercased() {
        case "bold", "700", "800", "900": return .bold
        case "600", "semibold": return .semibold
        case "500", "medium": return .medium
        case "300", "light": return .light
        default: return root ? .semibold : .medium
        }
    }

    private func markerWidth(_ marker: MindmapMarkerChip) -> CGFloat {
        marker.label.count <= 2 ? 21 : max(24, CGFloat(marker.label.count) * 7)
    }

    private func markerColor(_ marker: MindmapMarkerChip) -> Color {
        switch marker.id {
        case "flag-red", "priority-1": return Color(red: 0.73, green: 0.16, blue: 0.12)
        case "flag-yellow", "star-yellow", "priority-2": return Color(red: 0.82, green: 0.52, blue: 0.08)
        case "priority-3": return Color(red: 0.46, green: 0.42, blue: 0.18)
        case "task-done": return Color(red: 0.18, green: 0.52, blue: 0.31)
        case "task-pause": return Color(red: 0.43, green: 0.45, blue: 0.49)
        default:
            switch marker.family {
            case "priority": return Color(red: 0.72, green: 0.34, blue: 0.15)
            case "task": return Color(red: 0.23, green: 0.42, blue: 0.56)
            case "flag": return Color(red: 0.72, green: 0.22, blue: 0.18)
            case "star": return Color(red: 0.78, green: 0.56, blue: 0.10)
            case "people": return Color(red: 0.30, green: 0.48, blue: 0.36)
            case "symbol": return Color(red: 0.50, green: 0.31, blue: 0.58)
            default: return Color(red: 0.34, green: 0.36, blue: 0.38)
            }
        }
    }

    private func todoCheckboxRect(for node: MindmapNodeLayout) -> CGRect {
        let y = node.rect.midY - 7
        return CGRect(x: node.rect.minX + 9, y: y, width: 14, height: 14)
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

            let command = event.modifierFlags.contains(.command)
            let option = event.modifierFlags.contains(.option)
            let control = event.modifierFlags.contains(.control)
            let shift = event.modifierFlags.contains(.shift)
            let key = event.charactersIgnoringModifiers?.lowercased()

            if self.model.textInputFocused {
                if self.model.editingID != nil {
                    if event.keyCode == 53 {
                        self.model.cancelInlineEdit()
                        return nil
                    }
                    if event.keyCode == 36 {
                        if shift {
                            self.model.inlineTitle += "\n"
                        } else {
                            self.model.commitInlineEdit()
                        }
                        return nil
                    }
                }
                return event
            }

            if command, key == "z" {
                shift ? self.model.redo() : self.model.undo()
                return nil
            }
            if command, key == "y" {
                self.model.redo()
                return nil
            }
            if command, key == "o" {
                KiteMindmapWindowController.chooseAndOpen()
                return nil
            }
            if command, key == "s" {
                self.model.saveCurrentContent()
                return nil
            }
            if command, shift, key == "n" {
                self.model.status = "备注尚未实现"
                return nil
            }
            if command, key == "n" {
                self.model.status = "新建 XMind 文档尚未实现"
                return nil
            }
            if command, key == "f" {
                self.model.requestSearchFocus()
                return nil
            }
            if command, key == "0" {
                self.model.zoomReset()
                return nil
            }
            if command, key == "+" || (command && key == "=") {
                self.model.zoomIn()
                return nil
            }
            if command, key == "-" {
                self.model.zoomOut()
                return nil
            }
            if command, event.keyCode == 96 {
                self.model.fit()
                return nil
            }
            if command, event.keyCode == 115 {
                self.model.centerRoot()
                return nil
            }
            if command, option, key == "f" {
                self.model.toggleSidebar()
                return nil
            }
            if option, event.keyCode == 126 {
                self.model.moveSelectedUp()
                return nil
            }
            if option, event.keyCode == 125 {
                self.model.moveSelectedDown()
                return nil
            }
            if command, shift, event.keyCode == 36 || control, shift, event.keyCode == 36 {
                self.model.addParent()
                return nil
            }
            if command, key == "c", !option {
                self.model.copySelected()
                return nil
            }
            if command, key == "x" {
                self.model.cutSelected()
                return nil
            }
            if command, shift, key == "v" {
                self.model.pasteTextAsTopic()
                return nil
            }
            if command, key == "v" {
                self.model.pasteCopiedTopic()
                return nil
            }
            if command, option, key == "c" || command, option, key == "v" {
                self.model.status = "复制/粘贴样式尚未实现"
                return nil
            }
            if command, shift, key == "l" {
                self.model.status = "自由主题尚未实现"
                return nil
            }
            if command, key == "l" {
                self.model.status = "联系线尚未实现"
                return nil
            }
            if command, key == "b" {
                self.model.status = "外框尚未实现"
                return nil
            }
            if command, key == "]" {
                self.model.status = "概要尚未实现"
                return nil
            }
            if command, key == "k" {
                self.model.status = "超链接尚未实现"
                return nil
            }
            if event.keyCode == 100 {
                self.model.toggleZenMode()
                return nil
            }

            switch event.keyCode {
            case 48:
                self.model.addChild()
                return nil
            case 36:
                if shift {
                    self.model.addSiblingBefore()
                } else {
                    self.model.addSibling()
                }
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
                if key == "/" {
                    self.model.toggleBranch()
                    return nil
                }
                if key == "*" {
                    self.model.expandAllDescendants()
                    return nil
                }
                return event
            }
        }
    }
}

private struct FlowMarkerRow: View {
    let markers: [MindmapMarkerChip]
    let remove: (String) -> Void

    private let columns = [GridItem(.adaptive(minimum: 68), spacing: 4, alignment: .leading)]

    var body: some View {
        LazyVGrid(columns: columns, alignment: .leading, spacing: 4) {
            ForEach(markers) { marker in
                HStack(spacing: 3) {
                    Text(marker.label)
                        .font(.system(size: 8.5, weight: .semibold, design: .monospaced))
                    Button {
                        remove(marker.id)
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 6.5, weight: .bold))
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 6)
                .frame(height: 20)
                .background(Color.white.opacity(0.08), in: Capsule())
            }
        }
    }
}

private extension MindmapGraphStructureDto {
    var supportedStructure: MindmapGraphStructureDto? {
        self == .unknown ? nil : self
    }
}

private extension MindmapConnectorKindDto {
    var supportedConnector: MindmapConnectorKindDto? {
        self == .unknown ? nil : self
    }
}

private extension Color {
    init?(hex: String?) {
        guard let hex else { return nil }
        let value = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        guard value.hasPrefix("#") else { return nil }
        let digits = String(value.dropFirst())
        let number: UInt64
        switch digits.count {
        case 6:
            guard let parsed = UInt64(digits, radix: 16) else { return nil }
            number = parsed << 8 | 0xFF
        case 8:
            guard let parsed = UInt64(digits, radix: 16) else { return nil }
            number = parsed
        default:
            return nil
        }
        self.init(
            red: Double((number >> 24) & 0xFF) / 255,
            green: Double((number >> 16) & 0xFF) / 255,
            blue: Double((number >> 8) & 0xFF) / 255,
            opacity: Double(number & 0xFF) / 255
        )
    }

    var hexString: String? {
        guard let color = NSColor(self).usingColorSpace(.sRGB) else { return nil }
        return String(
            format: "#%02X%02X%02X",
            Int(round(color.redComponent * 255)),
            Int(round(color.greenComponent * 255)),
            Int(round(color.blueComponent * 255))
        )
    }
}
