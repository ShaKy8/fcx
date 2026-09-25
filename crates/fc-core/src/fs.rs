//! Directory listing. Names stay as `OsString`; convert lossily only for display.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("cannot read directory {path}: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink { target_is_dir: bool },
    BrokenSymlink,
    Other,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    pub kind: EntryKind,
    /// Size of the entry itself (a symlink's own size, not its target's).
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Entry {
    pub fn is_hidden(&self) -> bool {
        self.name.as_encoded_bytes().first() == Some(&b'.')
    }

    /// True for directories and symlinks pointing at directories.
    pub fn is_dir_like(&self) -> bool {
        matches!(
            self.kind,
            EntryKind::Dir
                | EntryKind::Symlink {
                    target_is_dir: true
                }
        )
    }

    /// Permission bits only, e.g. `0o755`.
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// Lists `dir` without following symlinks for the entry itself.
///
/// Entries that vanish or can't be stat'ed mid-listing are skipped rather than
/// failing the whole listing. Order is unspecified; sorting is the caller's job.
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, FsError> {
    let read = fs::read_dir(dir).map_err(|source| FsError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;

    Ok(read
        .filter_map(Result::ok)
        .filter_map(|de| {
            let meta = de.path().symlink_metadata().ok()?;
            let ft = meta.file_type();
            let kind = if ft.is_symlink() {
                match fs::metadata(de.path()) {
                    Ok(target) => EntryKind::Symlink {
                        target_is_dir: target.is_dir(),
                    },
                    Err(_) => EntryKind::BrokenSymlink,
                }
            } else if ft.is_dir() {
                EntryKind::Dir
            } else if ft.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            Some(Entry {
                name: de.file_name(),
                kind,
                size: meta.len(),
                modified: meta.modified().ok(),
                mode: meta.permissions().mode(),
                uid: meta.uid(),
                gid: meta.gid(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    fn find<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"))
    }

    #[test]
    fn lists_kinds_without_following_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("file.txt"), b"hello").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        symlink(root.join("sub"), root.join("link-dir")).unwrap();
        symlink(root.join("missing"), root.join("broken")).unwrap();
        fs::write(root.join(".hidden"), b"").unwrap();

        let entries = list_dir(root).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(find(&entries, "file.txt").kind, EntryKind::File);
        assert_eq!(find(&entries, "file.txt").size, 5);
        assert_eq!(find(&entries, "sub").kind, EntryKind::Dir);
        assert_eq!(
            find(&entries, "link-dir").kind,
            EntryKind::Symlink {
                target_is_dir: true
            }
        );
        assert!(find(&entries, "link-dir").is_dir_like());
        assert_eq!(find(&entries, "broken").kind, EntryKind::BrokenSymlink);
        assert!(find(&entries, ".hidden").is_hidden());
        assert!(!find(&entries, "file.txt").is_hidden());
    }

    #[test]
    fn keeps_non_utf8_names() {
        let tmp = tempfile::tempdir().unwrap();
        let name = OsStr::from_bytes(b"bad-\xff.bin");
        fs::write(tmp.path().join(name), b"").unwrap();

        let entries = list_dir(tmp.path()).unwrap();
        assert_eq!(entries[0].name, name);
    }

    #[test]
    fn missing_dir_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            list_dir(&tmp.path().join("nope")),
            Err(FsError::ReadDir { .. })
        ));
    }
}
