//! List-model item: a core `Entry` plus display/sort strings computed once off the hot path.

use std::ffi::OsStr;
use std::path::Path;

use fc_core::fs::{Entry, EntryKind};
use fc_core::{format, sort};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

pub struct Row {
    pub entry: Entry,
    pub display_name: String,
    pub sort_key: String,
    /// Lowercased extension for sorting; empty for folders.
    pub ext_key: String,
    pub is_parent: bool,
}

impl Row {
    pub fn new(entry: Entry) -> Self {
        let display_name = entry.name.to_string_lossy().into_owned();
        let ext_key = if entry.is_dir_like() {
            String::new()
        } else {
            ext_of(&display_name).to_lowercase()
        };
        Self {
            sort_key: sort::name_key(&entry.name),
            display_name,
            ext_key,
            is_parent: false,
            entry,
        }
    }

    /// The synthetic `..` row shown at the top of every non-root listing.
    pub fn parent() -> Self {
        let mut row = Self::new(Entry {
            name: "..".into(),
            kind: EntryKind::Dir,
            size: 0,
            modified: None,
            mode: 0,
            uid: 0,
            gid: 0,
        });
        row.is_parent = true;
        row
    }

    pub fn name(&self) -> &OsStr {
        &self.entry.name
    }

    /// 0 = `..`, 1 = directories, 2 = everything else. Kept fixed regardless of column sort direction.
    pub fn group(&self) -> u8 {
        match (self.is_parent, self.entry.is_dir_like()) {
            (true, _) => 0,
            (false, true) => 1,
            (false, false) => 2,
        }
    }

    pub fn ext_text(&self) -> String {
        if self.is_parent || self.entry.is_dir_like() {
            String::new()
        } else {
            ext_of(&self.display_name).to_owned()
        }
    }

    pub fn size_text(&self) -> String {
        if self.entry.is_dir_like() || self.is_parent {
            String::new()
        } else {
            format::human_size(self.entry.size)
        }
    }

    pub fn modified_text(&self) -> String {
        self.entry
            .modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
            .and_then(|dt| dt.format("%Y-%m-%d %H:%M").ok())
            .map(Into::into)
            .unwrap_or_default()
    }

    pub fn mode_text(&self) -> String {
        if self.is_parent {
            String::new()
        } else {
            format::mode_string(&self.entry)
        }
    }

    /// Icon with GIO's fallback chain (e.g. `text-x-rust` → `text-x-generic`), so
    /// files the icon theme has no specific icon for still get a sensible one.
    pub fn icon(&self) -> gio::Icon {
        let names: &[&str] = match self.entry.kind {
            _ if self.is_parent => &["go-up-symbolic", "go-up"],
            EntryKind::Dir
            | EntryKind::Symlink {
                target_is_dir: true,
            } => &["folder", "inode-directory"],
            EntryKind::BrokenSymlink => &["emblem-unreadable", "dialog-error", "text-x-generic"],
            EntryKind::Other => &["inode-blockdevice", "text-x-generic"],
            EntryKind::File | EntryKind::Symlink { .. } => {
                let (content_type, _) = gio::content_type_guess(Some(&self.display_name), None);
                return gio::content_type_get_icon(&content_type);
            }
        };
        gio::ThemedIcon::from_names(names).upcast()
    }
}

/// Extension without the dot; dotfiles like `.bashrc` have none.
fn ext_of(name: &str) -> &str {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
}
