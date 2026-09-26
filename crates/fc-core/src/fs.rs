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

/// Which entries a flat listing keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flat {
    Files,
    Folders,
    All,
}

/// Entry for `path`, named `name` (any relative name the caller wants).
fn entry_at(path: &Path, name: OsString) -> Option<Entry> {
    let meta = path.symlink_metadata().ok()?;
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        match fs::metadata(path) {
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
        name,
        kind,
        size: meta.len(),
        modified: meta.modified().ok(),
        mode: meta.permissions().mode(),
        uid: meta.uid(),
        gid: meta.gid(),
    })
}

/// FC's "plain view": every entry under `dir`, named by its path relative to
/// `dir` (`docs/notes.txt`). Symlinked folders are listed but not descended.
pub fn list_recursive(dir: &Path, keep: Flat) -> Result<Vec<Entry>, FsError> {
    fs::read_dir(dir).map_err(|source| FsError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(entries) = fs::read_dir(dir.join(&rel)) else {
            continue;
        };
        for de in entries.flatten() {
            let rel_name = rel.join(de.file_name());
            let Some(entry) = entry_at(&de.path(), rel_name.clone().into_os_string()) else {
                continue;
            };
            let is_real_dir = entry.kind == EntryKind::Dir;
            let wanted = match keep {
                Flat::Files => !entry.is_dir_like(),
                Flat::Folders => entry.is_dir_like(),
                Flat::All => true,
            };
            if wanted {
                out.push(entry);
            }
            if is_real_dir {
                stack.push(rel_name);
            }
        }
    }
    Ok(out)
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

    #[test]
    fn flat_listing_names_by_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("docs/deep")).unwrap();
        fs::write(root.join("top.txt"), b"1").unwrap();
        fs::write(root.join("docs/a.md"), b"22").unwrap();
        fs::write(root.join("docs/deep/b.md"), b"333").unwrap();
        symlink(root.join("docs"), root.join("link")).unwrap();

        let mut files: Vec<String> = list_recursive(root, Flat::Files)
            .unwrap()
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, ["docs/a.md", "docs/deep/b.md", "top.txt"]);

        let mut folders: Vec<String> = list_recursive(root, Flat::Folders)
            .unwrap()
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        folders.sort();
        assert_eq!(
            folders,
            ["docs", "docs/deep", "link"],
            "symlinked folder listed, not descended"
        );

        assert_eq!(list_recursive(root, Flat::All).unwrap().len(), 6);
        assert!(list_recursive(&root.join("nope"), Flat::All).is_err());
    }
}
