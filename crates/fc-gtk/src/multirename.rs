//! Multi rename dialog (Ctrl+M): masks, search/replace, case, counter, and a
//! live preview computed by `fc_core::rename::plan` on every edit.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

use fc_core::rename::{self, CaseMode, Plan, Problem, RenameSpec, Source};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops;

const CASES: [(&str, CaseMode); 5] = [
    ("Unchanged", CaseMode::Unchanged),
    ("lowercase", CaseMode::Lower),
    ("UPPERCASE", CaseMode::Upper),
    ("First upper", CaseMode::FirstUpper),
    ("Title Case", CaseMode::Title),
];

struct PreviewRow {
    old: String,
    new: String,
    problem: Option<Problem>,
}

struct Controls {
    name_mask: gtk::Entry,
    ext_mask: gtk::Entry,
    search: gtk::Entry,
    replace: gtk::Entry,
    regex: gtk::CheckButton,
    case_sensitive: gtk::CheckButton,
    case: gtk::DropDown,
    start: gtk::SpinButton,
    step: gtk::SpinButton,
    digits: gtk::SpinButton,
}

impl Controls {
    fn spec(&self) -> RenameSpec {
        RenameSpec {
            name_mask: self.name_mask.text().to_string(),
            ext_mask: self.ext_mask.text().to_string(),
            search: self.search.text().to_string(),
            replace: self.replace.text().to_string(),
            use_regex: self.regex.is_active(),
            case_sensitive: self.case_sensitive.is_active(),
            case: CASES[self.case.selected() as usize].1,
            counter_start: self.start.value() as i64,
            counter_step: self.step.value() as i64,
            counter_digits: self.digits.value() as usize,
        }
    }
}

fn labelled(grid: &gtk::Grid, col: i32, row: i32, text: &str, widget: &impl IsA<gtk::Widget>) {
    let label = gtk::Label::builder().label(text).xalign(1.0).build();
    label.add_css_class("dim-label");
    grid.attach(&label, col, row, 1, 1);
    grid.attach(widget, col + 1, row, 1, 1);
}

fn spin(min: f64, max: f64, initial: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(min, max, 1.0);
    spin.set_value(initial);
    spin
}

/// Opens the tool for `sources` (in list order) living in `dir`. `on_done`
/// receives the applied `(old, new)` pairs for undo.
pub fn show(
    parent: &gtk::Window,
    dir: PathBuf,
    sources: Vec<Source>,
    on_done: impl Fn(Vec<(OsString, String)>) + 'static,
) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(format!("Multi rename — {} items", sources.len()))
        .default_width(900)
        .default_height(620)
        .build();

    let controls = Rc::new(Controls {
        name_mask: gtk::Entry::builder().text("[N]").hexpand(true).build(),
        ext_mask: gtk::Entry::builder().text("[E]").hexpand(true).build(),
        search: gtk::Entry::builder().hexpand(true).build(),
        replace: gtk::Entry::builder().hexpand(true).build(),
        regex: gtk::CheckButton::with_label("Regular expression"),
        case_sensitive: gtk::CheckButton::with_label("Case sensitive"),
        case: gtk::DropDown::from_strings(&CASES.map(|(l, _)| l)),
        start: spin(-1_000_000.0, 1_000_000_000.0, 1.0),
        step: spin(-1_000_000.0, 1_000_000.0, 1.0),
        digits: spin(1.0, 12.0, 1.0),
    });

    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(8)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    labelled(&grid, 0, 0, "Name mask", &controls.name_mask);
    labelled(&grid, 2, 0, "Extension mask", &controls.ext_mask);
    labelled(&grid, 0, 1, "Search", &controls.search);
    labelled(&grid, 2, 1, "Replace", &controls.replace);
    let options = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    options.append(&controls.regex);
    options.append(&controls.case_sensitive);
    grid.attach(&options, 1, 2, 3, 1);
    labelled(&grid, 0, 3, "Case", &controls.case);
    let counter = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    counter.append(&gtk::Label::new(Some("Counter [C] start")));
    counter.append(&controls.start);
    counter.append(&gtk::Label::new(Some("step")));
    counter.append(&controls.step);
    counter.append(&gtk::Label::new(Some("digits")));
    counter.append(&controls.digits);
    grid.attach(&counter, 2, 3, 2, 1);
    let hint = gtk::Label::builder()
        .label("[N] name  [N2-5] [N3-] [N-3] ranges  [E] extension  [C] counter  [P] parent folder  [Y] [M] [D] [h] [m] [s] modified time")
        .xalign(0.0)
        .wrap(true)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    grid.attach(&hint, 0, 4, 4, 1);

    // ---- preview ---------------------------------------------------------------
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(store.clone()))));
    view.add_css_class("data-table");
    text_column(&view, "Old name", |r| r.old.clone(), false);
    text_column(&view, "New name", |r| r.new.clone(), true);
    text_column(
        &view,
        "Status",
        |r| match r.problem {
            Some(Problem::Collision) => "Collision".into(),
            Some(Problem::Empty) => "Empty name".into(),
            Some(Problem::Invalid) => "Invalid name".into(),
            None if r.old == r.new => "unchanged".into(),
            None => String::new(),
        },
        true,
    );
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .margin_start(12)
        .margin_end(12)
        .build();

    let status = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Rename");
    apply.add_css_class("suggested-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .margin_end(12)
        .margin_bottom(12)
        .build();
    buttons.append(&cancel);
    buttons.append(&apply);
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status.set_hexpand(true);
    footer.append(&status);
    footer.append(&buttons);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&grid);
    content.append(&scroller);
    content.append(&footer);
    window.set_child(Some(&content));
    // Enter in any field applies, like FC.
    window.set_default_widget(Some(&apply));
    for entry in [
        &controls.name_mask,
        &controls.ext_mask,
        &controls.search,
        &controls.replace,
    ] {
        entry.set_activates_default(true);
    }

    // ---- live preview ------------------------------------------------------------
    let sources = Rc::new(sources);
    let current: Rc<RefCell<Option<Plan>>> = Rc::new(RefCell::new(None));
    let refresh = {
        let controls = controls.clone();
        let sources = sources.clone();
        let current = current.clone();
        let store = store.clone();
        let status = status.clone();
        let apply = apply.clone();
        Rc::new(move || {
            let spec = controls.spec();
            match rename::plan(&spec, &sources) {
                Ok(plan) => {
                    let rows: Vec<glib::BoxedAnyObject> = plan
                        .items
                        .iter()
                        .map(|p| {
                            glib::BoxedAnyObject::new(PreviewRow {
                                old: p.old.to_string_lossy().into_owned(),
                                new: p.new.clone(),
                                problem: p.problem,
                            })
                        })
                        .collect();
                    store.splice(0, store.n_items(), &rows);
                    let changing = plan.changing();
                    let problems = plan.problems();
                    let unchanged = plan.items.len() - changing - problems;
                    let mut text = format!("{changing} to rename · {unchanged} unchanged");
                    if problems > 0 {
                        text.push_str(&format!(" · {problems} problem(s)"));
                        status.add_css_class("error");
                    } else {
                        status.remove_css_class("error");
                    }
                    status.set_text(&text);
                    apply.set_sensitive(changing > 0 && problems == 0);
                    *current.borrow_mut() = Some(plan);
                }
                Err(err) => {
                    status.set_text(&err.to_string());
                    status.add_css_class("error");
                    apply.set_sensitive(false);
                    *current.borrow_mut() = None;
                }
            }
        })
    };
    for entry in [
        &controls.name_mask,
        &controls.ext_mask,
        &controls.search,
        &controls.replace,
    ] {
        let refresh = refresh.clone();
        entry.connect_changed(move |_| refresh());
    }
    for check in [&controls.regex, &controls.case_sensitive] {
        let refresh = refresh.clone();
        check.connect_toggled(move |_| refresh());
    }
    {
        let refresh = refresh.clone();
        controls.case.connect_selected_notify(move |_| refresh());
    }
    for spin in [&controls.start, &controls.step, &controls.digits] {
        let refresh = refresh.clone();
        spin.connect_value_changed(move |_| refresh());
    }
    refresh();

    // ---- actions ---------------------------------------------------------------------
    cancel.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.destroy()
    ));
    let on_done = Rc::new(on_done);
    apply.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[strong]
        current,
        move |_| {
            let Some(plan) = current.borrow().clone() else {
                return;
            };
            match rename::execute(&dir, &plan) {
                Ok(applied) => {
                    window.destroy();
                    on_done(applied);
                }
                Err(err) => ops::alert(&window, "Rename failed", &err.to_string()),
            }
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
    controls.name_mask.grab_focus();
}

fn text_column(view: &gtk::ColumnView, title: &str, text: fn(&PreviewRow) -> String, expand: bool) {
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
        let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let row = obj.borrow::<PreviewRow>();
        label.set_text(&text(&row));
        if row.problem.is_some() {
            label.add_css_class("error");
        } else {
            label.remove_css_class("error");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(expand);
    column.set_resizable(true);
    view.append_column(&column);
}
