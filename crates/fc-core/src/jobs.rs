//! Copy / move / delete engine.
//!
//! A job runs on a worker thread: it first *plans* (walks the sources to count
//! files and bytes, so progress is meaningful) and then *executes*, reporting to a
//! [`Sink`]. Conflicts and errors are questions the sink answers synchronously;
//! the GUI's sink blocks the worker while a dialog is up, tests answer with a
//! fixed policy. A shared [`Control`] pauses or cancels the job between steps.
//!
//! Filesystem rules baked in here: symlinks are recreated, never followed;
//! metadata (mode, mtime) is preserved; moves try `rename(2)` first and fall
//! back to copy + verify-free delete only on `EXDEV`; a folder is never copied
//! into itself; file data goes through `io::copy`, which uses `copy_file_range`
//! (reflinks on btrfs) and is chunked only so progress and cancel stay responsive.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// Bytes copied between progress reports and cancel checks.
const CHUNK: u64 = 8 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Copy,
    Move,
    Delete,
}

#[derive(Debug, Clone)]
pub struct JobSpec {
    pub op: Operation,
    pub sources: Vec<PathBuf>,
    /// Destination directory for Copy/Move; unused for Delete.
    pub dest: Option<PathBuf>,
}

impl JobSpec {
    pub fn copy(sources: Vec<PathBuf>, dest: PathBuf) -> Self {
        JobSpec {
            op: Operation::Copy,
            sources,
            dest: Some(dest),
        }
    }

    pub fn move_to(sources: Vec<PathBuf>, dest: PathBuf) -> Self {
        JobSpec {
            op: Operation::Move,
            sources,
            dest: Some(dest),
        }
    }

    pub fn delete(sources: Vec<PathBuf>) -> Self {
        JobSpec {
            op: Operation::Delete,
            sources,
            dest: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub done_files: u64,
    pub total_files: u64,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// The item being worked on right now.
    pub current: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictReply {
    /// Replace the destination (a file replaced by a folder, or vice versa, is removed first).
    Overwrite,
    Skip,
    /// Copy under a free name like `name (2).ext`.
    Rename,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorReply {
    Skip,
    Retry,
    Abort,
}

/// Where a running job reports to and asks questions of.
pub trait Sink {
    fn progress(&mut self, progress: &Progress);
    /// `dest` already exists and is not a folder being merged into.
    fn conflict(&mut self, source: &Path, dest: &Path) -> ConflictReply;
    fn error(&mut self, path: &Path, error: &io::Error) -> ErrorReply;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
    Aborted,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub outcome: Outcome,
    pub failures: Vec<Failure>,
    pub progress: Progress,
}

#[derive(Default)]
struct ControlState {
    paused: bool,
    cancelled: bool,
}

/// Pause/cancel switch shared between the UI and the worker.
#[derive(Clone, Default)]
pub struct Control(Arc<(Mutex<ControlState>, Condvar)>);

impl Control {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let (lock, cv) = &*self.0;
        lock.lock().unwrap().cancelled = true;
        cv.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        let (lock, cv) = &*self.0;
        lock.lock().unwrap().paused = paused;
        cv.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.0.lock().unwrap().cancelled
    }

    pub fn is_paused(&self) -> bool {
        self.0.0.lock().unwrap().paused
    }

    /// Blocks while paused; returns false once cancelled.
    fn proceed(&self) -> bool {
        let (lock, cv) = &*self.0;
        let mut state = lock.lock().unwrap();
        while state.paused && !state.cancelled {
            state = cv.wait(state).unwrap();
        }
        !state.cancelled
    }
}

/// First of `path`, `stem (2).ext`, `stem (3).ext`, … that does not exist.
pub fn next_free_name(path: &Path) -> PathBuf {
    if !path.exists() && fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let ext = path.extension();
    for n in 2u32.. {
        let mut name = stem.clone();
        name.push(format!(" ({n})"));
        if let Some(ext) = ext {
            name.push(".");
            name.push(ext);
        }
        let candidate = path.with_file_name(name);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    unreachable!()
}

pub fn run(spec: &JobSpec, control: &Control, sink: &mut dyn Sink) -> Report {
    let mut exec = Exec {
        control,
        sink,
        progress: Progress::default(),
        failures: Vec::new(),
    };
    let outcome = match exec.run(spec) {
        Ok(()) => Outcome::Completed,
        Err(Stop::Cancelled) => Outcome::Cancelled,
        Err(Stop::Aborted) => Outcome::Aborted,
    };
    Report {
        outcome,
        failures: exec.failures,
        progress: exec.progress,
    }
}

enum Stop {
    Cancelled,
    Aborted,
}

struct Exec<'a> {
    control: &'a Control,
    sink: &'a mut dyn Sink,
    progress: Progress,
    failures: Vec<Failure>,
}

impl Exec<'_> {
    fn run(&mut self, spec: &JobSpec) -> Result<(), Stop> {
        for src in &spec.sources {
            let (files, bytes) = count(src);
            self.progress.total_files += files;
            self.progress.total_bytes += bytes;
        }
        self.sink.progress(&self.progress);

        match spec.op {
            Operation::Delete => {
                for src in &spec.sources {
                    self.delete_item(src)?;
                }
            }
            Operation::Copy | Operation::Move => {
                let dest_dir = spec.dest.as_deref().expect("copy/move needs a destination");
                let moving = spec.op == Operation::Move;
                for src in &spec.sources {
                    let Some(name) = src.file_name() else {
                        self.fail(src, "has no file name");
                        continue;
                    };
                    if contains(src, dest_dir) {
                        self.fail(src, "cannot copy a folder into itself");
                        self.skip_subtree(src);
                        continue;
                    }
                    let dest = dest_dir.join(name);
                    if src.as_path() == dest && !moving {
                        // Copy into its own folder: always needs a new name.
                        self.transfer(src, next_free_name(&dest), false)?;
                        continue;
                    }
                    if src.as_path() == dest {
                        self.skip_subtree(src);
                        continue;
                    }
                    self.transfer(src, dest, moving)?;
                }
            }
        }
        Ok(())
    }

    // ---- copy / move -------------------------------------------------------

    /// Copies or moves one item (file, symlink, or folder tree) to exactly `dest`.
    fn transfer(&mut self, src: &Path, dest: PathBuf, moving: bool) -> Result<(), Stop> {
        let Some(meta) = self.attempt(src, || fs::symlink_metadata(src))? else {
            self.skip_subtree(src);
            return Ok(());
        };
        let src_is_dir = meta.is_dir();

        // Decide the final destination: merge folders, ask about anything else.
        let dest = match fs::symlink_metadata(&dest) {
            Err(_) => dest,
            Ok(existing) if src_is_dir && existing.is_dir() => dest,
            Ok(existing) => match self.sink.conflict(src, &dest) {
                ConflictReply::Overwrite => {
                    let removed = if existing.is_dir() {
                        self.attempt(&dest, || fs::remove_dir_all(&dest))?
                    } else {
                        self.attempt(&dest, || fs::remove_file(&dest))?
                    };
                    if removed.is_none() {
                        self.skip_subtree(src);
                        return Ok(());
                    }
                    dest
                }
                ConflictReply::Skip => {
                    self.skip_subtree(src);
                    return Ok(());
                }
                ConflictReply::Rename => next_free_name(&dest),
                ConflictReply::Abort => return Err(Stop::Aborted),
            },
        };

        // A plain rename is the fast path for moves; EXDEV means copy + delete.
        if moving && fs::symlink_metadata(&dest).is_err() {
            match fs::rename(src, &dest) {
                Ok(()) => {
                    let (files, bytes) = count(&dest);
                    self.progress.done_files += files;
                    self.progress.done_bytes += bytes;
                    self.progress.current = src.to_path_buf();
                    self.sink.progress(&self.progress);
                    return Ok(());
                }
                Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {}
                Err(err) => match self.sink.error(src, &err) {
                    ErrorReply::Retry => return self.transfer(src, dest, moving),
                    ErrorReply::Skip => {
                        self.fail(src, &err.to_string());
                        self.skip_subtree(src);
                        return Ok(());
                    }
                    ErrorReply::Abort => {
                        self.fail(src, &err.to_string());
                        return Err(Stop::Aborted);
                    }
                },
            }
        }

        if meta.file_type().is_symlink() {
            self.step(src);
            let ok = self
                .attempt(src, || {
                    let target = fs::read_link(src)?;
                    if fs::symlink_metadata(&dest).is_ok() {
                        fs::remove_file(&dest)?;
                    }
                    std::os::unix::fs::symlink(target, &dest)
                })?
                .is_some();
            if ok && moving {
                self.attempt(src, || fs::remove_file(src))?;
            }
        } else if src_is_dir {
            self.copy_dir(src, &dest, &meta, moving)?;
        } else {
            self.step(src);
            let existed = fs::symlink_metadata(&dest).is_ok();
            let start = self.progress.done_bytes;
            // Inlined retry loop: copy_file needs sink and progress at the same time.
            let copied = loop {
                if !self.control.proceed() {
                    return Err(Stop::Cancelled);
                }
                match copy_file(
                    src,
                    &dest,
                    &meta,
                    self.control,
                    &mut *self.sink,
                    &mut self.progress,
                ) {
                    Ok(()) => break true,
                    Err(_) if self.control.is_cancelled() => return Err(Stop::Cancelled),
                    Err(err) => match self.sink.error(src, &err) {
                        ErrorReply::Retry => {
                            self.progress.done_bytes = start;
                            continue;
                        }
                        ErrorReply::Skip => {
                            self.fail(src, &err.to_string());
                            break false;
                        }
                        ErrorReply::Abort => {
                            self.fail(src, &err.to_string());
                            return Err(Stop::Aborted);
                        }
                    },
                }
            };
            if copied {
                if moving {
                    self.attempt(src, || fs::remove_file(src))?;
                }
            } else {
                if !existed {
                    let _ = fs::remove_file(&dest);
                }
                self.progress.done_bytes = start + meta.len();
            }
        }
        Ok(())
    }

    fn copy_dir(
        &mut self,
        src: &Path,
        dest: &Path,
        meta: &fs::Metadata,
        moving: bool,
    ) -> Result<(), Stop> {
        self.step(src);
        if fs::symlink_metadata(dest).is_err()
            && self.attempt(dest, || fs::create_dir(dest))?.is_none()
        {
            self.skip_subtree(src);
            return Ok(());
        }
        let Some(entries) = self.attempt(src, || fs::read_dir(src))? else {
            self.skip_subtree(src);
            return Ok(());
        };
        let mut children: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        children.sort();
        for child in &children {
            let name = child.file_name().expect("read_dir yields named entries");
            self.transfer(child, dest.join(name), moving)?;
        }
        let _ = fs::set_permissions(dest, meta.permissions());
        if let Ok(mtime) = meta.modified()
            && let Ok(dir) = fs::File::open(dest)
        {
            let _ = dir.set_modified(mtime);
        }
        if moving {
            // Fails harmlessly if some children were skipped and remain.
            let _ = fs::remove_dir(src);
        }
        Ok(())
    }

    // ---- delete ------------------------------------------------------------

    fn delete_item(&mut self, path: &Path) -> Result<(), Stop> {
        let Some(meta) = self.attempt(path, || fs::symlink_metadata(path))? else {
            self.skip_subtree(path);
            return Ok(());
        };
        if meta.is_dir() {
            self.step(path);
            let Some(entries) = self.attempt(path, || fs::read_dir(path))? else {
                self.skip_subtree(path);
                return Ok(());
            };
            let children: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
            for child in &children {
                self.delete_item(child)?;
            }
            self.attempt(path, || fs::remove_dir(path))?;
        } else {
            self.step(path);
            self.progress.done_bytes += data_len(&meta);
            self.attempt(path, || fs::remove_file(path))?;
        }
        Ok(())
    }

    // ---- bookkeeping -------------------------------------------------------

    /// Runs `f`, asking the sink what to do on failure. `Ok(None)` means skipped.
    fn attempt<T>(
        &mut self,
        path: &Path,
        mut f: impl FnMut() -> io::Result<T>,
    ) -> Result<Option<T>, Stop> {
        loop {
            if !self.control.proceed() {
                return Err(Stop::Cancelled);
            }
            match f() {
                Ok(value) => return Ok(Some(value)),
                Err(_) if self.control.is_cancelled() => return Err(Stop::Cancelled),
                Err(err) => match self.sink.error(path, &err) {
                    ErrorReply::Retry => continue,
                    ErrorReply::Skip => {
                        self.fail(path, &err.to_string());
                        return Ok(None);
                    }
                    ErrorReply::Abort => {
                        self.fail(path, &err.to_string());
                        return Err(Stop::Aborted);
                    }
                },
            }
        }
    }

    fn fail(&mut self, path: &Path, message: &str) {
        self.failures.push(Failure {
            path: path.to_path_buf(),
            message: message.to_owned(),
        });
    }

    /// Marks one item as started and reports.
    fn step(&mut self, path: &Path) {
        self.progress.done_files += 1;
        self.progress.current = path.to_path_buf();
        self.sink.progress(&self.progress);
    }

    /// Accounts for a whole subtree that will not be processed, so the bar still reaches 100%.
    fn skip_subtree(&mut self, path: &Path) {
        let (files, bytes) = count(path);
        self.progress.done_files += files;
        self.progress.done_bytes += bytes;
        self.sink.progress(&self.progress);
    }
}

/// Copies file contents in chunks, then mode and mtime. Cancellation surfaces as
/// an `Interrupted` error, which `attempt` maps to `Stop::Cancelled`.
fn copy_file(
    src: &Path,
    dest: &Path,
    meta: &fs::Metadata,
    control: &Control,
    sink: &mut dyn Sink,
    progress: &mut Progress,
) -> io::Result<()> {
    let mut input = fs::File::open(src)?;
    // Never write through a symlink sitting at the destination.
    if fs::symlink_metadata(dest).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(dest)?;
    }
    let mut output = fs::File::create(dest)?;
    let start = progress.done_bytes;
    loop {
        if !control.proceed() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = io::copy(&mut (&mut input).take(CHUNK), &mut output)?;
        if n == 0 {
            break;
        }
        progress.done_bytes += n;
        sink.progress(progress);
    }
    // Keep byte totals honest even if the file changed size since planning.
    progress.done_bytes = start + meta.len();
    output.set_permissions(meta.permissions())?;
    if let Ok(mtime) = meta.modified() {
        output.set_modified(mtime)?;
    }
    Ok(())
}

/// Files and bytes under `path` (symlinks count as one file, never followed).
pub fn count(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (1, data_len(&meta));
    }
    let mut files = 1;
    let mut bytes = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            files += 1;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += data_len(&meta);
            }
        }
    }
    (files, bytes)
}

/// Bytes that actually get copied: symlinks carry no data.
fn data_len(meta: &fs::Metadata) -> u64 {
    if meta.file_type().is_symlink() {
        0
    } else {
        meta.len()
    }
}

/// True if `dir` is `ancestor` or lies inside it (compared on canonical paths).
fn contains(ancestor: &Path, dir: &Path) -> bool {
    match (fs::canonicalize(ancestor), fs::canonicalize(dir)) {
        (Ok(a), Ok(d)) => {
            fs::symlink_metadata(ancestor).is_ok_and(|m| m.is_dir()) && d.starts_with(a)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// Answers every question with a fixed policy and records what was asked.
    struct Policy {
        conflict: ConflictReply,
        error: ErrorReply,
        conflicts: Vec<PathBuf>,
        errors: Vec<PathBuf>,
        last: Progress,
    }

    impl Policy {
        fn new(conflict: ConflictReply, error: ErrorReply) -> Self {
            Policy {
                conflict,
                error,
                conflicts: vec![],
                errors: vec![],
                last: Progress::default(),
            }
        }
    }

    impl Sink for Policy {
        fn progress(&mut self, progress: &Progress) {
            self.last = progress.clone();
        }
        fn conflict(&mut self, _: &Path, dest: &Path) -> ConflictReply {
            self.conflicts.push(dest.to_path_buf());
            self.conflict
        }
        fn error(&mut self, path: &Path, _: &io::Error) -> ErrorReply {
            self.errors.push(path.to_path_buf());
            self.error
        }
    }

    fn tree(root: &Path) -> PathBuf {
        let src = root.join("src");
        fs::create_dir_all(src.join("sub/deep")).unwrap();
        fs::write(src.join("a.txt"), b"alpha").unwrap();
        fs::write(src.join("sub/b.txt"), b"bravo!").unwrap();
        fs::write(src.join("sub/deep/c.bin"), vec![7u8; 3000]).unwrap();
        fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o751)).unwrap();
        symlink("../a.txt", src.join("sub/link")).unwrap();
        symlink("/nonexistent", src.join("dangling")).unwrap();
        src
    }

    fn read(p: &Path) -> Vec<u8> {
        fs::read(p).unwrap()
    }

    fn run_ok(spec: JobSpec, sink: &mut Policy) -> Report {
        let report = run(&spec, &Control::new(), sink);
        assert_eq!(report.outcome, Outcome::Completed, "{:?}", report.failures);
        report
    }

    #[test]
    fn copy_preserves_tree_links_and_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);

        let report = run_ok(JobSpec::copy(vec![src.clone()], dest.clone()), &mut sink);

        let out = dest.join("src");
        assert_eq!(read(&out.join("a.txt")), b"alpha");
        assert_eq!(read(&out.join("sub/deep/c.bin")).len(), 3000);
        assert_eq!(
            fs::metadata(out.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        assert_eq!(
            fs::read_link(out.join("sub/link")).unwrap(),
            Path::new("../a.txt")
        );
        assert_eq!(
            fs::read_link(out.join("dangling")).unwrap(),
            Path::new("/nonexistent")
        );
        assert!(src.exists(), "copy leaves the source alone");
        assert!(report.failures.is_empty());
        // 4 dirs? src, sub, deep = 3 dirs + 3 files + 2 links = 8 items
        assert_eq!(report.progress.total_files, 8);
        assert_eq!(report.progress.done_files, 8);
        assert_eq!(report.progress.total_bytes, 5 + 6 + 3000);
        assert_eq!(report.progress.done_bytes, report.progress.total_bytes);
    }

    #[test]
    fn move_same_filesystem_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);

        run_ok(JobSpec::move_to(vec![src.clone()], dest.clone()), &mut sink);

        assert!(!src.exists());
        assert_eq!(read(&dest.join("src/sub/b.txt")), b"bravo!");
        assert!(sink.errors.is_empty());
    }

    #[test]
    fn move_across_filesystems_copies_then_deletes() {
        // /tmp is tmpfs on the dev box; the project dir is on the root filesystem.
        // If they happen to be the same device this still passes via rename.
        let here =
            tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target")).unwrap();
        let there = tempfile::tempdir().unwrap();
        let src = tree(here.path());
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);

        let report = run_ok(
            JobSpec::move_to(vec![src.clone()], there.path().to_path_buf()),
            &mut sink,
        );

        assert!(!src.exists(), "source removed after move");
        let out = there.path().join("src");
        assert_eq!(read(&out.join("a.txt")), b"alpha");
        assert_eq!(read(&out.join("sub/deep/c.bin")).len(), 3000);
        assert_eq!(
            fs::read_link(out.join("sub/link")).unwrap(),
            Path::new("../a.txt")
        );
        assert!(report.failures.is_empty());
        assert_eq!(report.progress.done_bytes, report.progress.total_bytes);
    }

    #[test]
    fn move_merges_into_existing_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dest = tmp.path().join("dest");
        fs::create_dir_all(dest.join("src/sub")).unwrap();
        fs::write(dest.join("src/existing.txt"), b"keep me").unwrap();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);

        run_ok(JobSpec::move_to(vec![src.clone()], dest.clone()), &mut sink);

        assert!(!src.exists());
        assert_eq!(read(&dest.join("src/existing.txt")), b"keep me");
        assert_eq!(read(&dest.join("src/sub/b.txt")), b"bravo!");
        assert!(sink.conflicts.is_empty(), "folders merge without asking");
    }

    #[test]
    fn conflicts_skip_overwrite_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        fs::write(&src, b"new").unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("a.txt"), b"old").unwrap();

        let mut skip = Policy::new(ConflictReply::Skip, ErrorReply::Abort);
        run_ok(JobSpec::copy(vec![src.clone()], dest.clone()), &mut skip);
        assert_eq!(read(&dest.join("a.txt")), b"old");
        assert_eq!(skip.conflicts, vec![dest.join("a.txt")]);

        let mut rename = Policy::new(ConflictReply::Rename, ErrorReply::Abort);
        run_ok(JobSpec::copy(vec![src.clone()], dest.clone()), &mut rename);
        assert_eq!(read(&dest.join("a (2).txt")), b"new");
        assert_eq!(read(&dest.join("a.txt")), b"old");

        let mut overwrite = Policy::new(ConflictReply::Overwrite, ErrorReply::Abort);
        run_ok(
            JobSpec::copy(vec![src.clone()], dest.clone()),
            &mut overwrite,
        );
        assert_eq!(read(&dest.join("a.txt")), b"new");

        let mut abort = Policy::new(ConflictReply::Abort, ErrorReply::Abort);
        let report = run(&JobSpec::copy(vec![src], dest), &Control::new(), &mut abort);
        assert_eq!(report.outcome, Outcome::Aborted);
    }

    #[test]
    fn copy_into_same_folder_picks_free_name() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("notes.md");
        fs::write(&src, b"x").unwrap();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);
        run_ok(
            JobSpec::copy(vec![src], tmp.path().to_path_buf()),
            &mut sink,
        );
        assert!(tmp.path().join("notes (2).md").exists());
        assert!(sink.conflicts.is_empty());
    }

    #[test]
    fn refuses_to_copy_folder_into_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);
        let report = run_ok(JobSpec::copy(vec![src.clone()], src.join("sub")), &mut sink);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].message.contains("into itself"));
        assert!(!src.join("sub/src").exists());
    }

    #[test]
    fn delete_removes_tree_without_following_links() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("precious"), b"!").unwrap();
        let victim = tmp.path().join("victim");
        fs::create_dir_all(victim.join("sub")).unwrap();
        fs::write(victim.join("sub/f"), b"f").unwrap();
        symlink(&outside, victim.join("link-out")).unwrap();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);

        let report = run_ok(JobSpec::delete(vec![victim.clone()]), &mut sink);

        assert!(!victim.exists());
        assert!(
            outside.join("precious").exists(),
            "symlink target untouched"
        );
        assert_eq!(report.progress.done_files, 4);
    }

    #[test]
    fn errors_can_be_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("ok.txt"), b"fine").unwrap();
        fs::write(src.join("locked.txt"), b"nope").unwrap();
        fs::set_permissions(src.join("locked.txt"), fs::Permissions::from_mode(0o000)).unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Skip);

        let report = run_ok(JobSpec::copy(vec![src.clone()], dest.clone()), &mut sink);

        assert_eq!(read(&dest.join("src/ok.txt")), b"fine");
        if nix_is_root() {
            return; // root can read anything; nothing to skip
        }
        assert_eq!(sink.errors, vec![src.join("locked.txt")]);
        assert_eq!(report.failures.len(), 1);
        assert!(
            !dest.join("src/locked.txt").exists(),
            "partial file cleaned up"
        );
        assert_eq!(report.progress.done_files, report.progress.total_files);
    }

    #[test]
    fn cancelled_control_stops_before_copying() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let control = Control::new();
        control.cancel();
        let mut sink = Policy::new(ConflictReply::Abort, ErrorReply::Abort);

        let report = run(&JobSpec::copy(vec![src], dest.clone()), &control, &mut sink);

        assert_eq!(report.outcome, Outcome::Cancelled);
        assert!(!dest.join("src/a.txt").exists());
    }

    #[test]
    fn next_free_name_counts_up() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("r.tar.gz");
        assert_eq!(next_free_name(&p), p);
        fs::write(&p, b"").unwrap();
        fs::write(tmp.path().join("r.tar (2).gz"), b"").unwrap();
        assert_eq!(next_free_name(&p), tmp.path().join("r.tar (3).gz"));
        let dot = tmp.path().join(".bashrc");
        fs::write(&dot, b"").unwrap();
        assert_eq!(next_free_name(&dot), tmp.path().join(".bashrc (2)"));
    }

    fn nix_is_root() -> bool {
        std::os::unix::fs::MetadataExt::uid(&fs::metadata("/").unwrap()) == 0
            && fs::read("/proc/self/status")
                .ok()
                .and_then(|s| String::from_utf8(s).ok())
                .is_some_and(|s| s.lines().any(|l| l.starts_with("Uid:\t0\t")))
    }
}
