# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is
`fc` is a FreeCommander-style dual-pane file manager built in Rust + GTK4 for Omarchy Linux (Arch, Hyprland, Wayland), licensed GPL-3.0-or-later and meant to ship via the AUR. The full design and build order lives in the approved plan at `~/.claude/plans/pasted-content-id-4341-i-m-planning-binary-patterson.md`; follow its step sequence (panes → keymap/selection → tabs/favorites → job engine → multi-rename → search → compare → viewer/archives → Omarchy packaging).

## Commands
```sh
cargo build                          # whole workspace
cargo run -p fc-gtk                  # launch the app (binary is named `fc`)
cargo test -p fc-core                # core tests (no display needed)
cargo test -p fc-core fs::tests::keeps_non_utf8_names   # single test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```
System deps: `rustup` (stable), plus `gtk4` (already installed on Omarchy).

## Architecture rules
- **`crates/fc-core` never depends on GTK/glib/gio.** All filesystem, job, rename, search, compare, archive, and config logic goes here as plain Rust with `tempfile`-based tests. `crates/fc-gtk` is only UI wiring.
- **The GTK main thread never touches the filesystem.** Listing, stat, copying, and searching run on worker threads and report back through glib channels. Target: stays responsive with 100k+ entry directories.
- **All user-triggerable behavior goes through the named action registry** (`copy`, `move`, `rename`, …). Keys map to action names via `keymap.toml`; default bindings are Commander-style (F5 copy, F6 move, F7 mkdir, F8 trash, Shift+Del permanent delete, F2 rename, F3 view, Tab switch pane).
- **File operations are jobs:** plan (enumerate) then execute, streaming progress/conflict events, cancellable and pausable, with per-file error handling (skip/retry/abort) instead of whole-job failure.

## Filesystem invariants
- Filenames are `OsString`/`PathBuf` end to end; convert lossily only at display time.
- Never follow symlinks during recursive copy/delete; the walker must detect loops.
- Delete defaults to the freedesktop trash (via GIO in the UI layer); permanent delete requires Shift+Del and confirmation.
- Copy with `std::fs::copy` (uses `copy_file_range`, which reflinks on btrfs, Omarchy's default FS) — don't hand-roll read/write loops. Preserve mode and mtime.
- `rename(2)` returns `EXDEV` across filesystems (e.g. home → `/tmp` tmpfs, USB); moves fall back to copy, verify, then delete source.
- Multi-rename must use two-phase temp names so swaps/cycles (a↔b) succeed, and must reject collisions before touching disk.

## Omarchy integration
- Theming: ship a `*.tpl` for `~/.config/omarchy/themed/`; Omarchy substitutes `{{ background }}`, `{{ foreground }}`, `{{ accent }}`, `{{ color0 }}`…`{{ color15 }}` (plus `_strip`/`_rgb` modifiers) on theme switch and writes output under `~/.local/state/omarchy/current/`. The app should watch the rendered CSS and reload live. Don't use libadwaita — its stylesheet overrides theme colors.
- The stock file-manager binding (Super+Shift+F → nautilus) is in `/usr/share/omarchy/default/hypr/bindings/applications.lua`; never edit files under `/usr/share/omarchy`, override from the user's config instead.
