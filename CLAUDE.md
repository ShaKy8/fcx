# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is
`fcx` (crates `fc-core` + `fc-gtk`) is a FreeCommander-style dual-pane file manager built in Rust + GTK4 for Omarchy Linux (Arch, Hyprland, Wayland), licensed GPL-3.0-or-later and meant to ship via the AUR. The full design and build order lives in the approved plan at `~/.claude/plans/pasted-content-id-4341-i-m-planning-binary-patterson.md`; follow its step sequence (panes → keymap/selection → tabs/favorites → job engine → multi-rename → search → compare → viewer/archives → Omarchy packaging).

## Commands
```sh
cargo build                          # whole workspace
cargo run -p fc-gtk                  # launch the app (binary is named `fcx`; `fc` is a shell builtin)
cargo test -p fc-core                # core tests (no display needed)
cargo test -p fc-core fs::tests::keeps_non_utf8_names   # single test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```
System deps: `rustup` (stable), plus `gtk4` (already installed on Omarchy).

## Architecture rules
- **`crates/fc-core` never depends on GTK/glib/gio.** All filesystem, job, rename, search, compare, archive, and config logic goes here as plain Rust with `tempfile`-based tests. `crates/fc-gtk` is only UI wiring.
- **The GTK main thread never touches the filesystem.** Listing, stat, copying, and searching run on worker threads and report back through glib channels. Target: stays responsive with 100k+ entry directories.
- **All user-triggerable behavior goes through the named action registry** (`copy`, `move`, `rename`, …). Keys map to action names via `keymap.toml`. **Default bindings must match FreeCommander XE** (the user does not want to relearn them): look up FC's shortcut before assigning a key, and never reuse an FC chord for something else (e.g. Ctrl+Q is FC's quick view, not quit). The source of truth is `crates/fc-core/src/keymap.default.toml`.
- **File operations are jobs:** plan (enumerate) then execute, streaming progress/conflict events, cancellable and pausable, with per-file error handling (skip/retry/abort) instead of whole-job failure.

## Module map (fc-gtk)
`app.rs` owns the window and dispatches every `Action` (keys, menus, toolbar, functions bar, context menu all call `App::run`). `chrome.rs` builds the keymap-driven menu bar/toolbar/places bar/functions bar and the F1 shortcuts list. `host.rs` is one side of the window: a tab bar over a stack of `Pane`s (one full pane per tab). `pane.rs` is a listing: path bar, folder tree, details/list/thumbnail views sharing one `SingleSelection`, quick search, marks. `item.rs`/`row.rs` are the list-model object and its precomputed display strings. `ops.rs` has dialogs, Nautilus-compatible clipboard, trash, and the job runner with its progress panel. `props.rs` is the properties dialog; `favorites.rs` the favorites menu/editor; `settings.rs` the F12 dialog (values live in `fc_core::config::Settings`, saved to `~/.config/fcx/settings.toml`; the tab/window session goes to `~/.local/state/fcx/session.toml` on close and is restored when no folders are given on the command line); `viewer.rs`, `search.rs`, `sync.rs`, `multirename.rs` are the F3/Ctrl+F/Alt+S/Ctrl+M windows; `theme.rs` loads the Omarchy-rendered CSS; `checksums.rs` and `filediff.rs` are the Ctrl+K / Ctrl+Alt+V windows.

## Manual UI testing on this machine
Drive the real app with synthetic input and screenshots (no PIL available):
```sh
./target/debug/fcx /tmp/some/dir /tmp/other &   # G_DEBUG=fatal-criticals to catch GTK criticals
hyprctl dispatch focuswindow class:org.omarchy.fcx
wtype -k F5; wtype -M alt -k Return -m alt      # keys and chords
G=$(hyprctl clients -j | jq -r '.[] | select(.class=="org.omarchy.fcx") | "\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"'); grim -g "$G" shot.png
```
Pointer input (drag-and-drop, clicks): `tools/vmouse.py` creates uinput mouse/keyboard devices — `tools/vmouse.py drag X1 Y1 X2 Y2 [--mod ctrl|shift]`, `click X Y`, `move X Y` in Hyprland logical coordinates (screenshot px ÷ monitor scale + window `at`). It needs `sudo setfacl -m u:$USER:rw /dev/uinput` once per boot (ask the user). Use `--mod`, not `wtype`, for held modifiers: wtype's virtual keyboard is not reflected in pointer-event modifier state. `FCX_DEBUG_DND=1` prints what each drop saw. Hyprland removes keyboard focus during a drag, so Ctrl/Shift are read on the press that starts it (`DragSource::prepare`). Beware the `Paned` divider: measure it from a screenshot before targeting right-pane rows.
Caveats: a virtual *lone* Ctrl press is rewritten to Escape by this machine's keyboard remapper (Ctrl+key combos are fine); `grim` hangs while the screen is locked; set `XDG_CONFIG_HOME` (and `XDG_CACHE_HOME` for archives) to a scratch dir so tests don't touch `~/.config/fcx/`; `FCX_THEME_CSS=path` loads a rendered theme CSS for testing; `/tmp` is tmpfs, where GIO cannot trash (the app offers permanent delete instead).

## Filesystem invariants
- Filenames are `OsString`/`PathBuf` end to end; convert lossily only at display time.
- Never follow symlinks during recursive copy/delete; the walker must detect loops.
- Delete defaults to the freedesktop trash (via GIO in the UI layer); permanent delete requires Shift+Del and confirmation.
- Copy with `std::fs::copy` (uses `copy_file_range`, which reflinks on btrfs, Omarchy's default FS) — don't hand-roll read/write loops. Preserve mode and mtime.
- `rename(2)` returns `EXDEV` across filesystems (e.g. home → `/tmp` tmpfs, USB); moves fall back to copy, verify, then delete source.
- Multi-rename must use two-phase temp names so swaps/cycles (a↔b) succeed, and must reject collisions before touching disk.

## Omarchy integration
- Theming: `packaging/omarchy/fcx.css.tpl` is installed to `~/.config/omarchy/themed/`; Omarchy substitutes `{{ background }}`, `{{ foreground }}`, `{{ accent }}`, `{{ muted }}`, `{{ red }}`… (keys of `colors.toml`, plus `_strip`/`_rgb` modifiers) on theme switch and writes `~/.local/state/omarchy/current/theme/fcx.css`. `theme.rs` loads that file at USER priority and reloads it when it changes. Don't use libadwaita — its stylesheet overrides theme colors.
- The stock file-manager binding (Super+Shift+F → nautilus) is in `/usr/share/omarchy/default/hypr/bindings/applications.lua`; never edit files under `/usr/share/omarchy`, override from the user's config instead.
