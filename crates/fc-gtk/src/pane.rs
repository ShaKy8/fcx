//! A single file-listing pane: path bar, sortable column view, status line.
//!
//! Listing runs on a worker thread; results are applied on the main thread only if
//! no newer navigation started in the meantime (tracked by `generation`).
//!
//! Selection is Commander-style: the *cursor* is GTK's single selection (moved by
//! arrows/mouse) and *marks* are a separate per-item flag. Actions operate on the
//! marked items, or on the cursor item when nothing is marked.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use fc_core::format::human_size;
use fc_core::fs::list_dir;
use fc_core::jobs::Operation;
use fc_core::sort::natural_cmp;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::item::{Item, row_of};
use crate::row::Row;

/// Coalesces bursts of file-monitor events (e.g. an extracting archive) into one reload.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Ext,
    Size,
    Date,
}

/// Files dropped on this pane, plus the operation forced by modifiers (Ctrl = copy,
/// Shift = move) or `None` to let the app decide.
type DropHandler = Rc<dyn Fn(Vec<PathBuf>, Option<Operation>)>;

#[derive(Clone)]
pub struct Pane(Rc<Inner>);

struct Columns {
    name: gtk::ColumnViewColumn,
    ext: gtk::ColumnViewColumn,
    size: gtk::ColumnViewColumn,
    modified: gtk::ColumnViewColumn,
}

struct Inner {
    root: gtk::Box,
    path_entry: gtk::Entry,
    view: gtk::ColumnView,
    store: gio::ListStore,
    filter: gtk::CustomFilter,
    selection: gtk::SingleSelection,
    status: gtk::Label,
    columns: Columns,
    show_hidden: Rc<Cell<bool>>,
    cwd: RefCell<Option<PathBuf>>,
    generation: Cell<u64>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    reload_timer: RefCell<Option<glib::SourceId>>,
    history: RefCell<Vec<PathBuf>>,
    history_pos: Cell<usize>,
    on_drop: RefCell<Option<DropHandler>>,
}

impl Pane {
    pub fn new() -> Self {
        let pane = Pane(Rc::new_cyclic(|weak: &Weak<Inner>| {
            let store = gio::ListStore::new::<Item>();

            let show_hidden = Rc::new(Cell::new(false));
            let filter = {
                let show_hidden = show_hidden.clone();
                gtk::CustomFilter::new(move |obj| {
                    let row = row_of(obj);
                    show_hidden.get() || row.is_parent || !row.entry.is_hidden()
                })
            };
            let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));

            let view = gtk::ColumnView::new(None::<gtk::SelectionModel>);
            view.add_css_class("data-table");
            view.set_reorderable(false);

            let by_name = |a: &Row, b: &Row| natural_cmp(&a.sort_key, &b.sort_key);
            let name = add_column(&view, "Name", true, name_factory(weak.clone()), by_name);
            let ext = add_column(
                &view,
                "Ext",
                false,
                text_factory(weak.clone(), Row::ext_text, 0.0),
                move |a, b| a.ext_key.cmp(&b.ext_key).then_with(|| by_name(a, b)),
            );
            let size = add_column(
                &view,
                "Size",
                false,
                text_factory(weak.clone(), Row::size_text, 1.0),
                move |a, b| a.entry.size.cmp(&b.entry.size).then_with(|| by_name(a, b)),
            );
            let modified = add_column(
                &view,
                "Modified",
                false,
                text_factory(weak.clone(), Row::modified_text, 0.0),
                move |a, b| {
                    a.entry
                        .modified
                        .cmp(&b.entry.modified)
                        .then_with(|| by_name(a, b))
                },
            );
            add_column(
                &view,
                "Permissions",
                false,
                text_factory(weak.clone(), Row::mode_text, 0.0),
                move |a, b| {
                    a.entry
                        .permissions()
                        .cmp(&b.entry.permissions())
                        .then_with(|| by_name(a, b))
                },
            );

            // `..` first, then directories, then files — independent of the column's sort direction.
            let sorter = gtk::MultiSorter::new();
            sorter.append(gtk::CustomSorter::new(|a, b| {
                row_of(a).group().cmp(&row_of(b).group()).into()
            }));
            sorter.append(view.sorter().expect("column view has a sorter"));
            let sorted = gtk::SortListModel::new(Some(filtered), Some(sorter));

            let selection = gtk::SingleSelection::new(Some(sorted));
            selection.set_can_unselect(false);
            view.set_model(Some(&selection));
            view.sort_by_column(Some(&name), gtk::SortType::Ascending);

            let path_entry = gtk::Entry::new();
            path_entry.add_css_class("path-bar");
            let status = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .margin_start(6)
                .margin_end(6)
                .margin_top(2)
                .margin_bottom(2)
                .build();
            status.add_css_class("status-bar");
            let scroller = gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .hexpand(true)
                .build();
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.add_css_class("pane");
            root.append(&path_entry);
            root.append(&scroller);
            root.append(&status);

            Inner {
                root,
                path_entry,
                view,
                store,
                filter,
                selection,
                status,
                columns: Columns {
                    name,
                    ext,
                    size,
                    modified,
                },
                show_hidden,
                cwd: RefCell::new(None),
                generation: Cell::new(0),
                monitor: RefCell::new(None),
                reload_timer: RefCell::new(None),
                history: RefCell::new(Vec::new()),
                history_pos: Cell::new(0),
                on_drop: RefCell::new(None),
            }
        }));
        pane.connect_signals();
        pane
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.0.root.upcast_ref()
    }

    pub fn focus(&self) {
        self.0.view.grab_focus();
    }

    /// Runs `f` whenever keyboard focus enters this pane (a click, Tab, etc.).
    pub fn connect_focus_enter(&self, f: impl Fn() + 'static) {
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(move |_| f());
        self.0.root.add_controller(focus);
    }

    pub fn connect_drop(&self, f: impl Fn(Vec<PathBuf>, Option<Operation>) + 'static) {
        *self.0.on_drop.borrow_mut() = Some(Rc::new(f));
    }

    /// Visual "this is the active pane" state. Focus itself is separate.
    pub fn set_active(&self, active: bool) {
        if active {
            self.0.root.add_css_class("active");
        } else {
            self.0.root.remove_css_class("active");
        }
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        self.0.cwd.borrow().clone()
    }

    /// List `path` asynchronously; once loaded, put the cursor on `select` if present.
    pub fn navigate(&self, path: PathBuf, select: Option<OsString>) {
        self.navigate_with(path, select, true);
    }

    fn navigate_with(&self, path: PathBuf, select: Option<OsString>, record: bool) {
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let target = path.clone();
            let result = gio::spawn_blocking(move || {
                list_dir(&target).map(|entries| entries.into_iter().map(Row::new).collect())
            })
            .await;
            let Some(pane) = Pane::upgrade(&weak) else {
                return;
            };
            if pane.0.generation.get() != generation {
                return;
            }
            match result {
                Ok(Ok(rows)) => pane.show_listing(path, rows, select, record),
                Ok(Err(err)) => pane.show_error(&err.to_string()),
                Err(_) => pane.show_error("directory listing thread panicked"),
            }
        });
    }

    pub fn go_up(&self) {
        let Some(cwd) = self.cwd() else {
            return;
        };
        if let Some(parent) = cwd.parent() {
            self.navigate(
                parent.to_path_buf(),
                cwd.file_name().map(OsStr::to_os_string),
            );
        }
    }

    pub fn go_root(&self) {
        self.navigate(PathBuf::from("/"), None);
    }

    pub fn back(&self) {
        let pos = self.0.history_pos.get();
        if pos == 0 {
            return;
        }
        self.jump_history(pos - 1);
    }

    pub fn forward(&self) {
        let pos = self.0.history_pos.get();
        if pos + 1 >= self.0.history.borrow().len() {
            return;
        }
        self.jump_history(pos + 1);
    }

    fn jump_history(&self, pos: usize) {
        let (target, from) = {
            let history = self.0.history.borrow();
            (
                history[pos].clone(),
                history[self.0.history_pos.get()].clone(),
            )
        };
        self.0.history_pos.set(pos);
        // Coming back from a subfolder, land the cursor on it.
        let select = from
            .strip_prefix(&target)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_os_string());
        self.navigate_with(target, select, false);
    }

    pub fn toggle_hidden(&self) {
        let keep = self.cursor_name();
        self.0.show_hidden.set(!self.0.show_hidden.get());
        self.0.filter.changed(gtk::FilterChange::Different);
        if let Some(name) = keep {
            self.select_name(&name);
        }
        self.update_status();
    }

    /// Re-list the current directory, keeping cursor and marks. If the directory
    /// itself was deleted, fall back to the nearest surviving ancestor.
    pub fn reload(&self) {
        let Some(cwd) = self.cwd() else {
            return;
        };
        match cwd.ancestors().find(|p| p.is_dir()) {
            Some(dir) if dir == cwd => self.navigate_with(cwd, self.cursor_name(), false),
            Some(dir) => self.navigate(dir.to_path_buf(), None),
            None => {}
        }
    }

    pub fn focus_path_entry(&self) {
        self.0.path_entry.grab_focus();
        self.0.path_entry.select_region(0, -1);
    }

    /// Sort by `column`; sorting by the current column again flips the direction.
    pub fn sort_by(&self, column: SortColumn) {
        let column = match column {
            SortColumn::Name => &self.0.columns.name,
            SortColumn::Ext => &self.0.columns.ext,
            SortColumn::Size => &self.0.columns.size,
            SortColumn::Date => &self.0.columns.modified,
        };
        let sorter = self.0.view.sorter().and_downcast::<gtk::ColumnViewSorter>();
        let current = sorter.as_ref().and_then(|s| s.primary_sort_column());
        let ascending = sorter
            .as_ref()
            .is_some_and(|s| s.primary_sort_order() == gtk::SortType::Ascending);
        let order = if current.as_ref() == Some(column) && ascending {
            gtk::SortType::Descending
        } else {
            gtk::SortType::Ascending
        };
        self.0.view.sort_by_column(Some(column), order);
    }

    // ---- marks -------------------------------------------------------------

    pub fn toggle_mark(&self) {
        if let Some(pos) = self.cursor() {
            self.toggle_mark_at(pos);
        }
    }

    /// Insert-style: toggle the cursor item, then step the cursor by `step` rows.
    pub fn toggle_mark_and_step(&self, step: i32) {
        let Some(pos) = self.cursor() else {
            return;
        };
        self.toggle_mark_at(pos);
        if let Some(next) = pos.checked_add_signed(step) {
            self.select_pos(next);
        }
    }

    pub fn mark_all(&self, marked: bool) {
        for item in self.visible_items() {
            if !item.row().is_parent {
                item.set_marked(marked);
            }
        }
        self.update_status();
    }

    /// Flip every mark; with `files_only`, folders keep theirs (FC's Ctrl+Num*).
    pub fn invert_marks(&self, files_only: bool) {
        for item in self.visible_items() {
            let row = item.row();
            if !row.is_parent && !(files_only && row.entry.is_dir_like()) {
                item.set_marked(!item.marked());
            }
        }
        self.update_status();
    }

    /// Mark or unmark every visible entry whose name matches `mask`.
    pub fn mark_matching(&self, mask: &fc_core::glob::Mask, marked: bool) {
        for item in self.visible_items() {
            let row = item.row();
            if !row.is_parent && mask.matches(&row.display_name) {
                item.set_marked(marked);
            }
        }
        self.update_status();
    }

    /// Mark or unmark every file sharing the cursor item's extension.
    pub fn mark_same_ext(&self, marked: bool) {
        let Some(ext) = self
            .cursor()
            .and_then(|pos| self.item_at(pos))
            .filter(|item| !item.row().is_parent && !item.row().entry.is_dir_like())
            .map(|item| item.row().ext_key.clone())
        else {
            return;
        };
        for item in self.visible_items() {
            let row = item.row();
            if !row.is_parent && !row.entry.is_dir_like() && row.ext_key == ext {
                item.set_marked(marked);
            }
        }
        self.update_status();
    }

    pub fn marked_names(&self) -> Vec<OsString> {
        self.visible_items()
            .filter(|item| item.marked())
            .map(|item| item.row().entry.name.clone())
            .collect()
    }

    /// Names that file operations should act on: the marked items, or the cursor
    /// item if nothing is marked. Never includes `..`.
    pub fn targets(&self) -> Vec<OsString> {
        let marked = self.marked_names();
        if !marked.is_empty() {
            return marked;
        }
        self.cursor()
            .and_then(|pos| self.item_at(pos))
            .filter(|item| !item.row().is_parent)
            .map(|item| vec![item.row().entry.name.clone()])
            .unwrap_or_default()
    }

    /// Name under the cursor, or `None` on `..` / an empty listing.
    pub fn cursor_name(&self) -> Option<OsString> {
        self.cursor()
            .and_then(|pos| self.item_at(pos))
            .filter(|item| !item.row().is_parent)
            .map(|item| item.row().entry.name.clone())
    }

    fn toggle_mark_at(&self, pos: u32) {
        if let Some(item) = self.item_at(pos)
            && !item.row().is_parent
        {
            item.set_marked(!item.marked());
            self.update_status();
        }
    }

    fn visible_items(&self) -> impl Iterator<Item = Item> + '_ {
        let model = &self.0.selection;
        (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<Item>())
    }

    // ---- internals ---------------------------------------------------------

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn upgrade(weak: &Weak<Inner>) -> Option<Pane> {
        weak.upgrade().map(Pane)
    }

    fn connect_signals(&self) {
        let weak = self.downgrade();
        self.0.view.connect_activate(move |_, pos| {
            if let Some(pane) = Pane::upgrade(&weak) {
                pane.activate(pos);
            }
        });

        let weak = self.downgrade();
        self.0.path_entry.connect_activate(move |entry| {
            let Some(pane) = Pane::upgrade(&weak) else {
                return;
            };
            let target = expand_path(&entry.text(), pane.cwd().as_deref());
            pane.navigate(target, None);
            pane.focus();
        });

        let weak = self.downgrade();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(pane) = Pane::upgrade(&weak) else {
                return glib::Propagation::Proceed;
            };
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            pane.sync_path_entry();
            pane.focus();
            glib::Propagation::Stop
        });
        self.0.path_entry.add_controller(keys);

        let weak = self.downgrade();
        let drop = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        drop.connect_drop(move |target, value, _, _| {
            let Some(pane) = Pane::upgrade(&weak) else {
                return false;
            };
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            let handler = pane.0.on_drop.borrow().clone();
            let (Some(handler), false) = (handler, paths.is_empty()) else {
                return false;
            };
            let state = target.current_event_state();
            let forced = if state.contains(gdk::ModifierType::CONTROL_MASK) {
                Some(Operation::Copy)
            } else if state.contains(gdk::ModifierType::SHIFT_MASK) {
                Some(Operation::Move)
            } else {
                None
            };
            handler(paths, forced);
            true
        });
        self.0.view.add_controller(drop);
    }

    fn activate(&self, pos: u32) {
        let Some(item) = self.item_at(pos) else {
            return;
        };
        let row = item.row();
        let Some(cwd) = self.cwd() else {
            return;
        };
        if row.is_parent {
            self.go_up();
        } else if row.entry.is_dir_like() {
            self.navigate(cwd.join(&row.entry.name), None);
        } else {
            self.open_file(&cwd.join(&row.entry.name));
        }
    }

    fn open_file(&self, path: &Path) {
        let uri = gio::File::for_path(path).uri();
        let ctx = self.0.view.display().app_launch_context();
        if let Err(err) = gio::AppInfo::launch_default_for_uri(&uri, Some(&ctx)) {
            self.show_error(&format!("cannot open {}: {err}", path.display()));
        }
    }

    fn show_listing(&self, path: PathBuf, rows: Vec<Row>, select: Option<OsString>, record: bool) {
        let changed_dir = self.cwd().as_deref() != Some(path.as_path());
        let previous_pos = self.0.selection.selected();

        // A reload of the same directory keeps marks; entering a new one starts clean.
        let marked: HashSet<OsString> = if changed_dir {
            HashSet::new()
        } else {
            self.0
                .store
                .iter::<Item>()
                .flatten()
                .filter(|item| item.marked())
                .map(|item| item.row().entry.name.clone())
                .collect()
        };

        let mut objects = Vec::with_capacity(rows.len() + 1);
        if path.parent().is_some() {
            objects.push(Item::new(Row::parent()));
        }
        objects.extend(rows.into_iter().map(|row| {
            let item = Item::new(row);
            if marked.contains(&item.row().entry.name) {
                item.set_marked(true);
            }
            item
        }));
        self.0.store.splice(0, self.0.store.n_items(), &objects);

        *self.0.cwd.borrow_mut() = Some(path.clone());
        self.sync_path_entry();
        if changed_dir {
            self.watch(&path);
            if record {
                let mut history = self.0.history.borrow_mut();
                if !history.is_empty() {
                    history.truncate(self.0.history_pos.get() + 1);
                }
                if history.last() != Some(&path) {
                    history.push(path.clone());
                }
                self.0.history_pos.set(history.len() - 1);
            }
        }
        self.0.status.remove_css_class("error");
        self.update_status();

        let found = select.is_some_and(|name| self.select_name(&name));
        if !found {
            let pos = if changed_dir { 0 } else { previous_pos };
            self.select_pos(pos.min(self.0.selection.n_items().saturating_sub(1)));
        }
    }

    fn show_error(&self, message: &str) {
        self.0.status.add_css_class("error");
        self.0.status.set_text(message);
        self.sync_path_entry();
    }

    fn watch(&self, path: &Path) {
        let monitor = gio::File::for_path(path)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok();
        if let Some(monitor) = &monitor {
            let weak = self.downgrade();
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(pane) = Pane::upgrade(&weak) {
                    pane.schedule_reload();
                }
            });
        }
        // Dropping the previous monitor cancels it.
        *self.0.monitor.borrow_mut() = monitor;
    }

    fn schedule_reload(&self) {
        if self.0.reload_timer.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(RELOAD_DEBOUNCE, move || {
            if let Some(pane) = Pane::upgrade(&weak) {
                pane.0.reload_timer.take();
                pane.reload();
            }
        });
        *self.0.reload_timer.borrow_mut() = Some(id);
    }

    fn cursor(&self) -> Option<u32> {
        let pos = self.0.selection.selected();
        (pos != gtk::INVALID_LIST_POSITION).then_some(pos)
    }

    fn item_at(&self, pos: u32) -> Option<Item> {
        self.0.selection.item(pos).and_downcast::<Item>()
    }

    /// Moves the cursor to the entry named `name`; returns false if it isn't visible.
    fn select_name(&self, name: &OsStr) -> bool {
        let pos = (0..self.0.selection.n_items()).find(|&i| {
            self.item_at(i)
                .is_some_and(|item| item.row().name() == name)
        });
        if let Some(pos) = pos {
            self.select_pos(pos);
        }
        pos.is_some()
    }

    fn select_pos(&self, pos: u32) {
        if pos >= self.0.selection.n_items() {
            return;
        }
        self.0.view.scroll_to(
            pos,
            None::<&gtk::ColumnViewColumn>,
            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
            None::<gtk::ScrollInfo>,
        );
    }

    fn sync_path_entry(&self) {
        let text = self
            .cwd()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.0.path_entry.set_text(&text);
    }

    fn update_status(&self) {
        let (mut dirs, mut files, mut bytes, mut hidden) = (0u32, 0u32, 0u64, 0u32);
        let (mut marked, mut marked_bytes) = (0u32, 0u64);
        for item in self.0.store.iter::<Item>().flatten() {
            let row = item.row();
            if row.is_parent {
                continue;
            }
            if row.entry.is_hidden() && !self.0.show_hidden.get() {
                hidden += 1;
                continue;
            }
            if item.marked() {
                marked += 1;
                marked_bytes += row.entry.size;
            }
            if row.entry.is_dir_like() {
                dirs += 1;
            } else {
                files += 1;
                bytes += row.entry.size;
            }
        }
        let mut text = String::new();
        if marked > 0 {
            text.push_str(&format!(
                "{marked} marked ({}) · ",
                human_size(marked_bytes)
            ));
        }
        text.push_str(&format!(
            "{dirs} folders, {files} files ({})",
            human_size(bytes)
        ));
        if hidden > 0 {
            text.push_str(&format!(" · {hidden} hidden"));
        }
        self.0.status.set_text(&text);
    }
}

/// Expands `~` and resolves relative input against `cwd`.
pub fn expand_path(text: &str, cwd: Option<&Path>) -> PathBuf {
    let text = text.trim();
    let path = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            glib::home_dir().join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    };
    match cwd {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    }
}

fn list_item(obj: &glib::Object) -> &gtk::ListItem {
    obj.downcast_ref().expect("factory items are ListItems")
}

/// Keeps `widget`'s CSS classes in sync with the bound item's `marked` flag:
/// `base` classes always, plus `marked` while the item is marked. Done once at
/// setup via an expression on the ListItem, so rebinding to another item is automatic.
fn bind_marked_class(
    item: &gtk::ListItem,
    widget: &impl IsA<gtk::Widget>,
    base: &'static [&'static str],
) {
    let item_expr = gtk::PropertyExpression::new(
        gtk::ListItem::static_type(),
        Some(&gtk::ConstantExpression::new(item)),
        "item",
    );
    let marked_expr = gtk::PropertyExpression::new(Item::static_type(), Some(&item_expr), "marked");
    let classes = gtk::ClosureExpression::new::<glib::StrV>(
        [&marked_expr],
        glib::closure!(move |_: Option<glib::Object>, marked: bool| {
            let mut classes: Vec<&str> = base.to_vec();
            if marked {
                classes.push("marked");
            }
            glib::StrV::from(classes.as_slice())
        }),
    );
    let widget = widget.upcast_ref::<gtk::Widget>();
    classes.bind(widget, "css-classes", Some(widget));
}

/// Lets a cell start a drag of its row: the marked set if the row is marked,
/// otherwise just this row. Files travel as `text/uri-list`, so Nautilus and
/// other GTK apps accept them too.
fn attach_drag(weak: Weak<Inner>, item: &gtk::ListItem, widget: &impl IsA<gtk::Widget>) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let item = item.clone();
    source.connect_prepare(move |_, _, _| {
        let pane = Pane::upgrade(&weak)?;
        let obj = item.item()?;
        let row = row_of(&obj);
        if row.is_parent {
            return None;
        }
        let cwd = pane.cwd()?;
        let marked = obj.downcast_ref::<Item>()?.marked();
        let names = if marked {
            pane.marked_names()
        } else {
            vec![row.entry.name.clone()]
        };
        let files: Vec<gio::File> = names
            .iter()
            .map(|n| gio::File::for_path(cwd.join(n)))
            .collect();
        Some(gdk::ContentProvider::for_value(
            &gdk::FileList::from_array(&files).to_value(),
        ))
    });
    let icon_source = widget.clone().upcast::<gtk::Widget>();
    source.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(Some(&icon_source));
        source.set_icon(Some(&paintable), 0, 0);
    });
    widget.add_controller(source);
}

fn add_column(
    view: &gtk::ColumnView,
    title: &str,
    expand: bool,
    factory: gtk::SignalListItemFactory,
    cmp: impl Fn(&Row, &Row) -> Ordering + 'static,
) -> gtk::ColumnViewColumn {
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(expand);
    column.set_resizable(true);
    column.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| {
        cmp(row_of(a), row_of(b)).into()
    })));
    view.append_column(&column);
    column
}

fn text_factory(
    weak: Weak<Inner>,
    text: fn(&Row) -> String,
    xalign: f32,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = list_item(item);
        let label = gtk::Label::builder().xalign(xalign).build();
        bind_marked_class(item, &label, &["numeric"]);
        attach_drag(weak.clone(), item, &label);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = list_item(item);
        let label = item
            .child()
            .and_downcast::<gtk::Label>()
            .expect("label child");
        let obj = item.item().expect("bound item");
        label.set_text(&text(row_of(&obj)));
    });
    factory
}

fn name_factory(weak: Weak<Inner>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = list_item(item);
        let icon = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let cell = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        cell.append(&icon);
        cell.append(&label);
        bind_marked_class(item, &cell, &[]);
        attach_drag(weak.clone(), item, &cell);
        item.set_child(Some(&cell));
    });
    factory.connect_bind(|_, item| {
        let item = list_item(item);
        let cell = item.child().and_downcast::<gtk::Box>().expect("box child");
        let icon = cell
            .first_child()
            .and_downcast::<gtk::Image>()
            .expect("icon");
        let label = cell
            .last_child()
            .and_downcast::<gtk::Label>()
            .expect("label");
        let obj = item.item().expect("bound item");
        let row = row_of(&obj);
        icon.set_from_gicon(&row.icon());
        label.set_text(&row.display_name);
        if matches!(row.entry.kind, fc_core::fs::EntryKind::BrokenSymlink) {
            label.add_css_class("error");
        } else {
            label.remove_css_class("error");
        }
    });
    factory
}
