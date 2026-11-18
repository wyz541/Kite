import AppKit
import SwiftUI

enum KiteArchiveOperation: Sendable {
    case compress(format: String)
    case extract(directory: String?)

    var name: String {
        switch self {
        case .compress: "压缩"
        case .extract: "解压"
        }
    }

    func formatName(source: String) -> String {
        switch self {
        case .compress(let format): format.uppercased()
        case .extract: KitePlatform.archiveFormat(source)?.uppercased() ?? "归档"
        }
    }

    func initialDestination(source: String) -> String {
        if case .extract(let directory?) = self { return directory }
        return URL(fileURLWithPath: source).deletingLastPathComponent().path
    }

    func run(core: KiteCore, source: String, monitor: ArchiveProgressMonitor) throws -> String {
        switch self {
        case .compress(let format):
            try core.compressPathWithProgress(path: source, format: format, monitor: monitor)
        case .extract(let directory):
            try core.extractArchiveWithProgress(path: source, destinationDirectory: directory, monitor: monitor)
        }
    }
}

@MainActor
final class KiteArchiveProgressState: ObservableObject {
    enum Outcome {
        case success(destination: String)
        case failure(message: String)
    }

    let monitor = ArchiveProgressMonitor()
    let operation: KiteArchiveOperation
    let source: String
    private let startTime = ProcessInfo.processInfo.systemUptime
    @Published private(set) var outcome: Outcome?
    private var endTime: TimeInterval?

    init(operation: KiteArchiveOperation, source: String) {
        self.operation = operation
        self.source = source
    }

    var elapsed: Int {
        Int((endTime ?? ProcessInfo.processInfo.systemUptime) - startTime)
    }

    func finish(_ outcome: Outcome) {
        endTime = ProcessInfo.processInfo.systemUptime
        self.outcome = outcome
    }
}

@MainActor
final class KiteArchiveProgressWindowController: NSWindowController, NSWindowDelegate {
    private static var openWindows: [UUID: KiteArchiveProgressWindowController] = [:]
    private let id = UUID()

    private init(state: KiteArchiveProgressState) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 280),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Kite \(state.operation.name)进度"
        window.contentMinSize = NSSize(width: 480, height: 280)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        window.contentViewController = NSHostingController(
            rootView: KiteArchiveProgressView(state: state, close: { [weak self] in self?.close() })
        )
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show(operation: KiteArchiveOperation, source: String) -> KiteArchiveProgressState {
        let state = KiteArchiveProgressState(operation: operation, source: source)
        let controller = KiteArchiveProgressWindowController(state: state)
        openWindows[controller.id] = controller
        controller.window?.center()
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
        return state
    }

    func windowWillClose(_ notification: Notification) {
        Self.openWindows.removeValue(forKey: id)
    }
}

private struct KiteArchiveProgressView: View {
    @ObservedObject var state: KiteArchiveProgressState
    let close: () -> Void

    var body: some View {
        if state.outcome == nil {
            TimelineView(.periodic(from: .now, by: 0.2)) { _ in
                content(state.monitor.snapshot())
            }
        } else {
            content(state.monitor.snapshot())
        }
    }

    private func content(_ progress: ArchiveProgressDto) -> some View {
        let percent = percentage(progress)
        return VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text(status(progress.phase)).font(.headline)
                Spacer()
                Text(state.operation.formatName(source: state.source)).foregroundStyle(.secondary)
            }
            progressIndicator(percent)
            HStack {
                Text(amount(progress, percent: percent)).lineLimit(1)
                Spacer()
                Text("用时 \(String(format: "%02d:%02d", state.elapsed / 60, state.elapsed % 60))")
                    .fixedSize()
            }
            .font(.callout.monospacedDigit())
            .foregroundStyle(.secondary)

            Divider()

            pathRow("源项目", state.source)
            pathRow("目标", destination(progress))

            if case .failure(let message) = state.outcome {
                ScrollView {
                    Text(message)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 32, maxHeight: 80)
            }

            Spacer(minLength: 0)
            Button(state.outcome == nil ? "隐藏" : "关闭", action: close)
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .padding(20)
        .frame(minWidth: 480, minHeight: 260)
    }

    @ViewBuilder
    private func progressIndicator(_ percent: Int?) -> some View {
        Group {
            if let percent {
                ProgressView(value: Double(percent), total: 100)
            } else if state.outcome != nil {
                ProgressView(value: 0)
            } else {
                ProgressView()
            }
        }
        .progressViewStyle(.linear)
        .accessibilityLabel(Text("\(state.operation.name)进度"))
    }

    private func pathRow(_ label: String, _ path: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label).foregroundStyle(.secondary).frame(width: 50, alignment: .leading)
            Text(path)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .help(path)
        }
        .font(.callout)
    }

    private func status(_ phase: ArchiveProgressPhaseDto) -> String {
        switch state.outcome {
        case .success: return "\(state.operation.name)完成"
        case .failure: return "\(state.operation.name)失败"
        case nil: break
        }
        switch phase {
        case .preparing: return "正在准备…"
        case .checking: return "正在检查压缩包…"
        case .working: return "正在\(state.operation.name)…"
        case .finishing: return "正在保存压缩包…"
        }
    }

    private func percentage(_ progress: ArchiveProgressDto) -> Int? {
        if case .success = state.outcome { return 100 }
        guard let processed = progress.processedBytes,
              let total = progress.totalBytes, total > 0 else { return nil }
        return Int(min(99, Double(processed) / Double(total) * 100))
    }

    private func amount(_ progress: ArchiveProgressDto, percent: Int?) -> String {
        let percentageText = percent.map { "\($0)%" }
        guard let processed = progress.processedBytes else { return percentageText ?? "" }
        let processedText = ByteCountFormatter.string(fromByteCount: Int64(clamping: processed), countStyle: .file)
        let bytesText = if let total = progress.totalBytes {
            "\(processedText) / \(ByteCountFormatter.string(fromByteCount: Int64(clamping: total), countStyle: .file))"
        } else {
            "已处理 \(processedText)"
        }
        return percentageText.map { "\($0) · \(bytesText)" } ?? bytesText
    }

    private func destination(_ progress: ArchiveProgressDto) -> String {
        if case .success(let destination) = state.outcome { return destination }
        return progress.destination ?? state.operation.initialDestination(source: state.source)
    }
}
