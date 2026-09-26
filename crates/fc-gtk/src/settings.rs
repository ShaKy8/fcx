//! Settings dialog (F12). Edits a copy of the settings; Save writes the file
//! and hands the new values to the app to apply live.

use std::rc::Rc;

use fc_core::config::{DragDefault, Settings, ViewKind};
use gtk::prelude::*;
use gtk::{gdk, glib};

const VIEWS: [(&str, ViewKind); 3] = [
    ("Details", ViewKind::Details),
    ("List", ViewKind::List),
    ("Thumbnails", ViewKind::Thumbnails),
];
const DRAGS: [(&str, DragDefault); 3] = [
    ("Move within a filesystem, copy across", DragDefault::Auto),
    ("Always copy", DragDefault::Copy),
    ("Always move", DragDefault::Move),
];

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).build();
    label.add_css_class("heading");
    label.set_margin_top(6);
    label
}

fn check(label: &str, active: bool) -> gtk::CheckButton {
    gtk::CheckButton::builder()
        .label(label)
        .active(active)
        .build()
}

fn dropdown<T: PartialEq + Copy>(choices: &[(&str, T)], current: T) -> gtk::DropDown {
    let labels: Vec<&str> = choices.iter().map(|(l, _)| *l).collect();
    let dropdown = gtk::DropDown::from_strings(&labels);
    if let Some(i) = choices.iter().position(|(_, v)| *v == current) {
        dropdown.set_selected(i as u32);
    }
    dropdown
}

/// `on_save` receives the new settings; `on_edit_keymap` opens the key file.
pub fn show(
    parent: &gtk::Window,
    current: Settings,
    on_save: impl Fn(Settings) + 'static,
    on_edit_keymap: impl Fn() + 'static,
) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Settings")
        .default_width(520)
        .resizable(false)
        .build();

    let confirm_trash = check("Confirm before moving to the trash", current.confirm_trash);
    let confirm_permanent = check(
        "Confirm before deleting permanently",
        current.confirm_permanent_delete,
    );
    let show_hidden = check("Show hidden files in new panes", current.show_hidden);
    let restore_tabs = check("Reopen last session's tabs on start", current.restore_tabs);
    let default_view = dropdown(&VIEWS, current.default_view);
    let drag_default = dropdown(&DRAGS, current.drag_default);
    let menu_bar = check("Menu bar", current.show_menu_bar);
    let toolbar = check("Toolbar", current.show_toolbar);
    let places_bar = check("Places bar", current.show_places_bar);
    let functions_bar = check("Functions bar (F-keys)", current.show_functions_bar);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&heading("Confirmations"));
    content.append(&confirm_trash);
    content.append(&confirm_permanent);
    content.append(&heading("Panes"));
    content.append(&show_hidden);
    content.append(&restore_tabs);
    let view_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    view_row.append(&gtk::Label::new(Some("Default view")));
    view_row.append(&default_view);
    content.append(&view_row);
    content.append(&heading("Drag and drop"));
    let drag_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    drag_row.append(&gtk::Label::new(Some("Dropping files")));
    drag_row.append(&drag_default);
    content.append(&drag_row);
    content.append(&heading("Window"));
    for c in [&menu_bar, &toolbar, &places_bar, &functions_bar] {
        content.append(c);
    }
    content.append(&heading("Keys"));
    let keymap_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let edit_keys = gtk::Button::with_label("Edit key bindings…");
    let keys_note = gtk::Label::builder()
        .label("Opens ~/.config/fcx/keymap.toml in your editor; changes apply on restart.")
        .xalign(0.0)
        .wrap(true)
        .build();
    keys_note.add_css_class("dim-label");
    keymap_row.append(&edit_keys);
    keymap_row.append(&keys_note);
    content.append(&keymap_row);

    let reset = gtk::Button::with_label("Reset to defaults");
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(12)
        .build();
    buttons.append(&reset);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    buttons.append(&spacer);
    buttons.append(&cancel);
    buttons.append(&save);
    content.append(&buttons);
    window.set_child(Some(&content));

    let collect = {
        let (confirm_trash, confirm_permanent) = (confirm_trash.clone(), confirm_permanent.clone());
        let (show_hidden, restore_tabs) = (show_hidden.clone(), restore_tabs.clone());
        let (default_view, drag_default) = (default_view.clone(), drag_default.clone());
        let (menu_bar, toolbar, places_bar, functions_bar) = (
            menu_bar.clone(),
            toolbar.clone(),
            places_bar.clone(),
            functions_bar.clone(),
        );
        move || Settings {
            confirm_trash: confirm_trash.is_active(),
            confirm_permanent_delete: confirm_permanent.is_active(),
            show_hidden: show_hidden.is_active(),
            default_view: VIEWS[default_view.selected() as usize].1,
            restore_tabs: restore_tabs.is_active(),
            drag_default: DRAGS[drag_default.selected() as usize].1,
            show_menu_bar: menu_bar.is_active(),
            show_toolbar: toolbar.is_active(),
            show_places_bar: places_bar.is_active(),
            show_functions_bar: functions_bar.is_active(),
        }
    };
    let on_save = Rc::new(on_save);
    save.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        on_save,
        move |_| {
            on_save(collect());
            window.destroy();
        }
    ));
    reset.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| {
            on_save(Settings::default());
            window.destroy();
        }
    ));
    cancel.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.destroy()
    ));
    edit_keys.connect_clicked(move |_| on_edit_keymap());

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
