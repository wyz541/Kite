# Kite v0.6.90

## Git client

- Windows and macOS: add a shared Git client window opened with `Ctrl+Shift+G` on Windows and `Command+Shift+G` on macOS.
- Browse changes and inspect file, hunk, and line-level diffs in a dedicated side-by-side viewer.
- Stage, unstage, and discard selected files, hunks, or lines while preserving CRLF content and missing final newlines.
- Commit with amend, sign-off, GPG/SSH signing, skipped hooks, a custom author, commit templates, recent-message reuse, and optional push.
- Manage branches, tags, remotes, Fetch, merge/rebase/fast-forward-only Pull, push previews, and force-with-lease recovery.
- Create full or shallow clones and initialize Git in the current directory without losing existing files.
- Use stash, reset, revert, cherry-pick, and interactive rebase operations across both platforms.
- Follow the repository's Git configuration, credentials, hooks, signing setup, and submodule behavior through the Git CLI.
- Add immediate cancellation and progress reporting for long-running network and history operations.

## Fixes and reliability

- Windows: resolve strict Clippy failures in diff image rendering and UTF-16 span mapping.
- Git workflows require Git 2.30 or newer and surface structured authentication, conflict, rejected-push, and repository-state errors.
- Add repository and operation coverage for clones, remotes, commits, line-level staging, stash/history operations, and rebases.

## Install

- Windows: use `Kite-v0.6.90-windows-x64.zip` or the standalone `Kite.exe`.
- macOS: use `Kite-v0.6.90-macOS-arm64.zip` on Apple Silicon.
- Verify downloads with `SHA256SUMS.txt`.
