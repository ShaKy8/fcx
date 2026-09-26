//! One side of the window: a tab bar over a stack of [`Pane`]s. Each tab is a
//! complete pane, so its folder, history, marks, cursor, and view mode are all
//! its own; switching tabs just raises a different widget.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::pane::Pane;

/// How many closed tabs Shift+Ctrl+T can bring back.
const CLOSED_HISTORY: usize = 20;

#[derive(Clone)]
pub struct PaneHost(Rc<Inner>);

struct Tab {
    pane: Pane,
    button: gtk::ToggleButton,
}

struct Inner {
    root: gtk::Box,
    tab_bar: gtk::Box,
    stack: gtk::Stack,
    tabs: RefCell<Vec<Tab>>,
    current: Cell<usize>,
    last_active: Cell<usize>,
    closed: RefCell<Vec<PathBuf>>,
    active: Cell<bool>,
    /// Wires a freshly created pane into the app (focus, drop, context menu…).
    on_new_pane: Rc<dyn Fn(&Pane)>,
}

impl PaneHost {
    pub fn new(on_new_pane: impl Fn(&Pane) + 'static) -> Self {
        let tab_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tab_bar.add_css_class("tab-bar");
        let tab_scroller = gtk::ScrolledWindow::builder()
            .child(&tab_bar)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::External)
            .build();
        let stack = gtk::Stack::builder().vexpand(true).hexpand(true).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&tab_scroller);
        root.append(&stack);
        PaneHost(Rc::new(Inner {
            root,
            tab_bar,
            stack,
            tabs: RefCell::new(Vec::new()),
            current: Cell::new(0),
            last_active: Cell::new(0),
            closed: RefCell::new(Vec::new()),
            active: Cell::new(false),
            on_new_pane: Rc::new(on_new_pane),
        }))
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.0.root.upcast_ref()
    }

    pub fn current(&self) -> Pane {
        let tabs = self.0.tabs.borrow();
        tabs[self.0.current.get().min(tabs.len() - 1)].pane.clone()
    }

    pub fn panes(&self) -> Vec<Pane> {
        self.0
            .tabs
            .borrow()
            .iter()
            .map(|t| t.pane.clone())
            .collect()
    }

    /// Active-side highlight, applied to whichever tab is showing.
    pub fn set_active(&self, active: bool) {
        self.0.active.set(active);
        for (i, tab) in self.0.tabs.borrow().iter().enumerate() {
            tab.pane.set_active(active && i == self.0.current.get());
        }
    }

    /// Ctrl+T: a new tab showing `path`, raised and focused.
    pub fn open_tab(&self, path: PathBuf) -> Pane {
        let pane = Pane::new();
        (self.0.on_new_pane)(&pane);
        let button = gtk::ToggleButton::builder()
            .label(tab_label(&path))
            .can_focus(false)
            .build();
        button.add_css_class("flat");
        button.add_css_class("tab");
        if let Some(first) = self.0.tabs.borrow().first() {
            button.set_group(Some(&first.button));
        }
        self.0.tab_bar.append(&button);
        self.0.stack.add_child(pane.widget());

        let index = self.0.tabs.borrow().len();
        self.0.tabs.borrow_mut().push(Tab {
            pane: pane.clone(),
            button: button.clone(),
        });

        // Label follows the pane's folder.
        let weak_button = button.downgrade();
        pane.connect_navigated(move |path| {
            if let Some(button) = weak_button.upgrade() {
                button.set_label(&tab_label(path));
                button.set_tooltip_text(Some(&path.to_string_lossy()));
            }
        });

        let weak = self.downgrade();
        button.connect_toggled(move |b| {
            if b.is_active()
                && let Some(host) = PaneHost::upgrade(&weak)
                && let Some(i) = host.index_of_button(b)
                && host.0.current.get() != i
            {
                host.switch_to(i, true);
            }
        });
        // Middle-click closes, like every tabbed app.
        let middle = gtk::GestureClick::builder()
            .button(gdk::BUTTON_MIDDLE)
            .build();
        let weak = self.downgrade();
        middle.connect_pressed(glib::clone!(
            #[weak]
            button,
            move |_, _, _, _| {
                if let Some(host) = PaneHost::upgrade(&weak)
                    && let Some(i) = host.index_of_button(&button)
                {
                    host.close_tab(i);
                }
            }
        ));
        button.add_controller(middle);

        pane.navigate(path, None);
        self.switch_to(index, true);
        pane
    }

    /// Ctrl+W: close the current tab; the last tab stays open.
    pub fn close_current(&self) {
        self.close_tab(self.0.current.get());
    }

    fn close_tab(&self, index: usize) {
        if self.0.tabs.borrow().len() <= 1 {
            return;
        }
        let tab = self.0.tabs.borrow_mut().remove(index);
        if let Some(cwd) = tab.pane.cwd() {
            let mut closed = self.0.closed.borrow_mut();
            closed.push(cwd);
            if closed.len() > CLOSED_HISTORY {
                closed.remove(0);
            }
        }
        self.0.tab_bar.remove(&tab.button);
        self.0.stack.remove(tab.pane.widget());
        let count = self.0.tabs.borrow().len();
        let current = self.0.current.get();
        let next = if index < current {
            current - 1
        } else {
            current.min(count - 1)
        };
        let last = self.0.last_active.get();
        self.0.last_active.set(if last > index {
            last - 1
        } else {
            last.min(count - 1)
        });
        self.0.current.set(usize::MAX); // force switch_to to apply
        self.switch_to(next, true);
    }

    /// Shift+Ctrl+W: close every tab but the current one.
    pub fn close_others(&self) {
        while self.0.tabs.borrow().len() > 1 {
            let victim = if self.0.current.get() == 0 { 1 } else { 0 };
            self.close_tab(victim);
        }
    }

    /// Shift+Ctrl+T: reopen the most recently closed tab.
    pub fn restore_closed(&self) {
        let path = self.0.closed.borrow_mut().pop();
        if let Some(path) = path {
            self.open_tab(path);
        }
    }

    /// Ctrl+PgUp: jump to the tab that was active before this one.
    pub fn switch_last_active(&self) {
        let last = self.0.last_active.get();
        if last != self.0.current.get() && last < self.0.tabs.borrow().len() {
            self.switch_to(last, true);
        }
    }

    pub fn switch_relative(&self, delta: i32) {
        let count = self.0.tabs.borrow().len() as i32;
        let next = (self.0.current.get() as i32 + delta).rem_euclid(count);
        self.switch_to(next as usize, true);
    }

    fn switch_to(&self, index: usize, focus: bool) {
        let previous = self.0.current.get();
        if previous == index {
            return;
        }
        let (pane, button) = {
            let tabs = self.0.tabs.borrow();
            let Some(tab) = tabs.get(index) else {
                return;
            };
            (tab.pane.clone(), tab.button.clone())
        };
        if previous != usize::MAX {
            self.0.last_active.set(previous);
        }
        self.0.current.set(index);
        if !button.is_active() {
            button.set_active(true);
        }
        self.0.stack.set_visible_child(pane.widget());
        self.set_active(self.0.active.get());
        if focus {
            pane.focus();
        }
    }

    fn index_of_button(&self, button: &gtk::ToggleButton) -> Option<usize> {
        self.0
            .tabs
            .borrow()
            .iter()
            .position(|t| &t.button == button)
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn upgrade(weak: &Weak<Inner>) -> Option<PaneHost> {
        weak.upgrade().map(PaneHost)
    }
}

fn tab_label(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => "/".to_owned(),
    }
}
