# fcx

A FreeCommander-style dual-pane file manager for [Omarchy](https://omarchy.org)
(Arch Linux, Hyprland, Wayland), written in Rust with GTK4. Keyboard first,
with FreeCommander XE's default shortcuts so nothing has to be relearned.

## Build and run

```sh
cargo build --release -p fc-gtk
./target/release/fcx [LEFT_DIR [RIGHT_DIR]]
```

Requires `gtk4` and `libarchive` (both standard on Omarchy). `fcx` is the
command name because `fc` is a shell builtin.

## Install

Locally:

```sh
cargo build --release -p fc-gtk
ln -sf "$PWD/target/release/fcx" ~/.local/bin/fcx
mkdir -p ~/.local/share/applications ~/.local/share/icons/hicolor/scalable/apps
cp packaging/fcx.desktop ~/.local/share/applications/
cp packaging/fcx.svg ~/.local/share/icons/hicolor/scalable/apps/
xdg-mime default fcx.desktop inode/directory      # optional: make it the folder handler
```

As a package (from this checkout):

```sh
cd packaging && makepkg -si
```

### Omarchy theme colours

```sh
cp packaging/omarchy/fcx.css.tpl ~/.config/omarchy/themed/
```

Omarchy renders the template on every theme switch to
`~/.local/state/omarchy/current/theme/fcx.css`; fcx reloads it live. To apply
it immediately without switching themes, re-apply the current theme
(`omarchy-theme-set "$(cat ~/.local/state/omarchy/current/theme.name)"`).

### Dialogs on Hyprland

Secondary windows (search, synchronize, viewer…) are sized to fit inside the
main window. To centre them on the monitor instead, add to your Hyprland config:

```
windowrule = center, class:^(fcx)$, floating:1
```

### Super+Shift+F

Append to `~/.config/hypr/bindings.lua`:

```lua
hl.unbind("SUPER + SHIFT + F")
o.bind("SUPER + SHIFT + F", "File manager", "setsid uwsm-app -- fcx")
hl.unbind("SUPER + ALT + SHIFT + F")
o.bind("SUPER + ALT + SHIFT + F", "File manager (cwd)", "setsid uwsm-app -- fcx \"$(omarchy-cmd-terminal-cwd)\"")
```

To make it the default handler for folders (what other apps open on "show in folder"):

```sh
xdg-mime default fcx.desktop inode/directory
```

## Keys

Press **F1** inside fcx for the full list. The defaults follow FreeCommander XE:
F2 rename · F3 view · F4 edit · F5 copy · F6 move · F7 new folder · F8/Del trash ·
Ctrl+M multi rename · Ctrl+F / Alt+F7 search · Ctrl+Y quick filter · Alt+V compare ·
Alt+S synchronize · Ctrl+Q quick view · Alt+F5 pack · Alt+F6 unpack · Ctrl+T/W tabs ·
Shift+Ctrl+V add favorite · Alt+Up favorites · Alt+F favorites panel · Alt+Down history ·
Ctrl+B / Shift+Ctrl+B / Ctrl+Alt+B plain view · Ctrl+S show only selected · Ctrl+K checksums ·
Ctrl+Alt+V compare files · F12 settings. Override any of them in
`~/.config/fcx/keymap.toml` (see `crates/fc-core/src/keymap.default.toml`).

## License

GPL-3.0-or-later.
