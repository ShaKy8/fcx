//! Properties dialog (Alt+Enter / Shift+Enter): what the item is, who owns it,
//! an editable permission grid with an octal mirror, and an editable
//! modification time. Folder sizes are computed on a worker thread.

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fc_core::format::human_size;
use fc_core::jobs::count;
use fc_core::users::{group_name, user_name};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ops;

const DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn format_time(time: Option<SystemTime>) -> String {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
        .and_then(|dt| dt.format(DATE_FORMAT).ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Parses what [`format_time`] produced (local time) back into a timestamp.
fn parse_time(text: &str) -> Option<SystemTime> {
    let iso = text.trim().replacen(' ', "T", 1);
    let tz = glib::TimeZone::local();
    let dt = glib::DateTime::from_iso8601(&iso, Some(&tz)).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(dt.to_unix()).ok()?))
}

fn add_row(grid: &gtk::Grid, row: i32, label: &str, value: &impl IsA<gtk::Widget>) {
    let key = gtk::Label::builder().label(label).xalign(1.0).build();
    key.add_css_class("dim-label");
    grid.attach(&key, 0, row, 1, 1);
    grid.attach(value, 1, row, 1, 1);
}

fn value_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .width_chars(30)
        .max_width_chars(48)
        .hexpand(true)
        .build()
}

/// Opens the dialog for `path`. `focus_attributes` puts the cursor in the
/// permission editor (Shift+Enter). `on_changed` runs after a successful apply.
pub fn show(
    parent: &gtk::Window,
    path: PathBuf,
    focus_attributes: bool,
    on_changed: impl Fn() + 'static,
) {
    let Ok(lmeta) = fs::symlink_metadata(&path) else {
        ops::alert(parent, "Cannot read properties", &path.to_string_lossy());
        return;
    };
    let is_link = lmeta.file_type().is_symlink();
    // Permissions and size belong to the target for links; kind to the link itself.
    let meta = if is_link {
        fs::metadata(&path).unwrap_or_else(|_| lmeta.clone())
    } else {
        lmeta.clone()
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(format!("{name} — Properties"))
        .default_width(520)
        .resizable(false)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    // ---- header: icon + name --------------------------------------------------
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let (content_type, _) = if meta.is_dir() {
        ("inode/directory".into(), false)
    } else {
        gio::content_type_guess(Some(&name), None)
    };
    let icon = gtk::Image::from_gicon(&gio::content_type_get_icon(&content_type));
    icon.set_pixel_size(48);
    header.append(&icon);
    let title = gtk::Label::builder()
        .label(&name)
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .width_chars(20)
        .max_width_chars(36)
        .hexpand(true)
        .build();
    title.add_css_class("title-3");
    header.append(&title);
    content.append(&header);

    // ---- facts -------------------------------------------------------------------
    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(6)
        .build();
    let mut row = 0;
    let kind = if is_link {
        format!(
            "Link to {}",
            fs::read_link(&path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "?".into())
        )
    } else {
        gio::content_type_get_description(&content_type).to_string()
    };
    add_row(&grid, row, "Type", &value_label(&kind));
    row += 1;
    let location = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    add_row(&grid, row, "Location", &value_label(&location));
    row += 1;

    let size = value_label("");
    if meta.is_dir() {
        size.set_text("Calculating…");
        let target = path.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak]
            size,
            async move {
                let (files, bytes) = gio::spawn_blocking(move || count(&target))
                    .await
                    .unwrap_or_default();
                size.set_text(&format!(
                    "{} ({bytes} bytes, {} items)",
                    human_size(bytes),
                    files.saturating_sub(1)
                ));
            }
        ));
    } else {
        size.set_text(&format!(
            "{} ({} bytes)",
            human_size(meta.len()),
            meta.len()
        ));
    }
    add_row(&grid, row, "Size", &size);
    row += 1;

    let modified = gtk::Entry::builder()
        .text(format_time(meta.modified().ok()))
        .hexpand(true)
        .build();
    add_row(&grid, row, "Modified", &modified);
    row += 1;
    add_row(
        &grid,
        row,
        "Accessed",
        &value_label(&format_time(meta.accessed().ok())),
    );
    row += 1;
    add_row(
        &grid,
        row,
        "Owner",
        &value_label(&format!("{} ({})", user_name(meta.uid()), meta.uid())),
    );
    row += 1;
    add_row(
        &grid,
        row,
        "Group",
        &value_label(&format!("{} ({})", group_name(meta.gid()), meta.gid())),
    );
    content.append(&grid);

    // ---- permissions ---------------------------------------------------------------
    let perms_label = gtk::Label::builder()
        .label("Permissions")
        .xalign(0.0)
        .build();
    perms_label.add_css_class("heading");
    content.append(&perms_label);
    let perms = gtk::Grid::builder()
        .column_spacing(18)
        .row_spacing(4)
        .build();
    for (col, title) in ["Read", "Write", "Execute"].iter().enumerate() {
        let l = gtk::Label::new(Some(title));
        l.add_css_class("dim-label");
        perms.attach(&l, col as i32 + 1, 0, 1, 1);
    }
    const BITS: [[u32; 3]; 3] = [
        [0o400, 0o200, 0o100],
        [0o040, 0o020, 0o010],
        [0o004, 0o002, 0o001],
    ];
    const SPECIAL: [(u32, &str); 3] = [
        (0o4000, "Set user ID"),
        (0o2000, "Set group ID"),
        (0o1000, "Sticky"),
    ];
    let original_mode = meta.permissions().mode() & 0o7777;
    let checks: Rc<Vec<(u32, gtk::CheckButton)>> = Rc::new(
        ["Owner", "Group", "Others"]
            .iter()
            .enumerate()
            .flat_map(|(r, who)| {
                let l = gtk::Label::builder().label(*who).xalign(0.0).build();
                perms.attach(&l, 0, r as i32 + 1, 1, 1);
                BITS[r]
                    .iter()
                    .enumerate()
                    .map(move |(c, &bit)| {
                        let check = gtk::CheckButton::new();
                        check.set_active(original_mode & bit != 0);
                        check.set_halign(gtk::Align::Center);
                        (bit, check, (c as i32 + 1, r as i32 + 1))
                    })
                    .collect::<Vec<_>>()
            })
            .map(|(bit, check, (c, r))| {
                perms.attach(&check, c, r, 1, 1);
                (bit, check)
            })
            .chain(SPECIAL.iter().enumerate().map(|(i, &(bit, label))| {
                let check = gtk::CheckButton::with_label(label);
                check.set_active(original_mode & bit != 0);
                perms.attach(&check, i as i32 + 1, 4, 1, 1);
                (bit, check)
            }))
            .collect(),
    );
    let octal = gtk::Entry::builder()
        .text(format!("{original_mode:04o}"))
        .max_length(4)
        .width_chars(6)
        .build();
    let octal_label = gtk::Label::new(Some("Octal"));
    octal_label.add_css_class("dim-label");
    perms.attach(&octal_label, 0, 5, 1, 1);
    perms.attach(&octal, 1, 5, 1, 1);
    content.append(&perms);

    // Keep the checkboxes and the octal field in sync without feedback loops.
    let syncing = Rc::new(Cell::new(false));
    let mode_of = |checks: &[(u32, gtk::CheckButton)]| {
        checks
            .iter()
            .filter(|(_, c)| c.is_active())
            .fold(0u32, |m, (bit, _)| m | bit)
    };
    for (_, check) in checks.iter() {
        check.connect_toggled(glib::clone!(
            #[strong]
            checks,
            #[weak]
            octal,
            #[strong]
            syncing,
            move |_| {
                if syncing.replace(true) {
                    return;
                }
                octal.set_text(&format!("{:04o}", mode_of(&checks)));
                syncing.set(false);
            }
        ));
    }
    octal.connect_changed(glib::clone!(
        #[strong]
        checks,
        #[strong]
        syncing,
        move |entry| {
            if syncing.replace(true) {
                return;
            }
            if let Ok(mode) = u32::from_str_radix(entry.text().trim(), 8) {
                for (bit, check) in checks.iter() {
                    check.set_active(mode & bit != 0);
                }
            }
            syncing.set(false);
        }
    ));

    if is_link {
        let note = gtk::Label::builder()
            .label("Permissions and time apply to the link target.")
            .xalign(0.0)
            .build();
        note.add_css_class("dim-label");
        content.append(&note);
    }

    // ---- buttons -----------------------------------------------------------------------
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .build();
    let close = gtk::Button::with_label("Close");
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    buttons.append(&close);
    buttons.append(&apply);
    content.append(&buttons);
    window.set_child(Some(&content));

    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.destroy()
    ));
    let on_changed = Rc::new(on_changed);
    let original_time = modified.text().to_string();
    let do_apply = glib::clone!(
        #[weak]
        window,
        #[weak]
        modified,
        #[strong]
        checks,
        move || {
            let mut errors = Vec::new();
            let mode = mode_of(&checks);
            if mode != original_mode
                && let Err(err) = fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            {
                errors.push(format!("permissions: {err}"));
            }
            let time_text = modified.text().to_string();
            if time_text != original_time {
                match parse_time(&time_text) {
                    Some(time) => {
                        if let Err(err) = fs::File::open(&path).and_then(|f| f.set_modified(time)) {
                            errors.push(format!("modified time: {err}"));
                        }
                    }
                    None => errors.push(format!("cannot parse “{time_text}” as {DATE_FORMAT}")),
                }
            }
            if errors.is_empty() {
                window.destroy();
                on_changed();
            } else {
                ops::alert(&window, "Some changes failed", &errors.join("\n"));
            }
        }
    );
    apply.connect_clicked(glib::clone!(
        #[strong]
        do_apply,
        move |_| do_apply()
    ));
    modified.connect_activate(glib::clone!(
        #[strong]
        do_apply,
        move |_| do_apply()
    ));
    octal.connect_activate(move |_| do_apply());

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
    if focus_attributes {
        octal.grab_focus();
    }
}
