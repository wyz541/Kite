# Mindmap import/format acceptance record (Windows host)

Date: 2026-11-27
Scope: #210, #211, #213-#217, #230

## Automated gates completed

The following ran on Windows x64 with `TEMP`/`TMP` moved to `D:\temp\kite-build`:

- `cargo fmt --all -- --check`
- `cargo test --locked --lib mindmap`
- Every `tests/mindmap_*.rs` integration target
- `cargo test --locked --lib context_menu`
- `cargo test --locked --features windows-ui --bin kite-explorer`
- `cargo clippy --locked --all-targets --features windows-ui,agent-fixture -- -D warnings`
- `cargo check --locked --all-targets --features windows-ui,agent-fixture`
- `cargo test --locked --lib -- --test-threads=1` (470 passed, 13 ignored)

Covered behavior includes:

- Five import adapters with normal, edge, malformed, deterministic output,
  warning/stat contracts, performance sanity, and save/reopen round trips.
- Byte-stable fixed corpus under Windows `core.autocrlf=true`.
- Cross-platform canonical tree goldens consumed by the Windows UI parser and the
  extracted macOS Swift canonicalizer.
- Graph structure, connector width/color/pattern/alpha, marker limits, TODO modern
  task fields, dates, font fallback, and node style persistence.
- Conversion preview stats, complete warnings, TXT confidence, overwrite choice,
  and failed-generation atomicity.
- Context-menu action mapping for `.xmind` and convertible documents.

## XMind readback evidence

A generated package containing graph structure, connector style, markers, TODO,
date text, font, fill, border, corner radius, unknown JSON fields, and an unknown
ZIP entry was created at:

```text
D:\Temp\kite-build\audit-combined-final.xmind
```

The package passes ZIP integrity checks. XMind 8 opened it successfully. The
installed XMind 27.1.5088 build was launched, but the interactive session was
locked and its first-run EULA remained unaccepted, so XMind 27.1 visual/file
readback is **not claimed as passed** by this record.

## Remaining release-gate blockers

- Unlock the Windows interactive session and accept/complete the XMind 27.1
  first-run flow; then open `audit-combined-final.xmind` and capture marker, TODO,
  date, connector, font, border, and corner-radius evidence.
- Run the macOS tree-parity smoke and macOS/Windows screenshot comparison on
  macOS CI or hardware.
- Do not close #210, #211, #213-#217, or #230 until those manual platform gates
  are recorded.
