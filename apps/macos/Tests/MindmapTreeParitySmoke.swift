import Darwin
import Foundation

@main
struct MindmapTreeParitySmoke {
    static func main() throws {
        guard CommandLine.arguments.count == 2 else {
            fail("usage: MindmapTreeParitySmoke <repository-root>")
        }
        let repositoryRoot = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let fixtureRoot = repositoryRoot
            .appendingPathComponent("tests/fixtures/mindmap/ui-tree-parity", isDirectory: true)
        let content = try String(
            contentsOf: fixtureRoot.appendingPathComponent("content.json"),
            encoding: .utf8
        )
        let parsed = try MindmapTreeCanonicalizer.parse(contentJSON: content)
        let actual = try parsed.canonicalJSONData()
        let expectedData = try Data(
            contentsOf: fixtureRoot.appendingPathComponent("expected.canonical.json")
        )
        let expectedObject = try JSONSerialization.jsonObject(with: expectedData)
        let expected = try MindmapCanonicalJSONWriter.compactData(expectedObject)
        guard actual == expected else {
            fail("macOS UI parser canonical JSON differs from the committed Windows/macOS golden")
        }

        let expectedHash = try String(
            contentsOf: fixtureRoot.appendingPathComponent("expected.canonical.sha256"),
            encoding: .utf8
        ).trimmingCharacters(in: .whitespacesAndNewlines)
        let actualHash = try parsed.canonicalSHA256()
        guard actualHash == expectedHash else {
            fail("macOS UI parser canonical SHA-256 differs from the committed golden")
        }

        guard parsed.structure == "logic-right" else {
            fail("structure fallback drifted: \(parsed.structure)")
        }
        guard parsed.topics["alpha"]?.opaqueMarkerCount == 2 else {
            fail("opaque marker count drifted")
        }
        guard parsed.topics["empty-title"]?.title == "未命名主题" else {
            fail("empty title fallback drifted")
        }
        guard parsed.topics["alpha"]?.markers.map(\.id) == [
            "priority-1", "flag-red", "future-marker",
        ] else {
            fail("marker order or unknown marker retention drifted")
        }
        guard let task = parsed.topics["modern-task"]?.task else {
            fail("modern XMind 27.1 task fields were not parsed")
        }
        guard task.effectiveStatus == "done",
              task.effectiveProgress == 1.0,
              task.legacyStatus == "not-started",
              task.hasConflict,
              task.taskStartDate == "2026-11-27",
              task.taskDueDate == "2026-11-30" else {
            fail("modern task precedence or conflict normalization drifted")
        }
        guard let stringProgressTask = parsed.topics["task-string-progress"]?.task,
              stringProgressTask.effectiveStatus == "paused",
              stringProgressTask.effectiveProgress == 0.5,
              stringProgressTask.todoProgress == 0.5 else {
            fail("string percentage task progress normalization drifted")
        }

        try verifyErrors(fixtureRoot: fixtureRoot)
        print("Mindmap UI tree parity smoke passed: \(expectedHash)")
    }

    private static func verifyErrors(fixtureRoot: URL) throws {
        let expectedData = try Data(
            contentsOf: fixtureRoot.appendingPathComponent("expected-errors.json")
        )
        let expectedObject = try JSONSerialization.jsonObject(with: expectedData)
        guard let expected = expectedObject as? [String: String] else {
            fail("expected-errors.json must be a string map")
        }
        for fixture in expected.keys.sorted() {
            let path = fixtureRoot.appendingPathComponent("invalid").appendingPathComponent(fixture)
            let content = try String(contentsOf: path, encoding: .utf8)
            do {
                _ = try MindmapTreeCanonicalizer.parse(contentJSON: content)
                fail("\(fixture) unexpectedly parsed")
            } catch let error as MindmapTreeCanonicalizationError {
                guard error.code == expected[fixture] else {
                    fail("\(fixture): expected \(expected[fixture]!), got \(error.code)")
                }
            }
        }
    }

    private static func fail(_ message: String) -> Never {
        FileHandle.standardError.write(Data("Mindmap UI tree parity smoke failed: \(message)\n".utf8))
        exit(1)
    }
}
