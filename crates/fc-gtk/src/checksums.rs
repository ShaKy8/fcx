//! Checksums window (Ctrl+K): hashes the given files on a worker thread,
//! shows the results, copies them, or writes a `*.sha256`-style sum file.

use std::path::PathBuf;
use std::rc::Rc;

use fc_core::checksum::{self, Algorithm};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops;

struct Row {
    name: String,
    hash: String,
}

pub fn show(parent: &gtk::Window, dir: PathBuf, files: Vec<PathBuf>) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title(format!("Checksums — {} file(s)", files.len()))
        .default_width(820)
        .default_height(480)
        .build();
    crate::ops::fit(&window, parent, 820, 480);

    let algorithm = gtk::DropDown::from_strings(&Algorithm::ALL.map(Algorithm::label));
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(store.clone()))));
    view.add_css_class("data-table");
    for (title, expand, text) in [
        (
            "File",
            true,
            (|r: &Row| r.name.clone()) as fn(&Row) -> String,
        ),
        ("Checksum", true, |r: &Row| r.hash.clone()),
    ] {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let label = gtk::Label::builder()
                .xalign(0.0)
                .selectable(true)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            label.add_css_class("numeric");
            item.set_child(Some(&label));
        });
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let label = item.child().and_downcast::<gtk::Label>().expect("label");
            if let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() {
                label.set_text(&text(&obj.borrow::<Row>()));
            }
        });
        let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        column.set_expand(expand);
        column.set_resizable(true);
        view.append_column(&column);
    }

    let status = gtk::Label::builder().xalign(0.0).hexpand(true).build();
    let copy = gtk::Button::with_label("Copy");
    let save = gtk::Button::with_label("Save sum file");
    let close = gtk::Button::with_label("Close");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&gtk::Label::new(Some("Algorithm")));
    top.append(&algorithm);
    let footer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    footer.append(&status);
    for b in [&copy, &save, &close] {
        footer.append(b);
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&top);
    content.append(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build(),
    );
    content.append(&footer);
    window.set_child(Some(&content));

    let files = Rc::new(files);
    let compute = {
        let files = files.clone();
        let store = store.clone();
        let status = status.clone();
        let algorithm = algorithm.clone();
        Rc::new(move || {
            let algo = Algorithm::ALL[algorithm.selected() as usize];
            let files: Vec<PathBuf> = (*files).clone();
            store.remove_all();
            status.set_text("Computing…");
            glib::spawn_future_local(glib::clone!(
                #[weak]
                store,
                #[weak]
                status,
                async move {
                    let results = gio::spawn_blocking(move || {
                        files
                            .iter()
                            .map(|path| {
                                let name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let hash = checksum::compute(path, algo)
                                    .unwrap_or_else(|err| format!("error: {err}"));
                                (name, hash)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await
                    .unwrap_or_default();
                    let n = results.len();
                    let objects: Vec<glib::BoxedAnyObject> = results
                        .into_iter()
                        .map(|(name, hash)| glib::BoxedAnyObject::new(Row { name, hash }))
                        .collect();
                    store.splice(0, store.n_items(), &objects);
                    status.set_text(&format!("{n} file(s) · {}", algo.label()));
                }
            ));
        })
    };
    algorithm.connect_selected_notify(glib::clone!(
        #[strong]
        compute,
        move |_| compute()
    ));
    compute();

    let rows = {
        let store = store.clone();
        move || -> Vec<(String, String)> {
            (0..store.n_items())
                .filter_map(|i| store.item(i).and_downcast::<glib::BoxedAnyObject>())
                .map(|o| {
                    let r = o.borrow::<Row>();
                    (r.name.clone(), r.hash.clone())
                })
                .collect()
        }
    };
    copy.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        rows,
        move |_| {
            let text: Vec<String> = rows()
                .into_iter()
                .map(|(n, h)| format!("{h}  {n}"))
                .collect();
            window.clipboard().set_text(&text.join("\n"));
        }
    ));
    save.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[weak]
        algorithm,
        move |_| {
            let algo = Algorithm::ALL[algorithm.selected() as usize];
            let entries = rows();
            let stem = if entries.len() == 1 {
                entries[0].0.clone()
            } else {
                "checksums".to_owned()
            };
            match checksum::write_sum_file(&dir, &stem, algo, &entries) {
                Ok(path) => ops::alert(&window, "Saved", &path.to_string_lossy()),
                Err(err) => ops::alert(&window, "Cannot save", &err.to_string()),
            }
        }
    ));
    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.destroy()
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
