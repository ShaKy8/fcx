//! Omarchy theme integration: load the CSS Omarchy renders from
//! `fcx.css.tpl` and reload it whenever the theme changes.
//!
//! Omarchy writes rendered templates to `~/.local/state/omarchy/current/theme/`,
//! swapping that directory on every theme switch, so both the file and its
//! parent are watched. `FCX_THEME_CSS=<file>` overrides the location (tests).

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

const RELOAD_DEBOUNCE: Duration = Duration::from_millis(150);

thread_local! {
    /// Keeps the provider and monitors alive for the life of the process.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

struct State {
    provider: gtk::CssProvider,
    path: PathBuf,
    _monitors: Vec<gio::FileMonitor>,
    pending: Option<glib::SourceId>,
}

fn theme_css_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FCX_THEME_CSS") {
        return PathBuf::from(path);
    }
    glib::home_dir()
        .join(".local/state/omarchy/current/theme")
        .join("fcx.css")
}

/// Call once at startup (after the display exists).
pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let path = theme_css_path();
    let provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    let mut monitors = Vec::new();
    let mut watch = |target: PathBuf| {
        let file = gio::File::for_path(&target);
        if let Ok(monitor) =
            file.monitor(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            monitor.connect_changed(|_, _, _, _| schedule_reload());
            monitors.push(monitor);
        }
    };
    watch(path.clone());
    if let Some(parent) = path.parent() {
        watch(parent.to_path_buf());
        // The `theme` directory itself is replaced on switch; watch its parent too.
        if let Some(grandparent) = parent.parent() {
            watch(grandparent.to_path_buf());
        }
    }

    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            provider,
            path,
            _monitors: monitors,
            pending: None,
        })
    });
    reload();
}

fn schedule_reload() {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        if let Some(id) = state.pending.take() {
            id.remove();
        }
        state.pending = Some(glib::timeout_add_local_once(RELOAD_DEBOUNCE, || {
            STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.pending = None;
                }
            });
            reload();
        }));
    });
}

fn reload() {
    STATE.with(|s| {
        let state = s.borrow();
        let Some(state) = state.as_ref() else {
            return;
        };
        match std::fs::read_to_string(&state.path) {
            Ok(css) => state.provider.load_from_string(&css),
            // No theme file: fall back to plain GTK styling.
            Err(_) => state.provider.load_from_string(""),
        }
    });
}
