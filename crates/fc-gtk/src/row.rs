//! List-model item: a core `Entry` plus display/sort strings computed once off the hot path.

use std::ffi::OsStr;

use fc_core::fs::{Entry, EntryKind};
use fc_core::{format, sort};
use gtk::glib;

pub struct Row {
    pub entry: Entry,
    pub display_name: String,
    pub sort_key: String,
    pub is_parent: bool,
}

impl Row {
    pub fn new(entry: Entry) -> Self {
        Self {
            display_name: entry.name.to_string_lossy().into_owned(),
            sort_key: sort::name_key(&entry.name),
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

    pub fn icon_name(&self) -> String {
        match self.entry.kind {
            _ if self.is_parent => "go-up".into(),
            EntryKind::Dir
            | EntryKind::Symlink {
                target_is_dir: true,
            } => "folder".into(),
            EntryKind::BrokenSymlink => "emblem-unreadable".into(),
            _ => {
                let (content_type, _) =
                    gtk::gio::content_type_guess(Some(&self.display_name), None);
                gtk::gio::content_type_get_generic_icon_name(&content_type)
                    .map(Into::into)
                    .unwrap_or_else(|| "text-x-generic".into())
            }
        }
    }
}
