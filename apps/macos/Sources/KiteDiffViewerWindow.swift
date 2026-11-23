import AppKit
import SwiftUI

enum DiffDisplayMode: String, CaseIterable, Identifiable, Equatable {
    case unified
    case sideBySide

    var id: String { rawValue }

    var title: String {
        switch self {
        case .unified: return "统一"
        case .sideBySide: return "并排"
        }
    }
}

enum DiffComparisonPreset: String, CaseIterable, Identifiable, Equatable {
    case headToWorkingTree
    case headToIndex
    case indexToWorkingTree
    case custom

    var id: String { rawValue }

    var title: String {
        switch self {
        case .headToWorkingTree: return "HEAD ↔ 工作区"
        case .headToIndex: return "HEAD ↔ 暂存区"
        case .indexToWorkingTree: return "暂存区 ↔ 工作区"
        case .custom: return "自定义"
        }
    }
}

@MainActor
final class KiteDiffViewerModel: ObservableObject {
    @Published private(set) var document: GitDiffDocumentDto?
    @Published private(set) var loading = false
    @Published private(set) var currentChange = 0
    @Published private(set) var scrollTargetRow: Int?
    @Published var displayMode: DiffDisplayMode = .unified
    @Published var hideUnchanged = false
    @Published var ignoreWhitespace = false {
        didSet {
            guard ignoreWhitespace != oldValue else { return }
            reload()
        }
    }
    @Published var preset: DiffComparisonPreset = .headToWorkingTree
    @Published var oldRevision = "HEAD"
    @Published var newRevision = "工作区"
    @Published var errorMessage: String?

    private weak var appModel: KiteModel?
    private var path = ""
    private var generation = 0
    private var task: Task<Void, Never>?

    var title: String {
        (path as NSString).lastPathComponent
    }

    var visibleRows: [VisibleDiffRow] {
        guard let document else { return [] }
        return Array(document.rows.enumerated()).compactMap { index, row in
            guard !hideUnchanged || row.kind != .context else { return nil }
            return VisibleDiffRow(index: index, row: row)
        }
    }

    var changeCount: Int {
        document?.changeRows.count ?? 0
    }

    var changePositionLabel: String {
        guard changeCount > 0 else { return "无改动" }
        return "\(currentChange + 1) / \(changeCount)"
    }

    func show(path: String, appModel: KiteModel, preset: DiffComparisonPreset) {
        self.appModel = appModel
        self.path = path
        self.preset = preset
        reload()
    }

    func reload() {
        guard let appModel, !path.isEmpty else { return }
        generation &+= 1
        let generation = generation
        task?.cancel()
        loading = true
        errorMessage = nil
        currentChange = 0
        scrollTargetRow = nil

        let request = GitDiffRequestDto(
            path: path,
            old: endpoint(for: oldSelection),
            new: endpoint(for: newSelection),
            ignoreWhitespace: ignoreWhitespace
        )
        task = Task { [weak self] in
            do {
                let core = appModel.core
                let result = try await Task.detached(priority: .userInitiated) {
                    try core.gitDiff(request: request)
                }.value
                guard !Task.isCancelled, let self, self.generation == generation else { return }
                self.document = result
                self.loading = false
                self.currentChange = 0
                self.scrollTargetRow = result.changeRows.first.map { Int($0) }
            } catch {
                guard !Task.isCancelled, let self, self.generation == generation else { return }
                self.errorMessage = error.localizedDescription
                self.document = nil
                self.loading = false
            }
        }
    }

    func moveToNextChange() {
        moveChange(by: 1)
    }

    func moveToPreviousChange() {
        moveChange(by: -1)
    }

    func reloadCustomRevisions() {
        guard preset == .custom else { return }
        reload()
    }

    private var oldSelection: GitDiffEndpointDto {
        switch preset {
        case .headToWorkingTree, .headToIndex:
            return .revision(revision: "HEAD")
        case .indexToWorkingTree:
            return .index
        case .custom:
            return revisionEndpoint(oldRevision)
        }
    }

    private var newSelection: GitDiffEndpointDto {
        switch preset {
        case .headToWorkingTree:
            return .workingTree
        case .headToIndex:
            return .index
        case .indexToWorkingTree:
            return .workingTree
        case .custom:
            return revisionEndpoint(newRevision)
        }
    }

    private func endpoint(for endpoint: GitDiffEndpointDto) -> GitDiffEndpointDto {
        endpoint
    }

    private func revisionEndpoint(_ value: String) -> GitDiffEndpointDto {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed == "工作区" || trimmed.caseInsensitiveCompare("worktree") == .orderedSame {
            return .workingTree
        }
        if trimmed == "暂存区" || trimmed.caseInsensitiveCompare("index") == .orderedSame {
            return .index
        }
        return .revision(revision: trimmed.isEmpty ? "HEAD" : trimmed)
    }

    private func moveChange(by delta: Int) {
        guard let document, !document.changeRows.isEmpty else { return }
        let count = document.changeRows.count
        currentChange = (currentChange + delta + count) % count
        scrollTargetRow = Int(document.changeRows[currentChange])
    }

}

@MainActor
final class KiteDiffViewerWindowController: NSWindowController, NSWindowDelegate {
    static let shared = KiteDiffViewerWindowController()
    private weak var model: KiteDiffViewerModel?
    private var keyMonitor: Any?

    private init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1080, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Diff 查看器"
        window.titlebarAppearsTransparent = true
        window.minSize = NSSize(width: 820, height: 540)
        window.isReleasedWhenClosed = false
        window.center()
        window.setFrameAutosaveName("KiteDiffViewerWindow")
        super.init(window: window)
        window.delegate = self
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func show(
        path: String,
        appModel: KiteModel,
        preset: DiffComparisonPreset,
        oldRevision: String? = nil,
        newRevision: String? = nil
    ) {
        if window?.contentViewController == nil {
            let state = KiteDiffViewerModel()
            self.model = state
            window?.contentViewController = NSHostingController(
                rootView: KiteDiffViewerView(model: state)
            )
        }
        if let oldRevision { model?.oldRevision = oldRevision }
        if let newRevision { model?.newRevision = newRevision }
        model?.show(path: path, appModel: appModel, preset: preset)
        window?.title = "Diff 查看器 — \((path as NSString).lastPathComponent)"
        installKeyMonitor()
        if window?.isMiniaturized == true {
            window?.deminiaturize(nil)
        }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        removeKeyMonitor()
    }

    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self,
                  self.window?.isKeyWindow == true,
                  event.keyCode == 98
            else { return event }
            if event.modifierFlags.contains(.shift) {
                self.model?.moveToPreviousChange()
            } else {
                self.model?.moveToNextChange()
            }
            return nil
        }
    }

    private func removeKeyMonitor() {
        if let keyMonitor {
            NSEvent.removeMonitor(keyMonitor)
            self.keyMonitor = nil
        }
    }
}

private struct KiteDiffViewerView: View {
    @ObservedObject var model: KiteDiffViewerModel

    var body: some View {
        ZStack {
            KiteGlassBackdrop()

            VStack(spacing: 0) {
                toolbar
                Divider().opacity(0.45)
                content
            }
        }
        .frame(minWidth: 820, minHeight: 540)
        .onChange(of: model.preset) { _ in
            model.reload()
        }
        .onExitCommand {
            NSApp.keyWindow?.performClose(nil)
        }
    }

    private var toolbar: some View {
        VStack(spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: "square.split.2x1")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.secondary)

                Picker("比较", selection: $model.preset) {
                    ForEach(DiffComparisonPreset.allCases) { preset in
                        Text(preset.title).tag(preset)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .controlSize(.small)

                Spacer(minLength: 12)

                Picker("显示模式", selection: $model.displayMode) {
                    ForEach(DiffDisplayMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                .controlSize(.small)
                .frame(width: 126)

                Toggle("忽略空白", isOn: $model.ignoreWhitespace)
                    .toggleStyle(.checkbox)
                    .controlSize(.small)

                Toggle("折叠未修改", isOn: $model.hideUnchanged)
                    .toggleStyle(.checkbox)
                    .controlSize(.small)

                Spacer(minLength: 8)

                Button {
                    model.moveToPreviousChange()
                } label: {
                    Image(systemName: "chevron.up")
                }
                .buttonStyle(.borderless)
                .help("上一处改动 (⇧F7)")

                Text(model.changePositionLabel)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 54)

                Button {
                    model.moveToNextChange()
                } label: {
                    Image(systemName: "chevron.down")
                }
                .buttonStyle(.borderless)
                .help("下一处改动 (F7)")
            }

            if model.preset == .custom {
                HStack(spacing: 8) {
                    TextField("旧端点，例如 HEAD~1、main 或暂存区", text: $model.oldRevision)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 11, design: .monospaced))
                    TextField("新端点，例如 HEAD、feature 或工作区", text: $model.newRevision)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 11, design: .monospaced))
                    Button("重新比较") {
                        model.reloadCustomRevisions()
                    }
                    .controlSize(.small)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    @ViewBuilder
    private var content: some View {
        if model.loading {
            centered {
                ProgressView("正在计算差异…")
                    .controlSize(.small)
            }
        } else if let error = model.errorMessage {
            centered {
                VStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 22))
                        .foregroundStyle(.orange)
                    Text(error)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 560)
                }
            }
        } else if let document = model.document {
            documentContent(document)
        } else {
            centered { Text("没有可显示的差异").foregroundStyle(.secondary) }
        }
    }

    @ViewBuilder
    private func documentContent(_ document: GitDiffDocumentDto) -> some View {
        VStack(spacing: 0) {
            summaryBar(document)
            Divider().opacity(0.35)

            switch document.contentKind {
            case .binary:
                centered {
                    VStack(spacing: 8) {
                        Image(systemName: "doc.badge.gearshape")
                            .font(.system(size: 28))
                            .foregroundStyle(.secondary)
                        Text(document.message ?? "二进制文件已变化，无法显示文本差异。")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                    }
                }
            case .image:
                imageDiff(document)
            case .text:
                if document.rows.isEmpty {
                    centered { Text("没有可显示的差异").foregroundStyle(.secondary) }
                } else {
                    diffScrollView(document)
                }
            }
        }
    }

    private func summaryBar(_ document: GitDiffDocumentDto) -> some View {
        HStack(spacing: 12) {
            Text("\(document.oldLabel) → \(document.newLabel)")
                .font(.system(size: 11, weight: .medium))
                .lineLimit(1)
            if document.largeFile {
                Text("大文件预览")
                    .foregroundStyle(.orange)
                Text("\(byteLabel(document.stats.oldBytes)) → \(byteLabel(document.stats.newBytes))")
                    .foregroundStyle(.secondary)
            } else {
                Text("+\(document.stats.additions)")
                    .foregroundStyle(.green)
                Text("-\(document.stats.deletions)")
                    .foregroundStyle(.red)
                Text("\(document.stats.hunks) 处 hunk")
                    .foregroundStyle(.secondary)
                if document.stats.files > 1 {
                    Text("\(document.stats.files) 个文件")
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            if let message = document.message {
                Text(message)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .font(.system(size: 10.5, design: .monospaced))
        .padding(.horizontal, 14)
        .padding(.vertical, 7)
    }

    private func diffScrollView(_ document: GitDiffDocumentDto) -> some View {
        ScrollViewReader { proxy in
            ScrollView([.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(model.visibleRows) { visible in
                        rowView(visible.row, document: document)
                            .id(visible.index)
                    }
                }
                .padding(.vertical, 5)
            }
            .onChange(of: model.scrollTargetRow) { target in
                guard let target else { return }
                withAnimation(.easeInOut(duration: 0.16)) {
                    proxy.scrollTo(target, anchor: .center)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private func rowView(_ row: DiffRowDto, document: GitDiffDocumentDto) -> some View {
        let current = row.changeIndex != nil && Int(row.changeIndex!) == model.currentChange
        switch row.kind {
        case .file:
            fileRow(row.text)
        case .hunk:
            hunkRow(row.text)
        case .context:
            if model.displayMode == .unified {
                unifiedLine(row.left, kind: .context, current: false)
            } else {
                sideBySideLine(row.left, row.right, kind: .context, current: false)
            }
        case .modification:
            if model.displayMode == .unified {
                unifiedLine(row.left, kind: .deletion, current: current)
                unifiedLine(row.right, kind: .addition, current: current)
            } else {
                sideBySideLine(row.left, row.right, kind: .modification, current: current)
            }
        case .deletion:
            if model.displayMode == .unified {
                unifiedLine(row.left, kind: .deletion, current: current)
            } else {
                sideBySideLine(row.left, nil, kind: .deletion, current: current)
            }
        case .addition:
            if model.displayMode == .unified {
                unifiedLine(row.right, kind: .addition, current: current)
            } else {
                sideBySideLine(nil, row.right, kind: .addition, current: current)
            }
        }
    }

    private func fileRow(_ text: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "doc.text")
            Text(text).lineLimit(1)
        }
        .font(.system(size: 11, weight: .semibold, design: .monospaced))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.045))
    }

    private func hunkRow(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 10.5, weight: .medium, design: .monospaced))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.accentColor.opacity(0.07))
    }

    private func unifiedLine(
        _ line: DiffLineDto?,
        kind: UnifiedLineKind,
        current: Bool
    ) -> some View {
        let prefix: String
        let background: Color
        let numberColor: Color
        switch kind {
        case .context:
            prefix = " "
            background = .clear
            numberColor = .secondary
        case .deletion:
            prefix = "-"
            background = Color.red.opacity(current ? 0.2 : 0.1)
            numberColor = .red
        case .addition:
            prefix = "+"
            background = Color.green.opacity(current ? 0.2 : 0.1)
            numberColor = .green
        }
        let oldNumber = line?.oldLine.map(String.init) ?? ""
        let newNumber = line?.newLine.map(String.init) ?? ""
        return HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(oldNumber)
                .frame(width: 44, alignment: .trailing)
                .padding(.trailing, 5)
                .foregroundStyle(numberColor.opacity(0.85))
            Text(newNumber)
                .frame(width: 44, alignment: .trailing)
                .padding(.trailing, 5)
                .foregroundStyle(numberColor.opacity(0.85))
            Text(prefix)
                .frame(width: 15)
                .foregroundStyle(numberColor)
            highlightedText(line, baseColor: kind == .context ? .primary : numberColor)
            if line?.missingNewline == true {
                Text("  ⏎").foregroundStyle(.tertiary)
            }
        }
        .font(.system(size: 11, design: .monospaced))
        .padding(.horizontal, 8)
        .padding(.vertical, 1)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(background)
        .overlay(alignment: .leading) {
            if current {
                Rectangle().fill(Color.accentColor).frame(width: 2)
            }
        }
        .fixedSize(horizontal: true, vertical: false)
    }

    private func sideBySideLine(
        _ left: DiffLineDto?,
        _ right: DiffLineDto?,
        kind: SideBySideKind,
        current: Bool
    ) -> some View {
        HStack(spacing: 0) {
            sidePanel(left, side: .left, kind: kind, current: current)
                .frame(width: 520, alignment: .leading)
            Rectangle()
                .fill(Color.primary.opacity(0.1))
                .frame(width: 1)
            sidePanel(right, side: .right, kind: kind, current: current)
                .frame(width: 520, alignment: .leading)
        }
        .fixedSize(horizontal: true, vertical: false)
    }

    private func sidePanel(
        _ line: DiffLineDto?,
        side: DiffSide,
        kind: SideBySideKind,
        current: Bool
    ) -> some View {
        let isChanged = (side == .left && (kind == .deletion || kind == .modification))
            || (side == .right && (kind == .addition || kind == .modification))
        let background: Color
        if kind == .context {
            background = .clear
        } else if side == .left && kind != .addition {
            background = Color.red.opacity(current ? 0.2 : 0.1)
        } else if side == .right && kind != .deletion {
            background = Color.green.opacity(current ? 0.2 : 0.1)
        } else {
            background = Color.primary.opacity(0.025)
        }
        let number = (side == .left ? line?.oldLine : line?.newLine).map(String.init) ?? ""
        let color: Color = side == .left ? .red : .green

        return HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(number)
                .frame(width: 44, alignment: .trailing)
                .padding(.trailing, 6)
                .foregroundStyle(isChanged ? color.opacity(0.9) : .secondary)
            if let line {
                highlightedText(line, baseColor: isChanged ? color : .primary)
            }
        }
        .font(.system(size: 11, design: .monospaced))
        .padding(.horizontal, 8)
        .padding(.vertical, 1)
        .frame(height: 19, alignment: .leading)
        .background(background)
        .overlay(alignment: side == .left ? .trailing : .leading) {
            if current && isChanged {
                Rectangle().fill(Color.accentColor).frame(width: 2)
            }
        }
    }

    private func highlightedText(_ line: DiffLineDto?, baseColor: Color) -> Text {
        guard let line else { return Text("") }
        guard !line.highlighted.isEmpty || !line.syntax.isEmpty else {
            return Text(line.text).foregroundColor(baseColor)
        }
        let nsText = line.text as NSString
        var boundaries: Set<Int> = [0, nsText.length]
        for range in line.highlighted {
            boundaries.insert(max(0, min(Int(range.startUtf16), nsText.length)))
            boundaries.insert(max(0, min(Int(range.endUtf16), nsText.length)))
        }
        for span in line.syntax {
            boundaries.insert(max(0, min(Int(span.startUtf16), nsText.length)))
            boundaries.insert(max(0, min(Int(span.endUtf16), nsText.length)))
        }
        let sortedBoundaries = boundaries.sorted()
        var result = Text("")
        for index in 0..<(sortedBoundaries.count - 1) {
            let start = sortedBoundaries[index]
            let end = sortedBoundaries[index + 1]
            guard start < end else { continue }
            let changed = line.highlighted.contains { range in
                let rangeStart = Int(range.startUtf16)
                let rangeEnd = Int(range.endUtf16)
                return rangeStart <= start && start < rangeEnd
            }
            let syntax = line.syntax.first { span in
                let spanStart = Int(span.startUtf16)
                let spanEnd = Int(span.endUtf16)
                return spanStart <= start && start < spanEnd
            }?.kind
            var segment = Text(nsText.substring(with: NSRange(location: start, length: end - start)))
            if changed {
                segment = segment
                    .foregroundColor(.accentColor)
                    .bold()
                    .underline()
            } else if let syntax {
                segment = segment.foregroundColor(syntaxColor(syntax))
            } else {
                segment = segment.foregroundColor(baseColor)
            }
            result = result + segment
        }
        return result
    }

    private func syntaxColor(_ kind: DiffSyntaxKindDto) -> Color {
        switch kind {
        case .keyword: return .purple
        case .string: return .green
        case .number: return .orange
        case .comment: return .secondary
        case .typeName: return .cyan
        case .function: return .blue
        }
    }

    @ViewBuilder
    private func imageDiff(_ document: GitDiffDocumentDto) -> some View {
        HStack(spacing: 1) {
            imagePanel("旧", data: document.oldImage)
            imagePanel("新", data: document.newImage)
        }
        .padding(14)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func imagePanel(_ title: String, data: Data?) -> some View {
        VStack(spacing: 8) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
            if let data, let image = NSImage(data: data) {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Text("此端点没有图片")
                    .font(.system(size: 11))
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
    }

    private func centered<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(24)
    }

    private func byteLabel(_ bytes: UInt64?) -> String {
        guard let bytes else { return "—" }
        return ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
}

private enum UnifiedLineKind: Equatable {
    case context, deletion, addition
}

private enum SideBySideKind: Equatable {
    case context, deletion, addition, modification
}

private enum DiffSide: Equatable {
    case left, right
}

struct VisibleDiffRow: Identifiable {
    let index: Int
    let row: DiffRowDto

    var id: Int { index }
}
