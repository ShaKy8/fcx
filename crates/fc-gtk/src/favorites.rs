//! Favorites UI: the dynamic menu section (also used for the Alt+Up popup) and
//! the Shift+Ctrl+F editor dialog. Storage lives in `fc_core::favorites`.

use std::path::PathBuf;
use std::rc::Rc;

use fc_core::favorites::{Favorite, Favorites};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops;

/// GAction that opens a favorite; the target is the folder path.
pub const OPEN_ACTION: &str = "open-favorite";

/// Rebuilds `menu` from `favorites` and (re)binds Shift+Ctrl+1…0 to the first ten.
pub fn refresh_menu(gtk_app: &gtk::Application, menu: &gio::Menu, favorites: &Favorites) {
    menu.remove_all();
    for (i, fav) in favorites.items.iter().enumerate() {
        let item = gio::MenuItem::new(Some(&fav.name), None);
        let target = fav.path.to_string_lossy().to_variant();
        item.set_action_and_target_value(Some(&format!("app.{OPEN_ACTION}")), Some(&target));
        menu.append_item(&item);
        let detailed = gio::Action::print_detailed_name(OPEN_ACTION, Some(&target));
        let accels: Vec<String> = match i {
            0..=8 => vec![format!("<Shift><Control>{}", i + 1)],
            9 => vec!["<Shift><Control>0".to_owned()],
            _ => Vec::new(),
        };
        let refs: Vec<&str> = accels.iter().map(String::as_str).collect();
        gtk_app.set_accels_for_action(&format!("app.{detailed}"), &refs);
    }
    if favorites.items.is_empty() {
        let item = gio::MenuItem::new(
            Some("(no favorites yet — Shift+Ctrl+V adds the current folder)"),
            None,
        );
        menu.append_item(&item);
    }
}

/// Alt+Up: the favorites as a popover anchored on `anchor`.
pub fn popup(anchor: &gtk::Widget, menu: &gio::Menu) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(anchor);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(24, 24, 1, 1)));
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}

/// Shift+Ctrl+F: edit names, paths, and order. `on_save` receives the new list.
pub fn edit(
    parent: &gtk::Window,
    favorites: Favorites,
    current_folder: Option<PathBuf>,
    on_save: impl Fn(Favorites) + 'static,
) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Favorites")
        .default_width(640)
        .default_height(420)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .vexpand(true)
        .build();
    list.add_css_class("boxed-list");
    for fav in &favorites.items {
        list.append(&row(fav));
    }

    let add = gtk::Button::with_label("Add current folder");
    add.set_sensitive(current_folder.is_some());
    let remove = gtk::Button::with_label("Remove");
    let up = gtk::Button::from_icon_name("go-up-symbolic");
    let down = gtk::Button::from_icon_name("go-down-symbolic");
    let side = gtk::Box::new(gtk::Orientation::Vertical, 6);
    for b in [&add, &remove, &up, &down] {
        side.append(b);
    }

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hexpand(true)
        .vexpand(true)
        .build();
    body.append(&scroller);
    body.append(&side);

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&cancel);
    buttons.append(&save);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&body);
    content.append(&buttons);
    window.set_child(Some(&content));

    add.connect_clicked(glib::clone!(
        #[weak]
        list,
        move |_| {
            if let Some(path) = &current_folder {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into());
                let r = row(&Favorite {
                    name,
                    path: path.clone(),
                });
                list.append(&r);
                list.select_row(Some(&r));
            }
        }
    ));
    remove.connect_clicked(glib::clone!(
        #[weak]
        list,
        move |_| {
            if let Some(r) = list.selected_row() {
                list.remove(&r);
            }
        }
    ));
    let shift = |list: &gtk::ListBox, delta: i32| {
        let Some(r) = list.selected_row() else {
            return;
        };
        let index = r.index();
        let target = index + delta;
        if target < 0 {
            return;
        }
        list.remove(&r);
        list.insert(&r, target);
        list.select_row(Some(&r));
    };
    up.connect_clicked(glib::clone!(
        #[weak]
        list,
        move |_| shift(&list, -1)
    ));
    down.connect_clicked(glib::clone!(
        #[weak]
        list,
        move |_| shift(&list, 1)
    ));
    cancel.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.destroy()
    ));
    let on_save = Rc::new(on_save);
    save.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[weak]
        list,
        move |_| {
            let mut favorites = Favorites::default();
            let mut i = 0;
            while let Some(r) = list.row_at_index(i) {
                if let Some(fav) = favorite_of(&r) {
                    favorites.items.push(fav);
                }
                i += 1;
            }
            window.destroy();
            on_save(favorites);
        }
    ));

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

/// A row is two entries: name and path.
fn row(fav: &Favorite) -> gtk::ListBoxRow {
    let name = gtk::Entry::builder()
        .text(&fav.name)
        .width_chars(16)
        .build();
    let path = gtk::Entry::builder()
        .text(fav.path.to_string_lossy())
        .hexpand(true)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(6)
        .margin_end(6)
        .build();
    content.append(&name);
    content.append(&path);
    gtk::ListBoxRow::builder().child(&content).build()
}

fn favorite_of(row: &gtk::ListBoxRow) -> Option<Favorite> {
    let content = row.child().and_downcast::<gtk::Box>()?;
    let name = content.first_child().and_downcast::<gtk::Entry>()?;
    let path = content.last_child().and_downcast::<gtk::Entry>()?;
    let path = path.text().trim().to_owned();
    if path.is_empty() {
        return None;
    }
    let mut name = name.text().trim().to_owned();
    if name.is_empty() {
        name = path.clone();
    }
    Some(Favorite {
        name,
        path: PathBuf::from(path),
    })
}

/// Reports a favorites file problem without stopping the app.
pub fn warn(parent: &gtk::Window, err: &dyn std::fmt::Display) {
    ops::alert(parent, "Favorites", &err.to_string());
}
