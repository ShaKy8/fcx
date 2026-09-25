//! The main window: two panes side by side, an active-pane pointer, and the
//! key → chord → action dispatcher. Every keyboard feature goes through
//! [`App::run`], so menus and a command palette can reuse it later.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use fc_core::action::Action;
use fc_core::keymap::{Chord, Keymap, Mods};
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::pane::Pane;

pub struct App {
    window: gtk::ApplicationWindow,
    panes: [Pane; 2],
    active: Cell<usize>,
    keymap: Keymap,
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
            .build();

        let window = gtk::ApplicationWindow::builder()
            .application(gtk_app)
            .title("fc")
            .default_width(1200)
            .default_height(760)
            .child(&split)
            .build();
        // Split evenly once the window has a size.
        window.connect_default_width_notify(glib::clone!(
            #[weak]
            split,
            move |w| split.set_position(w.default_width() / 2)
        ));
        split.set_position(600);

        let app = Rc::new(App {
            window,
            panes,
            active: Cell::new(0),
            keymap,
        });

        for (i, pane) in app.panes.iter().enumerate() {
            let weak = Rc::downgrade(&app);
            pane.connect_focus_enter(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_active(i, false);
                }
            });
        }

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&app);
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
        let other = &self.panes[1 - self.active.get()];
        match action {
            Action::SwitchPane => self.set_active(1 - self.active.get(), true),
            Action::GoUp => pane.go_up(),
            Action::ToggleHidden => pane.toggle_hidden(),
            Action::FocusPath => pane.focus_path_entry(),
            Action::Reload => pane.reload(),
            Action::ToggleMark => pane.toggle_mark(),
            Action::MarkAndDown | Action::MarkDown => pane.toggle_mark_and_step(1),
            Action::MarkUp => pane.toggle_mark_and_step(-1),
            Action::MarkAll => pane.mark_all(true),
            Action::UnmarkAll => pane.mark_all(false),
            Action::InvertMarks => pane.invert_marks(),
            Action::MirrorPath => {
                if let Some(cwd) = pane.cwd() {
                    other.navigate(cwd, None);
                }
            }
            Action::SwapPanes => {
                if let (Some(a), Some(b)) = (self.panes[0].cwd(), self.panes[1].cwd()) {
                    self.panes[0].navigate(b, None);
                    self.panes[1].navigate(a, None);
                }
            }
            Action::Quit => self.window.close(),
        }
    }

    fn active_pane(&self) -> &Pane {
        &self.panes[self.active.get()]
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
