# Kite v0.6.91

## Drag and drop uploads

- macOS: offer a real file promise instead of a URL-only drag item, so Safari, Chrome, and other web upload targets receive the file bytes rather than a path string.
- macOS: keep the file URL and Kite's internal multi-selection payload as secondary representations, preserving Finder drops and cut/copy moves inside Kite.
- Windows: retain the Shell `CF_HDROP` drag object and strengthen Chrome/Edge coverage to require a file `DataTransferItem` with matching SHA-256 content.

## Git client

- Add branch and tag management alongside the native commit history graph.
- Show local and remote branches, inferred upstream tracking, and commit refs directly in the history view.
- Improve commit graph rendering and keep unavailable Git actions hidden when a repository cannot support them.

## Reliability

- Add macOS file-drag smoke coverage for real file and folder representations.
- Make Windows drag-upload end-to-end tests fail if the browser sees only text, a URL, or an otherwise non-file drop.
- Preserve CRLF and missing-final-newline content throughout Git and editor workflows.

## Install

- Windows: use `Kite-v0.6.91-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.91-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
