# Mindmap UI tree parity fixture

This fixture is consumed by both native UI parsers:

- Windows: `MindmapTree::from_content_json` in `src/mindmap_ui_model.rs`.
- macOS: `MindmapTreeCanonicalizer.parse` in
  `apps/macos/Sources/MindmapTreeCanonicalizer.swift`.

`content.json` is a modern `content.json` sheet fixture. The checked-in
`expected.canonical.json` and `expected.canonical.sha256` are the shared golden.
Tests on both platforms must produce the same canonical projection and compact
JSON SHA-256. The hash is computed over RFC 8785-style compact JSON: object keys
sorted lexicographically, UTF-8 strings minimally escaped, integral doubles
rendered as integers, sibling order preserved.

## Canonical schema

The top-level projection contains `schema_version`, `structure`,
`structure_class`, `connector`, `root_id`, and a preorder `topics` array.
Every topic contains stable `id`/`parent_id`/`depth`, ordered `children`,
`title`, `shape_class`, ordered `markers`, `opaque_marker_count`, resolved
`style`, and nullable `task` semantics. Paths, timestamps, random UUIDs and
other volatile source fields are deliberately excluded.

XMind 27.1 top-level task fields are parsed directly: `taskPresentation`,
`todoStatus`, `todoProgress`, `taskProgress`, `taskStartDate`, and
`taskDueDate`. Modern fields take precedence over legacy `task-*` markers; the
latter are only a fallback. Both effective and legacy values are retained in
the canonical task object, together with `hasConflict`, so incompatible modern
and legacy state is normalized identically instead of hidden.

## Shared abnormal-input contract

- Missing topic ID: `missing_topic_id`.
- Duplicate topic ID: `duplicate_topic_id`.
- Empty or whitespace-only title: fallback to `未命名主题`.
- Marker entry without a usable non-empty `markerId`: counted in
  `opaque_marker_count`; ordered valid markers are retained.
- Unknown but valid marker ID: retained in order with family `other` and
  `opaque: true`.
- Modern task fields conflicting with a legacy task marker: modern task fields
  win, legacy values remain visible, and `hasConflict` is `true`.

Regenerate the golden only after reviewing the semantic change:

```powershell
$env:UPDATE_MINDMAP_UI_TREE_GOLDEN = "1"
cargo test --locked --test mindmap_windows_model windows_ui_tree_matches_the_shared_macos_canonical_golden
```

The macOS smoke test is:

```bash
bash apps/macos/Tests/run-mindmap-tree-parity-smoke.sh
```
