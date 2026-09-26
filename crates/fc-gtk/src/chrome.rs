//! Window chrome: menu bar, toolbar, places bar, functions bar, shortcuts list.
//!
//! Everything here is driven by the [`Keymap`], so menus, tooltips, and the
//! functions bar always show the bindings that are actually in effect.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use fc_core::action::Action;
use fc_core::keymap::{Chord, Keymap, Mods};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

pub type Run = Rc<dyn Fn(Action)>;

/// Menu bar layout: title → sections of actions. Also the order of the shortcuts list.
const MENUS: &[(&str, &[&[Action]])] = &[
    (
        "_File",
        &[
            &[
                Action::Open,
                Action::OpenWith,
                Action::View,
                Action::Edit,
                Action::NewFile,
            ],
            &[
                Action::Copy,
                Action::Move,
                Action::NewFolder,
                Action::Rename,
                Action::MultiRename,
                Action::UndoRename,
            ],
            &[Action::Delete, Action::DeletePermanent],
            &[Action::Properties, Action::ChangeAttributes],
            &[Action::OpenTerminal],
            &[Action::Quit],
        ],
    ),
    (
        "_Edit",
        &[
            &[
                Action::ClipboardCut,
                Action::ClipboardCopy,
                Action::ClipboardPaste,
            ],
            &[
                Action::MarkAll,
                Action::UnmarkAll,
                Action::MarkPattern,
                Action::UnmarkPattern,
                Action::MarkSameExt,
                Action::UnmarkSameExt,
                Action::InvertMarks,
                Action::InvertFileMarks,
            ],
            &[
                Action::CopyFullPaths,
                Action::CopyNames,
                Action::CopyFolderPath,
            ],
        ],
    ),
    (
        "F_older",
        &[
            &[
                Action::GoUp,
                Action::GoRoot,
                Action::Back,
                Action::Forward,
                Action::GoToFolder,
            ],
            &[
                Action::SameFolderBoth,
                Action::SwapPanes,
                Action::SwitchPane,
            ],
            &[Action::CalcSize, Action::CalcSizeAll],
            &[
                Action::SortByName,
                Action::SortByExt,
                Action::SortByDate,
                Action::SortBySize,
            ],
        ],
    ),
    (
        "_View",
        &[
            &[Action::Reload, Action::ReloadAll],
            &[Action::ToggleHidden, Action::ToggleTree],
            &[
                Action::ViewDetails,
                Action::ViewList,
                Action::ViewThumbnails,
                Action::ViewCycle,
            ],
            &[
                Action::ToggleSplitOrientation,
                Action::ToggleSinglePane,
                Action::ToggleFullscreen,
            ],
            &[
                Action::ToggleMenuBar,
                Action::ToggleToolbar,
                Action::TogglePlacesBar,
                Action::ToggleFunctionsBar,
            ],
        ],
    ),
    ("Hel_p", &[&[Action::ShowShortcuts]]),
];

/// Toolbar buttons: (action, icon name).
const TOOLBAR: &[&[(Action, &str)]] = &[
    &[
        (Action::Back, "go-previous-symbolic"),
        (Action::Forward, "go-next-symbolic"),
        (Action::GoUp, "go-up-symbolic"),
        (Action::Reload, "view-refresh-symbolic"),
    ],
    &[
        (Action::NewFolder, "folder-new-symbolic"),
        (Action::Copy, "edit-copy-symbolic"),
        (Action::Move, "edit-cut-symbolic"),
        (Action::Rename, "document-edit-symbolic"),
        (Action::Delete, "user-trash-symbolic"),
    ],
    &[
        (Action::ToggleHidden, "view-reveal-symbolic"),
        (Action::SameFolderBoth, "view-dual-symbolic"),
        (Action::SwapPanes, "object-flip-horizontal-symbolic"),
        (Action::OpenTerminal, "utilities-terminal-symbolic"),
    ],
];

// ---- keys ↔ GTK accelerators -------------------------------------------------

/// The GDK key for a normalized chord key name (`f5`, `backspace`, `page_up`, `a`).
fn gdk_key(name: &str) -> Option<gdk::Key> {
    let special = match name {
        "backspace" => Some("BackSpace"),
        "page_up" => Some("Page_Up"),
        "page_down" => Some("Page_Down"),
        "iso_left_tab" => Some("ISO_Left_Tab"),
        _ => None,
    };
    let mut candidates: Vec<String> = special.map(str::to_owned).into_iter().collect();
    candidates.push(name.to_owned());
    let mut first_upper = name.chars();
    if let Some(c) = first_upper.next() {
        candidates.push(c.to_uppercase().chain(first_upper).collect());
    }
    candidates.push(name.to_uppercase());
    candidates
        .iter()
        .filter_map(gdk::Key::from_name)
        .find(|k| *k != gdk::Key::VoidSymbol)
}

fn modifier_type(mods: Mods) -> gdk::ModifierType {
    let mut m = gdk::ModifierType::empty();
    if mods.ctrl {
        m |= gdk::ModifierType::CONTROL_MASK;
    }
    if mods.shift {
        m |= gdk::ModifierType::SHIFT_MASK;
    }
    if mods.alt {
        m |= gdk::ModifierType::ALT_MASK;
    }
    if mods.super_ {
        m |= gdk::ModifierType::SUPER_MASK;
    }
    m
}

/// GTK accelerator string (`<Control>F5`) for a chord.
pub fn accel_for(chord: &Chord) -> Option<String> {
    let key = gdk_key(&chord.key)?;
    Some(gtk::accelerator_name(key, modifier_type(chord.mods)).to_string())
}

/// Human label (`Ctrl+F5`) for a chord.
pub fn chord_label(chord: &Chord) -> String {
    match gdk_key(&chord.key) {
        Some(key) => gtk::accelerator_get_label(key, modifier_type(chord.mods)).to_string(),
        None => chord.key.clone(),
    }
}

/// `Copy… (F5)` for tooltips.
fn tooltip(keymap: &Keymap, action: Action) -> String {
    let label = action.label().trim_end_matches('…').to_owned();
    match keymap.chords_for(action).first() {
        Some(chord) => format!("{label} ({})", chord_label(chord)),
        None => label,
    }
}

// ---- GActions + menu bar -------------------------------------------------------

/// Registers `app.<id>` for every action and its accelerators, so the menu bar
/// shows shortcuts and menu items dispatch through `run`.
pub fn register_actions(gtk_app: &gtk::Application, keymap: &Keymap, run: Run) {
    for &action in Action::ALL {
        let simple = gio::SimpleAction::new(action.id(), None);
        let run = run.clone();
        simple.connect_activate(move |_, _| run(action));
        gtk_app.add_action(&simple);
        let accels: Vec<String> = keymap
            .chords_for(action)
            .iter()
            .filter_map(|c| accel_for(c))
            .collect();
        let refs: Vec<&str> = accels.iter().map(String::as_str).collect();
        gtk_app.set_accels_for_action(&format!("app.{}", action.id()), &refs);
    }
}

/// The menu bar plus the section that lists favorites (filled by the app).
pub fn menu_bar() -> (gtk::PopoverMenuBar, gio::Menu) {
    let root = gio::Menu::new();
    let favorites_section = gio::Menu::new();
    for (title, sections) in MENUS {
        let menu = gio::Menu::new();
        for section in sections.iter() {
            let items = gio::Menu::new();
            for action in section.iter() {
                items.append(Some(action.label()), Some(&format!("app.{}", action.id())));
            }
            menu.append_section(None, &items);
        }
        root.append_submenu(Some(title), &menu);
        if *title == "F_older" {
            // Favorites sit between Folder and View, like FC's own menu order.
            let fav = gio::Menu::new();
            let fixed = gio::Menu::new();
            for action in [Action::AddFavorite, Action::EditFavorites] {
                fixed.append(Some(action.label()), Some(&format!("app.{}", action.id())));
            }
            fav.append_section(None, &fixed);
            fav.append_section(None, &favorites_section);
            root.append_submenu(Some("Fav_orites"), &fav);
        }
    }
    let bar = gtk::PopoverMenuBar::from_model(Some(&root));
    bar.add_css_class("menu-bar");
    (bar, favorites_section)
}

// ---- toolbar -------------------------------------------------------------------

pub fn toolbar(keymap: &Keymap, run: Run) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    bar.add_css_class("toolbar");
    for (i, group) in TOOLBAR.iter().enumerate() {
        if i > 0 {
            bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        }
        for &(action, icon) in group.iter() {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip(keymap, action))
                .can_focus(false)
                .build();
            button.add_css_class("flat");
            let run = run.clone();
            button.connect_clicked(move |_| run(action));
            bar.append(&button);
        }
    }
    bar
}

// ---- functions bar ---------------------------------------------------------------

/// FreeCommander's bottom row: one button per F-key showing what it does with
/// the modifiers currently held.
pub struct FunctionsBar {
    root: gtk::Box,
    buttons: Vec<gtk::Button>,
    keymap: Keymap,
    current: RefCell<Vec<Option<Action>>>,
}

impl FunctionsBar {
    pub fn new(keymap: Keymap, run: Run) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .homogeneous(true)
            .build();
        root.add_css_class("functions-bar");
        let buttons: Vec<gtk::Button> = (1..=12)
            .map(|_| {
                let button = gtk::Button::builder().can_focus(false).build();
                button.add_css_class("flat");
                let label = gtk::Label::builder()
                    .use_markup(true)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build();
                button.set_child(Some(&label));
                root.append(&button);
                button
            })
            .collect();
        let bar = Rc::new(FunctionsBar {
            root,
            buttons,
            keymap,
            current: RefCell::new(vec![None; 12]),
        });
        for (i, button) in bar.buttons.iter().enumerate() {
            let bar = Rc::downgrade(&bar);
            let run = run.clone();
            button.connect_clicked(move |_| {
                if let Some(bar) = bar.upgrade()
                    && let Some(action) = bar.current.borrow()[i]
                {
                    run(action);
                }
            });
        }
        bar.set_mods(Mods::default());
        bar
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Relabel for the modifiers now held (called on modifier press/release).
    pub fn set_mods(&self, mods: Mods) {
        let mut prefix = String::new();
        if mods.ctrl {
            prefix.push_str("Ctrl+");
        }
        if mods.alt {
            prefix.push_str("Alt+");
        }
        if mods.shift {
            prefix.push_str("Shift+");
        }
        let mut current = self.current.borrow_mut();
        for (i, button) in self.buttons.iter().enumerate() {
            let chord = Chord::new(&format!("f{}", i + 1), mods);
            let action = self.keymap.lookup(&chord);
            current[i] = action;
            let label = button
                .child()
                .and_downcast::<gtk::Label>()
                .expect("label child");
            let key = glib::markup_escape_text(&format!("{prefix}F{}", i + 1));
            match action {
                Some(action) => {
                    let text = glib::markup_escape_text(action.label().trim_end_matches('…'));
                    label.set_markup(&format!("<b>{key}</b> {text}"));
                    button.set_sensitive(true);
                }
                None => {
                    label.set_markup(&format!("<b>{key}</b>"));
                    button.set_sensitive(false);
                }
            }
        }
    }
}

// ---- places bar ------------------------------------------------------------------

/// Home, root, and every mounted or mountable volume GIO knows about, as buttons.
/// Right-click a mount to unmount or eject it.
pub struct PlacesBar {
    root: gtk::Box,
    monitor: gio::VolumeMonitor,
    open: Rc<dyn Fn(PathBuf)>,
    window: gtk::Window,
}

impl PlacesBar {
    pub fn new(window: &gtk::Window, open: impl Fn(PathBuf) + 'static) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        root.add_css_class("places-bar");
        let bar = Rc::new(PlacesBar {
            root,
            monitor: gio::VolumeMonitor::get(),
            open: Rc::new(open),
            window: window.clone(),
        });
        let weak = Rc::downgrade(&bar);
        let rebuild = move || {
            if let Some(bar) = weak.upgrade() {
                bar.rebuild();
            }
        };
        let m = &bar.monitor;
        m.connect_mount_added(glib::clone!(
            #[strong]
            rebuild,
            move |_, _| rebuild()
        ));
        m.connect_mount_removed(glib::clone!(
            #[strong]
            rebuild,
            move |_, _| rebuild()
        ));
        m.connect_mount_changed(glib::clone!(
            #[strong]
            rebuild,
            move |_, _| rebuild()
        ));
        m.connect_volume_added(glib::clone!(
            #[strong]
            rebuild,
            move |_, _| rebuild()
        ));
        m.connect_volume_removed(glib::clone!(
            #[strong]
            rebuild,
            move |_, _| rebuild()
        ));
        m.connect_volume_changed(move |_, _| rebuild());
        bar.rebuild();
        bar
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    fn rebuild(&self) {
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        self.add_place("Home", "user-home-symbolic", glib::home_dir(), None);
        self.add_place("/", "drive-harddisk-symbolic", PathBuf::from("/"), None);
        // Mountable but not yet mounted volumes (a USB stick just plugged in).
        for volume in self.monitor.volumes() {
            if volume.get_mount().is_none() && volume.can_mount() {
                self.add_volume(volume);
            }
        }
        for mount in self.monitor.mounts() {
            if mount.is_shadowed() {
                continue;
            }
            if let Some(path) = mount.root().path() {
                let icon = mount.symbolic_icon();
                let button = self.add_place(
                    &mount.name(),
                    "drive-removable-media-symbolic",
                    path,
                    Some(&icon),
                );
                if mount.can_unmount() || mount.can_eject() {
                    self.attach_eject_menu(&button, mount);
                }
            }
        }
    }

    fn add_place(
        &self,
        name: &str,
        icon: &str,
        path: PathBuf,
        gicon: Option<&gio::Icon>,
    ) -> gtk::Button {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let image = match gicon {
            Some(gicon) => gtk::Image::from_gicon(gicon),
            None => gtk::Image::from_icon_name(icon),
        };
        content.append(&image);
        content.append(&gtk::Label::new(Some(name)));
        let button = gtk::Button::builder()
            .child(&content)
            .tooltip_text(path.to_string_lossy())
            .can_focus(false)
            .build();
        button.add_css_class("flat");
        let open = self.open.clone();
        button.connect_clicked(move |_| open(path.clone()));
        self.root.append(&button);
        button
    }

    fn add_volume(&self, volume: gio::Volume) {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        content.append(&gtk::Image::from_gicon(&volume.symbolic_icon()));
        content.append(&gtk::Label::new(Some(&volume.name())));
        let button = gtk::Button::builder()
            .child(&content)
            .tooltip_text("Not mounted — click to mount")
            .can_focus(false)
            .build();
        button.add_css_class("flat");
        button.add_css_class("dim-label");
        let open = self.open.clone();
        let window = self.window.clone();
        button.connect_clicked(move |_| {
            let operation = gtk::MountOperation::new(Some(&window));
            let open = open.clone();
            volume.mount(
                gio::MountMountFlags::NONE,
                Some(&operation),
                gio::Cancellable::NONE,
                glib::clone!(
                    #[strong]
                    volume,
                    move |result| {
                        if result.is_ok()
                            && let Some(path) = volume.get_mount().and_then(|m| m.root().path())
                        {
                            open(path);
                        }
                    }
                ),
            );
        });
        self.root.append(&button);
    }

    fn attach_eject_menu(&self, button: &gtk::Button, mount: gio::Mount) {
        let popover = gtk::Popover::builder().has_arrow(true).build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let label = if mount.can_eject() {
            "Eject"
        } else {
            "Unmount"
        };
        let item = gtk::Button::builder().label(label).build();
        item.add_css_class("flat");
        let window = self.window.clone();
        item.connect_clicked(glib::clone!(
            #[weak]
            popover,
            move |_| {
                popover.popdown();
                let operation = gtk::MountOperation::new(Some(&window));
                let flags = gio::MountUnmountFlags::NONE;
                if mount.can_eject() {
                    mount.eject_with_operation(
                        flags,
                        Some(&operation),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                } else {
                    mount.unmount_with_operation(
                        flags,
                        Some(&operation),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                }
            }
        ));
        content.append(&item);
        popover.set_child(Some(&content));
        popover.set_parent(button);
        let gesture = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        gesture.connect_pressed(move |_, _, _, _| popover.popup());
        button.add_controller(gesture);
    }
}

// ---- shortcuts window --------------------------------------------------------------

/// A scrollable list of every action with its current bindings, in menu order.
pub fn shortcuts_window(parent: &gtk::Window, keymap: &Keymap) {
    let grid = gtk::Grid::builder()
        .column_spacing(24)
        .row_spacing(4)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let mut row = 0;
    let mut listed = std::collections::HashSet::new();
    let add_group = |title: &str, actions: &[Action], row: &mut i32| {
        let heading = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .margin_top(if *row == 0 { 0 } else { 12 })
            .build();
        heading.add_css_class("heading");
        grid.attach(&heading, 0, *row, 2, 1);
        *row += 1;
        for &action in actions {
            let keys: Vec<String> = keymap
                .chords_for(action)
                .iter()
                .map(|c| chord_label(c))
                .collect();
            let name = gtk::Label::builder()
                .label(action.label())
                .xalign(0.0)
                .build();
            let chord = gtk::Label::builder()
                .label(keys.join(", "))
                .xalign(0.0)
                .build();
            chord.add_css_class("dim-label");
            grid.attach(&name, 0, *row, 1, 1);
            grid.attach(&chord, 1, *row, 1, 1);
            *row += 1;
        }
    };
    for (title, sections) in MENUS {
        let actions: Vec<Action> = sections.iter().flat_map(|s| s.iter().copied()).collect();
        listed.extend(actions.iter().copied());
        add_group(&title.replace('_', ""), &actions, &mut row);
    }
    let rest: Vec<Action> = Action::ALL
        .iter()
        .copied()
        .filter(|a| !listed.contains(a))
        .collect();
    if !rest.is_empty() {
        add_group("Selection & other", &rest, &mut row);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .child(&grid)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title("Keyboard shortcuts")
        .default_width(520)
        .default_height(640)
        .child(&scroller)
        .build();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.destroy();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    ));
    window.add_controller(keys);
    window.present();
}
