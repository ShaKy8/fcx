//! File-operation UI: prompts, confirmations, clipboard interop, and the job
//! runner that executes `fc_core::jobs` on worker threads while showing progress.
//!
//! A worker reports through a `ChannelSink`; conflicts and errors block the
//! worker on an `mpsc` reply channel until the user answers a dialog here.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fc_core::format::human_size;
use fc_core::jobs::{
    self, ConflictReply, Control, ErrorReply, JobSpec, Operation, Outcome, Progress, Report,
};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Nautilus' clipboard format: first line `copy` or `cut`, then one URI per line.
const GNOME_FILES: &str = "x-special/gnome-copied-files";

/// Minimum gap between progress messages so 100k tiny files don't flood the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(40);

// ---- simple dialogs ---------------------------------------------------------

/// Modal one-line text prompt. `select` picks the initially selected text range
/// (e.g. a filename without its extension); `on_ok` receives the trimmed text.
pub fn prompt(
    parent: &gtk::Window,
    title: &str,
    message: &str,
    initial: &str,
    select: Option<(i32, i32)>,
    ok_label: &str,
    on_ok: impl Fn(String) + 'static,
) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(560)
        .resizable(false)
        .build();
    let label = gtk::Label::builder()
        .label(message)
        .xalign(0.0)
        .wrap(true)
        .build();
    let entry = gtk::Entry::builder().text(initial).hexpand(true).build();
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label(ok_label);
    ok.add_css_class("suggested-action");

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&cancel);
    buttons.append(&ok);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&label);
    content.append(&entry);
    content.append(&buttons);
    window.set_child(Some(&content));

    let on_ok = Rc::new(on_ok);
    let submit = glib::clone!(
        #[weak]
        window,
        #[weak]
        entry,
        move || {
            let text = entry.text().trim().to_owned();
            window.destroy();
            if !text.is_empty() {
                on_ok(text);
            }
        }
    );
    entry.connect_activate(glib::clone!(
        #[strong]
        submit,
        move |_| submit()
    ));
    ok.connect_clicked(move |_| submit());
    cancel.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.destroy()
    ));
    close_on_escape(&window);

    window.present();
    entry.grab_focus();
    if let Some((start, end)) = select {
        entry.select_region(start, end);
    }
}

/// Yes/no confirmation; `on_ok` runs only if the user picks `ok_label`.
/// Enter confirms (focus starts on the confirm button), Escape cancels.
pub fn confirm(
    parent: &gtk::Window,
    message: &str,
    detail: &str,
    ok_label: &str,
    on_ok: impl FnOnce() + 'static,
) {
    let parent = parent.clone();
    let (message, detail, ok_label) = (message.to_owned(), detail.to_owned(), ok_label.to_owned());
    glib::spawn_future_local(async move {
        let buttons = [
            ("Cancel", false, None),
            (ok_label.as_str(), true, Some("suggested-action")),
        ];
        if let Some((true, _)) = choose(&parent, &message, &message, &detail, "", 1, &buttons).await
        {
            on_ok();
        }
    });
}

pub fn alert(parent: &gtk::Window, message: &str, detail: &str) {
    gtk::AlertDialog::builder()
        .message(message)
        .detail(detail)
        .modal(true)
        .build()
        .show(Some(parent));
}

fn close_on_escape(window: &gtk::Window) {
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
}

/// A modal question with several answer buttons and an optional "apply to all"
/// checkbox. Resolves to `None` if the window is closed without choosing.
async fn choose<T: Copy + 'static>(
    parent: &gtk::Window,
    title: &str,
    message: &str,
    detail: &str,
    check_label: &str,
    default: usize,
    buttons: &[(&str, T, Option<&str>)],
) -> Option<(T, bool)> {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(560)
        .resizable(false)
        .build();
    let heading = gtk::Label::builder()
        .label(message)
        .xalign(0.0)
        .wrap(true)
        .build();
    heading.add_css_class("title-4");
    let body = gtk::Label::builder()
        .label(detail)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    let check = gtk::CheckButton::with_label(check_label);
    check.set_visible(!check_label.is_empty());

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .build();
    let (tx, rx) = async_channel::bounded::<(T, bool)>(1);
    let mut widgets = Vec::new();
    for &(label, value, class) in buttons {
        let button = gtk::Button::with_label(label);
        if let Some(class) = class {
            button.add_css_class(class);
        }
        button.connect_clicked(glib::clone!(
            #[weak]
            window,
            #[weak]
            check,
            #[strong]
            tx,
            move |_| {
                let _ = tx.try_send((value, check.is_active()));
                window.destroy();
            }
        ));
        row.append(&button);
        widgets.push(button);
    }
    drop(tx);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&heading);
    content.append(&body);
    content.append(&check);
    content.append(&row);
    window.set_child(Some(&content));
    close_on_escape(&window);
    window.present();
    if let Some(button) = widgets.get(default) {
        button.grab_focus();
    }

    rx.recv().await.ok()
}

fn file_summary(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => "folder".to_owned(),
        Ok(meta) => {
            let when = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
                .and_then(|dt| dt.format("%Y-%m-%d %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default();
            format!("{}, {when}", human_size(meta.len()))
        }
        Err(_) => "unknown".to_owned(),
    }
}

// ---- clipboard --------------------------------------------------------------

/// Puts files on the clipboard in both the GTK (`text/uri-list`) and Nautilus
/// formats, so pasting works here and in Nautilus alike.
pub fn clipboard_put(widget: &impl IsA<gtk::Widget>, paths: &[PathBuf], cut: bool) {
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    let mut special = String::from(if cut { "cut" } else { "copy" });
    for file in &files {
        special.push('\n');
        special.push_str(&file.uri());
    }
    let providers = [
        gdk::ContentProvider::for_bytes(
            GNOME_FILES,
            &glib::Bytes::from_owned(special.into_bytes()),
        ),
        gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()),
    ];
    let _ = widget
        .clipboard()
        .set_content(Some(&gdk::ContentProvider::new_union(&providers)));
}

/// Files currently on the clipboard and whether they were cut.
pub async fn clipboard_take(widget: &impl IsA<gtk::Widget>) -> Option<(Vec<PathBuf>, bool)> {
    let clipboard = widget.clipboard();
    if clipboard.formats().contain_mime_type(GNOME_FILES) {
        let (stream, _) = clipboard
            .read_future(&[GNOME_FILES], glib::Priority::DEFAULT)
            .await
            .ok()?;
        let mut raw = Vec::new();
        loop {
            let chunk = stream
                .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
                .await
                .ok()?;
            if chunk.is_empty() {
                break;
            }
            raw.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&raw);
        let mut lines = text.lines();
        let cut = lines.next() == Some("cut");
        let paths = lines
            .filter(|l| !l.is_empty())
            .filter_map(|uri| gio::File::for_uri(uri).path())
            .collect();
        return Some((paths, cut));
    }
    let value = clipboard
        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
        .await
        .ok()?;
    let list = value.get::<gdk::FileList>().ok()?;
    Some((
        list.files().iter().filter_map(|f| f.path()).collect(),
        false,
    ))
}

// ---- trash ------------------------------------------------------------------

/// One item the trash refused.
pub struct TrashFailure {
    pub path: PathBuf,
    pub message: String,
    /// GIO cannot trash on this mount (tmpfs, some removable media); permanent delete is the only option.
    pub unsupported: bool,
}

/// Moves `paths` to the freedesktop trash (same one Nautilus uses) on a worker
/// thread; `on_done` gets the failures.
pub fn trash(paths: Vec<PathBuf>, on_done: impl FnOnce(Vec<TrashFailure>) + 'static) {
    glib::spawn_future_local(async move {
        let failures = gio::spawn_blocking(move || {
            paths
                .into_iter()
                .filter_map(|path| {
                    gio::File::for_path(&path)
                        .trash(gio::Cancellable::NONE)
                        .err()
                        .map(|err| TrashFailure {
                            path,
                            unsupported: err.matches(gio::IOErrorEnum::NotSupported),
                            message: err.to_string(),
                        })
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        on_done(failures);
    });
}

// ---- job runner -------------------------------------------------------------

enum Msg {
    Progress(Progress),
    Conflict {
        source: PathBuf,
        dest: PathBuf,
        reply: mpsc::Sender<(ConflictReply, bool)>,
    },
    Error {
        path: PathBuf,
        message: String,
        reply: mpsc::Sender<(ErrorReply, bool)>,
    },
    Done(Report),
}

/// Worker-side sink: streams progress to the UI, blocks on questions.
struct ChannelSink {
    tx: async_channel::Sender<Msg>,
    last_sent: Instant,
    conflict_all: Option<ConflictReply>,
    error_all: Option<ErrorReply>,
}

impl jobs::Sink for ChannelSink {
    fn progress(&mut self, progress: &Progress) {
        if self.last_sent.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last_sent = Instant::now();
        let _ = self.tx.send_blocking(Msg::Progress(progress.clone()));
    }

    fn conflict(&mut self, source: &Path, dest: &Path) -> ConflictReply {
        if let Some(reply) = self.conflict_all {
            return reply;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let msg = Msg::Conflict {
            source: source.to_path_buf(),
            dest: dest.to_path_buf(),
            reply: reply_tx,
        };
        if self.tx.send_blocking(msg).is_err() {
            return ConflictReply::Abort;
        }
        match reply_rx.recv() {
            Ok((reply, all)) => {
                if all {
                    self.conflict_all = Some(reply);
                }
                reply
            }
            Err(_) => ConflictReply::Abort,
        }
    }

    fn error(&mut self, path: &Path, error: &std::io::Error) -> ErrorReply {
        if let Some(reply) = self.error_all {
            return reply;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let msg = Msg::Error {
            path: path.to_path_buf(),
            message: error.to_string(),
            reply: reply_tx,
        };
        if self.tx.send_blocking(msg).is_err() {
            return ErrorReply::Abort;
        }
        match reply_rx.recv() {
            Ok((reply, all)) => {
                if all && reply == ErrorReply::Skip {
                    self.error_all = Some(reply);
                }
                reply
            }
            Err(_) => ErrorReply::Abort,
        }
    }
}

struct Queued {
    title: String,
    spec: JobSpec,
}

/// Runs jobs one at a time and shows them in a panel at the bottom of the window.
#[derive(Clone)]
pub struct JobRunner(Rc<RunnerInner>);

struct RunnerInner {
    window: gtk::Window,
    panel: gtk::Box,
    queue: RefCell<VecDeque<Queued>>,
    busy: Cell<bool>,
    on_finished: RefCell<Option<Rc<dyn Fn()>>>,
}

impl JobRunner {
    pub fn new(window: &gtk::Window) -> Self {
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .build();
        panel.add_css_class("jobs");
        JobRunner(Rc::new(RunnerInner {
            window: window.clone(),
            panel,
            queue: RefCell::new(VecDeque::new()),
            busy: Cell::new(false),
            on_finished: RefCell::new(None),
        }))
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.0.panel.upcast_ref()
    }

    /// Called after every job, whatever its outcome (panes refresh here).
    pub fn connect_finished(&self, f: impl Fn() + 'static) {
        *self.0.on_finished.borrow_mut() = Some(Rc::new(f));
    }

    pub fn enqueue(&self, title: String, spec: JobSpec) {
        self.0.queue.borrow_mut().push_back(Queued { title, spec });
        self.pump();
    }

    fn pump(&self) {
        if self.0.busy.get() {
            return;
        }
        let Some(job) = self.0.queue.borrow_mut().pop_front() else {
            return;
        };
        self.0.busy.set(true);
        self.start(job);
    }

    fn start(&self, job: Queued) {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title = gtk::Label::builder()
            .label(&job.title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let pause = gtk::ToggleButton::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text("Pause")
            .build();
        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel")
            .build();
        header.append(&title);
        header.append(&pause);
        header.append(&cancel);
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        detail.add_css_class("dim-label");
        let bar = gtk::ProgressBar::builder().show_text(true).build();
        row.append(&header);
        row.append(&detail);
        row.append(&bar);
        self.0.panel.append(&row);
        self.0.panel.set_visible(true);

        let control = Control::new();
        pause.connect_toggled(glib::clone!(
            #[strong]
            control,
            move |b| control.set_paused(b.is_active())
        ));
        cancel.connect_clicked(glib::clone!(
            #[strong]
            control,
            move |_| {
                control.cancel();
                control.set_paused(false);
            }
        ));

        let (tx, rx) = async_channel::unbounded::<Msg>();
        let spec = job.spec.clone();
        let worker_control = control.clone();
        std::thread::spawn(move || {
            let mut sink = ChannelSink {
                tx: tx.clone(),
                last_sent: Instant::now() - PROGRESS_INTERVAL,
                conflict_all: None,
                error_all: None,
            };
            let report = jobs::run(&spec, &worker_control, &mut sink);
            let _ = tx.send_blocking(Msg::Done(report));
        });

        let weak: Weak<RunnerInner> = Rc::downgrade(&self.0);
        let verb = verb(job.spec.op);
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let runner = JobRunner(inner);
                match msg {
                    Msg::Progress(p) => {
                        let fraction = if p.total_bytes > 0 {
                            p.done_bytes as f64 / p.total_bytes as f64
                        } else if p.total_files > 0 {
                            p.done_files as f64 / p.total_files as f64
                        } else {
                            0.0
                        };
                        bar.set_fraction(fraction.clamp(0.0, 1.0));
                        bar.set_text(Some(&format!(
                            "{} / {} files · {} / {}",
                            p.done_files,
                            p.total_files,
                            human_size(p.done_bytes),
                            human_size(p.total_bytes)
                        )));
                        detail.set_text(&p.current.to_string_lossy());
                    }
                    Msg::Conflict {
                        source,
                        dest,
                        reply,
                    } => {
                        let answer = runner.ask_conflict(&source, &dest).await;
                        let _ = reply.send(answer.unwrap_or((ConflictReply::Abort, false)));
                    }
                    Msg::Error {
                        path,
                        message,
                        reply,
                    } => {
                        let answer = runner.ask_error(&path, &message).await;
                        let _ = reply.send(answer.unwrap_or((ErrorReply::Abort, false)));
                    }
                    Msg::Done(report) => {
                        runner.0.panel.remove(&row);
                        if runner.0.panel.first_child().is_none() {
                            runner.0.panel.set_visible(false);
                        }
                        runner.report(verb, &report);
                        runner.0.busy.set(false);
                        let on_finished = runner.0.on_finished.borrow().clone();
                        if let Some(f) = on_finished {
                            f();
                        }
                        runner.pump();
                        break;
                    }
                }
            }
        });
    }

    async fn ask_conflict(&self, source: &Path, dest: &Path) -> Option<(ConflictReply, bool)> {
        let name = dest.file_name().unwrap_or_default().to_string_lossy();
        let folder = dest.parent().unwrap_or(dest).to_string_lossy();
        let detail = format!(
            "Source: {}\nDestination: {}",
            file_summary(source),
            file_summary(dest)
        );
        choose(
            &self.0.window,
            "File exists",
            &format!("“{name}” already exists in {folder}"),
            &detail,
            "Apply to all remaining conflicts",
            1,
            &[
                ("Abort", ConflictReply::Abort, None),
                ("Skip", ConflictReply::Skip, None),
                ("Rename", ConflictReply::Rename, None),
                (
                    "Overwrite",
                    ConflictReply::Overwrite,
                    Some("destructive-action"),
                ),
            ],
        )
        .await
    }

    async fn ask_error(&self, path: &Path, message: &str) -> Option<(ErrorReply, bool)> {
        choose(
            &self.0.window,
            "Error",
            &format!("Cannot process {}", path.to_string_lossy()),
            message,
            "Skip all further errors",
            2,
            &[
                ("Abort", ErrorReply::Abort, None),
                ("Skip", ErrorReply::Skip, None),
                ("Retry", ErrorReply::Retry, Some("suggested-action")),
            ],
        )
        .await
    }

    fn report(&self, verb: &str, report: &Report) {
        match report.outcome {
            Outcome::Cancelled => return,
            Outcome::Aborted if report.failures.is_empty() => {
                alert(&self.0.window, &format!("{verb} aborted"), "");
                return;
            }
            _ if report.failures.is_empty() => return,
            _ => {}
        }
        let n = report.failures.len();
        let mut detail: Vec<String> = report
            .failures
            .iter()
            .take(15)
            .map(|f| format!("{}: {}", f.path.to_string_lossy(), f.message))
            .collect();
        if n > 15 {
            detail.push(format!("… and {} more", n - 15));
        }
        let message = if report.outcome == Outcome::Aborted {
            format!("{verb} aborted after {n} error(s)")
        } else {
            format!("{verb} finished with {n} error(s)")
        };
        alert(&self.0.window, &message, &detail.join("\n"));
    }
}

pub fn verb(op: Operation) -> &'static str {
    match op {
        Operation::Copy => "Copy",
        Operation::Move => "Move",
        Operation::Delete => "Delete",
    }
}

/// `“name”` for one item, `N items` otherwise.
pub fn describe(names: &[std::ffi::OsString]) -> String {
    match names {
        [one] => format!("“{}”", one.to_string_lossy()),
        many => format!("{} items", many.len()),
    }
}
