import AppKit
import Combine
import SwiftUI
import WebKit

@MainActor
final class TextEditorDocumentModel: ObservableObject {
    private let core: KiteCore
    private let gitModel: KiteModel?
    private(set) var document: TextDocumentDto
    weak var textView: NSTextView?

    @Published private(set) var isDirty = false
    @Published private(set) var message = ""
    @Published private(set) var line = 1
    @Published private(set) var column = 1
    @Published private(set) var lineCount = 1
    @Published private(set) var markdownSource = ""
    @Published private(set) var blameEnabled = false
    @Published private(set) var blameLoading = false
    @Published private(set) var blameError: String?
    @Published private(set) var ignoreWhitespace = true
    @Published private(set) var selectedBlame: GitBlameLineDto?
    private(set) var blameLines: [GitBlameLineDto] = []
    private var blameLookup: [UInt32: GitBlameLineDto] = [:]
    private var blameGeneration = 0

    init(path: String, core: KiteCore = KiteCore(), gitModel: KiteModel? = nil) throws {
        self.core = core
        self.gitModel = gitModel
        self.document = try core.openTextDocument(path: path)
        if document.language == "markdown" {
            markdownSource = document.text
        }
    }

    var isMarkdown: Bool { document.language == "markdown" }

    var fileName: String {
        URL(fileURLWithPath: document.path).lastPathComponent
    }

    var languageLabel: String {
        switch document.language {
        case "rust": return "Rust"
        case "javascript": return "JavaScript"
        case "typescript": return "TypeScript"
        case "tsx": return "TSX"
        case "python": return "Python"
        case "json": return "JSON"
        case "toml": return "TOML"
        case "yaml": return "YAML"
        case "markdown": return "Markdown"
        case "html": return "HTML"
        case "css": return "CSS"
        case "c": return "C"
        case "cpp": return "C++"
        case "csharp": return "C#"
        case "java": return "Java"
        case "go": return "Go"
        case "kotlin": return "Kotlin"
        case "bash": return "Shell"
        case "sql": return "SQL"
        case "swift": return "Swift"
        case "lua": return "Lua"
        case "ruby": return "Ruby"
        case "php": return "PHP"
        case "proto": return "Protocol Buffers"
        case "cmake": return "CMake"
        case "make": return "Make"
        case "diff": return "Diff"
        default: return "纯文本"
        }
    }

    var metadataLabel: String {
        let ending = document.lineEnding == "crlf" ? "CRLF" : "LF"
        let bom = document.hasBom ? " · BOM" : ""
        return "\(languageLabel) · UTF-8\(bom) · \(ending)"
    }

    func attach(_ textView: NSTextView) {
        self.textView = textView
        updateStatistics()
    }

    func markChanged() {
        isDirty = true
        message = "未保存"
        if isMarkdown, let textView {
            markdownSource = textView.string
        }
        updateStatistics()
        textView?.enclosingScrollView?.verticalRulerView?.needsDisplay = true
    }

    func updateCursor() {
        updateStatistics()
    }

    func save() -> Bool {
        guard let textView else { return false }
        do {
            document = try core.saveTextDocument(
                path: document.path,
                text: textView.string,
                lineEnding: document.lineEnding,
                hasBom: document.hasBom,
                expectedSha256: document.contentSha256
            )
            isDirty = false
            message = "已保存"
            if blameEnabled {
                loadBlame()
            }
            return true
        } catch {
            message = error.localizedDescription
            return false
        }
    }

    func undo() {
        textView?.undoManager?.undo()
    }

    func redo() {
        textView?.undoManager?.redo()
    }

    func showFindPanel() {
        showTextFinderAction(.showFindInterface)
    }

    func showReplacePanel() {
        showTextFinderAction(.showReplaceInterface)
    }

    func setBlameEnabled(_ enabled: Bool) {
        guard blameEnabled != enabled else { return }
        blameEnabled = enabled
        selectedBlame = nil
        updateBlameRuler()
        if enabled && blameLines.isEmpty {
            loadBlame()
        }
    }

    func setIgnoreWhitespace(_ value: Bool) {
        guard ignoreWhitespace != value else { return }
        ignoreWhitespace = value
        if blameEnabled {
            loadBlame()
        }
    }

    func blameLine(at lineNumber: Int) -> GitBlameLineDto? {
        guard lineNumber > 0 else { return nil }
        return blameLookup[UInt32(lineNumber)]
    }

    func selectBlame(at lineNumber: Int) {
        guard let line = blameLine(at: lineNumber) else { return }
        selectedBlame = line
        gitModel?.openGitLog(path: document.path, initialHash: line.commitHash)
    }

    func clearSelectedBlame() {
        selectedBlame = nil
    }

    func loadBlame() {
        blameGeneration &+= 1
        let generation = blameGeneration
        let path = document.path
        let ignoreWhitespace = ignoreWhitespace
        let core = self.core
        blameLoading = true
        blameError = nil
        selectedBlame = nil

        Task { [weak self] in
            guard let self else { return }
            do {
                var page = try await Task.detached(priority: .userInitiated) {
                    try core.gitBlame(
                        path: path,
                        startLine: 1,
                        lineCount: 400,
                        ignoreWhitespace: ignoreWhitespace
                    )
                }.value
                guard self.blameGeneration == generation else { return }
                self.replaceBlame(with: page.lines)

                var nextLine = page.endLine + 1
                while nextLine <= page.totalLines {
                    guard self.blameGeneration == generation else { return }
                    let pageStart = nextLine
                    page = try await Task.detached(priority: .utility) {
                        try core.gitBlame(
                            path: path,
                            startLine: pageStart,
                            lineCount: 2_000,
                            ignoreWhitespace: ignoreWhitespace
                        )
                    }.value
                    guard self.blameGeneration == generation else { return }
                    self.appendBlame(page.lines)
                    nextLine = page.endLine + 1
                }
            } catch {
                guard self.blameGeneration == generation else { return }
                self.blameError = error.localizedDescription
            }
            guard self.blameGeneration == generation else { return }
            self.blameLoading = false
            self.updateBlameRuler()
        }
    }

    private func showTextFinderAction(_ action: NSTextFinder.Action) {
        guard let textView else { return }
        let sender = NSMenuItem()
        sender.tag = action.rawValue
        textView.performTextFinderAction(sender)
    }

    private func replaceBlame(with lines: [GitBlameLineDto]) {
        blameLines = lines
        blameLookup = Dictionary(uniqueKeysWithValues: lines.map { ($0.lineNumber, $0) })
        updateBlameRuler()
    }

    private func appendBlame(_ lines: [GitBlameLineDto]) {
        blameLines.append(contentsOf: lines)
        for line in lines {
            blameLookup[line.lineNumber] = line
        }
        updateBlameRuler()
    }

    private func updateBlameRuler() {
        if let ruler = textView?.enclosingScrollView?.verticalRulerView as? BlameRulerView {
            ruler.setBlameVisible(blameEnabled)
            ruler.needsDisplay = true
        }
    }

    private func updateStatistics() {
        guard let textView else { return }
        let value = textView.string as NSString
        let cursor = min(textView.selectedRange().location, value.length)
        let beforeCursor = value.substring(to: cursor)
        let currentLine = beforeCursor.reduce(into: 1) { count, character in
            if character == "\n" { count += 1 }
        }
        let currentColumn = beforeCursor
            .split(separator: "\n", omittingEmptySubsequences: false)
            .last?
            .count ?? 0
        line = currentLine
        column = currentColumn + 1
        lineCount = textView.string.reduce(into: 1) { count, character in
            if character == "\n" { count += 1 }
        }
    }
}

@MainActor
private struct KiteTextEditorSurface: NSViewRepresentable {
    @ObservedObject var model: TextEditorDocumentModel

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay

        let textView = NSTextView(frame: .zero)
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true
        textView.isContinuousSpellCheckingEnabled = false
        textView.isGrammarCheckingEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.smartInsertDeleteEnabled = false
        textView.drawsBackground = false
        textView.textColor = .labelColor
        textView.insertionPointColor = .controlAccentColor
        textView.font = .monospacedSystemFont(ofSize: 13.5, weight: .regular)
        textView.textContainerInset = NSSize(width: 14, height: 14)
        textView.minSize = NSSize(width: 0, height: 0)
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = true
        textView.autoresizingMask = [.width]
        textView.textContainer?.containerSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.textContainer?.widthTracksTextView = false
        textView.textContainer?.lineFragmentPadding = 8
        textView.string = model.document.text
        textView.delegate = context.coordinator

        scrollView.documentView = textView
        scrollView.verticalRulerView = BlameRulerView(textView: textView, model: model)
        scrollView.hasVerticalRuler = true
        scrollView.rulersVisible = true

        model.attach(textView)
        DispatchQueue.main.async {
            textView.window?.makeFirstResponder(textView)
            textView.setSelectedRange(NSRange(location: 0, length: 0))
            textView.scrollRangeToVisible(NSRange(location: 0, length: 0))
        }
        return scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        private let model: TextEditorDocumentModel

        init(model: TextEditorDocumentModel) {
            self.model = model
        }

        func textDidChange(_ notification: Notification) {
            model.markChanged()
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            model.updateCursor()
        }
    }
}

@MainActor
private struct TextEditorWindowView: View {
    @ObservedObject var model: TextEditorDocumentModel
    @State private var showMarkdownPreview = true

    var body: some View {
        ZStack {
            KiteGlassBackdrop()

            VStack(spacing: 0) {
                toolbar
                HStack(spacing: 0) {
                    VStack(spacing: 0) {
                        if model.isMarkdown && showMarkdownPreview {
                            paneHeading("Markdown 源码")
                        }
                        KiteTextEditorSurface(model: model)
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                    if model.isMarkdown && showMarkdownPreview {
                        Divider()
                        VStack(spacing: 0) {
                            paneHeading("实时预览")
                            LiveMarkdownPreview(source: model.markdownSource)
                                .accessibilityLabel("Markdown 实时预览")
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
                if model.blameEnabled, let blame = model.selectedBlame {
                    blameDetailBar(blame)
                }
                statusBar
            }
            .padding(10)
        }
        .frame(minWidth: 720, minHeight: 520)
    }

    private func paneHeading(_ title: String) -> some View {
        Text(title)
            .font(.caption)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Button {
                _ = model.save()
            } label: {
                Label("保存", systemImage: "square.and.arrow.down")
            }
            .disabled(!model.isDirty)
            .help("保存 ⌘S")

            Button {
                model.undo()
            } label: {
                Image(systemName: "arrow.uturn.backward")
            }
            .help("撤销 ⌘Z")

            Button {
                model.redo()
            } label: {
                Image(systemName: "arrow.uturn.forward")
            }
            .help("重做 ⇧⌘Z")

            Divider()
                .frame(height: 18)

            Button {
                model.showFindPanel()
            } label: {
                Label("查找", systemImage: "magnifyingglass")
            }
            .help("查找 ⌘F")

            Button {
                model.showReplacePanel()
            } label: {
                Label("替换", systemImage: "arrow.triangle.2.circlepath")
            }
            .help("查找并替换 ⌥⌘F")

            Toggle("Blame", isOn: blameEnabledBinding)
                .toggleStyle(.button)
                .help("在行号旁显示逐行提交信息")

            if model.blameEnabled {
                Toggle("忽略空白", isOn: ignoreWhitespaceBinding)
                    .toggleStyle(.checkbox)
            }

            if model.isMarkdown {
                Toggle("实时预览", isOn: $showMarkdownPreview)
                    .toggleStyle(.button)
            }

            Spacer()

            Text(model.metadataLabel)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 12)
        .frame(height: 38)
        .kiteGlassGroup(cornerRadius: 12)
    }

    private var blameEnabledBinding: Binding<Bool> {
        Binding(
            get: { model.blameEnabled },
            set: { model.setBlameEnabled($0) }
        )
    }

    private var ignoreWhitespaceBinding: Binding<Bool> {
        Binding(
            get: { model.ignoreWhitespace },
            set: { model.setIgnoreWhitespace($0) }
        )
    }

    private func blameDetailBar(_ blame: GitBlameLineDto) -> some View {
        HStack(spacing: 10) {
            Image(systemName: blame.isUncommitted ? "pencil.line" : "arrow.triangle.branch")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(blame.isUncommitted ? Color.red : Color.accentColor)

            Text(blame.shortHash)
                .font(.system(size: 10.5, weight: .semibold, design: .monospaced))

            Text(blame.summary)
                .font(.system(size: 11))
                .lineLimit(1)

            Spacer()

            Text("\(blame.author) · \(Date(timeIntervalSince1970: TimeInterval(blame.authorTime)).formatted(date: .abbreviated, time: .shortened))")
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .lineLimit(1)

            Button {
                model.clearSelectedBlame()
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.borderless)
        }
        .padding(.horizontal, 12)
        .frame(height: 30)
        .background(
            Color.accentColor.opacity(0.075),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .padding(.top, 6)
    }

    private var statusBar: some View {
        HStack(spacing: 12) {
            Text(model.isDirty ? "● 未保存" : "已保存")
            Text("第 \(model.line) 行，第 \(model.column) 列")
            Text("\(model.lineCount) 行")
            Spacer()
            if !model.message.isEmpty {
                Text(model.message)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            if model.blameLoading {
                Text("正在读取 Blame…")
            } else if let error = model.blameError {
                Text(error)
                    .foregroundStyle(.red)
                    .lineLimit(1)
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .frame(height: 28)
        .kiteGlassGroup(cornerRadius: 10)
    }
}

@MainActor
private struct LiveMarkdownPreview: NSViewRepresentable {
    let source: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        context.coordinator.update(source, in: view)
    }

    static func dismantleNSView(_ view: WKWebView, coordinator: Coordinator) {
        coordinator.pending?.cancel()
        view.stopLoading()
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate {
        private var source: String?
        var pending: Task<Void, Never>?

        func update(_ source: String, in view: WKWebView) {
            guard source != self.source else { return }
            let firstRender = self.source == nil
            self.source = source
            pending?.cancel()
            pending = Task { @MainActor [weak view] in
                if !firstRender {
                    try? await Task.sleep(for: .milliseconds(150))
                }
                guard !Task.isCancelled else { return }
                let html = await Task.detached(priority: .userInitiated) {
                    renderLiveMarkdown(source: source)
                }.value
                guard !Task.isCancelled else { return }
                view?.loadHTMLString(Self.page(html), baseURL: nil)
            }
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction
        ) async -> WKNavigationActionPolicy {
            if navigationAction.navigationType == .linkActivated,
               let url = navigationAction.request.url,
               url.scheme == "https" || url.scheme == "http" {
                NSWorkspace.shared.open(url)
            }
            return navigationAction.request.url?.absoluteString == "about:blank" ? .allow : .cancel
        }

        private static func page(_ content: String) -> String {
            """
            <!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
            <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'">
            <meta name="color-scheme" content="light dark">
            <style>
            body { font: 14px/1.55 -apple-system, sans-serif; margin: 24px; overflow-wrap: anywhere; }
            pre { overflow: auto; padding: 12px; }
            pre, th { background: light-dark(#f3f3f5, #252527); }
            table { border-collapse: collapse; }
            td, th { border: 1px solid GrayText; padding: 4px 8px; }
            blockquote { margin-left: 0; padding-left: 12px; border-left: 2px solid GrayText; }
            </style></head><body>\(content)</body></html>
            """
        }
    }
}

@MainActor
private final class BlameRulerView: NSRulerView, NSViewToolTipOwner {
    private weak var textView: NSTextView?
    private weak var model: TextEditorDocumentModel?
    private var blameVisible = false
    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    init(textView: NSTextView, model: TextEditorDocumentModel) {
        self.textView = textView
        self.model = model
        super.init(scrollView: textView.enclosingScrollView, orientation: .verticalRuler)
        clientView = textView
        ruleThickness = 48
    }

    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func setBlameVisible(_ visible: Bool) {
        guard blameVisible != visible else { return }
        blameVisible = visible
        ruleThickness = visible ? 320 : 48
        scrollView?.tile()
        needsDisplay = true
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        refreshToolTip()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        refreshToolTip()
    }

    private func refreshToolTip() {
        removeAllToolTips()
        guard window != nil, !bounds.isEmpty else { return }
        addToolTip(bounds, owner: self, userData: nil)
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        NSColor.textBackgroundColor.withAlphaComponent(0.14).setFill()
        NSBezierPath(rect: rect).fill()

        guard let textView,
              let model,
              let layoutManager = textView.layoutManager,
              let textContainer = textView.textContainer,
              let scrollView else {
            return
        }

        let visibleRect = scrollView.contentView.bounds
        let glyphRange = layoutManager.glyphRange(
            forBoundingRect: visibleRect,
            in: textContainer
        )
        let characterRange = layoutManager.characterRange(
            forGlyphRange: glyphRange,
            actualGlyphRange: nil
        )
        let value = textView.string as NSString
        let numberFont = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        let numberAttributes: [NSAttributedString.Key: Any] = [
            .font: numberFont,
            .foregroundColor: NSColor.secondaryLabelColor
        ]

        var lineNumber = 1
        var index = 0
        while index < min(characterRange.location, value.length) {
            if value.character(at: index) == 10 {
                lineNumber += 1
            }
            index += 1
        }

        if glyphRange.length == 0 {
            let y = textView.textContainerOrigin.y
            drawLineNumber(1, atY: y, attributes: numberAttributes)
            if blameVisible {
                drawBlame(model.blameLine(at: 1), lineNumber: 1, atY: y, height: 16)
            }
            return
        }

        var glyphIndex = glyphRange.location
        while glyphIndex < NSMaxRange(glyphRange) {
            var lineGlyphRange = NSRange()
            let lineRect = layoutManager.lineFragmentRect(
                forGlyphAt: glyphIndex,
                effectiveRange: &lineGlyphRange
            )
            let rectInTextView = lineRect.offsetBy(dx: 0, dy: textView.textContainerOrigin.y)
            let rectInRuler = textView.convert(rectInTextView, to: self)
            if blameVisible {
                let blame = model.blameLine(at: lineNumber)
                drawBlame(blame, lineNumber: lineNumber, atY: rectInRuler.minY, height: rectInRuler.height)
            }
            drawLineNumber(lineNumber, atY: rectInRuler.minY, attributes: numberAttributes)
            lineNumber += 1
            glyphIndex = NSMaxRange(lineGlyphRange)
        }
    }

    private func drawLineNumber(
        _ lineNumber: Int,
        atY y: CGFloat,
        attributes: [NSAttributedString.Key: Any]
    ) {
        let value = "\(lineNumber)" as NSString
        let size = value.size(withAttributes: attributes)
        value.draw(
            at: NSPoint(x: 42 - size.width, y: y + 1),
            withAttributes: attributes
        )
    }

    private func drawBlame(
        _ line: GitBlameLineDto?,
        lineNumber: Int,
        atY y: CGFloat,
        height: CGFloat
    ) {
        let blameRect = NSRect(x: 48, y: y, width: max(0, ruleThickness - 50), height: max(14, height))
        let batch = line.map { UInt32(bitPattern: Int32(truncatingIfNeeded: $0.lineNumber / 24)) } ?? 0
        let hue = CGFloat(batch % 12) / 12
        let color = NSColor(calibratedHue: hue, saturation: 0.18, brightness: 0.72, alpha: 0.09)
        color.setFill()
        NSBezierPath(rect: blameRect).fill()

        let text: String
        if let line {
            text = "\(line.shortHash)  \(line.author)  \(Self.dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(line.authorTime))))"
        } else if model?.blameLoading == true {
            text = "… 正在读取 Blame"
        } else {
            text = "—"
        }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 9.2, weight: .regular),
            .foregroundColor: blameColor(line)
        ]
        (text as NSString).draw(
            in: NSRect(x: 54, y: y + 1, width: max(0, ruleThickness - 58), height: 13),
            withAttributes: attributes
        )
    }

    private func blameColor(_ line: GitBlameLineDto?) -> NSColor {
        guard let line else { return .tertiaryLabelColor }
        if line.isUncommitted {
            return .systemRed
        }
        let age = max(0, Date().timeIntervalSince1970 - TimeInterval(line.authorTime))
        let years = age / (365 * 86_400)
        let alpha = max(0.42, 1 - min(years, 8) / 10)
        return NSColor.controlAccentColor.withAlphaComponent(alpha)
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        if blameVisible, let lineNumber = lineNumber(at: point) {
            model?.selectBlame(at: lineNumber)
            return
        }
        super.mouseDown(with: event)
    }

    func view(
        _ view: NSView,
        stringForToolTip tag: NSView.ToolTipTag,
        point: NSPoint,
        userData data: UnsafeMutableRawPointer?
    ) -> String {
        guard let lineNumber = lineNumber(at: point),
              let line = model?.blameLine(at: lineNumber) else {
            return "未加载 Blame 信息"
        }
        var parts = [
            "\(line.shortHash)  \(line.author)",
            Self.dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(line.authorTime))),
            line.summary
        ]
        if let mail = line.authorMail {
            parts.append(mail)
        }
        return parts.joined(separator: " · ")
    }

    private func lineNumber(at rulerPoint: NSPoint) -> Int? {
        guard let textView else {
            return nil
        }
        let textPoint = convert(rulerPoint, to: textView)
        let insertion = textView.characterIndexForInsertion(at: textPoint)
        let character = max(0, min(insertion, (textView.string as NSString).length))
        let prefix = (textView.string as NSString).substring(to: character)
        return prefix.reduce(into: 1) { count, value in
            if value == "\n" { count += 1 }
        }
    }
}

@MainActor
final class TextEditorWindowController: NSWindowController, NSWindowDelegate {
    private static var controllers: [String: TextEditorWindowController] = [:]
    private let documentModel: TextEditorDocumentModel
    private let path: String

    init(path: String, model: KiteModel? = nil) throws {
        self.path = URL(fileURLWithPath: path).standardizedFileURL.path
        self.documentModel = try TextEditorDocumentModel(path: path, gitModel: model)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 980, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = documentModel.fileName
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isMovableByWindowBackground = true
        window.minSize = NSSize(width: 720, height: 520)
        window.contentViewController = NSHostingController(
            rootView: TextEditorWindowView(model: documentModel)
        )
        super.init(window: window)
        window.delegate = self
        updateTitle()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func open(path: String, model: KiteModel? = nil) throws {
        let normalized = URL(fileURLWithPath: path).standardizedFileURL.path
        if let controller = controllers[normalized] {
            controller.showWindow(nil)
            controller.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let controller = try TextEditorWindowController(path: normalized, model: model)
        controllers[normalized] = controller
        controller.showWindow(nil)
        controller.window?.center()
        controller.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    static func saveKeyWindow() {
        controller(for: NSApp.keyWindow)?.save()
    }

    static func showFindInKeyWindow(replace: Bool) {
        guard let model = controller(for: NSApp.keyWindow)?.documentModel else { return }
        if replace {
            model.showReplacePanel()
        } else {
            model.showFindPanel()
        }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard documentModel.isDirty else { return true }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "有未保存的更改"
        alert.informativeText = "关闭窗口会丢失当前修改。"
        alert.addButton(withTitle: "保存并关闭")
        alert.addButton(withTitle: "取消")
        alert.addButton(withTitle: "放弃更改")

        switch alert.runModal() {
        case .alertFirstButtonReturn:
            if documentModel.save() {
                return true
            }
            let error = NSAlert()
            error.alertStyle = .critical
            error.messageText = "无法保存文件"
            error.informativeText = documentModel.message
            error.runModal()
            return false
        case .alertThirdButtonReturn:
            return true
        default:
            return false
        }
    }

    func windowWillClose(_ notification: Notification) {
        TextEditorWindowController.controllers.removeValue(forKey: path)
    }

    private func save() {
        if documentModel.save() {
            updateTitle()
        }
    }

    private func updateTitle() {
        window?.title = documentModel.fileName
        window?.representedURL = URL(fileURLWithPath: documentModel.document.path)
    }

    private static func controller(for window: NSWindow?) -> TextEditorWindowController? {
        controllers.values.first { $0.window === window }
    }
}
