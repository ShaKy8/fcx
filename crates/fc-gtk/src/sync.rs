//! Synchronize folders dialog (Alt+S): compare two folders on a worker thread,
//! preview what each direction would do, then hand the actions to the app.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fc_core::compare::{self, Diff, Direction, Method, Options, Report, Status, SyncAction};
use fc_core::format::human_size;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops;

const METHODS: [(&str, Method); 2] = [
    ("Size and time", Method::SizeAndTime),
    ("Content", Method::Content),
];
const DIRECTIONS: [(&str, Direction); 3] = [
    ("Left → Right", Direction::LeftToRight),
    ("Right → Left", Direction::RightToLeft),
    ("Both ways (newer wins)", Direction::Both),
];

struct PreviewRow {
    diff: Diff,
    action: String,
}

fn meta_text(meta: Option<compare::Meta>, is_dir: bool) -> String {
    match meta {
        None => String::new(),
        Some(_) if is_dir => "folder".into(),
        Some(m) => {
            let when = m
                .modified
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
                .and_then(|dt| dt.format("%Y-%m-%d %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default();
            format!("{} · {when}", human_size(m.size))
        }
    }
}

fn status_text(status: Status) -> &'static str {
    match status {
        Status::LeftOnly => "only left",
        Status::RightOnly => "only right",
        Status::Same => "same",
        Status::Different => "differs",
        Status::LeftNewer => "newer left",
        Status::RightNewer => "newer right",
    }
}

fn action_text(action: Option<&SyncAction>) -> String {
    match action {
        None => String::new(),
        Some(SyncAction::Copy { to, .. }) => format!("copy → {}", to.to_string_lossy()),
        Some(SyncAction::Delete { path }) => format!("delete {}", path.to_string_lossy()),
        Some(SyncAction::Skip { reason, .. }) => format!("skip: {reason}"),
    }
}

fn labelled(grid: &gtk::Grid, col: i32, row: i32, text: &str, widget: &impl IsA<gtk::Widget>) {
    let label = gtk::Label::builder().label(text).xalign(1.0).build();
    label.add_css_class("dim-label");
    grid.attach(&label, col, row, 1, 1);
    grid.attach(widget, col + 1, row, 1, 1);
}

/// Opens the dialog for `left` and `right`; `on_sync` receives the actions to run.
pub fn show(
    parent: &gtk::Window,
    left: PathBuf,
    right: PathBuf,
    on_sync: impl Fn(Vec<SyncAction>) + 'static,
) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title("Synchronize folders")
        .default_width(1000)
        .default_height(680)
        .build();

    let left_entry = gtk::Entry::builder()
        .text(left.to_string_lossy())
        .width_chars(16)
        .hexpand(true)
        .build();
    let right_entry = gtk::Entry::builder()
        .text(right.to_string_lossy())
        .width_chars(16)
        .hexpand(true)
        .build();
    let recursive = gtk::CheckButton::builder()
        .label("Include subfolders")
        .active(true)
        .build();
    let hidden = gtk::CheckButton::builder()
        .label("Include hidden")
        .active(true)
        .build();
    let method = gtk::DropDown::from_strings(&METHODS.map(|(l, _)| l));
    let direction = gtk::DropDown::from_strings(&DIRECTIONS.map(|(l, _)| l));
    let delete_extra =
        gtk::CheckButton::with_label("Delete items only on the target side (to trash)");
    let show_same = gtk::CheckButton::with_label("Show identical files");

    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(8)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    labelled(&grid, 0, 0, "Left", &left_entry);
    labelled(&grid, 0, 1, "Right", &right_entry);
    let options = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    options.append(&recursive);
    options.append(&hidden);
    options.append(&gtk::Label::new(Some("Compare by")));
    options.append(&method);
    options.append(&show_same);
    grid.attach(&options, 1, 2, 1, 1);
    let sync_opts = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    sync_opts.append(&gtk::Label::new(Some("Direction")));
    sync_opts.append(&direction);
    sync_opts.append(&delete_extra);
    grid.attach(&sync_opts, 1, 3, 1, 1);

    // ---- preview ---------------------------------------------------------------
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(store.clone()))));
    view.add_css_class("data-table");
    column(&view, "Path", true, |r| {
        r.diff.rel.to_string_lossy().into_owned()
    });
    column(&view, "Left", false, |r| {
        meta_text(r.diff.left, r.diff.is_dir)
    });
    column(&view, "Status", false, |r| {
        status_text(r.diff.status).into()
    });
    column(&view, "Right", false, |r| {
        meta_text(r.diff.right, r.diff.is_dir)
    });
    column(&view, "Action", true, |r| r.action.clone());
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .margin_start(12)
        .margin_end(12)
        .build();

    let status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .margin_start(12)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let compare_button = gtk::Button::with_label("Compare");
    compare_button.add_css_class("suggested-action");
    let stop = gtk::Button::builder()
        .label("Stop")
        .sensitive(false)
        .build();
    let sync_button = gtk::Button::builder()
        .label("Synchronize")
        .sensitive(false)
        .build();
    sync_button.add_css_class("destructive-action");
    sync_button.set_tooltip_text(Some("Ctrl+Enter"));
    let close = gtk::Button::with_label("Close");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .margin_end(12)
        .margin_bottom(12)
        .build();
    for b in [&compare_button, &stop, &sync_button, &close] {
        buttons.append(b);
    }
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    footer.append(&status);
    footer.append(&buttons);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&grid);
    content.append(&scroller);
    content.append(&footer);
    window.set_child(Some(&content));
    // Enter in a path field re-compares; Ctrl+Enter synchronizes (once compared).
    for entry in [&left_entry, &right_entry] {
        entry.connect_activate(glib::clone!(
            #[weak]
            compare_button,
            move |_| compare_button.emit_clicked()
        ));
    }

    // ---- state -------------------------------------------------------------------
    struct State {
        report: Option<Report>,
        roots: (PathBuf, PathBuf),
        cancel: Option<Arc<AtomicBool>>,
    }
    let state = Rc::new(RefCell::new(State {
        report: None,
        roots: (left, right),
        cancel: None,
    }));

    // Re-plan actions from the current report whenever an option changes.
    let refresh = {
        let state = state.clone();
        let store = store.clone();
        let status = status.clone();
        let direction = direction.clone();
        let delete_extra = delete_extra.clone();
        let show_same = show_same.clone();
        let sync_button = sync_button.clone();
        Rc::new(move || {
            let st = state.borrow();
            let Some(report) = &st.report else {
                return;
            };
            let dir = DIRECTIONS[direction.selected() as usize].1;
            delete_extra.set_sensitive(dir != Direction::Both);
            let deleting = delete_extra.is_active() && dir != Direction::Both;
            let actions = compare::plan_sync(&st.roots.0, &st.roots.1, report, dir, deleting);
            let mut by_rel: std::collections::HashMap<&std::path::Path, &SyncAction> =
                std::collections::HashMap::new();
            let mut ai = actions.iter();
            let (mut copies, mut deletes, mut skips) = (0, 0, 0);
            for diff in report.differences() {
                if let Some(action) = ai.next() {
                    by_rel.insert(&diff.rel, action);
                    match action {
                        SyncAction::Copy { .. } => copies += 1,
                        SyncAction::Delete { .. } => deletes += 1,
                        SyncAction::Skip { .. } => skips += 1,
                    }
                }
            }
            let rows: Vec<glib::BoxedAnyObject> = report
                .items
                .iter()
                .filter(|d| show_same.is_active() || d.status != Status::Same)
                .map(|d| {
                    glib::BoxedAnyObject::new(PreviewRow {
                        action: action_text(by_rel.get(d.rel.as_path()).copied()),
                        diff: d.clone(),
                    })
                })
                .collect();
            store.splice(0, store.n_items(), &rows);
            let mut text = format!(
                "{} differences · {copies} to copy · {deletes} to delete · {skips} skipped",
                report.differences().count()
            );
            if report.cancelled {
                text.push_str(" · stopped");
            }
            if !report.errors.is_empty() {
                text.push_str(&format!(" · {} folders unreadable", report.errors.len()));
            }
            status.set_text(&text);
            sync_button.set_sensitive(copies + deletes > 0);
        })
    };
    direction.connect_selected_notify(glib::clone!(
        #[strong]
        refresh,
        move |_| refresh()
    ));
    delete_extra.connect_toggled(glib::clone!(
        #[strong]
        refresh,
        move |_| refresh()
    ));
    show_same.connect_toggled(glib::clone!(
        #[strong]
        refresh,
        move |_| refresh()
    ));

    let run_compare = {
        let state = state.clone();
        let refresh = refresh.clone();
        let (left_entry, right_entry) = (left_entry.clone(), right_entry.clone());
        let (recursive, hidden, method) = (recursive.clone(), hidden.clone(), method.clone());
        let (status, store) = (status.clone(), store.clone());
        let (compare_button, stop, sync_button) =
            (compare_button.clone(), stop.clone(), sync_button.clone());
        Rc::new(move || {
            let l = crate::pane::expand_path(&left_entry.text(), None);
            let r = crate::pane::expand_path(&right_entry.text(), None);
            let options = Options {
                recursive: recursive.is_active(),
                method: METHODS[method.selected() as usize].1,
                include_hidden: hidden.is_active(),
            };
            let flag = Arc::new(AtomicBool::new(false));
            {
                let mut st = state.borrow_mut();
                if let Some(previous) = st.cancel.take() {
                    previous.store(true, Ordering::Relaxed);
                }
                st.cancel = Some(flag.clone());
                st.roots = (l.clone(), r.clone());
                st.report = None;
            }
            store.remove_all();
            status.set_text("Comparing…");
            compare_button.set_sensitive(false);
            stop.set_sensitive(true);
            sync_button.set_sensitive(false);

            let (tx, rx) = async_channel::bounded::<Result<Report, String>>(1);
            let worker_flag = flag.clone();
            std::thread::spawn(move || {
                let result =
                    compare::compare(&l, &r, &options, &worker_flag).map_err(|e| e.to_string());
                let _ = tx.send_blocking(result);
            });
            glib::spawn_future_local(glib::clone!(
                #[strong]
                state,
                #[strong]
                refresh,
                #[weak]
                status,
                #[weak]
                compare_button,
                #[weak]
                stop,
                async move {
                    let Ok(result) = rx.recv().await else {
                        return;
                    };
                    // Ignore results of a comparison that was superseded.
                    let current = state.borrow().cancel.as_ref().map(Arc::as_ptr);
                    if current != Some(Arc::as_ptr(&flag)) {
                        return;
                    }
                    compare_button.set_sensitive(true);
                    stop.set_sensitive(false);
                    match result {
                        Ok(report) => {
                            state.borrow_mut().report = Some(report);
                            refresh();
                        }
                        Err(err) => status.set_text(&err),
                    }
                }
            ));
        })
    };
    compare_button.connect_clicked(glib::clone!(
        #[strong]
        run_compare,
        move |_| run_compare()
    ));
    stop.connect_clicked(glib::clone!(
        #[strong]
        state,
        move |_| {
            if let Some(flag) = state.borrow().cancel.as_ref() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    ));

    let on_sync = Rc::new(on_sync);
    sync_button.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        state,
        #[weak]
        direction,
        #[weak]
        delete_extra,
        move |_| {
            let actions = {
                let st = state.borrow();
                let Some(report) = &st.report else {
                    return;
                };
                let dir = DIRECTIONS[direction.selected() as usize].1;
                let deleting = delete_extra.is_active() && dir != Direction::Both;
                compare::plan_sync(&st.roots.0, &st.roots.1, report, dir, deleting)
            };
            let deletes = actions
                .iter()
                .filter(|a| matches!(a, SyncAction::Delete { .. }))
                .count();
            let copies = actions
                .iter()
                .filter(|a| matches!(a, SyncAction::Copy { .. }))
                .count();
            let on_sync = on_sync.clone();
            let window2 = window.clone();
            ops::confirm(
                &window,
                &format!("Synchronize: copy {copies} item(s), delete {deletes}?"),
                "Copies overwrite the target side without asking. Deleted items go to the trash.",
                "Synchronize",
                move || {
                    window2.destroy();
                    on_sync(actions);
                },
            );
        }
    ));
    let close_window = glib::clone!(
        #[weak]
        window,
        #[strong]
        state,
        move || {
            if let Some(flag) = state.borrow().cancel.as_ref() {
                flag.store(true, Ordering::Relaxed);
            }
            window.destroy();
        }
    );
    close.connect_clicked(glib::clone!(
        #[strong]
        close_window,
        move |_| close_window()
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        sync_button,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Escape => {
                    close_window();
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter if ctrl => {
                    if sync_button.is_sensitive() {
                        sync_button.emit_clicked();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    window.add_controller(keys);

    window.present();
    run_compare();
}

fn column(view: &gtk::ColumnView, title: &str, expand: bool, text: fn(&PreviewRow) -> String) {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let label = item.child().and_downcast::<gtk::Label>().expect("label");
        if let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() {
            let row = obj.borrow::<PreviewRow>();
            label.set_text(&text(&row));
            if row.action.starts_with("delete") {
                label.add_css_class("error");
            } else {
                label.remove_css_class("error");
            }
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(expand);
    column.set_resizable(true);
    view.append_column(&column);
}
