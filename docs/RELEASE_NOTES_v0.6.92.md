# Kite v0.6.92

## Column search

- Rank column-search results by exact name, prefix, separator segment, then substring match, with stable tie-breaking by depth, position, and name length.
- Skip generated directories such as `node_modules`, `target`, `build`, `dist`, and `.git` during recursive column searches.
- Narrow complete result sets locally as the query grows and keep the previous results visible while a new recursive search runs.
- Show relative directories in result rows while preserving grouping and ranked order within each group.
- Add keyboard navigation across grouped results, Enter and Ctrl/Cmd+Enter behavior, and Esc focus restoration.
- Keep the search column visible and float its preview over columns to the right instead of truncating the column strip.
- Preserve an existing preview, restore selections hidden by a query, and rerun active searches after file-operation refreshes.

## Reliability

- Add core coverage for search ranking, generated-directory exclusions, local narrowing, grouped results, keyboard steps, and selection restoration.
- Keep Windows and macOS on the same ranked search results from the shared Core.

## Install

- Windows: use `Kite-v0.6.92-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.92-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
