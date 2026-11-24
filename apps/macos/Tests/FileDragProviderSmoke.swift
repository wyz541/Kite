import AppKit
import Foundation
import UniformTypeIdentifiers

@main
enum FileDragProviderSmoke {
    static func main() async throws {
        guard KiteFileDragProvider.internalDragOperation(copyRequested: false) == .cut,
              KiteFileDragProvider.internalDragOperation(copyRequested: true) == .copy else {
            throw SmokeError("internal drag must use cut + paste or copy + paste")
        }

        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("kite-file-drag-smoke-\(UUID().uuidString)")
        let file = root.appendingPathComponent("sample.txt")
        let folder = root.appendingPathComponent("Sample Folder", isDirectory: true)
        let nested = folder.appendingPathComponent("nested.txt")

        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        try Data("complete file\n".utf8).write(to: file)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try Data("complete folder\n".utf8).write(to: nested)

        try await verify(url: file)
        try await verify(url: folder)
        print("FileDragProviderSmoke passed")
    }

    private static func verify(url: URL) async throws {
        let provider = KiteFileDragProvider.makeProvider(
            paths: [url.path],
            primaryPath: url.path
        )
        let types = provider.registeredTypeIdentifiers
        let expectedType = dragContentType(of: url)
        let primaryTypeIndex = types.firstIndex(of: expectedType.identifier)
        let fileTypeIndex = types.firstIndex(of: UTType.fileURL.identifier)
        let internalTypeIndex = types.firstIndex(of: KiteFileDragProvider.contentType.identifier)

        guard provider.suggestedName == url.lastPathComponent,
              let primaryTypeIndex,
              primaryTypeIndex == types.startIndex,
              let fileTypeIndex,
              let internalTypeIndex,
              fileTypeIndex < internalTypeIndex,
              internalTypeIndex == types.count - 1 else {
            throw SmokeError("real content must precede file URL and internal drag types: \(types)")
        }

        // Web upload widgets require a file promise. A URL-only representation
        // gives them a path string, while a file representation gives them bytes.
        try await verifyFileRepresentation(
            from: provider,
            typeIdentifier: expectedType.identifier,
            originalURL: url
        )

        let loadedItem = try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<NSSecureCoding, Error>) in
            provider.loadItem(
                forTypeIdentifier: UTType.fileURL.identifier,
                options: nil
            ) { item, error in
                if let error {
                    continuation.resume(throwing: error)
                } else if let item {
                    continuation.resume(returning: item)
                } else {
                    continuation.resume(throwing: SmokeError("file URL item was missing"))
                }
            }
        }
        guard let loadedURL = dragURL(from: loadedItem),
              loadedURL.standardizedFileURL == url.standardizedFileURL else {
            throw SmokeError("file URL representation did not point to the original item")
        }
        try verifyContents(of: loadedURL, like: url)

        let paths = await withCheckedContinuation { continuation in
            KiteFileDragProvider.loadInternalPaths(from: provider) {
                continuation.resume(returning: $0)
            }
        }
        guard paths == [url.path] else {
            throw SmokeError("internal drag payload did not preserve its paths")
        }
    }

    private static func verifyFileRepresentation(
        from provider: NSItemProvider,
        typeIdentifier: String,
        originalURL: URL
    ) async throws {
        try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<Void, Error>) in
            provider.loadFileRepresentation(forTypeIdentifier: typeIdentifier) {
                loadedURL, _, error in
                if let error {
                    continuation.resume(throwing: error)
                    return
                }
                guard let loadedURL else {
                    continuation.resume(throwing: SmokeError("file representation was missing"))
                    return
                }
                do {
                    // The URL may be a temporary materialization. Verify its
                    // bytes before the completion handler returns and releases it.
                    try verifyContents(of: loadedURL, like: originalURL)
                    continuation.resume()
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    private static func dragContentType(of url: URL) -> UTType {
        let values = try? url.resourceValues(forKeys: [.isDirectoryKey])
        if values?.isDirectory == true {
            return .folder
        }
        return UTType(filenameExtension: url.pathExtension) ?? .data
    }

    private static func dragURL(from item: NSSecureCoding) -> URL? {
        if let url = item as? URL {
            return url
        }
        if let url = item as? NSURL {
            return url as URL
        }
        if let data = item as? Data {
            return URL(dataRepresentation: data, relativeTo: nil)
        }
        return nil
    }

    private static func verifyContents(of url: URL, like originalURL: URL) throws {
        let values = try? originalURL.resourceValues(forKeys: [.isDirectoryKey])
        if values?.isDirectory == true {
            let nested = url.appendingPathComponent("nested.txt")
            let loaded = try Data(contentsOf: nested)
            guard loaded == Data("complete folder\n".utf8) else {
                throw SmokeError("folder contents were incomplete")
            }
        } else {
            let loaded = try Data(contentsOf: url)
            guard loaded == Data("complete file\n".utf8) else {
                throw SmokeError("file contents were incomplete")
            }
        }
    }

}

private struct SmokeError: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}
