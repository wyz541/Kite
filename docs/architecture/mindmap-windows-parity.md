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
- System font discovery, size, bold, italic, underline, strike, text/fill/border
  colors, border width/pattern, node corner radius, alignment, line height, and
  letter-spacing controls. Missing fonts remain persisted while the canvas falls
  back to a system font and shows a warning.
- Copy and paste topic style in addition to whole-subtree clipboard operation.
- XMind 27.1 top-level task fields are consumed alongside legacy `task-*`
  markers; TODO edits keep both representations synchronized while retaining
  unknown task fields.

### Conversion flow

- Detects supported source format before opening.
- Selects structure-first, expanded-content, strict-outline, or sentence-split
  presets.
- Controls body splitting, paragraph-note preservation, short-fragment merging,
  table/code/image strategies.
- Shows node/structure/content counts, maximum depth, notes, warnings, split and
  merge statistics, skipped items, TXT structure confidence, and a bounded sample
  tree.
- `RowsAsTopics` creates one child topic per table data row while retaining
  caption/header context.
- Uses a platform save dialog for output, exposes an explicit overwrite option,
  validates the preview's source SHA-256 before generating, and opens the
  generated XMind.
- Cancellation does not write a partial `.xmind`; failed generation preserves an
  existing target.

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

- Windows runtime screenshot acceptance and macOS/Windows screenshot comparison.
- XMind 27.1 actual file/visual readback; the installed 27.1 build is currently
  gated by its first-run EULA on the acceptance machine.
- macOS Swift tree-parity smoke has been added but must execute on macOS CI or
  hardware; this Windows host cannot compile SwiftUI.
- Relation lines, boundaries, summaries, rich note editing, and hyperlink editing.
- GPUI does not expose native letter-spacing or dotted border primitives; current
  rendering is an explicit visual approximation while persisted semantics remain.
- XMind 27.1 did not expose confirmed `fo:line-height` or `fo:letter-spacing`
  handling in static resource inspection, so those fields are marked experimental.
- Parent-TODO progress aggregation and dynamic date tokens.

These gaps mean #210, #211, and #213-#217 remain open even when this slice builds.

## Validation

Validation status is recorded in the PR and issue. A compile-only Windows result
must not be described as runtime or XMind visual acceptance.
