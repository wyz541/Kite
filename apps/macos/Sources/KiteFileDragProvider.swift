import AppKit
import Foundation
import UniformTypeIdentifiers

enum KiteInternalDragOperation: Equatable {
    case cut
    case copy
}

enum KiteFileDragProvider {
    static let contentType = UTType(exportedAs: "com.wyz541.kite.file-items")

    static func internalDragOperation(copyRequested: Bool) -> KiteInternalDragOperation {
        copyRequested ? .copy : .cut
    }

    private struct FileDragPayload: Codable {
        let paths: [String]
    }

    static func makeProvider(paths: [String], primaryPath: String) -> NSItemProvider {
        var seen = Set<String>()
        let uniquePaths = paths.filter { seen.insert($0).inserted }
        let primaryURL = URL(fileURLWithPath: primaryPath)
        let primaryType = fileContentType(for: primaryURL)

        // Register a real file promise first. A file URL item only tells browsers
        // where the item lives, so upload widgets can receive a path string
        // instead of the file bytes. The promise gives WebKit and native drop
        // targets an actual file to copy, while the URL representation below
        // keeps Finder and other file-URL consumers working.
        let provider = NSItemProvider()
        provider.suggestedName = primaryURL.lastPathComponent
        provider.registerFileRepresentation(
            forTypeIdentifier: primaryType.identifier,
            fileOptions: [],
            visibility: .all
        ) { completion in
            completion(primaryURL, false, nil)
            return nil
        }
        provider.registerItem(forTypeIdentifier: UTType.fileURL.identifier) {
            completion, _, _ in
            completion?(primaryURL as NSURL, nil)
        }
        provider.registerDataRepresentation(
            forTypeIdentifier: contentType.identifier,
            visibility: .all
        ) { completion in
            do {
                let data = try JSONEncoder().encode(FileDragPayload(paths: uniquePaths))
                completion(data, nil)
            } catch {
                completion(nil, error)
            }
            return nil
        }
        return provider
    }

    private static func fileContentType(for url: URL) -> UTType {
        let values = try? url.resourceValues(forKeys: [.contentTypeKey, .isDirectoryKey])
        if values?.isDirectory == true {
            return .folder
        }
        if let contentType = values?.contentType, !contentType.isDynamic {
            return contentType
        }
        if let contentType = UTType(filenameExtension: url.pathExtension), !contentType.isDynamic {
            return contentType
        }
        return .data
    }

    static func loadInternalPaths(
        from provider: NSItemProvider,
        completion: @escaping ([String]?) -> Void
    ) {
        provider.loadDataRepresentation(
            forTypeIdentifier: contentType.identifier
        ) { data, _ in
            guard let data else {
                completion(nil)
                return
            }
            let payload = try? JSONDecoder().decode(FileDragPayload.self, from: data)
            completion(payload?.paths)
        }
    }
}
