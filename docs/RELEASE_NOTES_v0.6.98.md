# Kite v0.6.98

## Context menu 2.0

- Replace scattered, hand-written context menus with one cross-platform action Composer shared by file, directory, multi-select, background, search, sidebar, preview, and device surfaces.
- Keep the Kite menu as the primary action plane and expose the full OS-native extension menu as a stable final escape hatch.
- Separate default Open, Open With, Open In, and Compare With, including compatible-application discovery, recent choices, tool categories, and exact argv launches.
- Add state-driven Git and SVN menus for repository, worktree, index, conflict, recovery, ahead/behind, and cleanup states.
- Freeze target snapshots when a menu opens, hide inapplicable single-target actions for multi-select menus, and validate targets before dispatch.
- Remove duplicate Git entries and stabilize action ordering, visibility, risk placement, and disabled reasons across Windows and macOS.

## Mindmap import and formatting

- Add shared document import for Markdown, DOCX, TXT, HTML, and OPML into new `.xmind` files without overwriting source documents.
- Add sentence-aware body splitting, paragraph preservation, heuristic text hierarchy, conversion previews, warnings, and deterministic XMind output.
- Add document structure, connector styles, markers, TODO completion, date insertion, system font controls, and topic text/node styling.
- Preserve unknown XMind JSON and ZIP data while applying supported format operations atomically.
- Add Mindmap editor entry points and macOS UI for the new import and formatting capabilities.

## Git workspace and Agent UX

- Add scoped Git Graph workspaces with repository, directory, and file history contexts and a reusable diff viewer.
- Refactor the Agent panel into a conversation transcript with a fixed composer and inline permission cards.
- Complete keyboard focus traversal for Agent permission controls and context chips on macOS, including Tab/Shift+Tab and Return activation.

## Verification

- Shared Rust library suite: 443 passed, 0 failed, 8 ignored.
- macOS SwiftUI/AppKit build and Context Menu, Open With, and Tool Registry smoke tests passed.
- GitHub CI passed Linux checks, macOS SwiftUI/UniFFI smoke, Windows GPUI, Windows and macOS startup, and platform parity.
- Release workflow builds and verifies the Windows ZIP/standalone EXE and macOS Apple Silicon ZIP with combined SHA-256 checksums.

## Known limitations

- Real Windows Shell extensions, MTP/UWP associations, and third-party tool launches still require machine-specific manual validation.
- macOS Services are not a complete Finder analogue of the Windows `IContextMenu` extension plane.
- Windows file-list rows retain the existing single-row selection model; frozen multi-target context actions are available in column mode and on macOS.
- Windows GPUI Mindmap import and formatting UI, plus final XMind 27 visual round-trip validation, remain follow-up work.

## Install

- Windows: use `Kite-v0.6.98-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.98-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
