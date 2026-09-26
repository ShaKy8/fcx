//! File viewer: a widget that shows text (with encoding detection, wrap and
//! hex toggles, find), images, or a summary for anything else. Used by the F3
//! window and by the Ctrl+Q quick-view panel.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use fc_core::format::human_size;
use fc_core::text::{self, Encoding};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Text files are read up to this size; the rest is noted, not shown.
const TEXT_LIMIT: usize = 8 << 20;
/// Hex view covers this many bytes.
const HEX_LIMIT: usize = 1 << 20;
/// Images are decoded scaled down to at most this many pixels per side.
const IMAGE_MAX: i32 = 4096;

enum Loaded {
    Text {
        text: String,
        encoding: Encoding,
        bytes: Vec<u8>,
        truncated: bool,
    },
    Image {
        width: i32,
        height: i32,
        stride: usize,
        has_alpha: bool,
        pixels: glib::Bytes,
    },
    Other {
        summary: String,
    },
    Error(String),
}

fn load(path: &Path) -> Loaded {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) => return Loaded::Error(err.to_string()),
    };
    if meta.is_dir() {
        return Loaded::Other {
            summary: "Folder".into(),
        };
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (content_type, _) = gio::content_type_guess(Some(&name), None);
    if content_type.starts_with("image/") {
        match gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, IMAGE_MAX, IMAGE_MAX, true) {
            Ok(pixbuf) => {
                return Loaded::Image {
                    width: pixbuf.width(),
                    height: pixbuf.height(),
                    stride: pixbuf.rowstride() as usize,
                    has_alpha: pixbuf.has_alpha(),
                    pixels: pixbuf.read_pixel_bytes(),
                };
            }
            Err(err) => return Loaded::Error(format!("cannot decode image: {err}")),
        }
    }
    let mut bytes = Vec::new();
    let truncated = match std::fs::File::open(path) {
        Ok(file) => {
            use std::io::Read;
            let mut limited = file.take(TEXT_LIMIT as u64 + 1);
            if let Err(err) = limited.read_to_end(&mut bytes) {
                return Loaded::Error(err.to_string());
            }
            if bytes.len() > TEXT_LIMIT {
                bytes.truncate(TEXT_LIMIT);
                true
            } else {
                false
            }
        }
        Err(err) => return Loaded::Error(err.to_string()),
    };
    let (decoded, encoding) = text::decode(&bytes, truncated);
    Loaded::Text {
        text: decoded.unwrap_or_default(),
        encoding,
        bytes,
        truncated,
    }
}

/// The viewer widget.
#[derive(Clone)]
pub struct Viewer(Rc<Inner>);

struct Inner {
    root: gtk::Box,
    stack: gtk::Stack,
    text_view: gtk::TextView,
    picture: gtk::Picture,
    other: gtk::Label,
    info: gtk::Label,
    find_bar: gtk::Revealer,
    find_entry: gtk::Entry,
    wrap: gtk::ToggleButton,
    hex: gtk::ToggleButton,
    current: RefCell<Option<PathBuf>>,
    loaded: RefCell<Option<Rc<Loaded>>>,
    generation: Cell<u64>,
}

impl Viewer {
    pub fn new() -> Self {
        let text_view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .left_margin(8)
            .right_margin(8)
            .top_margin(6)
            .wrap_mode(gtk::WrapMode::None)
            .build();
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .build();
        let other = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        other.add_css_class("dim-label");
        let stack = gtk::Stack::builder().vexpand(true).hexpand(true).build();
        stack.add_named(
            &gtk::ScrolledWindow::builder().child(&text_view).build(),
            Some("text"),
        );
        stack.add_named(
            &gtk::ScrolledWindow::builder().child(&picture).build(),
            Some("image"),
        );
        stack.add_named(&other, Some("other"));

        let info = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .margin_start(6)
            .build();
        info.add_css_class("status-bar");
        let wrap = gtk::ToggleButton::builder()
            .icon_name("format-justify-fill-symbolic")
            .tooltip_text("Wrap lines (w)")
            .can_focus(false)
            .build();
        let hex = gtk::ToggleButton::builder()
            .label("hex")
            .tooltip_text("Hex view (h)")
            .can_focus(false)
            .build();
        for b in [&wrap, &hex] {
            b.add_css_class("flat");
        }
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        bar.append(&info);
        bar.append(&wrap);
        bar.append(&hex);

        let find_entry = gtk::Entry::builder()
            .placeholder_text("Find (Enter: next, Shift+Enter: previous, Esc: close)")
            .hexpand(true)
            .build();
        find_entry
            .set_icon_from_icon_name(gtk::EntryIconPosition::Primary, Some("edit-find-symbolic"));
        let find_bar = gtk::Revealer::builder()
            .child(&find_entry)
            .reveal_child(false)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viewer");
        root.append(&find_bar);
        root.append(&stack);
        root.append(&bar);

        let viewer = Viewer(Rc::new(Inner {
            root,
            stack,
            text_view,
            picture,
            other,
            info,
            find_bar,
            find_entry,
            wrap,
            hex,
            current: RefCell::new(None),
            loaded: RefCell::new(None),
            generation: Cell::new(0),
        }));
        viewer.connect();
        viewer
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.0.root.upcast_ref()
    }

    pub fn current(&self) -> Option<PathBuf> {
        self.0.current.borrow().clone()
    }

    /// Load `path` on a worker thread and show it when ready.
    pub fn show_file(&self, path: PathBuf) {
        if self.current().as_deref() == Some(path.as_path()) {
            return;
        }
        *self.0.current.borrow_mut() = Some(path.clone());
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        self.0
            .info
            .set_text(&format!("Loading {}…", path.to_string_lossy()));
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let target = path.clone();
            let loaded = gio::spawn_blocking(move || load(&target)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.generation.get() != generation {
                return;
            }
            let loaded = match loaded {
                Ok(loaded) => loaded,
                Err(_) => Loaded::Error("viewer thread panicked".into()),
            };
            Viewer(inner).render(&path, Rc::new(loaded));
        });
    }

    pub fn clear(&self) {
        *self.0.current.borrow_mut() = None;
        *self.0.loaded.borrow_mut() = None;
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.other.set_text("");
        self.0.stack.set_visible_child_name("other");
        self.0.info.set_text("");
    }

    fn render(&self, path: &Path, loaded: Rc<Loaded>) {
        *self.0.loaded.borrow_mut() = Some(loaded.clone());
        let name = path.to_string_lossy();
        match &*loaded {
            Loaded::Text { .. } => {
                self.0.hex.set_sensitive(true);
                self.render_text();
                self.0.stack.set_visible_child_name("text");
            }
            Loaded::Image {
                width,
                height,
                stride,
                has_alpha,
                pixels,
            } => {
                let format = if *has_alpha {
                    gdk::MemoryFormat::R8g8b8a8
                } else {
                    gdk::MemoryFormat::R8g8b8
                };
                let texture = gdk::MemoryTexture::new(*width, *height, format, pixels, *stride);
                self.0.picture.set_paintable(Some(&texture));
                self.0.stack.set_visible_child_name("image");
                self.0.hex.set_sensitive(false);
                self.0.info.set_text(&format!("{name} · {width}×{height}"));
            }
            Loaded::Other { summary } => {
                self.0.other.set_text(summary);
                self.0.stack.set_visible_child_name("other");
                self.0.hex.set_sensitive(false);
                self.0.info.set_text(&name);
            }
            Loaded::Error(err) => {
                self.0.other.set_text(err);
                self.0.stack.set_visible_child_name("other");
                self.0.hex.set_sensitive(false);
                self.0.info.set_text(&name);
            }
        }
    }

    /// Fill the text view according to the hex toggle (binary always shows hex).
    fn render_text(&self) {
        let loaded = self.0.loaded.borrow().clone();
        let Some(loaded) = loaded else {
            return;
        };
        let Loaded::Text {
            text,
            encoding,
            bytes,
            truncated,
        } = &*loaded
        else {
            return;
        };
        let name = self
            .current()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let hex = self.0.hex.is_active() || *encoding == Encoding::Binary;
        let buffer = self.0.text_view.buffer();
        if hex {
            let shown = bytes.len().min(HEX_LIMIT);
            buffer.set_text(&text::hex_dump(&bytes[..shown]));
            let note = if shown < bytes.len() || *truncated {
                format!(" · first {} shown", human_size(shown as u64))
            } else {
                String::new()
            };
            self.0.info.set_text(&format!(
                "{name} · {} · hex{note}",
                human_size(bytes.len() as u64)
            ));
        } else {
            buffer.set_text(text);
            let lines = text.lines().count();
            let note = if *truncated {
                format!(" · first {} shown", human_size(TEXT_LIMIT as u64))
            } else {
                String::new()
            };
            self.0.info.set_text(&format!(
                "{name} · {} · {lines} lines · {}{note}",
                human_size(bytes.len() as u64),
                encoding.label()
            ));
        }
        buffer.place_cursor(&buffer.start_iter());
    }

    fn connect(&self) {
        let weak = Rc::downgrade(&self.0);
        self.0.wrap.connect_toggled(move |b| {
            if let Some(inner) = weak.upgrade() {
                inner.text_view.set_wrap_mode(if b.is_active() {
                    gtk::WrapMode::WordChar
                } else {
                    gtk::WrapMode::None
                });
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.hex.connect_toggled(move |_| {
            if let Some(inner) = weak.upgrade() {
                Viewer(inner).render_text();
            }
        });

        // Find: Enter next, Shift+Enter previous, Escape closes.
        let weak = Rc::downgrade(&self.0);
        self.0.find_entry.connect_activate(move |_| {
            if let Some(inner) = weak.upgrade() {
                Viewer(inner).find(true);
            }
        });
        let weak = Rc::downgrade(&self.0);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let viewer = Viewer(inner);
            match key {
                gdk::Key::Escape => {
                    viewer.0.find_bar.set_reveal_child(false);
                    viewer.0.text_view.grab_focus();
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter
                    if state.contains(gdk::ModifierType::SHIFT_MASK) =>
                {
                    viewer.find(false);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.0.find_entry.add_controller(keys);
    }

    /// Keys for the viewer body: Ctrl+F find, w wrap, h hex. Returns true if handled.
    pub fn handle_key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::f | gdk::Key::F if ctrl => {
                self.0.find_bar.set_reveal_child(true);
                self.0.find_entry.grab_focus();
                true
            }
            gdk::Key::F3 if self.0.find_bar.reveals_child() => {
                self.find(!state.contains(gdk::ModifierType::SHIFT_MASK));
                true
            }
            gdk::Key::w if !ctrl => {
                self.0.wrap.set_active(!self.0.wrap.is_active());
                true
            }
            gdk::Key::h if !ctrl && self.0.hex.is_sensitive() => {
                self.0.hex.set_active(!self.0.hex.is_active());
                true
            }
            _ => false,
        }
    }

    fn find(&self, forward: bool) {
        let needle = self.0.find_entry.text();
        if needle.is_empty() {
            return;
        }
        let buffer = self.0.text_view.buffer();
        let flags = gtk::TextSearchFlags::CASE_INSENSITIVE;
        let (sel_start, sel_end) = buffer.selection_bounds().unwrap_or_else(|| {
            let c = buffer.iter_at_mark(&buffer.get_insert());
            (c, c)
        });
        let hit = if forward {
            sel_end
                .forward_search(&needle, flags, None)
                .or_else(|| buffer.start_iter().forward_search(&needle, flags, None))
        } else {
            sel_start
                .backward_search(&needle, flags, None)
                .or_else(|| buffer.end_iter().backward_search(&needle, flags, None))
        };
        match hit {
            Some((start, end)) => {
                buffer.select_range(&start, &end);
                self.0
                    .text_view
                    .scroll_to_iter(&mut start.clone(), 0.1, false, 0.0, 0.5);
                self.0.find_entry.remove_css_class("error");
            }
            None => self.0.find_entry.add_css_class("error"),
        }
    }
}

/// F3: a standalone viewer window for `path`.
pub fn window(parent: &gtk::Window, path: PathBuf) {
    let viewer = Viewer::new();
    let title = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title(format!("{title} — View"))
        .default_width(900)
        .default_height(700)
        .child(viewer.widget())
        .build();
    crate::ops::fit(&window, parent, 900, 700);
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[strong]
        viewer,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| {
            let in_entry = GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>());
            if in_entry {
                return glib::Propagation::Proceed;
            }
            if matches!(key, gdk::Key::Escape | gdk::Key::F3 | gdk::Key::q)
                && !viewer.0.find_bar.reveals_child()
            {
                window.destroy();
                return glib::Propagation::Stop;
            }
            if viewer.handle_key(key, state) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    ));
    window.add_controller(keys);
    viewer.show_file(path);
    window.present();
    viewer.0.text_view.grab_focus();
}
