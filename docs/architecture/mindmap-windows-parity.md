# Mindmap Windows GPUI parity slice

Tracking issue: #230

## Product boundary

The Windows renderer is a real `.xmind` editor, not a preview-only surface. XMind
`content.json` remains authoritative. Every title edit, structural edit, graph
style, marker, TODO, date, and topic style operation passes through the shared
Rust Core functions used by macOS and preserves unknown JSON fields and unknown
ZIP entries.

## Delivered surface

### Entry points

- Main toolbar `Mindmap` button.
- File menu `打开 Mindmap…`.
- Mindmap menu `打开 XMind…`.
- `Ctrl+Shift+M` opens an XMind picker.
- Double-clicking `.xmind` in Kite routes to the Mindmap editor instead of the
  Windows shell.
- `.xmind` context menu exposes `使用 Mindmap 编辑`.
- Supported Markdown, DOCX, TXT, HTML, and OPML files expose
  `转换为 Mindmap…`.
- Toolbar document conversion action and File/Mindmap menu conversion action.

### Editor

- Independent GPUI window.
- Searchable outline with collapse markers and marker/TODO summaries.
- Canvas with selection, practical pan/zoom, fit, center, inspector toggle, and
  Zen mode.
- Inline title input with IME-capable GPUI `Input`.
- Create sibling, child, preceding sibling, and parent.
- Delete, move up/down, copy/cut/paste topic subtrees, and paste plain text as a
  child topic.
- Undo/redo snapshots replayed through `save_mindmap_content`, retaining the
  same external-change checks as normal edits.
- Atomic Core save after each mutation; `Ctrl+S` performs an explicit package
  save check.

### Format controls

- Mind map, logic right/left, org chart, and tree layout.
- Curved, straight, elbow, and rounded-elbow connector rendering.
- Connector corner radius.
- Priority P1-P3 and clear.
- Task progress start/25/50/75/100 percent and clear.
- Flag, star, and tag generic markers.
- TODO completion toggle and completed styling.
- Date inserted into title or note with ISO/Chinese/English/System display modes.
- Font family, size, bold, italic, underline, strike, text/fill/border colors,
  border width, and node corner radius.
- Copy and paste topic style in addition to whole-subtree clipboard operation.

### Conversion flow

- Detects supported source format before opening.
- Selects structure-first, expanded-content, strict-outline, or sentence-split
  presets.
- Controls body splitting, paragraph-note preservation, short-fragment merging,
  table/code/image strategies.
- Shows node/structure/content counts, maximum depth, notes, warnings, and a
  bounded sample tree.
- Uses a platform save dialog for output, refuses implicit overwrite, validates
  the preview's source SHA-256 before generating, and opens the generated XMind.
- Cancellation does not write a partial `.xmind`.

## Shared keyboard map

Context: `KiteMindmap`.

| Action | Windows |
| --- | --- |
| New sibling | `Enter` |
| New child | `Tab` |
| Insert preceding sibling | `Shift+Enter` |
| New parent | `Ctrl+Shift+Enter` |
| Inline edit | `F2`, `Space` |
| Delete | `Delete`, `Backspace` |
| Reorder | `Alt+Up`, `Alt+Down` |
| Copy/cut/paste subtree | `Ctrl+C`, `Ctrl+X`, `Ctrl+V` |
| Paste plain text as topic | `Ctrl+Shift+V` |
| Copy/paste style | `Ctrl+Alt+C`, `Ctrl+Alt+V` |
| Zoom/reset/fit | `Ctrl++`, `Ctrl+-`, `Ctrl+0`, `Ctrl+F5` |
| Center root | `Ctrl+Home` |
| Toggle/expand branch | `/`, `Shift+8` |
| Zen mode | `F8` |
| Toggle inspector | `Ctrl+Alt+F` |
| Search outline | `Ctrl+F` |
| Save/undo/redo | `Ctrl+S`, `Ctrl+Z`, `Ctrl+Y` / `Ctrl+Shift+Z` |

Relation, boundary, summary, note, and hyperlink shortcuts are bound to explicit
status messages. The current Windows slice does not claim these editing surfaces
are implemented.

## Current gaps

- Windows runtime interaction and screenshot acceptance in the UTM VM.
- XMind 27.01 visual readback for the Windows-generated document.
- Relation lines, boundaries, summaries, rich note editing, and hyperlink editing.
- Exact XMind theme inheritance and text measurement parity.
- Live preview refresh after changing every numeric option without reopening the
  conversion window; the current UI refreshes on strategy/preset changes.
- Parent-TODO progress aggregation and dynamic date tokens.

These gaps mean #210, #211, and #213-#217 remain open even when this slice builds.

## Validation

Validation status is recorded in the PR and issue. A compile-only Windows result
must not be described as runtime or XMind visual acceptance.
