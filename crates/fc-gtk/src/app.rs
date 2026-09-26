//! The main window: chrome (menu, toolbar, places, functions bar), two panes,
//! the jobs panel, and the key → chord → action dispatcher. Every feature goes
//! through [`App::run`], which menus, toolbar, and the functions bar share.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::{Rc, Weak};

use fc_core::action::Action;
use fc_core::favorites::Favorites;
use fc_core::glob::Mask;
use fc_core::jobs::{JobSpec, Operation};
use fc_core::keymap::{Chord, Keymap, Mods};
use fc_core::rename::{self, Source};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::chrome::{self, FunctionsBar, PlacesBar};
use crate::favorites;
use crate::host::PaneHost;
use crate::multirename;
use crate::ops::{self, JobRunner};
use crate::pane::{Pane, SortColumn, ViewMode, expand_path};
use crate::props;

pub struct App {
    weak: Weak<App>,
    gtk_app: gtk::Application,
    window: gtk::ApplicationWindow,
    hosts: [PaneHost; 2],
    active: Cell<usize>,
    keymap: Keymap,
    runner: JobRunner,
    favorites: RefCell<Favorites>,
    favorites_menu: gio::Menu,
    /// Last multi rename, for undo.
    last_rename: RefCell<Option<AppliedRename>>,
    split: gtk::Paned,
    menu_bar: gtk::PopoverMenuBar,
    toolbar: gtk::Box,
    places: Rc<PlacesBar>,
    functions: Rc<FunctionsBar>,
}

impl App {
    pub fn new(gtk_app: &gtk::Application, start: [PathBuf; 2], keymap: Keymap) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder()
            .application(gtk_app)
            .title("fc")
            .default_width(1200)
            .default_height(760)
            .show_menubar(false)
            .build();
        let runner = JobRunner::new(window.upcast_ref());

        let app = Rc::new_cyclic(|weak: &Weak<App>| {
            let run: chrome::Run = {
                let weak = weak.clone();
                Rc::new(move |action| {
                    if let Some(app) = weak.upgrade() {
                        app.run(action);
                    }
                })
            };
            chrome::register_actions(gtk_app, &keymap, run.clone());
            let places = {
                let weak = weak.clone();
                PlacesBar::new(window.upcast_ref(), move |path| {
                    if let Some(app) = weak.upgrade() {
                        app.active_pane().navigate(path, None);
                    }
                })
            };
            // Every pane, in any tab on either side, gets the same wiring.
            let host = |side: usize| {
                let weak = weak.clone();
                PaneHost::new(move |pane| {
                    let w = weak.clone();
                    pane.connect_focus_enter(move || {
                        if let Some(app) = w.upgrade() {
                            app.set_active(side, false);
                        }
                    });
                    let w = weak.clone();
                    pane.connect_drop(move |paths, forced| {
                        if let Some(app) = w.upgrade() {
                            app.dropped(side, paths, forced);
                        }
                    });
                    let w = weak.clone();
                    pane.connect_context_menu(move |anchor, x, y| {
                        if let Some(app) = w.upgrade() {
                            app.set_active(side, false);
                            app.show_context_menu(anchor, x, y);
                        }
                    });
                })
            };
            let hosts = [host(0), host(1)];
            let split = gtk::Paned::builder()
                .orientation(gtk::Orientation::Horizontal)
                .start_child(hosts[0].widget())
                .end_child(hosts[1].widget())
                .resize_start_child(true)
                .resize_end_child(true)
                .shrink_start_child(false)
                .shrink_end_child(false)
                .vexpand(true)
                .build();
            // No explicit position: with both children resizable GTK splits the width evenly.
            let (menu_bar, favorites_menu) = chrome::menu_bar();
            App {
                weak: weak.clone(),
                gtk_app: gtk_app.clone(),
                menu_bar,
                favorites_menu,
                favorites: RefCell::new(Favorites::default()),
                last_rename: RefCell::new(None),
                toolbar: chrome::toolbar(&keymap, run.clone()),
                places,
                functions: FunctionsBar::new(keymap.clone(), run),
                window,
                hosts,
                active: Cell::new(0),
                keymap,
                runner,
                split,
            }
        });

        // Opening a favorite from the menu, the popup, or Shift+Ctrl+1..0.
        let open_favorite =
            gio::SimpleAction::new(favorites::OPEN_ACTION, Some(&String::static_variant_type()));
        let weak = app.weak.clone();
        open_favorite.connect_activate(move |_, target| {
            if let (Some(app), Some(path)) =
                (weak.upgrade(), target.and_then(|v| v.get::<String>()))
            {
                app.active_pane().navigate(PathBuf::from(path), None);
            }
        });
        gtk_app.add_action(&open_favorite);
        app.load_favorites();

        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        layout.append(&app.menu_bar);
        layout.append(&app.toolbar);
        layout.append(app.places.widget());
        layout.append(&app.split);
        layout.append(app.runner.widget());
        layout.append(app.functions.widget());
        app.window.set_child(Some(&layout));

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
            app.functions.set_mods(mods_after(key, state, true));
            app.on_key(key, state)
        });
        let weak = app.weak.clone();
        keys.connect_key_released(move |_, key, _, state| {
            if let Some(app) = weak.upgrade() {
                app.functions.set_mods(mods_after(key, state, false));
            }
        });
        app.window.add_controller(keys);
        // A modifier released while another window had focus never reports back.
        let focus = gtk::EventControllerFocus::new();
        let weak = app.weak.clone();
        focus.connect_leave(move |_| {
            if let Some(app) = weak.upgrade() {
                app.functions.set_mods(Mods::default());
            }
        });
        app.window.add_controller(focus);

        let [left, right] = start;
        app.hosts[0].open_tab(left);
        app.hosts[1].open_tab(right);
        app.set_active(0, true);
        app.window.present();
        app
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        // While typing in an entry, only the entry's own handling applies.
        if GtkWindowExt::focus(&self.window).is_some_and(|w| w.is::<gtk::Text>()) {
            return glib::Propagation::Proceed;
        }
        let pane = self.active_pane();
        // An active quick search owns Escape and Backspace.
        if pane.quick_search_active()
            && matches!(key, gdk::Key::Escape | gdk::Key::BackSpace)
            && pane.quick_search_key(key)
        {
            return glib::Propagation::Stop;
        }
        let Some(chord) = chord_for(key, state) else {
            return glib::Propagation::Proceed;
        };
        if let Some(action) = self.keymap.lookup(&chord) {
            self.run(action);
            return glib::Propagation::Stop;
        }
        // Unbound printable keys type into the quick search.
        let plain =
            !state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
        if plain && pane.quick_search_key(key) {
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }

    pub fn run(&self, action: Action) {
        let pane = self.active_pane();
        match action {
            Action::SwitchPane => self.set_active(1 - self.active.get(), true),
            Action::GoUp => pane.go_up(),
            Action::GoRoot => pane.go_root(),
            Action::Back => pane.back(),
            Action::Forward => pane.forward(),
            Action::GoToFolder => pane.focus_path_entry(),
            Action::Reload => pane.reload(),
            Action::ReloadAll => self.reload_all(),
            Action::ToggleHidden => pane.toggle_hidden(),
            Action::SameFolderBoth => {
                if let Some(cwd) = pane.cwd() {
                    self.other_pane().navigate(cwd, None);
                }
            }
            Action::SwapPanes => {
                let (left, right) = (self.hosts[0].current(), self.hosts[1].current());
                if let (Some(a), Some(b)) = (left.cwd(), right.cwd()) {
                    left.navigate(b, None);
                    right.navigate(a, None);
                }
            }
            Action::NewTab => {
                let path = pane.cwd().unwrap_or_else(glib::home_dir);
                self.active_host().open_tab(path);
            }
            Action::CloseTab => self.active_host().close_current(),
            Action::CloseOtherTabs => self.active_host().close_others(),
            Action::RestoreTab => self.active_host().restore_closed(),
            Action::LastActiveTab => self.active_host().switch_last_active(),
            Action::NextTab => self.active_host().switch_relative(1),
            Action::PrevTab => self.active_host().switch_relative(-1),
            Action::AddFavorite => self.add_favorite(),
            Action::EditFavorites => self.edit_favorites(),
            Action::FavoritesMenu => favorites::popup(&pane.view_widget(), &self.favorites_menu),
            Action::OpenTerminal => self.open_terminal(),
            Action::ToggleTree => pane.toggle_tree(),
            Action::CalcSize => pane.calc_sizes(false),
            Action::CalcSizeAll => pane.calc_sizes(true),
            Action::Open => pane.activate_cursor(),
            Action::OpenWith => self.open_with(),
            Action::Properties => self.properties(false),
            Action::ChangeAttributes => self.properties(true),
            Action::ContextMenu => pane.context_menu_at_cursor(),
            Action::ViewList => pane.set_view_mode(ViewMode::List),
            Action::ViewDetails => pane.set_view_mode(ViewMode::Details),
            Action::ViewThumbnails => pane.set_view_mode(ViewMode::Thumbnails),
            Action::ViewCycle => pane.cycle_view(),
            Action::ToggleMark => pane.toggle_mark(),
            Action::MarkAndDown | Action::MarkDown => pane.toggle_mark_and_step(1),
            Action::MarkUp => pane.toggle_mark_and_step(-1),
            Action::MarkAll => pane.mark_all(true),
            Action::UnmarkAll => pane.mark_all(false),
            Action::MarkPattern => self.mark_pattern(true),
            Action::UnmarkPattern => self.mark_pattern(false),
            Action::MarkSameExt => pane.mark_same_ext(true),
            Action::UnmarkSameExt => pane.mark_same_ext(false),
            Action::InvertMarks => pane.invert_marks(false),
            Action::InvertFileMarks => pane.invert_marks(true),
            Action::ClipboardCopy => self.clipboard_put(false),
            Action::ClipboardCut => self.clipboard_put(true),
            Action::ClipboardPaste => self.clipboard_paste(),
            Action::CopyFullPaths => self.copy_text(PathText::Full),
            Action::CopyNames => self.copy_text(PathText::Names),
            Action::CopyFolderPath => self.copy_text(PathText::Folder),
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
            Action::MultiRename => self.multi_rename(),
            Action::UndoRename => self.undo_rename(),
            Action::ToggleSplitOrientation => {
                let flipped = match self.split.orientation() {
                    gtk::Orientation::Horizontal => gtk::Orientation::Vertical,
                    _ => gtk::Orientation::Horizontal,
                };
                self.split.set_orientation(flipped);
            }
            Action::ToggleSinglePane => {
                let other = self.hosts[1 - self.active.get()].widget();
                other.set_visible(!other.is_visible());
            }
            Action::ToggleFullscreen => {
                if self.window.is_fullscreen() {
                    self.window.unfullscreen();
                } else {
                    self.window.fullscreen();
                }
            }
            Action::ToggleMenuBar => toggle(&self.menu_bar),
            Action::ToggleToolbar => toggle(&self.toolbar),
            Action::TogglePlacesBar => toggle(self.places.widget()),
            Action::ToggleFunctionsBar => toggle(self.functions.widget()),
            Action::ShowShortcuts => chrome::shortcuts_window(self.win(), &self.keymap),
            Action::Quit => self.window.close(),
        }
    }

    // ---- pane helpers ------------------------------------------------------

    fn active_host(&self) -> &PaneHost {
        &self.hosts[self.active.get()]
    }

    fn active_pane(&self) -> Pane {
        self.hosts[self.active.get()].current()
    }

    fn other_pane(&self) -> Pane {
        self.hosts[1 - self.active.get()].current()
    }

    fn set_active(&self, index: usize, grab_focus: bool) {
        self.active.set(index);
        for (i, host) in self.hosts.iter().enumerate() {
            host.set_active(i == index);
        }
        if grab_focus {
            self.hosts[index].current().focus();
        }
    }

    fn reload_all(&self) {
        for host in &self.hosts {
            for pane in host.panes() {
                pane.reload();
            }
        }
    }

    // ---- favorites ---------------------------------------------------------

    fn load_favorites(&self) {
        match Favorites::load(&Favorites::default_path()) {
            Ok(favs) => *self.favorites.borrow_mut() = favs,
            Err(err) => favorites::warn(self.win(), &err),
        }
        favorites::refresh_menu(
            &self.gtk_app,
            &self.favorites_menu,
            &self.favorites.borrow(),
        );
    }

    fn save_favorites(&self, favs: Favorites) {
        if let Err(err) = favs.save(&Favorites::default_path()) {
            favorites::warn(self.win(), &err);
        }
        *self.favorites.borrow_mut() = favs;
        favorites::refresh_menu(
            &self.gtk_app,
            &self.favorites_menu,
            &self.favorites.borrow(),
        );
    }

    fn add_favorite(&self) {
        let Some(cwd) = self.active_pane().cwd() else {
            return;
        };
        let mut favs = self.favorites.borrow().clone();
        if favs.add(cwd) {
            self.save_favorites(favs);
        }
    }

    fn edit_favorites(&self) {
        let weak = self.weak.clone();
        favorites::edit(
            self.win(),
            self.favorites.borrow().clone(),
            self.active_pane().cwd(),
            move |favs| {
                if let Some(app) = weak.upgrade() {
                    app.save_favorites(favs);
                }
            },
        );
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

    /// Right-click menu, anchored inside `anchor` at (x, y).
    fn show_context_menu(&self, anchor: &gtk::Widget, x: f64, y: f64) {
        let sections: &[&[Action]] = &[
            &[Action::Open, Action::OpenWith, Action::View, Action::Edit],
            &[
                Action::ClipboardCut,
                Action::ClipboardCopy,
                Action::ClipboardPaste,
            ],
            &[Action::Copy, Action::Move, Action::Rename],
            &[Action::Delete, Action::DeletePermanent],
            &[
                Action::CopyFullPaths,
                Action::NewFolder,
                Action::OpenTerminal,
            ],
            &[Action::Properties],
        ];
        let menu = gio::Menu::new();
        for section in sections {
            let items = gio::Menu::new();
            for action in section.iter() {
                items.append(Some(action.label()), Some(&format!("app.{}", action.id())));
            }
            menu.append_section(None, &items);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(anchor);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| {
            // Unparent once the popover is done, outside the signal emission.
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    fn open_with(&self) {
        let pane = self.active_pane();
        let (Some(cwd), Some(name)) = (pane.cwd(), pane.cursor_name()) else {
            return;
        };
        let file = gio::File::for_path(cwd.join(name));
        let launcher = gtk::FileLauncher::new(Some(&file));
        launcher.set_always_ask(true);
        launcher.launch(Some(self.win()), gio::Cancellable::NONE, |_| {});
    }

    fn properties(&self, attributes: bool) {
        let pane = self.active_pane();
        let Some(cwd) = pane.cwd() else {
            return;
        };
        let path = match pane.cursor_name() {
            Some(name) => cwd.join(name),
            None => cwd,
        };
        let weak = self.weak.clone();
        props::show(self.win(), path, attributes, move || {
            if let Some(app) = weak.upgrade() {
                app.reload_all();
            }
        });
    }

    fn open_terminal(&self) {
        let Some(cwd) = self.active_pane().cwd() else {
            return;
        };
        // Omarchy's own launcher does exactly this; fall back to the XDG spec tool.
        let dir = format!("--dir={}", cwd.to_string_lossy());
        let mut command = if which("uwsm-app") {
            let mut c = Command::new("setsid");
            c.args(["uwsm-app", "--", "xdg-terminal-exec", &dir]);
            c
        } else {
            let mut c = Command::new("xdg-terminal-exec");
            c.arg(&dir);
            c
        };
        if let Err(err) = command.current_dir(&cwd).spawn() {
            ops::alert(self.win(), "Cannot open a terminal", &err.to_string());
        }
    }

    fn mark_pattern(&self, marked: bool) {
        let weak = self.weak.clone();
        let (title, message) = if marked {
            ("Select", "Select files matching:")
        } else {
            ("Deselect", "Deselect files matching:")
        };
        ops::prompt(
            self.win(),
            title,
            message,
            "*",
            Some((0, -1)),
            title,
            move |text| {
                let mask = Mask::parse(&text);
                if let Some(app) = weak.upgrade()
                    && !mask.is_empty()
                {
                    app.active_pane().mark_matching(&mask, marked);
                }
            },
        );
    }

    fn copy_text(&self, what: PathText) {
        let pane = self.active_pane();
        let Some(cwd) = pane.cwd() else {
            return;
        };
        let lines: Vec<String> = match what {
            PathText::Folder => vec![cwd.to_string_lossy().into_owned()],
            PathText::Names => pane
                .targets()
                .iter()
                .map(|n| n.to_string_lossy().into_owned())
                .collect(),
            PathText::Full => pane
                .targets()
                .iter()
                .map(|n| cwd.join(n).to_string_lossy().into_owned())
                .collect(),
        };
        if !lines.is_empty() {
            self.window.clipboard().set_text(&lines.join("\n"));
        }
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
        let Some(dest) = self.hosts[index].current().cwd() else {
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
                        app.reload_all();
                        app.after_trash(failures);
                    });
                },
            );
        }
    }

    /// Items the trash refused: offer permanent deletion where trashing is
    /// impossible on that mount, report anything else.
    fn after_trash(&self, failures: Vec<ops::TrashFailure>) {
        let (unsupported, other): (Vec<_>, Vec<_>) =
            failures.into_iter().partition(|f| f.unsupported);
        if !other.is_empty() {
            let detail: Vec<String> = other
                .iter()
                .map(|f| format!("{}: {}", f.path.to_string_lossy(), f.message))
                .collect();
            ops::alert(
                self.win(),
                "Some items could not be trashed",
                &detail.join("\n"),
            );
        }
        if unsupported.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = unsupported.into_iter().map(|f| f.path).collect();
        let names: Vec<OsString> = paths
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_os_string())
            .collect();
        let what = ops::describe(&names);
        let weak = self.weak.clone();
        ops::confirm(
            self.win(),
            &format!("Delete {what} permanently?"),
            "This filesystem has no trash, so the items cannot be recovered later.",
            "Delete",
            move || {
                if let Some(app) = weak.upgrade() {
                    app.runner
                        .enqueue(format!("Delete {what}"), JobSpec::delete(paths));
                }
            },
        );
    }

    fn rename(&self) {
        let pane = self.active_pane();
        // FC: F2 with several items selected is the multi rename tool.
        if pane.marked_names().len() > 1 {
            self.multi_rename();
            return;
        }
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

    fn multi_rename(&self) {
        let pane = self.active_pane();
        let Some(cwd) = pane.cwd() else {
            return;
        };
        let parent_name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let sources: Vec<Source> = pane
            .targets()
            .into_iter()
            .map(|name| {
                let meta = fs::symlink_metadata(cwd.join(&name)).ok();
                Source {
                    is_dir: cwd.join(&name).is_dir(),
                    modified: meta.and_then(|m| m.modified().ok()),
                    parent_name: parent_name.clone(),
                    name,
                }
            })
            .collect();
        if sources.is_empty() {
            return;
        }
        let weak = self.weak.clone();
        let dir = cwd.clone();
        multirename::show(self.win(), cwd, sources, move |applied| {
            if let Some(app) = weak.upgrade() {
                *app.last_rename.borrow_mut() = Some((dir.clone(), applied));
                app.active_pane().mark_all(false);
                app.reload_all();
            }
        });
    }

    fn undo_rename(&self) {
        let Some((dir, applied)) = self.last_rename.borrow_mut().take() else {
            ops::alert(
                self.win(),
                "Nothing to undo",
                "No multi rename has been done yet.",
            );
            return;
        };
        match rename::execute(&dir, &rename::undo_plan(&applied)) {
            Ok(_) => self.reload_all(),
            Err(err) => {
                // Keep it so the user can retry after fixing the cause.
                *self.last_rename.borrow_mut() = Some((dir, applied));
                ops::alert(self.win(), "Undo failed", &err.to_string());
            }
        }
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

/// The folder and the `(old, new)` pairs a multi rename applied.
type AppliedRename = (PathBuf, Vec<(OsString, String)>);

#[derive(Clone, Copy)]
enum PathText {
    Full,
    Names,
    Folder,
}

fn toggle(widget: &impl IsA<gtk::Widget>) {
    widget.set_visible(!widget.is_visible());
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

fn same_device(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Modifiers in effect after this key event. GDK's `state` describes the
/// moment *before* the event, so a modifier key being pressed or released has
/// to be folded in by hand; this keeps the functions bar labels in sync.
fn mods_after(key: gdk::Key, state: gdk::ModifierType, pressed: bool) -> Mods {
    let mut mods = Mods {
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        super_: state.contains(gdk::ModifierType::SUPER_MASK),
    };
    match key {
        gdk::Key::Control_L | gdk::Key::Control_R => mods.ctrl = pressed,
        gdk::Key::Shift_L | gdk::Key::Shift_R => mods.shift = pressed,
        gdk::Key::Alt_L | gdk::Key::Alt_R => mods.alt = pressed,
        gdk::Key::Super_L | gdk::Key::Super_R => mods.super_ = pressed,
        _ => {}
    }
    mods
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
