//! The main window: two panes side by side, an active-pane pointer, the jobs
//! panel, and the key → chord → action dispatcher. Every keyboard feature goes
//! through [`App::run`], so menus and a command palette can reuse it later.

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use fc_core::action::Action;
use fc_core::jobs::{JobSpec, Operation};
use fc_core::keymap::{Chord, Keymap, Mods};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops::{self, JobRunner};
use crate::pane::{Pane, SortColumn, expand_path};

pub struct App {
    weak: Weak<App>,
    window: gtk::ApplicationWindow,
    panes: [Pane; 2],
    active: Cell<usize>,
    keymap: Keymap,
    runner: JobRunner,
}

impl App {
    pub fn new(gtk_app: &gtk::Application, start: [PathBuf; 2], keymap: Keymap) -> Rc<Self> {
        let panes = [Pane::new(), Pane::new()];
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(panes[0].widget())
            .end_child(panes[1].widget())
            .resize_start_child(true)
            .resize_end_child(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .vexpand(true)
            .build();

        let window = gtk::ApplicationWindow::builder()
            .application(gtk_app)
            .title("fc")
            .default_width(1200)
            .default_height(760)
            .build();
        let runner = JobRunner::new(window.upcast_ref());
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        layout.append(&split);
        layout.append(runner.widget());
        window.set_child(Some(&layout));
        // No explicit position: with both children resizable GTK splits the width evenly.

        let app = Rc::new_cyclic(|weak: &Weak<App>| App {
            weak: weak.clone(),
            window,
            panes,
            active: Cell::new(0),
            keymap,
            runner,
        });

        for (i, pane) in app.panes.iter().enumerate() {
            let weak = app.weak.clone();
            pane.connect_focus_enter(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_active(i, false);
                }
            });
            let weak = app.weak.clone();
            pane.connect_drop(move |paths, forced| {
                if let Some(app) = weak.upgrade() {
                    app.dropped(i, paths, forced);
                }
            });
        }
        let weak = app.weak.clone();
        app.runner.connect_finished(move || {
            if let Some(app) = weak.upgrade() {
                app.reload_all();
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = app.weak.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(app) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            app.on_key(key, state)
        });
        app.window.add_controller(keys);

        let [left, right] = start;
        app.panes[0].navigate(left, None);
        app.panes[1].navigate(right, None);
        app.set_active(0, true);
        app.window.present();
        app
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        // While typing in an entry, only the entry's own handling applies.
        if GtkWindowExt::focus(&self.window).is_some_and(|w| w.is::<gtk::Text>()) {
            return glib::Propagation::Proceed;
        }
        let Some(chord) = chord_for(key, state) else {
            return glib::Propagation::Proceed;
        };
        match self.keymap.lookup(&chord) {
            Some(action) => {
                self.run(action);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    pub fn run(&self, action: Action) {
        let pane = self.active_pane();
        match action {
            Action::SwitchPane => self.set_active(1 - self.active.get(), true),
            Action::GoUp => pane.go_up(),
            Action::GoRoot => pane.go_root(),
            Action::Back => pane.back(),
            Action::Forward => pane.forward(),
            Action::FocusPath => pane.focus_path_entry(),
            Action::Reload => pane.reload(),
            Action::ToggleHidden => pane.toggle_hidden(),
            Action::OpenInLeft => self.open_in(0),
            Action::OpenInRight => self.open_in(1),
            Action::SwapPanes => {
                if let (Some(a), Some(b)) = (self.panes[0].cwd(), self.panes[1].cwd()) {
                    self.panes[0].navigate(b, None);
                    self.panes[1].navigate(a, None);
                }
            }
            Action::ToggleMark => pane.toggle_mark(),
            Action::MarkAndDown | Action::MarkDown => pane.toggle_mark_and_step(1),
            Action::MarkUp => pane.toggle_mark_and_step(-1),
            Action::MarkAll => pane.mark_all(true),
            Action::UnmarkAll => pane.mark_all(false),
            Action::InvertMarks => pane.invert_marks(),
            Action::SortByName => pane.sort_by(SortColumn::Name),
            Action::SortByExt => pane.sort_by(SortColumn::Ext),
            Action::SortBySize => pane.sort_by(SortColumn::Size),
            Action::SortByDate => pane.sort_by(SortColumn::Date),
            Action::View => self.open_cursor(false),
            Action::Edit => self.open_cursor(true),
            Action::Copy => self.transfer(Operation::Copy),
            Action::Move => self.transfer(Operation::Move),
            Action::NewFolder => self.create(true),
            Action::NewFile => self.create(false),
            Action::Delete => self.delete(false),
            Action::DeletePermanent => self.delete(true),
            Action::Rename => self.rename(),
            Action::ClipboardCopy => self.clipboard_put(false),
            Action::ClipboardCut => self.clipboard_put(true),
            Action::ClipboardPaste => self.clipboard_paste(),
            Action::Quit => self.window.close(),
        }
    }

    // ---- pane helpers ------------------------------------------------------

    fn active_pane(&self) -> &Pane {
        &self.panes[self.active.get()]
    }

    fn other_pane(&self) -> &Pane {
        &self.panes[1 - self.active.get()]
    }

    fn set_active(&self, index: usize, grab_focus: bool) {
        self.active.set(index);
        for (i, pane) in self.panes.iter().enumerate() {
            pane.set_active(i == index);
        }
        if grab_focus {
            self.panes[index].focus();
        }
    }

    fn reload_all(&self) {
        for pane in &self.panes {
            pane.reload();
        }
    }

    fn win(&self) -> &gtk::Window {
        self.window.upcast_ref()
    }

    /// The active pane's folder and the items an operation acts on, as names and full paths.
    fn sources(&self) -> Option<(PathBuf, Vec<OsString>, Vec<PathBuf>)> {
        let pane = self.active_pane();
        let cwd = pane.cwd()?;
        let names = pane.targets();
        if names.is_empty() {
            return None;
        }
        let paths = names.iter().map(|n| cwd.join(n)).collect();
        Some((cwd, names, paths))
    }

    /// Ctrl+Left/Right: show the folder under the cursor (or the current one) in pane `index`.
    fn open_in(&self, index: usize) {
        let pane = self.active_pane();
        let Some(cwd) = pane.cwd() else {
            return;
        };
        let folder = match pane.cursor_name() {
            Some(name) if cwd.join(&name).is_dir() => cwd.join(name),
            _ => cwd,
        };
        self.panes[index].navigate(folder, None);
    }

    // ---- file operations ---------------------------------------------------

    fn transfer(&self, op: Operation) {
        let Some((cwd, names, paths)) = self.sources() else {
            return;
        };
        let default_dest = self.other_pane().cwd().unwrap_or_else(|| cwd.clone());
        let verb = ops::verb(op);
        let what = ops::describe(&names);
        let weak = self.weak.clone();
        ops::prompt(
            self.win(),
            verb,
            &format!("{verb} {what} to:"),
            &default_dest.to_string_lossy(),
            None,
            verb,
            move |text| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                let dest = expand_path(&text, Some(&cwd));
                if !dest.is_dir()
                    && let Err(err) = fs::create_dir_all(&dest)
                {
                    ops::alert(
                        app.win(),
                        "Cannot create the destination folder",
                        &err.to_string(),
                    );
                    return;
                }
                app.enqueue_transfer(op, paths.clone(), dest);
                app.active_pane().mark_all(false);
            },
        );
    }

    fn enqueue_transfer(&self, op: Operation, paths: Vec<PathBuf>, dest: PathBuf) {
        let names: Vec<OsString> = paths
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_os_string())
            .collect();
        let title = format!(
            "{} {} → {}",
            ops::verb(op),
            ops::describe(&names),
            dest.to_string_lossy()
        );
        let spec = match op {
            Operation::Copy => JobSpec::copy(paths, dest),
            Operation::Move => JobSpec::move_to(paths, dest),
            Operation::Delete => JobSpec::delete(paths),
        };
        self.runner.enqueue(title, spec);
    }

    /// Drop onto pane `index`. Without a modifier this behaves like Nautilus and
    /// FreeCommander: move within one filesystem, copy across filesystems.
    fn dropped(&self, index: usize, paths: Vec<PathBuf>, forced: Option<Operation>) {
        let Some(dest) = self.panes[index].cwd() else {
            return;
        };
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| p.parent() != Some(dest.as_path()))
            .collect();
        let Some(first) = paths.first() else {
            return;
        };
        let op = forced.unwrap_or(if same_device(first, &dest) {
            Operation::Move
        } else {
            Operation::Copy
        });
        self.enqueue_transfer(op, paths, dest);
    }

    fn delete(&self, permanent: bool) {
        let Some((_, names, paths)) = self.sources() else {
            return;
        };
        let what = ops::describe(&names);
        let weak = self.weak.clone();
        if permanent {
            ops::confirm(
                self.win(),
                &format!("Permanently delete {what}?"),
                "This cannot be undone.",
                "Delete",
                move || {
                    if let Some(app) = weak.upgrade() {
                        app.runner
                            .enqueue(format!("Delete {what}"), JobSpec::delete(paths));
                        app.active_pane().mark_all(false);
                    }
                },
            );
        } else {
            ops::confirm(
                self.win(),
                &format!("Move {what} to the trash?"),
                "",
                "Move to Trash",
                move || {
                    let Some(app) = weak.upgrade() else {
                        return;
                    };
                    app.active_pane().mark_all(false);
                    ops::trash(paths, move |failures| {
                        let Some(app) = weak.upgrade() else {
                            return;
                        };
                        if !failures.is_empty() {
                            let detail: Vec<String> = failures
                                .iter()
                                .map(|(p, e)| format!("{}: {e}", p.to_string_lossy()))
                                .collect();
                            ops::alert(
                                app.win(),
                                "Some items could not be trashed",
                                &detail.join("\n"),
                            );
                        }
                        app.reload_all();
                    });
                },
            );
        }
    }

    fn rename(&self) {
        let pane = self.active_pane();
        let (Some(cwd), Some(name)) = (pane.cwd(), pane.cursor_name()) else {
            return;
        };
        let display = name.to_string_lossy().into_owned();
        let from = cwd.join(&name);
        // Preselect the stem so typing replaces the name but keeps the extension.
        let stem_end = if from.is_dir() {
            -1
        } else {
            display
                .rfind('.')
                .filter(|&i| i > 0)
                .map_or(-1, |i| i as i32)
        };
        let weak = self.weak.clone();
        ops::prompt(
            self.win(),
            "Rename",
            &format!("Rename “{display}” to:"),
            &display,
            Some((0, stem_end)),
            "Rename",
            move |new_name| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                let to = cwd.join(&new_name);
                if to == from {
                    return;
                }
                if fs::symlink_metadata(&to).is_ok() {
                    ops::alert(
                        app.win(),
                        "Cannot rename",
                        &format!("“{new_name}” already exists."),
                    );
                    return;
                }
                match fs::rename(&from, &to) {
                    Ok(()) => app
                        .active_pane()
                        .navigate(cwd.clone(), Some(OsString::from(&new_name))),
                    Err(err) => ops::alert(app.win(), "Cannot rename", &err.to_string()),
                }
            },
        );
    }

    fn create(&self, folder: bool) {
        let Some(cwd) = self.active_pane().cwd() else {
            return;
        };
        let (title, message) = if folder {
            ("New folder", "Folder name:")
        } else {
            ("New file", "File name:")
        };
        let weak = self.weak.clone();
        ops::prompt(
            self.win(),
            title,
            message,
            "",
            None,
            "Create",
            move |name| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                let path = cwd.join(&name);
                let result = if folder {
                    fs::create_dir_all(&path)
                } else {
                    fs::File::create_new(&path).map(drop)
                };
                match result {
                    Ok(()) => {
                        // Land the cursor on the first component, in case a nested path was typed.
                        let first = Path::new(&name)
                            .components()
                            .next()
                            .map(|c| c.as_os_str().to_os_string());
                        app.active_pane().navigate(cwd.clone(), first);
                    }
                    Err(err) => ops::alert(
                        app.win(),
                        &format!("Cannot create {}", title.to_lowercase()),
                        &err.to_string(),
                    ),
                }
            },
        );
    }

    /// F3/F4: open the cursor item with its default app, or with the default text editor.
    fn open_cursor(&self, editor: bool) {
        let pane = self.active_pane();
        let (Some(cwd), Some(name)) = (pane.cwd(), pane.cursor_name()) else {
            return;
        };
        let path = cwd.join(name);
        if path.is_dir() {
            pane.navigate(path, None);
            return;
        }
        let file = gio::File::for_path(&path);
        let ctx = WidgetExt::display(&self.window).app_launch_context();
        let result = if editor {
            match gio::AppInfo::default_for_type("text/plain", false) {
                Some(app) => app.launch(&[file], Some(&ctx)).map_err(|e| e.to_string()),
                None => Err("no default text editor is configured".to_owned()),
            }
        } else {
            gio::AppInfo::launch_default_for_uri(&file.uri(), Some(&ctx)).map_err(|e| e.to_string())
        };
        if let Err(err) = result {
            ops::alert(self.win(), "Cannot open file", &err);
        }
    }

    fn clipboard_put(&self, cut: bool) {
        if let Some((_, _, paths)) = self.sources() {
            ops::clipboard_put(&self.window, &paths, cut);
        }
    }

    fn clipboard_paste(&self) {
        let weak = self.weak.clone();
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            let Some((paths, cut)) = ops::clipboard_take(&window).await else {
                return;
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            let Some(dest) = app.active_pane().cwd() else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            let op = if cut {
                Operation::Move
            } else {
                Operation::Copy
            };
            app.enqueue_transfer(op, paths, dest);
            if cut {
                // The sources are gone after the move; don't offer them again.
                let _ = window
                    .clipboard()
                    .set_content(None::<&gdk::ContentProvider>);
            }
        });
    }
}

fn same_device(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Translate a GDK key event into a keymap chord.
///
/// Letters are matched case-insensitively with Shift kept as a modifier
/// (`Shift+a`); shifted symbols like `*` drop Shift so bindings can say
/// `asterisk` regardless of layout.
fn chord_for(key: gdk::Key, state: gdk::ModifierType) -> Option<Chord> {
    let unicode = key.to_unicode();
    let is_letter = unicode.is_some_and(char::is_alphabetic);
    let is_symbol =
        unicode.is_some_and(|c| !c.is_alphabetic() && !c.is_whitespace() && !c.is_control());
    let name = if is_letter {
        key.to_lower().name()?
    } else {
        key.name()?
    };
    let mods = Mods {
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK) && !is_symbol,
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        super_: state.contains(gdk::ModifierType::SUPER_MASK),
    };
    Some(Chord::new(&name, mods))
}
