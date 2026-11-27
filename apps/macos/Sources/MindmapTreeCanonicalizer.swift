import CoreFoundation
import CryptoKit
import Foundation

struct MindmapMarkerChip: Identifiable, Equatable {
    let id: String
    let family: String
    let label: String
}

struct MindmapTopicVisualStyle: Equatable {
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
    var lineHeight: String?
    var letterSpacing: String?

    var accentHex: String? { borderColorHex ?? fillColorHex ?? textColorHex }
}

struct MindmapTaskSemantics: Equatable {
    var taskPresentation: String?
    var todoStatus: String?
    var todoProgress: Double?
    var taskProgress: Double?
    var taskStartDate: String?
    var taskDueDate: String?
    var legacyStatus: String?
    var legacyProgress: Double?
    var effectiveStatus: String?
    var effectiveProgress: Double?
    var source: String?
    var hasConflict: Bool
}

struct MindmapTopic: Identifiable {
    let id: String
    var title: String
    let parentID: String?
    let depth: Int
    let childIDs: [String]
    let style: MindmapTopicVisualStyle
    let shapeClass: String?
    let markers: [MindmapMarkerChip]
    let opaqueMarkerCount: Int
    let isComplete: Bool
    let task: MindmapTaskSemantics?

    var hasChildren: Bool { !childIDs.isEmpty }
    var hasTodo: Bool { markers.contains { $0.family == "task" } }
    var headerMarkers: [MindmapMarkerChip] { markers }
}

struct MindmapCanonicalConnector: Equatable {
    var kind: String?
    var cornerRadius: String?
    var width: String?
    var color: String?
    var pattern: String?
}

struct MindmapTreeParseResult {
    let topics: [String: MindmapTopic]
    let rootID: String?
    let structure: String
    let structureClass: String?
    let connector: MindmapCanonicalConnector

    func canonicalObject() -> [String: Any] {
        var canonicalTopics: [Any] = []
        if let rootID {
            appendCanonicalTopic(rootID, to: &canonicalTopics)
        }
        return [
            "connector": canonicalConnectorObject(connector),
            "root_id": optionalJSONValue(rootID),
            "schema_version": 1,
            "structure": structure,
            "structure_class": optionalJSONValue(structureClass),
            "topics": canonicalTopics,
        ]
    }

    func canonicalJSONData() throws -> Data {
        try MindmapCanonicalJSONWriter.compactData(canonicalObject())
    }

    func canonicalJSONString() throws -> String {
        guard let value = String(data: try canonicalJSONData(), encoding: .utf8) else {
            throw MindmapTreeCanonicalizationError.invalidJSON("canonical JSON is not UTF-8")
        }
        return value
    }

    func canonicalSHA256() throws -> String {
        SHA256.hash(data: try canonicalJSONData()).map { String(format: "%02x", $0) }.joined()
    }

    private func appendCanonicalTopic(_ id: String, to output: inout [Any]) {
        guard let topic = topics[id] else { return }
        output.append(canonicalTopicObject(topic))
        for childID in topic.childIDs {
            appendCanonicalTopic(childID, to: &output)
        }
    }
}

enum MindmapTreeCanonicalizationError: Error, Equatable, LocalizedError {
    case invalidJSON(String)
    case invalidRootShape
    case missingRootTopic
    case invalidTopic(String)
    case missingTopicID(String)
    case duplicateTopicID(String)

    var code: String {
        switch self {
        case .invalidJSON: return "invalid_json"
        case .invalidRootShape: return "invalid_root_shape"
        case .missingRootTopic: return "missing_root_topic"
        case .invalidTopic: return "invalid_topic"
        case .missingTopicID: return "missing_topic_id"
        case .duplicateTopicID: return "duplicate_topic_id"
        }
    }

    var errorDescription: String? {
        switch self {
        case .invalidJSON(let message): return "invalid_json: \(message)"
        case .invalidRootShape: return "invalid_root_shape: content.json 根节点必须是数组"
        case .missingRootTopic: return "missing_root_topic: content.json 不包含 rootTopic"
        case .invalidTopic(let path): return "invalid_topic: topic 必须是对象（\(path)）"
        case .missingTopicID(let path): return "missing_topic_id: topic 缺少 id（\(path)）"
        case .duplicateTopicID(let id): return "duplicate_topic_id: topic id 重复（\(id)）"
        }
    }
}

enum MindmapTreeCanonicalizer {
    static func parse(contentJSON: String) throws -> MindmapTreeParseResult {
        let data = Data(contentJSON.utf8)
        let rootObject: Any
        do {
            rootObject = try JSONSerialization.jsonObject(with: data)
        } catch {
            throw MindmapTreeCanonicalizationError.invalidJSON(error.localizedDescription)
        }
        guard let sheets = rootObject as? [[String: Any]] else {
            throw MindmapTreeCanonicalizationError.invalidRootShape
        }
        guard let sheet = sheets.first,
              let rootTopic = sheet["rootTopic"] as? [String: Any] else {
            throw MindmapTreeCanonicalizationError.missingRootTopic
        }

        var topics: [String: MindmapTopic] = [:]
        var seenIDs = Set<String>()
        var rootID: String?

        func walk(_ object: [String: Any], parentID: String?, depth: Int, path: String) throws {
            guard let id = object["id"] as? String, !id.isEmpty else {
                throw MindmapTreeCanonicalizationError.missingTopicID(path)
            }
            guard seenIDs.insert(id).inserted else {
                throw MindmapTreeCanonicalizationError.duplicateTopicID(id)
            }
            if depth == 0 { rootID = id }

            let titleValue = (object["title"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            let title = titleValue.flatMap { $0.isEmpty ? nil : $0 } ?? "未命名主题"
            let properties = (object["style"] as? [String: Any])?["properties"] as? [String: Any]
            let style = parseVisualStyle(properties)
            let shapeClass = (object["shape-class"] as? String) ?? (properties?["shape-class"] as? String)
            let markerObjects = object["markers"] as? [Any] ?? []
            let markers = markerObjects.compactMap(parseMarker)
            let opaqueMarkerCount = markerObjects.count - markers.count
            let task = parseTask(object, markers: markers)
            let isComplete = markers.contains { $0.id == "task-done" }
            let childObjects = ((object["children"] as? [String: Any])?["attached"] as? [Any]) ?? []
            var childObjectsByID: [(String, [String: Any])] = []
            for (index, value) in childObjects.enumerated() {
                guard let child = value as? [String: Any] else {
                    throw MindmapTreeCanonicalizationError.invalidTopic("\(path).children.attached[\(index)]")
                }
                guard let childID = child["id"] as? String, !childID.isEmpty else {
                    throw MindmapTreeCanonicalizationError.missingTopicID("\(path).children.attached[\(index)]")
                }
                childObjectsByID.append((childID, child))
            }

            topics[id] = MindmapTopic(
                id: id,
                title: title,
                parentID: parentID,
                depth: depth,
                childIDs: childObjectsByID.map { $0.0 },
                style: style,
                shapeClass: shapeClass,
                markers: markers,
                opaqueMarkerCount: opaqueMarkerCount,
                isComplete: isComplete,
                task: task
            )
            for (childID, child) in childObjectsByID {
                try walk(child, parentID: id, depth: depth + 1, path: "\(path).children.attached[\(childID)]")
            }
        }

        try walk(rootTopic, parentID: nil, depth: 0, path: "rootTopic")
        let structureClass = rootTopic["structureClass"] as? String
        return MindmapTreeParseResult(
            topics: topics,
            rootID: rootID,
            structure: structureName(structureClass),
            structureClass: structureClass,
            connector: parseConnector(sheet: sheet, rootTopic: rootTopic)
        )
    }
}

private func parseVisualStyle(_ properties: [String: Any]?) -> MindmapTopicVisualStyle {
    var style = MindmapTopicVisualStyle()
    style.fontFamily = properties?["fo:font-family"] as? String
    style.fontSize = parseMeasurement(properties?["fo:font-size"]) ?? 12.5
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
    style.lineHeight = properties?["fo:line-height"] as? String
    style.letterSpacing = properties?["fo:letter-spacing"] as? String
    return style
}

private func parseMeasurement(_ value: Any?) -> Double? {
    if let number = numberValue(value) { return number }
    guard let string = value as? String else { return nil }
    let numeric = string.drop(while: { !$0.isNumber && $0 != "." })
    return Double(numeric.trimmingCharacters(in: CharacterSet(charactersIn: "ptpx%em ")))
}

private func numberValue(_ value: Any?) -> Double? {
    guard let number = value as? NSNumber else { return nil }
    if CFGetTypeID(number as CFTypeRef) == CFBooleanGetTypeID() { return nil }
    return number.doubleValue
}

private func progressValue(_ value: Any?) -> Double? {
    if let number = numberValue(value) { return number }
    guard let string = value as? String else { return nil }
    let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.hasSuffix("%") {
        return Double(String(trimmed.dropLast())).map { $0 / 100.0 }
    }
    return Double(trimmed)
}

private func parseMarker(_ value: Any) -> MindmapMarkerChip? {
    guard let object = value as? [String: Any],
          let markerID = object["markerId"] as? String,
          !markerID.isEmpty else {
        return nil
    }
    return MindmapMarkerChip(
        id: markerID,
        family: markerFamily(markerID),
        label: markerLabel(markerID)
    )
}

private func markerFamily(_ markerID: String) -> String {
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

private func markerLabel(_ markerID: String) -> String {
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

private func parseTask(_ object: [String: Any], markers: [MindmapMarkerChip]) -> MindmapTaskSemantics? {
    let taskPresentation = nonEmptyString(object["taskPresentation"])
    let todoStatus = nonEmptyString(object["todoStatus"])
    let todoProgress = progressValue(object["todoProgress"]).map(normalizeProgress)
    let taskProgress = progressValue(object["taskProgress"]).map(normalizeProgress)
    let taskStartDate = nonEmptyString(object["taskStartDate"])
    let taskDueDate = nonEmptyString(object["taskDueDate"])
    let legacyMarker = markers.first { $0.id.hasPrefix("task-") }
    let legacyState = legacyMarker.map { taskMarkerState($0.id) }
    let legacyStatus = legacyState?.status
    let legacyProgress = legacyState?.progress
    let modernPresent = taskPresentation != nil || todoStatus != nil || todoProgress != nil
        || taskProgress != nil || taskStartDate != nil || taskDueDate != nil
    let modernProgress = todoProgress ?? taskProgress
    let modernStatus = normalizeTaskStatus(todoStatus)
        ?? modernProgress.flatMap(statusFromProgress)
        ?? (modernPresent ? "not-started" : nil)

    guard modernPresent || legacyStatus != nil || legacyProgress != nil else { return nil }
    let effectiveStatus = modernStatus ?? legacyStatus
    let effectiveProgress = modernProgress ?? legacyProgress
    let statusConflict: Bool
    if let modernStatus, let legacyStatus {
        statusConflict = modernStatus != legacyStatus
    } else {
        statusConflict = false
    }
    let progressConflict: Bool
    if let modernProgress, let legacyProgress {
        progressConflict = abs(modernProgress - legacyProgress) > 0.000_001
    } else {
        progressConflict = false
    }
    let source: String?
    switch (modernPresent, legacyStatus != nil || legacyProgress != nil) {
    case (true, true): source = "modern+legacy"
    case (true, false): source = "modern"
    case (false, true): source = "legacy"
    case (false, false): source = nil
    }

    return MindmapTaskSemantics(
        taskPresentation: taskPresentation,
        todoStatus: todoStatus,
        todoProgress: todoProgress,
        taskProgress: taskProgress,
        taskStartDate: taskStartDate,
        taskDueDate: taskDueDate,
        legacyStatus: legacyStatus,
        legacyProgress: legacyProgress,
        effectiveStatus: effectiveStatus,
        effectiveProgress: effectiveProgress,
        source: source,
        hasConflict: statusConflict || progressConflict
    )
}

private func nonEmptyString(_ value: Any?) -> String? {
    guard let value = value as? String else { return nil }
    let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? nil : trimmed
}

private func optionalJSONValue(_ value: String?) -> Any {
    value ?? NSNull()
}

private func normalizeProgress(_ value: Double) -> Double {
    let normalized = value > 1.0 && value <= 100.0 ? value / 100.0 : value
    return min(max(normalized, 0.0), 1.0)
}

private func normalizeTaskStatus(_ value: String?) -> String? {
    guard let value else { return nil }
    switch value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
    case "todo", "not-started", "not_started", "notstarted", "open": return "not-started"
    case "in-progress", "in_progress", "inprogress", "progress", "started": return "in-progress"
    case "paused", "blocked", "on-hold", "on_hold": return "paused"
    case "done", "completed", "complete": return "done"
    default: return nil
    }
}

private func statusFromProgress(_ value: Double) -> String? {
    if value >= 1.0 { return "done" }
    if value > 0.0 { return "in-progress" }
    return "not-started"
}

private func taskMarkerState(_ markerID: String) -> (status: String, progress: Double?) {
    let progress: Double?
    switch markerID {
    case "task-start": progress = 0.0
    case "task-oct": progress = 1.0 / 8.0
    case "task-quarter": progress = 2.0 / 8.0
    case "task-3oct": progress = 3.0 / 8.0
    case "task-half": progress = 4.0 / 8.0
    case "task-5oct": progress = 5.0 / 8.0
    case "task-3quar": progress = 6.0 / 8.0
    case "task-7oct": progress = 7.0 / 8.0
    case "task-done": progress = 1.0
    default: progress = nil
    }
    let status: String
    if markerID == "task-pause" {
        status = "paused"
    } else if let progress, progress >= 1.0 {
        status = "done"
    } else if let progress, progress > 0.0 {
        status = "in-progress"
    } else if progress != nil {
        status = "not-started"
    } else {
        status = "in-progress"
    }
    return (status, progress)
}

private func structureName(_ value: String?) -> String {
    guard let value else { return "unknown" }
    switch value {
    case "org.xmind.ui.logic.right", "org.xmind.ui.logic-chart.right": return "logic-right"
    case "org.xmind.ui.logic.left", "org.xmind.ui.logic-chart.left": return "logic-left"
    case "org.xmind.ui.org-chart.down": return "org-chart"
    case "org.xmind.ui.tree.right": return "tree"
    case value where value.hasPrefix("org.xmind.ui.map"): return "mind-map"
    default: return "unknown"
    }
}

private func parseConnector(sheet: [String: Any], rootTopic: [String: Any]) -> MindmapCanonicalConnector {
    let rootProperties = (rootTopic["style"] as? [String: Any])?["properties"] as? [String: Any]
    let theme = sheet["theme"] as? [String: Any]
    let centralTopic = theme?["centralTopic"] as? [String: Any]
    let themeProperties = centralTopic?["properties"] as? [String: Any]
    let properties = rootProperties ?? themeProperties
    guard let properties else { return MindmapCanonicalConnector() }
    return MindmapCanonicalConnector(
        kind: (properties["line-class"] as? String).map(connectorKindName),
        cornerRadius: propertyString(properties, key: "line-corner"),
        width: propertyString(properties, key: "line-width"),
        color: propertyString(properties, key: "line-color"),
        pattern: propertyString(properties, key: "line-pattern")
    )
}

private func connectorKindName(_ value: String) -> String {
    switch value {
    case "org.xmind.branchConnection.curve": return "curved"
    case "org.xmind.branchConnection.straight": return "straight"
    case "org.xmind.branchConnection.elbow": return "elbow"
    case "org.xmind.branchConnection.roundedElbow": return "rounded-elbow"
    default: return value
    }
}

private func propertyString(_ properties: [String: Any], key: String) -> String? {
    if let value = properties[key] as? String { return value }
    if let value = numberValue(properties[key]) {
        return value == value.rounded() ? String(Int64(value)) : String(value)
    }
    return nil
}

private func canonicalTopicObject(_ topic: MindmapTopic) -> [String: Any] {
    [
        "children": topic.childIDs,
        "depth": topic.depth,
        "id": topic.id,
        "markers": topic.markers.map { marker in
            [
                "family": marker.family,
                "id": marker.id,
                "label": marker.label,
                "opaque": marker.family == "other",
            ] as [String: Any]
        },
        "opaque_marker_count": topic.opaqueMarkerCount,
        "parent_id": optionalJSONValue(topic.parentID),
        "shape_class": optionalJSONValue(topic.shapeClass),
        "style": canonicalStyleObject(topic.style),
        "task": topic.task.map { canonicalTaskObject($0) as Any } ?? NSNull(),
        "title": topic.title,
    ]
}

private func canonicalStyleObject(_ style: MindmapTopicVisualStyle) -> [String: Any] {
    [
        "alignment": optionalJSONValue(style.alignment),
        "border_color": optionalJSONValue(style.borderColorHex),
        "border_pattern": optionalJSONValue(style.borderPattern),
        "border_width": style.borderWidth,
        "corner_radius": style.cornerRadius.map { $0 as Any } ?? NSNull(),
        "fill_color": optionalJSONValue(style.fillColorHex),
        "font_family": optionalJSONValue(style.fontFamily),
        "font_size": style.fontSize,
        "font_weight": optionalJSONValue(style.fontWeight),
        "italic": style.italic,
        "strike": style.strike,
        "text_color": optionalJSONValue(style.textColorHex),
        "underline": style.underline,
    ]
}

private func canonicalConnectorObject(_ connector: MindmapCanonicalConnector) -> [String: Any] {
    [
        "color": optionalJSONValue(connector.color),
        "corner_radius": optionalJSONValue(connector.cornerRadius),
        "kind": optionalJSONValue(connector.kind),
        "pattern": optionalJSONValue(connector.pattern),
        "width": optionalJSONValue(connector.width),
    ]
}

private func canonicalTaskObject(_ task: MindmapTaskSemantics) -> [String: Any] {
    [
        "effectiveProgress": task.effectiveProgress.map { $0 as Any } ?? NSNull(),
        "effectiveStatus": optionalJSONValue(task.effectiveStatus),
        "hasConflict": task.hasConflict,
        "legacyProgress": task.legacyProgress.map { $0 as Any } ?? NSNull(),
        "legacyStatus": optionalJSONValue(task.legacyStatus),
        "source": optionalJSONValue(task.source),
        "taskDueDate": optionalJSONValue(task.taskDueDate),
        "taskPresentation": optionalJSONValue(task.taskPresentation),
        "taskProgress": task.taskProgress.map { $0 as Any } ?? NSNull(),
        "taskStartDate": optionalJSONValue(task.taskStartDate),
        "todoProgress": task.todoProgress.map { $0 as Any } ?? NSNull(),
        "todoStatus": optionalJSONValue(task.todoStatus),
    ]
}

enum MindmapCanonicalJSONWriter {
    static func compactData(_ value: Any) throws -> Data {
        var output = ""
        try append(value, to: &output)
        return Data(output.utf8)
    }

    private static func append(_ value: Any, to output: inout String) throws {
        switch value {
        case is NSNull:
            output += "null"
        case let value as String:
            appendString(value, to: &output)
        case let value as NSNumber:
            if CFGetTypeID(value as CFTypeRef) == CFBooleanGetTypeID() {
                output += value.boolValue ? "true" : "false"
            } else {
                appendNumber(value.doubleValue, to: &output)
            }
        case let value as [Any]:
            output += "["
            for (index, element) in value.enumerated() {
                if index > 0 { output += "," }
                try append(element, to: &output)
            }
            output += "]"
        case let value as [String: Any]:
            output += "{"
            for (index, key) in value.keys.sorted().enumerated() {
                if index > 0 { output += "," }
                appendString(key, to: &output)
                output += ":"
                try append(value[key]!, to: &output)
            }
            output += "}"
        default:
            throw MindmapTreeCanonicalizationError.invalidJSON("unsupported canonical JSON value \(type(of: value))")
        }
    }

    private static func appendNumber(_ value: Double, to output: inout String) {
        if !value.isFinite {
            output += "null"
        } else if value == value.rounded() && abs(value) <= Double(Int64.max) {
            output += String(Int64(value))
        } else {
            output += String(value)
        }
    }

    private static func appendString(_ value: String, to output: inout String) {
        output += "\""
        for scalar in value.unicodeScalars {
            switch scalar.value {
            case 0x22: output += "\\\""
            case 0x5C: output += "\\\\"
            case 0x08: output += "\\b"
            case 0x0C: output += "\\f"
            case 0x0A: output += "\\n"
            case 0x0D: output += "\\r"
            case 0x09: output += "\\t"
            case 0x00...0x1F: output += String(format: "\\u%04x", scalar.value)
            default: output += String(scalar)
            }
        }
        output += "\""
    }
}
