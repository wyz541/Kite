# Context Menu v1 — Cross-platform acceptance

## Scope and links

- Discussion: [#220](https://github.com/wyz541/Kite/discussions/220)
- Parent Feature: [#221](https://github.com/wyz541/Kite/issues/221)
- Acceptance issue: [#228](https://github.com/wyz541/Kite/issues/228)
- Canonical spec: [`docs/architecture/context-menu-spec.md`](architecture/context-menu-spec.md)
- Architecture: [`docs/architecture/context-menu.md`](architecture/context-menu.md)
- Draft PR: [#233](https://github.com/wyz541/Kite/pull/233)
- Feature slices: #222, #223, #224, #225, #226, #227

This document records automated evidence separately from manual UI evidence. A passed automated test does not prove visual, screen-reader, native-extension, or third-party application behavior.

## Accepted product decisions

1. Kite owns the primary menu. `system.native-menu` is a fixed, final, separately labelled escape hatch.
2. `open.default`, `open.with`, `open.in`, and `compare.with` remain distinct action identities and scopes.
3. Git/SVN use Kite's state-driven menu for modelled actions and the OS native plane for external or advanced commands.
4. A menu is composed from a frozen context snapshot. It must not leak entity actions into background menus or single-target actions into multi-select menus.
5. Every visible action has a stable `ActionId`; duplicate action instances are rejected. Dynamic variants use stable `variant_key` values.
6. The native entry remains visible when unavailable, but is disabled with `NATIVE_PLANE_UNAVAILABLE` rather than silently disappearing or pretending to work.

## Context × platform matrix

| Context | Automated Composer coverage | Windows UI/native validation | macOS UI/native validation |
|---|---|---|---|
| Single file | Covered: default/open-with/open-in/compare scopes, file actions, native last | Required | Required |
| Single directory | Covered: directory actions, no archive/file-only leakage | Required | Required |
| Multi-file selection | Covered: frozen order, single-target actions hidden, destructive risk promoted | Required; file list remains single-row | Required |
| Background | Covered: zero Entity targets, create/paste/VCS container actions only | Required | Required |
| Search result | Covered: identity-only actions; container mutations suppressed | Required | Required |
| Sidebar location/favorite/volume | Covered: separate surface allowlists | Required | Required |
| Preview “more” menu | Covered: file actions without background creation/paste | Required | Required |
| Device item/container | Covered: namespace capabilities, no local trash/rename leakage | Required on MTP/portable device | Not applicable; no MTP namespace UI |
| Git clean/dirty/staged/untracked/ignored | Covered with real temporary repositories | Required | Required |
| Git conflict/rebase/cherry-pick/revert/bisect | Covered: recovery controls and busy state | Required | Required |
| Git detached/unborn/ahead/behind | Covered | Required | Required |
| SVN versioned/unversioned/property/conflict/cleanup | Covered by XML fixtures; live test opportunistically skips without `svn` | Required | Required |
| Open With loading/ready/unavailable | Covered: default → recent → remaining → chooser; chooser stays enabled while loading | Required, including UWP/packaged handlers | Required |
| Open In / Compare With | Covered: capability matching, policy order, single-tool promotion, argv-only recipes | Required | Required |
| Missing/changed target at dispatch | Covered by target-kind validation tests and platform adapters | Required | Required |

## Automated evidence

Environment:

- macOS host, `aarch64-apple-darwin`
- Rust stable path supplied by the acceptance task
- PR #233 CI is the authoritative full Windows build; local Windows MSVC cross-build may be unavailable

| Check | Exact command | Result |
|---|---|---|
| Formatting | `cargo fmt --all -- --check` | PASS |
| Diff whitespace | `git diff --check` | PASS |
| Full library suite (parallel, final) | `cargo test --locked --lib` | PASS (443/443, 8 ignored). One earlier run hit the known ACP HTTP probe flake; the probe passed in isolation and the final parallel run passed. |
| Full library suite (serial) | `RUST_TEST_THREADS=1 cargo test --locked --lib` | PASS (443/443, 8 ignored) |
| ACP probe isolation | `cargo test --locked --lib probes_a_local_acp_http_endpoint -- --nocapture --test-threads=1` | PASS (1/1) |
| Context menu/FFI | `cargo test --locked --lib context_menu -- --nocapture` | PASS (31/31) |
| Canonicalization regression | `cargo test --locked --lib tampered_dto_target_is_ignored -- --nocapture` | PASS (1/1) |
| Clippy | `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| macOS build | `KITE_BUILD_PROFILE=debug bash apps/macos/build.sh` | PASS |
| Context-menu plan smoke | `bash apps/macos/Tests/run-context-menu-plan-smoke.sh` | PASS (`ContextMenuPlanSmoke passed`) |
| Open With smoke | `bash apps/macos/Tests/run-open-with-smoke.sh` | PASS (`OpenWithSmoke passed`) |
| Tool registry smoke | `bash apps/macos/Tests/run-tool-registry-smoke.sh` | PASS (`ToolRegistrySmoke passed`) |
| Windows GNU check | `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc cargo check --locked --bin kite-explorer --target x86_64-pc-windows-gnu --features windows-ui,agent-fixture` | PASS |
| Windows GNU clippy | `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc cargo clippy --locked --all-targets --target x86_64-pc-windows-gnu --features windows-ui,agent-fixture -- -D warnings` | PASS |
| PR #233 CI | `gh pr checks 233` | All checks PASS for PR head `4b1566d`: Linux checks, macOS Swift UniFFI smoke, Windows GPUI, Platform parity, Startup macos-arm64, Startup windows-x64. This local #228 commit was not pushed, so CI must be re-run on the next push. |

### Automated invariants now enforced

- Action instance keys are unique across the whole plan, not merely within one group.
- Enabled duplicate candidates beat disabled ones; explicit candidate precedence beats Provider order.
- Background menus reject Entity/Collection scopes.
- Multi-target menus reject single-target rules.
- High-risk actions must be in `G90`.
- `system.native-menu` is unique, in `G99`, and the final root entry.
- Safe fallback also preserves the native entry.
- Every action and submenu has a non-empty accessible label.
- Every disabled action has a stable reason code and non-empty user-facing reason.
- Ordinary root entries have a hard limit of 16; empty groups/submenus and single-item shells are rejected.
- FFI projection preserves native-last ordering and unavailable-native reason data.
- VCS writes carry and revalidate `state_revision`.
- Tool launches use typed argv recipes rather than shell strings.

## Manual validation still required

The following checks have not been performed on real Windows or macOS UI in this task and must not be reported as passed:

### Windows 10 and 11

- Mouse hover opens “Show more options” without executing the leaf action or hiding the Kite menu prematurely.
- `IContextMenu2/3` extensions, including TortoiseGit/TortoiseSVN when installed, open, cancel, and execute repeatedly without `FAST_FAIL_FATAL_APP_EXIT`.
- Keyboard traversal, Escape, arrow keys, first-character navigation, long-menu scrolling, and screen-reader labels/reasons.
- Open With with real file associations, UWP/packaged handlers, “Choose another app”, and stale/uninstalled recent applications.
- MTP/portable-device item versus container capabilities, Properties, multi-item native menu, and permanent-delete confirmation.
- Open In terminal/editor/IDE launches and Compare With diff/merge launches with actual installed tools.
- Visual grouping and HIG behavior at normal, narrow, and screen-edge positions.

### macOS

- Standard SwiftUI/AppKit menu keyboard behavior, Escape, submenu expansion, long-menu scrolling, and VoiceOver labels/reasons.
- AppKit Services availability and the unavailable-native fallback reason.
- Open With compatible application list, default application, recent ordering, chooser, and stale application removal.
- Actual Terminal/editor/IDE/diff tool launches.
- Visual grouping, overflow, and target behavior on file, directory, multi-select, background, search, sidebar, favorite, volume, and preview surfaces with real input.
- Compare visual hierarchy with the Windows implementation while preserving platform HIG.

### Performance evidence

Structural bounds are automated, but end-to-end latency has not been measured:

- Right-click first-interactive latency.
- Open With application discovery blocking time.
- Tool discovery blocking time.
- Git/SVN snapshot load on menu open.
- Native menu open/cancel/invoke latency.
- Long-menu scroll and keyboard traversal performance.

The menu snapshot currently loads VCS/tool/application catalogs during projection. The code uses bounded discovery and caches where implemented, but no release budget has been proven by a repeatable UI benchmark.

## Known limitations

- Windows file-list rows still expose a single-row selection model; Windows column mode and macOS selections support frozen multi-target actions.
- macOS's native plane is AppKit Services, not a complete Finder analogue of Windows `IContextMenu`.
- Shell extension, MTP, packaged/UWP association, and third-party tool behavior cannot be proven by synthetic tests.
- Automated tests intentionally do not launch third-party applications or manipulate real file associations.
- Full Windows CI remains the authority because the macOS host cannot always provide the MSVC toolchain required by native Windows dependencies.
- The full Rust library suite has an ACP HTTP probe that can fail under parallel resource load; it passes in isolation and is unrelated to context menus.
- Screenshot, VoiceOver/Narrator, visual regression, and latency evidence remain release gates rather than completed acceptance evidence.

## Release-facing summary

Kite v1 replaces scattered hand-written context menus with one cross-platform action Composer. File, directory, multi-select, background, search, sidebar, preview, device, and VCS surfaces now share stable action IDs and frozen targets. Open With, Open In, Compare With, and Git/SVN recovery controls have explicit boundaries. Third-party and advanced shell capability remains available through a final native-menu escape hatch instead of being duplicated inside Kite.

## Completion decision

The implementation and automated regression evidence are ready for review, but the feature should not be called fully accepted or release-complete until:

1. The local #228 commit is pushed and PR #233 CI is re-run; the current all-pass CI result applies to head `4b1566d` before this acceptance commit.
2. Windows and macOS manual interaction/accessibility/native-integration evidence is attached.
3. The performance checks above are run against a recorded baseline.
