//! Compare two files (Ctrl+Alt+V): a coloured unified diff for text, a plain
//! same/different verdict for binaries.

use std::path::PathBuf;

use fc_core::diff::{self, LineKind};
use fc_core::format::human_size;
use fc_core::text;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Text files are diffed up to this size.
const LIMIT: u64 = 16 << 20;

enum Outcome {
    Identical,
    Text(Vec<diff::Line>, diff::Summary),
    Binary { left: u64, right: u64 },
    Error(String),
}

fn compare(left: &PathBuf, right: &PathBuf) -> Outcome {
    let read = |p: &PathBuf| -> Result<Vec<u8>, String> {
        let meta = std::fs::metadata(p).map_err(|e| e.to_string())?;
        if meta.len() > LIMIT {
            return Err(format!(
                "{} is larger than {}",
                p.to_string_lossy(),
                human_size(LIMIT)
            ));
        }
        std::fs::read(p).map_err(|e| e.to_string())
    };
    let (a, b) = match (read(left), read(right)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Outcome::Error(e),
    };
    if a == b {
        return Outcome::Identical;
    }
    let (ta, ea) = text::decode(&a, false);
    let (tb, eb) = text::decode(&b, false);
    match (ta, tb) {
        (Some(ta), Some(tb)) if ea != text::Encoding::Binary && eb != text::Encoding::Binary => {
            let (lines, summary) = diff::unified(
                &left.to_string_lossy(),
                &ta,
                &right.to_string_lossy(),
                &tb,
                3,
            );
            Outcome::Text(lines, summary)
        }
        _ => Outcome::Binary {
            left: a.len() as u64,
            right: b.len() as u64,
        },
    }
}

pub fn show(parent: &gtk::Window, left: PathBuf, right: PathBuf) {
    let title = format!(
        "{} ↔ {}",
        left.file_name().unwrap_or_default().to_string_lossy(),
        right.file_name().unwrap_or_default().to_string_lossy()
    );
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title(title)
        .default_width(960)
        .default_height(700)
        .build();
    crate::ops::fit(&window, parent, 960, 700);
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .left_margin(8)
        .right_margin(8)
        .build();
    let buffer = view.buffer();
    let tags = buffer.tag_table();
    for (name, color) in [
        ("insert", "#4caf50"),
        ("delete", "#ef5350"),
        ("header", "#7aa2f7"),
    ] {
        let tag = gtk::TextTag::builder().name(name).foreground(color).build();
        tags.add(&tag);
    }
    let header_tag = gtk::TextTag::builder().name("bold").weight(700).build();
    tags.add(&header_tag);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    status.add_css_class("status-bar");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build(),
    );
    content.append(&status);
    window.set_child(Some(&content));

    status.set_text("Comparing…");
    let (l, r) = (left.clone(), right.clone());
    glib::spawn_future_local(glib::clone!(
        #[weak]
        buffer,
        #[weak]
        status,
        async move {
            let outcome = gio::spawn_blocking(move || compare(&l, &r))
                .await
                .unwrap_or_else(|_| Outcome::Error("compare thread panicked".into()));
            match outcome {
                Outcome::Identical => status.set_text("Files are identical."),
                Outcome::Binary { left, right } => status.set_text(&format!(
                    "Binary files differ ({} vs {}).",
                    human_size(left),
                    human_size(right)
                )),
                Outcome::Error(err) => status.set_text(&err),
                Outcome::Text(lines, summary) => {
                    for line in &lines {
                        let mut end = buffer.end_iter();
                        let tag = match line.kind {
                            LineKind::Header => Some("header"),
                            LineKind::Insert => Some("insert"),
                            LineKind::Delete => Some("delete"),
                            LineKind::Equal => None,
                        };
                        let text = format!("{}\n", line.text);
                        match tag {
                            Some(tag) if line.kind == LineKind::Header => {
                                buffer.insert_with_tags_by_name(&mut end, &text, &[tag, "bold"])
                            }
                            Some(tag) => buffer.insert_with_tags_by_name(&mut end, &text, &[tag]),
                            None => buffer.insert(&mut end, &text),
                        }
                    }
                    status.set_text(&format!(
                        "{} line(s) added, {} removed.",
                        summary.inserted, summary.deleted
                    ));
                }
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
            if matches!(key, gdk::Key::Escape | gdk::Key::q) {
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
