import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

private enum ImportPresetChoice: String, CaseIterable, Identifiable {
    case structureFirst
    case expandedContent
    case strictOutline
    case sentenceSplit

    var id: String { rawValue }

    var title: String {
        switch self {
        case .structureFirst: "结构优先"
        case .expandedContent: "正文展开"
        case .strictOutline: "严格大纲"
        case .sentenceSplit: "按句拆散"
        }
    }

    var detail: String {
        switch self {
        case .structureFirst: "保留标题和列表，拆分较长正文。"
        case .expandedContent: "保留结构，同时尽量展开正文层级。"
        case .strictOutline: "只按明确结构生成，正文进入备注。"
        case .sentenceSplit: "正文按句子和分号拆为同层主题。"
        }
    }

    var dto: MindmapImportPresetDto {
        switch self {
        case .structureFirst: .structureFirst
        case .expandedContent: .expandedContent
        case .strictOutline: .strictOutline
        case .sentenceSplit: .sentenceSplit
        }
    }
}

private enum ImportTableChoice: String, CaseIterable, Identifiable {
    case note
    case rowsAsTopics

    var id: String { rawValue }

    var title: String {
        switch self {
        case .note: "保留为备注"
        case .rowsAsTopics: "每行生成主题"
        }
    }

    var dto: MindmapImportTableStrategyDto {
        switch self {
        case .note: .note
        case .rowsAsTopics: .rowsAsTopics
        }
    }
}

private enum ImportCodeChoice: String, CaseIterable, Identifiable {
    case topicNote
    case parentNote

    var id: String { rawValue }

    var title: String {
        switch self {
        case .topicNote: "独立代码主题"
        case .parentNote: "折叠到父主题备注"
        }
    }

    var dto: MindmapImportCodeStrategyDto {
        switch self {
        case .topicNote: .topicNote
        case .parentNote: .parentNote
        }
    }
}

private enum ImportImageChoice: String, CaseIterable, Identifiable {
    case topicLink
    case parentNote

    var id: String { rawValue }

    var title: String {
        switch self {
        case .topicLink: "独立图片主题"
        case .parentNote: "折叠链接到父主题"
        }
    }

    var dto: MindmapImportImageStrategyDto {
        switch self {
        case .topicLink: .topicLink
        case .parentNote: .parentNote
        }
    }
}

private struct ImportPreviewTree: Decodable {
    let id: String
    let title: String
    let kind: String
    let noteCount: Int
    let childCount: Int?
    let children: [ImportPreviewTree]
}

private struct ImportPreviewRow: Identifiable {
    let id: String
    let title: String
    let kind: String
    let depth: Int
    let noteCount: Int
    let childCount: Int
    let isTruncated: Bool
}

@MainActor
private final class KiteMindmapImportModel: ObservableObject {
    let source: MindmapImportSourceDto
    let formats: [MindmapImportFormatDto]

    @Published var outputPath: String
    @Published var preset: ImportPresetChoice = .structureFirst
    @Published var splitContent = true
    @Published var preserveOriginalParagraphs = true
    @Published var mergeShortFragments = true
    @Published var minFragmentChars: UInt32 = 12
    @Published var tableChoice: ImportTableChoice = .note
    @Published var codeChoice: ImportCodeChoice = .topicNote
    @Published var imageChoice: ImportImageChoice = .topicLink
    @Published private(set) var preview: MindmapConversionPreviewDto?
    @Published private(set) var previewRows: [ImportPreviewRow] = []
    @Published private(set) var isPreviewing = false
    @Published private(set) var isConverting = false
    @Published var status = "正在分析源文档…"
    @Published var errorMessage: String?

    private var previewTask: Task<Void, Never>?
    private var overwriteApprovedPath: String?

    init(source: MindmapImportSourceDto) {
        self.source = source
        self.outputPath = source.defaultOutputPath
        self.formats = supportedMindmapImportFormats()
        requestPreview(debounce: false)
    }

    deinit {
        previewTask?.cancel()
    }

    var canConvert: Bool {
        preview != nil && !isPreviewing && !isConverting && !outputPath.isEmpty
    }

    var supportedFormatSummary: String {
        formats
            .filter(\.supported)
            .map(\.displayName)
            .joined(separator: " · ")
    }

    var unsupportedFormatSummary: String? {
        let values = formats.filter { !$0.supported }.map(\.displayName)
        return values.isEmpty ? nil : values.joined(separator: " · ")
    }

    func setPreset(_ choice: ImportPresetChoice) {
        preset = choice
        switch choice {
        case .structureFirst:
            splitContent = true
            mergeShortFragments = true
        case .expandedContent:
            splitContent = true
            mergeShortFragments = false
        case .strictOutline:
            splitContent = false
            mergeShortFragments = true
        case .sentenceSplit:
            splitContent = true
            mergeShortFragments = false
        }
        requestPreview()
    }

    func requestPreview(debounce: Bool = true) {
        previewTask?.cancel()
        let options = makeOptions()
        let sourcePath = source.path
        isPreviewing = true
        errorMessage = nil
        status = "正在生成预览…"

        previewTask = Task { [weak self] in
            if debounce {
                try? await Task.sleep(for: .milliseconds(180))
            }
            guard !Task.isCancelled else { return }
            do {
                let result = try await Task.detached(priority: .userInitiated) {
                    try previewMindmapConversion(sourcePath: sourcePath, options: options)
                }.value
                guard !Task.isCancelled, let self else { return }
                self.preview = result
                self.previewRows = Self.flatten(
                    Self.decodeTree(result.sampleTreeJson),
                    depth: 0
                )
                self.isPreviewing = false
                self.status = "预览已就绪 · \(result.nodeCount) 个主题"
            } catch {
                guard !Task.isCancelled, let self else { return }
                self.preview = nil
                self.previewRows = []
                self.isPreviewing = false
                self.errorMessage = error.localizedDescription
                self.status = "无法生成预览"
            }
        }
    }

    func chooseOutputPath() {
        let panel = NSSavePanel()
        panel.title = "保存转换后的 Mindmap"
        panel.prompt = "选择"
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        panel.nameFieldStringValue = URL(fileURLWithPath: outputPath).lastPathComponent
        panel.directoryURL = URL(fileURLWithPath: outputPath).deletingLastPathComponent()
        if let type = UTType(filenameExtension: "xmind") {
            panel.allowedContentTypes = [type]
        }
        guard panel.runModal() == .OK, var url = panel.url else { return }
        if url.pathExtension.lowercased() != "xmind" {
            url = url.appendingPathExtension("xmind")
        }
        outputPath = url.path
        overwriteApprovedPath = url.path
        requestPreview(debounce: false)
    }

    func convert() {
        guard let preview, canConvert else { return }
        let outputURL = URL(fileURLWithPath: outputPath)
        var overwrite = overwriteApprovedPath == outputPath
        if FileManager.default.fileExists(atPath: outputPath) && !overwrite {
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = "替换已有 Mindmap？"
            alert.informativeText = "\(outputURL.lastPathComponent) 已存在。源文档不会被修改。"
            alert.addButton(withTitle: "替换")
            alert.addButton(withTitle: "取消")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
            overwrite = true
            overwriteApprovedPath = outputPath
        }

        let options = makeOptions()
        let sourcePath = source.path
        let expectedSha = preview.sourceSha256
        isConverting = true
        errorMessage = nil
        status = "正在生成 XMind…"
        Task { [weak self] in
            do {
                let result = try await Task.detached(priority: .userInitiated) {
                    try convertMindmapFile(
                        sourcePath: sourcePath,
                        expectedSourceSha256: expectedSha,
                        outputPath: outputURL.path,
                        overwriteExistingOutput: overwrite,
                        options: options
                    )
                }.value
                guard let self else { return }
                self.isConverting = false
                if !result.sourceUnchanged {
                    self.errorMessage = "转换期间源文件发生了变化；生成结果仍保留，但请检查内容。"
                }
                KiteMindmapWindowController.open(path: result.outputPath)
                KiteMindmapImportWindowController.close(sourcePath: self.source.path)
            } catch {
                guard let self else { return }
                self.isConverting = false
                self.errorMessage = error.localizedDescription
                self.status = "转换失败"
            }
        }
    }

    private func makeOptions() -> MindmapConversionOptionsDto {
        MindmapConversionOptionsDto(
            preset: preset.dto,
            splitContent: splitContent,
            preserveOriginalParagraphs: preserveOriginalParagraphs,
            mergeShortFragments: mergeShortFragments,
            minFragmentChars: minFragmentChars,
            maxTitleChars: 120,
            maxNodes: 10_000,
            maxDepth: 8,
            includeSourcePath: false,
            tableStrategy: tableChoice.dto,
            codeStrategy: codeChoice.dto,
            imageStrategy: imageChoice.dto
        )
    }

    private static func decodeTree(_ json: String) -> ImportPreviewTree? {
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(ImportPreviewTree.self, from: data)
    }

    private static func flatten(_ node: ImportPreviewTree?, depth: Int) -> [ImportPreviewRow] {
        guard let node else { return [] }
        let row = ImportPreviewRow(
            id: node.id,
            title: node.title,
            kind: node.kind,
            depth: depth,
            noteCount: node.noteCount,
            childCount: node.childCount ?? node.children.count,
            isTruncated: (node.childCount ?? 0) > node.children.count
        )
        return [row] + node.children.flatMap { flatten($0, depth: depth + 1) }
    }
}

@MainActor
final class KiteMindmapImportWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: KiteMindmapImportWindowController] = [:]
    private let model: KiteMindmapImportModel

    init(source: MindmapImportSourceDto) {
        self.model = KiteMindmapImportModel(source: source)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1_080, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "转换为 Mindmap"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isMovableByWindowBackground = true
        window.minSize = NSSize(width: 940, height: 680)
        window.isReleasedWhenClosed = false
        window.contentViewController = NSHostingController(
            rootView: KiteMindmapImportView(model: model)
        )
        window.setFrameAutosaveName("KiteMindmapImportWindow")
        super.init(window: window)
        window.delegate = self
        window.center()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func chooseSource() {
        let panel = NSOpenPanel()
        panel.title = "转换为 Mindmap"
        panel.prompt = "转换"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        let extensions = Set(
            supportedMindmapImportFormats()
                .filter(\.supported)
                .flatMap(\.extensions)
        )
        let types = extensions.compactMap { UTType(filenameExtension: $0) }
        if !types.isEmpty {
            panel.allowedContentTypes = types
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        open(sourcePath: url.path)
    }

    static func open(sourcePath: String) {
        let normalized = URL(fileURLWithPath: sourcePath).standardizedFileURL.path
        if let existing = controllers[normalized] {
            existing.showWindow(nil)
            existing.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        guard let source = detectMindmapImportSource(path: normalized) else {
            presentError(
                title: "无法转换此文件",
                message: "Kite 未识别该文件格式。支持 Markdown、TXT、DOCX、HTML 和 OPML。"
            )
            return
        }
        guard source.format.supported else {
            presentError(
                title: "格式尚未支持",
                message: source.format.supportNote.isEmpty
                    ? "\(source.format.displayName) 转换尚未接入。"
                    : source.format.supportNote
            )
            return
        }
        let controller = KiteMindmapImportWindowController(source: source)
        controllers[normalized] = controller
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    static func close(sourcePath: String) {
        let normalized = URL(fileURLWithPath: sourcePath).standardizedFileURL.path
        guard let controller = controllers.removeValue(forKey: normalized) else { return }
        controller.close()
    }

    func windowWillClose(_ notification: Notification) {
        let normalized = URL(fileURLWithPath: model.source.path).standardizedFileURL.path
        Self.controllers.removeValue(forKey: normalized)
    }

    private static func presentError(title: String, message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        alert.runModal()
    }
}

private struct KiteMindmapImportView: View {
    @ObservedObject var model: KiteMindmapImportModel

    var body: some View {
        ZStack {
            KiteGlassBackdrop()
            VStack(spacing: 0) {
                header
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(height: 0.5)
                HSplitView {
                    optionsPane
                        .frame(minWidth: 330, idealWidth: 355, maxWidth: 390)
                    previewPane
                        .frame(minWidth: 560, maxWidth: .infinity, maxHeight: .infinity)
                }
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(height: 0.5)
                footer
            }
        }
        .frame(minWidth: 940, minHeight: 680)
    }

    private var header: some View {
        HStack(spacing: 14) {
            ZStack {
                RoundedRectangle(cornerRadius: 11, style: .continuous)
                    .fill(Color(red: 0.72, green: 0.34, blue: 0.15).opacity(0.17))
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 20, weight: .semibold))
                    .foregroundStyle(Color(red: 0.78, green: 0.39, blue: 0.18))
            }
            .frame(width: 44, height: 44)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text(model.source.fileName)
                        .font(.system(size: 16, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                    Text(model.source.format.displayName.uppercased())
                        .font(.system(size: 8.5, weight: .bold, design: .monospaced))
                        .tracking(0.8)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 3)
                        .background(Color.primary.opacity(0.08), in: Capsule())
                }
                Text(model.source.path)
                    .font(.system(size: 9.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer()

            VStack(alignment: .trailing, spacing: 3) {
                Text("DOCUMENT → MINDMAP")
                    .font(.system(size: 8.5, weight: .bold, design: .monospaced))
                    .tracking(1.1)
                    .foregroundStyle(.tertiary)
                Text("源文件只读 · 输出新 .xmind")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
    }

    private var optionsPane: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                sectionLabel("转换规则", detail: "Rust Core 统一执行")
                optionCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("预设")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.secondary)
                        Picker(
                            "预设",
                            selection: Binding(
                                get: { model.preset },
                                set: { model.setPreset($0) }
                            )
                        ) {
                            ForEach(ImportPresetChoice.allCases) { choice in
                                Text(choice.title).tag(choice)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                        Text(model.preset.detail)
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }

                optionCard {
                    VStack(alignment: .leading, spacing: 11) {
                        toggleRow(
                            "正文拆分",
                            detail: "按中英文句号、问号、感叹号和分号拆句",
                            isOn: $model.splitContent
                        )
                        Divider().overlay(Color.primary.opacity(0.06))
                        toggleRow(
                            "保留原始段落",
                            detail: "完整正文同时保留在主题备注中",
                            isOn: $model.preserveOriginalParagraphs
                        )
                        Divider().overlay(Color.primary.opacity(0.06))
                        toggleRow(
                            "合并过短片段",
                            detail: "避免“可以。”这类碎片变成单独主题",
                            isOn: $model.mergeShortFragments
                        )
                        HStack {
                            Text("最短片段")
                                .font(.system(size: 10.5))
                            Spacer()
                            Stepper(
                                "\(model.minFragmentChars)",
                                value: Binding(
                                    get: { Int(model.minFragmentChars) },
                                    set: { model.minFragmentChars = UInt32(max(1, $0)) }
                                ),
                                in: 1...60
                            )
                            .font(.system(size: 10.5, design: .monospaced))
                        }
                        .disabled(!model.mergeShortFragments)
                        .opacity(model.mergeShortFragments ? 1 : 0.45)
                    }
                }
                .onChange(of: model.splitContent) { _ in model.requestPreview() }
                .onChange(of: model.preserveOriginalParagraphs) { _ in model.requestPreview() }
                .onChange(of: model.mergeShortFragments) { _ in model.requestPreview() }
                .onChange(of: model.minFragmentChars) { _ in model.requestPreview() }

                sectionLabel("复杂内容", detail: "可逆层级的保守降级")
                optionCard {
                    VStack(alignment: .leading, spacing: 12) {
                        strategyPicker(
                            "表格",
                            selection: Binding(
                                get: { model.tableChoice },
                                set: {
                                    model.tableChoice = $0
                                    model.requestPreview()
                                }
                            )
                        ) {
                            ForEach(ImportTableChoice.allCases) { choice in
                                Text(choice.title).tag(choice)
                            }
                        }
                        Divider().overlay(Color.primary.opacity(0.06))
                        strategyPicker(
                            "代码块",
                            selection: Binding(
                                get: { model.codeChoice },
                                set: {
                                    model.codeChoice = $0
                                    model.requestPreview()
                                }
                            )
                        ) {
                            ForEach(ImportCodeChoice.allCases) { choice in
                                Text(choice.title).tag(choice)
                            }
                        }
                        Divider().overlay(Color.primary.opacity(0.06))
                        strategyPicker(
                            "图片",
                            selection: Binding(
                                get: { model.imageChoice },
                                set: {
                                    model.imageChoice = $0
                                    model.requestPreview()
                                }
                            )
                        ) {
                            ForEach(ImportImageChoice.allCases) { choice in
                                Text(choice.title).tag(choice)
                            }
                        }
                        Text("表格换行仅生成文本主题；不会伪装成 XMind 原生表格。")
                            .font(.system(size: 9.5))
                            .foregroundStyle(.tertiary)
                    }
                }

                sectionLabel("输出", detail: "不会覆盖源文档")
                optionCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("保存为")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.secondary)
                        HStack(spacing: 7) {
                            TextField("输出路径", text: $model.outputPath)
                                .textFieldStyle(.plain)
                                .font(.system(size: 10, design: .monospaced))
                                .padding(.horizontal, 8)
                                .frame(height: 28)
                                .background(
                                    Color.primary.opacity(0.055),
                                    in: RoundedRectangle(cornerRadius: 6)
                                )
                            Button("选择…") { model.chooseOutputPath() }
                                .controlSize(.small)
                        }
                    }
                }
            }
            .padding(16)
        }
        .background(Color.primary.opacity(0.018))
    }

    private var previewPane: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("结构预览")
                        .font(.system(size: 17, weight: .semibold, design: .rounded))
                    Text(model.preview?.documentTitle ?? model.source.fileName)
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer()
                if model.isPreviewing {
                    ProgressView()
                        .controlSize(.small)
                    Text("更新中")
                        .font(.system(size: 9.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }

            statsGrid
            warningsView
            treePreview
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .padding(18)
    }

    private var statsGrid: some View {
        Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
            GridRow {
                statCell("主题", value: "\(model.preview?.nodeCount ?? 0)", icon: "circle.grid.3x3")
                statCell("最大层级", value: "\(model.preview?.maxDepth ?? 0)", icon: "arrow.down.right")
                statCell("备注", value: "\(model.preview?.noteCount ?? 0)", icon: "note.text")
                statCell("警告", value: "\(model.preview?.warnings.count ?? 0)", icon: "exclamationmark.triangle")
            }
        }
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.7)
        }
    }

    @ViewBuilder
    private var warningsView: some View {
        if let error = model.errorMessage {
            warningBanner(
                icon: "xmark.octagon.fill",
                text: error,
                color: .red
            )
        } else if let warnings = model.preview?.warnings, !warnings.isEmpty {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text("\(warnings.count) 条转换提示")
                        .font(.system(size: 10.5, weight: .semibold))
                    Spacer()
                }
                ForEach(Array(warnings.prefix(4).enumerated()), id: \.offset) { _, warning in
                    Text(warning.message)
                        .font(.system(size: 9.5))
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                if warnings.count > 4 {
                    Text("另有 \(warnings.count - 4) 条提示")
                        .font(.system(size: 9))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(10)
            .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
            .overlay {
                RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(Color.orange.opacity(0.18), lineWidth: 0.7)
            }
        } else if model.preview != nil {
            warningBanner(
                icon: "checkmark.seal.fill",
                text: "没有结构降级警告",
                color: .green
            )
        }
    }

    private var treePreview: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("样本树")
                    .font(.system(size: 10, weight: .bold, design: .monospaced))
                    .tracking(1)
                    .foregroundStyle(.secondary)
                Spacer()
                if let preview = model.preview {
                    Text("显示 \(preview.sampledNodeCount) / \(preview.nodeCount)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 11)
            .padding(.vertical, 8)

            Divider().overlay(Color.primary.opacity(0.06))

            if model.isPreviewing && model.previewRows.isEmpty {
                VStack(spacing: 10) {
                    ProgressView()
                    Text("正在读取文档结构…")
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 1) {
                        ForEach(model.previewRows) { row in
                            previewRow(row)
                        }
                    }
                    .padding(8)
                }
            }
        }
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.7)
        }
    }

    private func previewRow(_ row: ImportPreviewRow) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 7) {
            Text(treePrefix(depth: row.depth))
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.tertiary)
                .frame(width: CGFloat(max(0, row.depth)) * 14 + 10, alignment: .trailing)
            Circle()
                .fill(kindColor(row.kind))
                .frame(width: 5, height: 5)
            Text(row.title)
                .font(.system(size: 11, weight: row.depth == 0 ? .semibold : .regular))
                .lineLimit(1)
                .truncationMode(.tail)
            if row.childCount > 0 {
                Text("\(row.childCount)")
                    .font(.system(size: 8.5, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(Color.primary.opacity(0.055), in: Capsule())
            }
            if row.noteCount > 0 {
                Image(systemName: "note.text")
                    .font(.system(size: 8))
                    .foregroundStyle(.tertiary)
            }
            if row.isTruncated {
                Text("预览截断")
                    .font(.system(size: 8))
                    .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 5)
        .padding(.vertical, 4)
        .background(
            row.depth == 0 ? Color.primary.opacity(0.035) : Color.clear,
            in: RoundedRectangle(cornerRadius: 5)
        )
    }

    private var footer: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(model.errorMessage == nil ? Color.green : Color.red)
                        .frame(width: 6, height: 6)
                    Text(model.isConverting ? "正在转换…" : model.status)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Text("支持 \(model.supportedFormatSummary)")
                    .font(.system(size: 9))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                if let unsupported = model.unsupportedFormatSummary {
                    Text("暂未支持：\(unsupported)")
                        .font(.system(size: 9))
                        .foregroundStyle(.tertiary)
                }
            }

            Spacer()

            Button("取消") {
                NSApp.keyWindow?.performClose(nil)
            }
            .keyboardShortcut(.cancelAction)

            Button {
                model.convert()
            } label: {
                HStack(spacing: 7) {
                    if model.isConverting {
                        ProgressView()
                            .controlSize(.small)
                    } else {
                        Image(systemName: "point.3.connected.trianglepath.dotted")
                    }
                    Text(model.isConverting ? "生成中…" : "生成 Mindmap")
                }
                .frame(minWidth: 120)
            }
            .buttonStyle(.borderedProminent)
            .keyboardShortcut(.defaultAction)
            .disabled(!model.canConvert)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private func sectionLabel(_ title: String, detail: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title.uppercased())
                .font(.system(size: 9, weight: .bold, design: .monospaced))
                .tracking(1.2)
            Spacer()
            Text(detail)
                .font(.system(size: 8.5))
                .foregroundStyle(.tertiary)
        }
    }

    private func optionCard<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        content()
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(11)
            .background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
            .overlay {
                RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.7)
            }
    }

    private func toggleRow(_ title: String, detail: String, isOn: Binding<Bool>) -> some View {
        Toggle(isOn: isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 10.5, weight: .medium))
                Text(detail)
                    .font(.system(size: 9.2))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .toggleStyle(.switch)
        .controlSize(.small)
    }

    private func strategyPicker<Selection: Hashable, Content: View>(
        _ title: String,
        selection: Binding<Selection>,
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack {
            Text(title)
                .font(.system(size: 10.5))
            Spacer()
            Picker(title, selection: selection) {
                content()
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(maxWidth: 190)
        }
    }

    private func statCell(_ label: String, value: String, icon: String) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 5) {
                Image(systemName: icon)
                    .font(.system(size: 8.5))
                Text(label)
                    .font(.system(size: 9))
            }
            .foregroundStyle(.secondary)
            Text(value)
                .font(.system(size: 17, weight: .semibold, design: .monospaced))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 11)
        .padding(.vertical, 9)
    }

    private func warningBanner(icon: String, text: String, color: Color) -> some View {
        HStack(spacing: 7) {
            Image(systemName: icon)
                .foregroundStyle(color)
            Text(text)
                .font(.system(size: 10))
                .lineLimit(3)
            Spacer()
        }
        .padding(9)
        .background(color.opacity(0.07), in: RoundedRectangle(cornerRadius: 8))
    }

    private func treePrefix(depth: Int) -> String {
        guard depth > 0 else { return "●" }
        return String(repeating: "│  ", count: max(0, depth - 1)) + "├─"
    }

    private func kindColor(_ kind: String) -> Color {
        switch kind {
        case "document": Color(red: 0.72, green: 0.34, blue: 0.15)
        case "structure": Color(red: 0.36, green: 0.48, blue: 0.62)
        case "sentence", "paragraph": Color(red: 0.52, green: 0.56, blue: 0.60)
        case "code": Color(red: 0.45, green: 0.58, blue: 0.42)
        case "image": Color(red: 0.58, green: 0.45, blue: 0.62)
        case "table": Color(red: 0.64, green: 0.52, blue: 0.30)
        default: Color.secondary
        }
    }
}
