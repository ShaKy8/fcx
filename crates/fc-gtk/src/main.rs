mod app;
mod chrome;
mod favorites;
mod host;
mod item;
mod multirename;
mod ops;
mod pane;
mod props;
mod row;
mod search;
mod sync;

use std::path::PathBuf;

use fc_core::keymap::Keymap;
use gtk::prelude::*;
use gtk::{Application, gdk, glib};

use crate::app::App;

const APP_ID: &str = "org.omarchy.fc";

const CSS: &str = "
.marked { color: @theme_selected_bg_color; font-weight: bold; }
.pane:not(.active) columnview > listview > row:selected {
    background-color: alpha(@theme_selected_bg_color, 0.35);
}
.pane.active .path-bar { border-color: @theme_selected_bg_color; }
.status-bar { font-size: 0.9em; }
.jobs { border-top: 1px solid alpha(currentColor, 0.2); }
.toolbar, .places-bar { padding: 2px 4px; border-bottom: 1px solid alpha(currentColor, 0.15); }
.toolbar separator { margin: 4px 3px; }
.functions-bar { border-top: 1px solid alpha(currentColor, 0.2); padding: 1px; }
.functions-bar button { padding: 2px 6px; min-height: 0; }
.file-grid { padding: 4px; }
.file-grid.thumbnails > child { padding: 6px; }
.folder-tree { font-size: 0.95em; }
.quick-filter { border-radius: 0; }
.tab-bar { padding: 2px 2px 0 2px; border-bottom: 1px solid alpha(currentColor, 0.15); }
.tab-bar button.tab { padding: 1px 10px; min-height: 0; border-radius: 6px 6px 0 0; }
.tab-bar button.tab:checked { background: alpha(@theme_selected_bg_color, 0.25); }
";

fn main() -> glib::ExitCode {
    // `fc [LEFT [RIGHT]]`: parsed by hand; GApplication would reject unknown arguments.
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let left = args
        .next()
        .and_then(|p| std::path::absolute(p).ok())
        .unwrap_or_else(glib::home_dir);
    let right = args
        .next()
        .and_then(|p| std::path::absolute(p).ok())
        .unwrap_or_else(|| left.clone());

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| install_css());
    app.connect_activate(move |app| {
        // App lives as long as its window; the closure holds it via the Rc.
        let keymap = load_keymap();
        let app = App::new(app, [left.clone(), right.clone()], keymap);
        std::mem::forget(app);
    });
    app.run_with_args::<&str>(&[])
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Defaults layered with `~/.config/fc/keymap.toml` when present. A broken user
/// file is reported on stderr and ignored rather than leaving the app unusable.
fn load_keymap() -> Keymap {
    let mut keymap = Keymap::defaults();
    let path = glib::user_config_dir().join("fc").join("keymap.toml");
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Err(err) = keymap.merge_toml(&text)
    {
        eprintln!("fc: ignoring {}: {err}", path.display());
        keymap = Keymap::defaults();
    }
    keymap
}
