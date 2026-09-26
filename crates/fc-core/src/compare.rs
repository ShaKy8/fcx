//! Folder comparison (FC's Alt+V) and synchronization planning (Alt+S).
//!
//! [`compare`] walks two folders side by side and reports every difference:
//! items only on one side, files that differ (by size and modification time,
//! or by content), and which side is newer. A folder present on one side only
//! is reported once, not its children — the job engine copies or deletes it
//! whole. [`plan_sync`] turns differences into copy/delete actions.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// FAT and some network filesystems store times at 2-second resolution.
const TIME_TOLERANCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Same size and modification time (within tolerance) means same.
    #[default]
    SizeAndTime,
    /// Same size and identical bytes means same; times are ignored.
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub recursive: bool,
    pub method: Method,
    pub include_hidden: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            recursive: true,
            method: Method::SizeAndTime,
            include_hidden: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    LeftOnly,
    RightOnly,
    Same,
    /// Files differ and neither is clearly newer (or times are ignored).
    Different,
    LeftNewer,
    RightNewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// Path relative to the compared roots.
    pub rel: PathBuf,
    pub is_dir: bool,
    pub status: Status,
    pub left: Option<Meta>,
    pub right: Option<Meta>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Every compared item, including `Same` ones, in path order.
    pub items: Vec<Diff>,
    pub cancelled: bool,
    /// Folders that could not be read.
    pub errors: Vec<PathBuf>,
}

impl Report {
    pub fn differences(&self) -> impl Iterator<Item = &Diff> {
        self.items.iter().filter(|d| d.status != Status::Same)
    }
}

/// Compare `left` and `right`. Symlinks are compared as what they point to
/// for files, and never descended for folders.
pub fn compare(
    left: &Path,
    right: &Path,
    options: &Options,
    cancel: &AtomicBool,
) -> io::Result<Report> {
    if !left.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            left.display().to_string(),
        ));
    }
    if !right.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            right.display().to_string(),
        ));
    }
    let mut report = Report::default();
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let l = list(&left.join(&rel), options.include_hidden, &mut report.errors);
        let r = list(
            &right.join(&rel),
            options.include_hidden,
            &mut report.errors,
        );
        let names: std::collections::BTreeSet<&OsString> = l.keys().chain(r.keys()).collect();
        let mut subdirs = Vec::new();
        for name in names {
            let item_rel = rel.join(name);
            match (l.get(name), r.get(name)) {
                (Some(a), None) => report.items.push(Diff {
                    rel: item_rel,
                    is_dir: a.is_dir,
                    status: Status::LeftOnly,
                    left: Some(a.meta),
                    right: None,
                }),
                (None, Some(b)) => report.items.push(Diff {
                    rel: item_rel,
                    is_dir: b.is_dir,
                    status: Status::RightOnly,
                    left: None,
                    right: Some(b.meta),
                }),
                (Some(a), Some(b)) => {
                    if a.is_dir && b.is_dir {
                        if options.recursive {
                            subdirs.push(item_rel);
                        }
                        // Folder pairs are not reported themselves; their contents are.
                        continue;
                    }
                    let status = if a.is_dir != b.is_dir {
                        Status::Different
                    } else {
                        file_status(
                            &left.join(&item_rel),
                            &right.join(&item_rel),
                            a.meta,
                            b.meta,
                            options.method,
                        )
                    };
                    report.items.push(Diff {
                        rel: item_rel,
                        is_dir: false,
                        status,
                        left: Some(a.meta),
                        right: Some(b.meta),
                    });
                }
                (None, None) => unreachable!("name came from one of the maps"),
            }
        }
        // Push in reverse so folders are visited in name order.
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(report)
}

struct Entry {
    is_dir: bool,
    meta: Meta,
}

fn list(dir: &Path, include_hidden: bool, errors: &mut Vec<PathBuf>) -> BTreeMap<OsString, Entry> {
    let mut out = BTreeMap::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            errors.push(dir.to_path_buf());
            return out;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !include_hidden && name.as_encoded_bytes().starts_with(b".") {
            continue;
        }
        // Follow links for files (compare targets); a link to a folder is a
        // folder we do not descend, so treat it as an opaque item.
        let Ok(lmeta) = entry.metadata() else {
            continue;
        };
        let is_link = lmeta.file_type().is_symlink();
        let meta = if is_link {
            fs::metadata(entry.path()).unwrap_or(lmeta)
        } else {
            lmeta
        };
        let is_dir = meta.is_dir() && !is_link;
        out.insert(
            name,
            Entry {
                is_dir,
                meta: Meta {
                    size: if is_dir { 0 } else { meta.len() },
                    modified: meta.modified().ok(),
                },
            },
        );
    }
    out
}

fn file_status(left: &Path, right: &Path, a: Meta, b: Meta, method: Method) -> Status {
    match method {
        Method::Content => {
            if a.size == b.size && same_content(left, right).unwrap_or(false) {
                Status::Same
            } else {
                newer(a, b).unwrap_or(Status::Different)
            }
        }
        Method::SizeAndTime => match (a.modified, b.modified) {
            (Some(x), Some(y)) if a.size == b.size && within_tolerance(x, y) => Status::Same,
            _ => newer(a, b).unwrap_or(Status::Different),
        },
    }
}

fn within_tolerance(x: SystemTime, y: SystemTime) -> bool {
    let gap = x
        .duration_since(y)
        .or_else(|e| Ok::<_, ()>(e.duration()))
        .unwrap_or_default();
    gap <= TIME_TOLERANCE
}

fn newer(a: Meta, b: Meta) -> Option<Status> {
    let (x, y) = (a.modified?, b.modified?);
    if within_tolerance(x, y) {
        return None;
    }
    Some(if x > y {
        Status::LeftNewer
    } else {
        Status::RightNewer
    })
}

fn same_content(left: &Path, right: &Path) -> io::Result<bool> {
    let mut a = fs::File::open(left)?;
    let mut b = fs::File::open(right)?;
    let mut buf_a = vec![0u8; 64 * 1024];
    let mut buf_b = vec![0u8; 64 * 1024];
    loop {
        let n = a.read(&mut buf_a)?;
        let m = b.read_exact(&mut buf_b[..n]);
        if n == 0 {
            // Both at EOF only if b has nothing left either.
            return Ok(b.read(&mut buf_b)? == 0);
        }
        if m.is_err() || buf_a[..n] != buf_b[..n] {
            return Ok(false);
        }
    }
}

// ---- synchronization ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    LeftToRight,
    RightToLeft,
    /// Copy each way; the newer file wins, conflicts are skipped.
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// Copy `from` over/into `to` (the full target path).
    Copy { from: PathBuf, to: PathBuf },
    /// Remove an item that only exists on the target side.
    Delete { path: PathBuf },
    /// Nothing to do, with the reason (shown in the preview).
    Skip { rel: PathBuf, reason: &'static str },
}

/// Turn a report into actions. `delete_extra` removes items that exist only
/// on the target side (one-way directions only).
pub fn plan_sync(
    left: &Path,
    right: &Path,
    report: &Report,
    direction: Direction,
    delete_extra: bool,
) -> Vec<SyncAction> {
    let mut actions = Vec::new();
    for diff in report.differences() {
        let l = left.join(&diff.rel);
        let r = right.join(&diff.rel);
        let action = match (direction, diff.status) {
            (Direction::LeftToRight, Status::RightOnly)
            | (Direction::RightToLeft, Status::LeftOnly) => {
                let path = if diff.status == Status::RightOnly {
                    r
                } else {
                    l
                };
                if delete_extra {
                    SyncAction::Delete { path }
                } else {
                    SyncAction::Skip {
                        rel: diff.rel.clone(),
                        reason: "only on target side",
                    }
                }
            }
            (Direction::LeftToRight, _) => SyncAction::Copy { from: l, to: r },
            (Direction::RightToLeft, _) => SyncAction::Copy { from: r, to: l },
            (Direction::Both, Status::LeftOnly | Status::LeftNewer) => {
                SyncAction::Copy { from: l, to: r }
            }
            (Direction::Both, Status::RightOnly | Status::RightNewer) => {
                SyncAction::Copy { from: r, to: l }
            }
            (Direction::Both, Status::Different) => SyncAction::Skip {
                rel: diff.rel.clone(),
                reason: "differs, neither is newer",
            },
            (_, Status::Same) => continue,
        };
        actions.push(action);
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_mtime(path: &Path, secs: u64) {
        fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    fn trees() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let l = tmp.path().join("left");
        let r = tmp.path().join("right");
        fs::create_dir_all(l.join("sub/deep")).unwrap();
        fs::create_dir_all(r.join("sub")).unwrap();
        fs::create_dir_all(l.join("only-left-dir")).unwrap();
        fs::write(l.join("only-left-dir/x"), "x").unwrap();
        fs::write(l.join("same.txt"), "same").unwrap();
        fs::write(r.join("same.txt"), "same").unwrap();
        fs::write(l.join("newer-left.txt"), "v2").unwrap();
        fs::write(r.join("newer-left.txt"), "v1").unwrap();
        fs::write(l.join("sub/deep/leaf"), "leaf").unwrap();
        fs::write(l.join("sub/shared"), "abc").unwrap();
        fs::write(r.join("sub/shared"), "abd").unwrap();
        fs::write(r.join("only-right.txt"), "r").unwrap();
        fs::write(l.join(".hidden"), "h").unwrap();
        for p in [
            l.join("same.txt"),
            r.join("same.txt"),
            l.join("sub/shared"),
            r.join("sub/shared"),
        ] {
            set_mtime(&p, 1_000_000);
        }
        set_mtime(&l.join("newer-left.txt"), 2_000_000);
        set_mtime(&r.join("newer-left.txt"), 1_000_000);
        (tmp, l, r)
    }

    fn statuses(report: &Report) -> Vec<(String, Status)> {
        report
            .items
            .iter()
            .map(|d| (d.rel.to_string_lossy().into_owned(), d.status))
            .collect()
    }

    #[test]
    fn recursive_size_and_time() {
        let (_tmp, l, r) = trees();
        let report = compare(&l, &r, &Options::default(), &AtomicBool::new(false)).unwrap();
        assert_eq!(
            statuses(&report),
            [
                (".hidden".to_owned(), Status::LeftOnly),
                ("newer-left.txt".to_owned(), Status::LeftNewer),
                ("only-left-dir".to_owned(), Status::LeftOnly),
                ("only-right.txt".to_owned(), Status::RightOnly),
                ("same.txt".to_owned(), Status::Same),
                ("sub/deep".to_owned(), Status::LeftOnly),
                // same size and time, so size+time cannot tell them apart
                ("sub/shared".to_owned(), Status::Same),
            ]
        );
        assert!(report.errors.is_empty() && !report.cancelled);
        let dir_diff = report
            .items
            .iter()
            .find(|d| d.rel.ends_with("only-left-dir"))
            .unwrap();
        assert!(
            dir_diff.is_dir,
            "one-sided folders are reported once, as folders"
        );
    }

    #[test]
    fn content_method_and_options() {
        let (_tmp, l, r) = trees();
        let options = Options {
            method: Method::Content,
            recursive: false,
            include_hidden: false,
        };
        let report = compare(&l, &r, &options, &AtomicBool::new(false)).unwrap();
        let names: Vec<String> = report
            .items
            .iter()
            .map(|d| d.rel.to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with('.')), "hidden skipped");
        assert!(
            !names.iter().any(|n| n.starts_with("sub/")),
            "not recursive"
        );

        let options = Options {
            method: Method::Content,
            ..Default::default()
        };
        let report = compare(&l, &r, &options, &AtomicBool::new(false)).unwrap();
        let shared = report
            .items
            .iter()
            .find(|d| d.rel == Path::new("sub/shared"))
            .unwrap();
        assert_eq!(
            shared.status,
            Status::Different,
            "content differs despite equal size/time"
        );
        let same = report
            .items
            .iter()
            .find(|d| d.rel == Path::new("same.txt"))
            .unwrap();
        assert_eq!(same.status, Status::Same);
    }

    #[test]
    fn sync_plans_per_direction() {
        let (_tmp, l, r) = trees();
        let report = compare(&l, &r, &Options::default(), &AtomicBool::new(false)).unwrap();

        let ltr = plan_sync(&l, &r, &report, Direction::LeftToRight, false);
        assert!(ltr.iter().any(|a| matches!(a, SyncAction::Copy { from, to } if from == &l.join("newer-left.txt") && to == &r.join("newer-left.txt"))));
        assert!(ltr.iter().any(
            |a| matches!(a, SyncAction::Copy { from, .. } if from == &l.join("only-left-dir"))
        ));
        assert!(ltr.iter().any(
            |a| matches!(a, SyncAction::Skip { rel, .. } if rel == Path::new("only-right.txt"))
        ));

        let ltr_del = plan_sync(&l, &r, &report, Direction::LeftToRight, true);
        assert!(ltr_del.iter().any(
            |a| matches!(a, SyncAction::Delete { path } if path == &r.join("only-right.txt"))
        ));

        let rtl = plan_sync(&l, &r, &report, Direction::RightToLeft, false);
        assert!(rtl.iter().any(|a| matches!(a, SyncAction::Copy { from, to } if from == &r.join("newer-left.txt") && to == &l.join("newer-left.txt"))));
        assert!(rtl.iter().any(
            |a| matches!(a, SyncAction::Copy { from, .. } if from == &r.join("only-right.txt"))
        ));

        let both = plan_sync(&l, &r, &report, Direction::Both, true);
        assert!(
            !both.iter().any(|a| matches!(a, SyncAction::Delete { .. })),
            "both never deletes"
        );
        assert!(both.iter().any(
            |a| matches!(a, SyncAction::Copy { from, .. } if from == &l.join("newer-left.txt"))
        ));
        assert!(both.iter().any(
            |a| matches!(a, SyncAction::Copy { from, .. } if from == &r.join("only-right.txt"))
        ));
        assert!(!both.iter().any(
            |a| matches!(a, SyncAction::Copy { from, .. } if from == &r.join("newer-left.txt"))
        ));
    }

    #[test]
    fn cancel_and_missing_roots() {
        let (_tmp, l, r) = trees();
        let report = compare(&l, &r, &Options::default(), &AtomicBool::new(true)).unwrap();
        assert!(report.cancelled);
        assert!(
            compare(
                &l.join("nope"),
                &r,
                &Options::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }

    #[test]
    fn content_comparison_edge_cases() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&a, &big).unwrap();
        fs::write(&b, &big).unwrap();
        assert!(same_content(&a, &b).unwrap());
        let mut changed = big.clone();
        changed[150_000] ^= 1;
        fs::write(&b, &changed).unwrap();
        assert!(!same_content(&a, &b).unwrap());
        fs::write(&b, &big[..100_000]).unwrap();
        assert!(!same_content(&a, &b).unwrap(), "prefix is not equal");
    }
}
