//! File search (FC's Alt+F7 / Ctrl+F): walk a tree without following symlinks,
//! filter by name mask, type, size, and modification time, optionally by
//! content, and stream hits to a callback. Cancellable between entries.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::glob::Mask;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Any,
    Files,
    Folders,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub root: PathBuf,
    /// File mask (`*.jpg; *.png`); empty matches everything.
    pub name: String,
    /// Text that must appear in the file; empty disables content search.
    pub content: String,
    pub content_regex: bool,
    pub case_sensitive: bool,
    pub kind: Kind,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<SystemTime>,
    pub modified_before: Option<SystemTime>,
    pub include_hidden: bool,
    /// `None` = unlimited; `Some(0)` = only the root folder itself.
    pub max_depth: Option<usize>,
}

impl Query {
    pub fn new(root: PathBuf) -> Self {
        Query {
            root,
            name: String::new(),
            content: String::new(),
            content_regex: false,
            case_sensitive: false,
            kind: Kind::Any,
            min_size: None,
            max_size: None,
            modified_after: None,
            modified_before: None,
            include_hidden: false,
            max_depth: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// First matching line (number, text) for content searches.
    pub line: Option<(u64, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub scanned: u64,
    pub matched: u64,
    /// Folders that could not be read.
    pub errors: u64,
    pub cancelled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("invalid regular expression: {0}")]
    Regex(#[from] regex::Error),
    #[error("cannot read {0}")]
    Root(PathBuf),
}

/// Bytes sniffed for NUL to skip binaries in content search.
const SNIFF: usize = 8192;
/// Longest line excerpt kept in a hit.
const EXCERPT: usize = 200;

/// Walks `query.root`, calling `on_hit` for every match; `on_hit` returns
/// `false` to stop early. `cancel` is polled between entries.
pub fn run(
    query: &Query,
    cancel: &AtomicBool,
    on_hit: &mut dyn FnMut(Hit) -> bool,
) -> Result<Stats, SearchError> {
    let mask = if query.name.trim().is_empty() {
        None
    } else {
        Some(Mask::parse(&query.name))
    };
    let content = if query.content.is_empty() {
        None
    } else {
        let pattern = if query.content_regex {
            query.content.clone()
        } else {
            regex::escape(&query.content)
        };
        let pattern = if query.case_sensitive {
            pattern
        } else {
            format!("(?i){pattern}")
        };
        Some(regex::Regex::new(&pattern)?)
    };
    if !query.root.is_dir() {
        return Err(SearchError::Root(query.root.clone()));
    }

    let mut stats = Stats::default();
    let mut stack: Vec<(PathBuf, usize)> = vec![(query.root.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            stats.cancelled = true;
            break;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let mut children: Vec<fs::DirEntry> = entries.flatten().collect();
        // Deterministic order helps tests and makes results stable across runs.
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            if cancel.load(Ordering::Relaxed) {
                stats.cancelled = true;
                return Ok(stats);
            }
            let name = entry.file_name();
            let hidden = name.as_encoded_bytes().starts_with(b".");
            if hidden && !query.include_hidden {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            let is_link = meta.file_type().is_symlink();
            // A symlink to a folder counts as a folder but is never descended.
            let is_dir = meta.is_dir() || (is_link && path.is_dir());
            if is_dir && !is_link && query.max_depth.is_none_or(|max| depth < max) {
                stack.push((path.clone(), depth + 1));
            }
            stats.scanned += 1;

            match query.kind {
                Kind::Files if is_dir => continue,
                Kind::Folders if !is_dir => continue,
                _ => {}
            }
            if let Some(mask) = &mask
                && !mask.matches(&name.to_string_lossy())
            {
                continue;
            }
            let size = if is_dir { 0 } else { meta.len() };
            if !is_dir {
                if query.min_size.is_some_and(|min| size < min) {
                    continue;
                }
                if query.max_size.is_some_and(|max| size > max) {
                    continue;
                }
            }
            let modified = meta.modified().ok();
            if let Some(after) = query.modified_after
                && !modified.is_some_and(|m| m >= after)
            {
                continue;
            }
            if let Some(before) = query.modified_before
                && !modified.is_some_and(|m| m <= before)
            {
                continue;
            }
            let line = match &content {
                None => None,
                Some(_) if is_dir => continue,
                Some(re) => match first_match(&path, re) {
                    Some(line) => Some(line),
                    None => continue,
                },
            };
            stats.matched += 1;
            let keep_going = on_hit(Hit {
                path,
                is_dir,
                size,
                modified,
                line,
            });
            if !keep_going {
                stats.cancelled = true;
                return Ok(stats);
            }
        }
    }
    Ok(stats)
}

/// First line of a text file matching `re`; `None` for binaries, unreadable
/// files, or no match.
fn first_match(path: &Path, re: &regex::Regex) -> Option<(u64, String)> {
    let mut file = fs::File::open(path).ok()?;
    let mut head = vec![0u8; SNIFF];
    let n = file.read(&mut head).ok()?;
    if head[..n].contains(&0) {
        return None;
    }
    // Restart from the top so line numbers are right.
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut line_no = 0u64;
    loop {
        buf.clear();
        let read = reader.read_until(b'\n', &mut buf).ok()?;
        if read == 0 {
            return None;
        }
        line_no += 1;
        let text = String::from_utf8_lossy(&buf);
        if re.is_match(&text) {
            let excerpt: String = text.trim_end().chars().take(EXCERPT).collect();
            return Some((line_no, excerpt));
        }
    }
}

/// `YYYY-MM-DD` in local time: the start of that day, or its last second
/// with `end_of_day` (for "modified before").
pub fn parse_date(text: &str, end_of_day: bool) -> Option<SystemTime> {
    use chrono::{NaiveDate, TimeZone};
    let date = NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()?;
    let naive = if end_of_day {
        date.and_hms_opt(23, 59, 59)?
    } else {
        date.and_hms_opt(0, 0, 0)?
    };
    let local = chrono::Local.from_local_datetime(&naive).single()?;
    Some(SystemTime::from(local))
}

/// `"10k"`, `"2.5M"`, `"1g"`, or plain bytes (binary units).
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }
    let (number, unit) = match text.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&text[..i], &text[i..]),
        None => (text.as_str(), ""),
    };
    let value: f64 = number.trim().parse().ok()?;
    let factor: f64 = match unit.trim_end_matches('b').trim_end_matches('i') {
        "" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        "t" => 1024.0f64.powi(4),
        _ => return None,
    };
    if value < 0.0 {
        return None;
    }
    Some((value * factor).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    fn tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("docs/deep")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("docs/readme.md"), "Hello World\nsecond line\n").unwrap();
        fs::write(
            root.join("docs/deep/notes.txt"),
            "nothing here\nhello again\n",
        )
        .unwrap();
        fs::write(root.join("big.bin"), vec![0u8; 5000]).unwrap();
        fs::write(root.join("hello.txt"), "x").unwrap();
        fs::write(root.join(".git/config"), "hello hidden").unwrap();
        symlink("docs", root.join("docs-link")).unwrap();
        tmp
    }

    fn names(query: &Query) -> Vec<String> {
        let mut out = Vec::new();
        run(query, &AtomicBool::new(false), &mut |hit| {
            out.push(
                hit.path
                    .strip_prefix(&query.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            true
        })
        .unwrap();
        out.sort();
        out
    }

    #[test]
    fn name_mask_recurses_without_following_links_or_hidden() {
        let tmp = tree();
        let mut q = Query::new(tmp.path().to_path_buf());
        q.name = "*.txt".into();
        assert_eq!(names(&q), ["docs/deep/notes.txt", "hello.txt"]);
        q.name = "hello".into();
        assert_eq!(names(&q), ["hello.txt"]);
        q.include_hidden = true;
        q.name = "config".into();
        assert_eq!(names(&q), [".git/config"]);
    }

    #[test]
    fn kind_depth_and_size_filters() {
        let tmp = tree();
        let mut q = Query::new(tmp.path().to_path_buf());
        q.kind = Kind::Folders;
        assert_eq!(names(&q), ["docs", "docs-link", "docs/deep"]);
        q.kind = Kind::Files;
        q.max_depth = Some(0);
        assert_eq!(names(&q), ["big.bin", "hello.txt"]);
        q.max_depth = None;
        q.min_size = Some(1000);
        assert_eq!(names(&q), ["big.bin"]);
        q.min_size = None;
        q.max_size = Some(10);
        assert_eq!(names(&q), ["hello.txt"]);
    }

    #[test]
    fn content_search_skips_binaries_and_reports_lines() {
        let tmp = tree();
        let mut q = Query::new(tmp.path().to_path_buf());
        q.content = "hello".into();
        let mut hits = Vec::new();
        let stats = run(&q, &AtomicBool::new(false), &mut |hit| {
            hits.push(hit);
            true
        })
        .unwrap();
        let mut got: Vec<(String, Option<(u64, String)>)> = hits
            .into_iter()
            .map(|h| {
                (
                    h.path.file_name().unwrap().to_string_lossy().into_owned(),
                    h.line,
                )
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("notes.txt".to_owned(), Some((2, "hello again".to_owned()))),
                ("readme.md".to_owned(), Some((1, "Hello World".to_owned()))),
            ]
        );
        assert_eq!(stats.matched, 2);

        q.case_sensitive = true;
        assert_eq!(names(&q), ["docs/deep/notes.txt"]);
        q.content_regex = true;
        q.content = "^Hello W".into();
        q.case_sensitive = true;
        assert_eq!(names(&q), ["docs/readme.md"]);
        q.content = "(".into();
        assert!(matches!(
            run(&q, &AtomicBool::new(false), &mut |_| true),
            Err(SearchError::Regex(_))
        ));
    }

    #[test]
    fn date_filters() {
        let tmp = tree();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        fs::File::open(tmp.path().join("hello.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let mut q = Query::new(tmp.path().to_path_buf());
        q.kind = Kind::Files;
        q.modified_before = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000));
        assert_eq!(names(&q), ["hello.txt"]);
        q.modified_before = None;
        q.modified_after = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000));
        assert!(!names(&q).contains(&"hello.txt".to_owned()));
    }

    #[test]
    fn cancellation_and_early_stop() {
        let tmp = tree();
        let q = Query::new(tmp.path().to_path_buf());
        let cancel = AtomicBool::new(true);
        let stats = run(&q, &cancel, &mut |_| true).unwrap();
        assert!(stats.cancelled);
        assert_eq!(stats.matched, 0);

        let mut count = 0;
        let stats = run(&q, &AtomicBool::new(false), &mut |_| {
            count += 1;
            count < 2
        })
        .unwrap();
        assert_eq!(count, 2);
        assert!(stats.cancelled);
    }

    #[test]
    fn missing_root_is_an_error() {
        let q = Query::new(PathBuf::from("/nonexistent/fc"));
        assert!(matches!(
            run(&q, &AtomicBool::new(false), &mut |_| true),
            Err(SearchError::Root(_))
        ));
    }

    #[test]
    fn dates_parse() {
        let start = parse_date("2026-09-26", false).unwrap();
        let end = parse_date(" 2026-09-26 ", true).unwrap();
        assert_eq!(
            end.duration_since(start).unwrap(),
            Duration::from_secs(23 * 3600 + 59 * 60 + 59)
        );
        assert_eq!(parse_date("26/09/2026", false), None);
        assert_eq!(parse_date("", false), None);
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("10k"), Some(10 * 1024));
        assert_eq!(parse_size("2.5 MiB"), Some(2_621_440));
        assert_eq!(parse_size("1G"), Some(1 << 30));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("lots"), None);
        assert_eq!(parse_size("-3k"), None);
    }
}
