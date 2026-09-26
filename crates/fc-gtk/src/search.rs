//! Search window (Ctrl+F / Alt+F7). The query runs on a worker thread and
//! streams hits into a result list; results can be shown in the active pane
//! or copied to the clipboard as files (paste into a pane copies them).

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fc_core::format::human_size;
use fc_core::search::{self, Hit, Kind, Query, Stats};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops;

const KINDS: [(&str, Kind); 3] = [
    ("Files and folders", Kind::Any),
    ("Files", Kind::Files),
    ("Folders", Kind::Folders),
];
/// Hits are batched to the UI at this interval or every `BATCH_SIZE` hits.
const BATCH_INTERVAL: Duration = Duration::from_millis(60);
const BATCH_SIZE: usize = 100;

enum Msg {
    Hits(Vec<Hit>),
    Done(Result<Stats, String>),
}

struct Fields {
    root: gtk::Entry,
    subfolders: gtk::CheckButton,
    name: gtk::Entry,
    kind: gtk::DropDown,
    content: gtk::Entry,
    regex: gtk::CheckButton,
    case_sensitive: gtk::CheckButton,
    size_min: gtk::Entry,
    size_max: gtk::Entry,
    date_from: gtk::Entry,
    date_to: gtk::Entry,
    hidden: gtk::CheckButton,
}

impl Fields {
    fn query(&self) -> Result<Query, String> {
        let root = crate::pane::expand_path(&self.root.text(), None);
        let mut q = Query::new(root);
        q.name = self.name.text().to_string();
        q.content = self.content.text().to_string();
        q.content_regex = self.regex.is_active();
        q.case_sensitive = self.case_sensitive.is_active();
        q.kind = KINDS[self.kind.selected() as usize].1;
        q.include_hidden = self.hidden.is_active();
        q.max_depth = if self.subfolders.is_active() {
            None
        } else {
            Some(0)
        };
        let size = |entry: &gtk::Entry, what: &str| -> Result<Option<u64>, String> {
            let text = entry.text();
            if text.trim().is_empty() {
                return Ok(None);
            }
            search::parse_size(&text).map(Some).ok_or_else(|| {
                format!(
                    "{what} size “{}” is not a size like 10k or 2.5M",
                    text.trim()
                )
            })
        };
        q.min_size = size(&self.size_min, "Minimum")?;
        q.max_size = size(&self.size_max, "Maximum")?;
        let date = |entry: &gtk::Entry, end: bool, what: &str| {
            let text = entry.text();
            if text.trim().is_empty() {
                return Ok(None);
            }
            search::parse_date(&text, end)
                .map(Some)
                .ok_or_else(|| format!("{what} date “{}” is not YYYY-MM-DD", text.trim()))
        };
        q.modified_after = date(&self.date_from, false, "From")?;
        q.modified_before = date(&self.date_to, true, "To")?;
        Ok(q)
    }
}

fn labelled(grid: &gtk::Grid, col: i32, row: i32, text: &str, widget: &impl IsA<gtk::Widget>) {
    let label = gtk::Label::builder().label(text).xalign(1.0).build();
    label.add_css_class("dim-label");
    grid.attach(&label, col, row, 1, 1);
    grid.attach(widget, col + 1, row, 1, 1);
}

/// Opens the search window rooted at `start_dir`. `show_in_pane` gets the
/// folder and name of a result the user wants to jump to.
pub fn show(
    parent: &gtk::Window,
    start_dir: PathBuf,
    show_in_pane: impl Fn(PathBuf, OsString) + 'static,
) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title("Search")
        .default_width(960)
        .default_height(680)
        .build();
    crate::ops::fit(&window, parent, 960, 680);

    let fields = Rc::new(Fields {
        root: gtk::Entry::builder()
            .text(start_dir.to_string_lossy())
            .width_chars(12)
            .hexpand(true)
            .build(),
        subfolders: gtk::CheckButton::builder()
            .label("Include subfolders")
            .active(true)
            .build(),
        name: gtk::Entry::builder()
            .placeholder_text("*.jpg; *.png  (empty = all)")
            .width_chars(12)
            .hexpand(true)
            .build(),
        kind: gtk::DropDown::from_strings(&KINDS.map(|(l, _)| l)),
        content: gtk::Entry::builder()
            .placeholder_text("text the file must contain")
            .width_chars(12)
            .hexpand(true)
            .build(),
        regex: gtk::CheckButton::with_label("Regular expression"),
        case_sensitive: gtk::CheckButton::with_label("Case sensitive"),
        size_min: gtk::Entry::builder()
            .width_chars(8)
            .placeholder_text("10k")
            .build(),
        size_max: gtk::Entry::builder()
            .width_chars(8)
            .placeholder_text("2M")
            .build(),
        date_from: gtk::Entry::builder()
            .width_chars(9)
            .placeholder_text("YYYY-MM-DD")
            .build(),
        date_to: gtk::Entry::builder()
            .width_chars(9)
            .placeholder_text("YYYY-MM-DD")
            .build(),
        hidden: gtk::CheckButton::with_label("Include hidden"),
    });

    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(8)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    labelled(&grid, 0, 0, "Search in", &fields.root);
    grid.attach(&fields.subfolders, 2, 0, 2, 1);
    labelled(&grid, 0, 1, "File name", &fields.name);
    labelled(&grid, 2, 1, "Type", &fields.kind);
    labelled(&grid, 0, 2, "Containing", &fields.content);
    let content_opts = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content_opts.append(&fields.regex);
    content_opts.append(&fields.case_sensitive);
    content_opts.append(&fields.hidden);
    grid.attach(&content_opts, 2, 2, 2, 1);
    let ranges = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    ranges.set_hexpand(true);
    ranges.append(&fields.size_min);
    ranges.append(&gtk::Label::new(Some("to")));
    ranges.append(&fields.size_max);
    ranges.append(&gtk::Label::new(Some("   Modified")));
    ranges.append(&fields.date_from);
    ranges.append(&gtk::Label::new(Some("to")));
    ranges.append(&fields.date_to);
    let size_label = gtk::Label::builder().label("Size").xalign(1.0).build();
    size_label.add_css_class("dim-label");
    grid.attach(&size_label, 0, 3, 1, 1);
    grid.attach(&ranges, 1, 3, 3, 1);

    // ---- results -----------------------------------------------------------------
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::MultiSelection::new(Some(store.clone()));
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("data-table");
    column(&view, "Name", true, |h| {
        h.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    column(&view, "Folder", true, |h| {
        h.path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    column(&view, "Size", false, |h| {
        if h.is_dir {
            String::new()
        } else {
            human_size(h.size)
        }
    });
    column(&view, "Modified", false, |h| {
        h.modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
            .and_then(|dt| dt.format("%Y-%m-%d %H:%M").ok())
            .map(|s| s.to_string())
            .unwrap_or_default()
    });
    column(&view, "Line", true, |h| match &h.line {
        Some((no, text)) => format!("{no}: {text}"),
        None => String::new(),
    });
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .margin_start(12)
        .margin_end(12)
        .build();

    let status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let start = gtk::Button::with_label("Start");
    start.add_css_class("suggested-action");
    let stop = gtk::Button::builder()
        .label("Stop")
        .sensitive(false)
        .build();
    let goto = gtk::Button::with_label("Show in pane");
    let copy = gtk::Button::with_label("Copy as files");
    let close = gtk::Button::with_label("Close");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .margin_end(12)
        .margin_bottom(12)
        .build();
    for b in [&start, &stop, &goto, &copy, &close] {
        buttons.append(b);
    }
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.set_margin_start(12);
    footer.append(&status);
    footer.append(&buttons);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&grid);
    content.append(&scroller);
    content.append(&footer);
    window.set_child(Some(&content));
    window.set_default_widget(Some(&start));
    for entry in [
        &fields.root,
        &fields.name,
        &fields.content,
        &fields.size_min,
        &fields.size_max,
        &fields.date_from,
        &fields.date_to,
    ] {
        entry.set_activates_default(true);
    }

    // ---- running -----------------------------------------------------------------------
    let cancel: Rc<RefCell<Option<Arc<AtomicBool>>>> = Rc::new(RefCell::new(None));
    let run_search = {
        let fields = fields.clone();
        let cancel = cancel.clone();
        let store = store.clone();
        let status = status.clone();
        let start = start.clone();
        let stop = stop.clone();
        let window = window.clone();
        Rc::new(move || {
            let query = match fields.query() {
                Ok(q) => q,
                Err(err) => {
                    ops::alert(&window, "Cannot search", &err);
                    return;
                }
            };
            if let Some(previous) = cancel.borrow_mut().take() {
                previous.store(true, Ordering::Relaxed);
            }
            let flag = Arc::new(AtomicBool::new(false));
            *cancel.borrow_mut() = Some(flag.clone());
            store.remove_all();
            status.set_text("Searching…");
            start.set_sensitive(false);
            stop.set_sensitive(true);

            let (tx, rx) = async_channel::unbounded::<Msg>();
            let worker_flag = flag.clone();
            std::thread::spawn(move || {
                let mut batch = Vec::new();
                let mut last = Instant::now();
                let result = search::run(&query, &worker_flag, &mut |hit| {
                    batch.push(hit);
                    if batch.len() >= BATCH_SIZE || last.elapsed() >= BATCH_INTERVAL {
                        last = Instant::now();
                        if tx
                            .send_blocking(Msg::Hits(std::mem::take(&mut batch)))
                            .is_err()
                        {
                            return false;
                        }
                    }
                    true
                });
                if !batch.is_empty() {
                    let _ = tx.send_blocking(Msg::Hits(batch));
                }
                let _ = tx.send_blocking(Msg::Done(result.map_err(|e| e.to_string())));
            });

            let started = Instant::now();
            glib::spawn_future_local(glib::clone!(
                #[weak]
                store,
                #[weak]
                status,
                #[weak]
                start,
                #[weak]
                stop,
                #[strong]
                cancel,
                async move {
                    let mut found = 0u64;
                    while let Ok(msg) = rx.recv().await {
                        match msg {
                            Msg::Hits(hits) => {
                                found += hits.len() as u64;
                                let objects: Vec<glib::BoxedAnyObject> =
                                    hits.into_iter().map(glib::BoxedAnyObject::new).collect();
                                store.splice(store.n_items(), 0, &objects);
                                status.set_text(&format!("Searching… {found} found"));
                            }
                            Msg::Done(result) => {
                                // Only the latest search owns the buttons.
                                let current = cancel.borrow().as_ref().map(Arc::as_ptr);
                                if current != Some(Arc::as_ptr(&flag)) {
                                    break;
                                }
                                let secs = started.elapsed().as_secs_f64();
                                match result {
                                    Ok(stats) => {
                                        let mut text = format!(
                                            "{} found · {} scanned · {secs:.1}s",
                                            stats.matched, stats.scanned
                                        );
                                        if stats.cancelled {
                                            text.push_str(" · stopped");
                                        }
                                        if stats.errors > 0 {
                                            text.push_str(&format!(
                                                " · {} folders unreadable",
                                                stats.errors
                                            ));
                                        }
                                        status.set_text(&text);
                                    }
                                    Err(err) => status.set_text(&err),
                                }
                                start.set_sensitive(true);
                                stop.set_sensitive(false);
                                break;
                            }
                        }
                    }
                }
            ));
        })
    };
    start.connect_clicked(glib::clone!(
        #[strong]
        run_search,
        move |_| run_search()
    ));
    stop.connect_clicked(glib::clone!(
        #[strong]
        cancel,
        move |_| {
            if let Some(flag) = cancel.borrow().as_ref() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    ));

    // ---- acting on results ---------------------------------------------------------
    let selected_hits = {
        let selection = selection.clone();
        let store = store.clone();
        move || -> Vec<PathBuf> {
            let chosen = selection.selection();
            let indices: Vec<u32> = if chosen.is_empty() {
                (0..store.n_items()).collect()
            } else {
                (0..store.n_items())
                    .filter(|&i| chosen.contains(i))
                    .collect()
            };
            indices
                .into_iter()
                .filter_map(|i| store.item(i).and_downcast::<glib::BoxedAnyObject>())
                .map(|obj| obj.borrow::<Hit>().path.clone())
                .collect()
        }
    };
    let show_in_pane = Rc::new(show_in_pane);
    let jump = {
        let selection = selection.clone();
        let store = store.clone();
        let show_in_pane = show_in_pane.clone();
        Rc::new(move |pos: Option<u32>| {
            let pos = pos.or_else(|| {
                let chosen = selection.selection();
                (!chosen.is_empty()).then(|| chosen.nth(0))
            });
            let Some(obj) = pos
                .and_then(|i| store.item(i))
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                return;
            };
            let path = obj.borrow::<Hit>().path.clone();
            if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
                show_in_pane(dir.to_path_buf(), name.to_os_string());
            }
        })
    };
    view.connect_activate(glib::clone!(
        #[strong]
        jump,
        move |_, pos| jump(Some(pos))
    ));
    goto.connect_clicked(glib::clone!(
        #[strong]
        jump,
        move |_| jump(None)
    ));
    copy.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        selected_hits,
        move |_| {
            let paths = selected_hits();
            if !paths.is_empty() {
                ops::clipboard_put(&window, &paths, false);
            }
        }
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[strong]
        selected_hits,
        #[strong]
        cancel,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            if key == gdk::Key::Escape {
                if let Some(flag) = cancel.borrow().as_ref() {
                    flag.store(true, Ordering::Relaxed);
                }
                window.destroy();
                return glib::Propagation::Stop;
            }
            if ctrl
                && key.to_lower() == gdk::Key::c
                && !GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>())
            {
                let paths = selected_hits();
                if !paths.is_empty() {
                    ops::clipboard_put(&window, &paths, false);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(keys);
    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        cancel,
        move |_| {
            if let Some(flag) = cancel.borrow().as_ref() {
                flag.store(true, Ordering::Relaxed);
            }
            window.destroy();
        }
    ));

    window.present();
    fields.name.grab_focus();
}

fn column(view: &gtk::ColumnView, title: &str, expand: bool, text: fn(&Hit) -> String) {
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
            label.set_text(&text(&obj.borrow::<Hit>()));
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(expand);
    column.set_resizable(true);
    view.append_column(&column);
}
