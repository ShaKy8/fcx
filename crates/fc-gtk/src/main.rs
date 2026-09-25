mod pane;
mod row;

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, glib};

use crate::pane::Pane;

const APP_ID: &str = "org.omarchy.fc";

fn main() -> glib::ExitCode {
    // `fc [DIR]`: we parse argv ourselves; GApplication would reject unknown arguments.
    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .and_then(|p| std::path::absolute(p).ok())
        .unwrap_or_else(glib::home_dir);

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| build_window(app, start.clone()));
    app.run_with_args::<&str>(&[])
}

fn build_window(app: &Application, start: PathBuf) {
    let pane = Pane::new();
    let window = ApplicationWindow::builder()
        .application(app)
        .title("fc")
        .default_width(1200)
        .default_height(760)
        .child(pane.widget())
        .build();
    pane.navigate(start, None);
    pane.focus();
    window.present();
}
