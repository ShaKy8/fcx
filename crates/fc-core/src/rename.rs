//! Multi-rename engine (FreeCommander's Ctrl+M tool).
//!
//! [`plan`] is pure: masks, search/replace, case, and counter turn a list of
//! source names into new names and a list of problems (collisions, empty or
//! invalid names). [`execute`] applies a plan on disk in two phases through
//! temporary names, so swaps and cycles (`a→b`, `b→a`) work, and it refuses to
//! start if any target already exists on disk that is not itself being renamed
//! away. It returns what it did so the caller can undo with [`undo_plan`].
//!
//! Mask tokens: `[N]` name without extension, `[N2-5]` / `[N3-]` / `[N-3]` /
//! `[N3]` character ranges (1-based, inclusive), `[E]` extension (same ranges),
//! `[C]` counter, `[P]` parent folder name, `[Y]` `[M]` `[D]` `[h]` `[m]` `[s]`
//! from the modification time. Anything else in brackets is kept literally.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, Timelike};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMode {
    #[default]
    Unchanged,
    Lower,
    Upper,
    /// First character upper, rest lower.
    FirstUpper,
    /// Every word capitalised.
    Title,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameSpec {
    pub name_mask: String,
    pub ext_mask: String,
    pub search: String,
    pub replace: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub case: CaseMode,
    pub counter_start: i64,
    pub counter_step: i64,
    pub counter_digits: usize,
}

impl Default for RenameSpec {
    fn default() -> Self {
        RenameSpec {
            name_mask: "[N]".into(),
            ext_mask: "[E]".into(),
            search: String::new(),
            replace: String::new(),
            use_regex: false,
            case_sensitive: false,
            case: CaseMode::Unchanged,
            counter_start: 1,
            counter_step: 1,
            counter_digits: 1,
        }
    }
}

/// One file to rename, as the UI knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Exact on-disk name.
    pub name: OsString,
    pub is_dir: bool,
    pub modified: Option<SystemTime>,
    pub parent_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub old: OsString,
    pub new: String,
    pub problem: Option<Problem>,
}

impl Planned {
    pub fn changes(&self) -> bool {
        self.problem.is_none() && self.old.as_os_str() != self.new.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    Empty,
    /// Contains `/` or is `.` / `..`.
    Invalid,
    /// Another item in the plan gets the same name.
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<Planned>,
}

impl Plan {
    pub fn changing(&self) -> usize {
        self.items.iter().filter(|p| p.changes()).count()
    }

    pub fn problems(&self) -> usize {
        self.items.iter().filter(|p| p.problem.is_some()).count()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    #[error("invalid regular expression: {0}")]
    Regex(#[from] regex::Error),
    #[error("{0} already exists in the folder")]
    TargetExists(String),
    #[error("plan has unresolved problems")]
    Problems,
    #[error("renaming {from:?} → {to:?}: {source}")]
    Io {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Compute new names. Fails only if the regex does not compile.
pub fn plan(spec: &RenameSpec, sources: &[Source]) -> Result<Plan, RenameError> {
    let search = if spec.search.is_empty() {
        None
    } else if spec.use_regex {
        let pattern = if spec.case_sensitive {
            spec.search.clone()
        } else {
            format!("(?i){}", spec.search)
        };
        Some(regex::Regex::new(&pattern)?)
    } else {
        let escaped = regex::escape(&spec.search);
        let pattern = if spec.case_sensitive {
            escaped
        } else {
            format!("(?i){escaped}")
        };
        Some(regex::Regex::new(&pattern)?)
    };
    // Plain-text replacement must not interpret `$1`; regex mode keeps it.
    let replacement: String = if spec.use_regex {
        spec.replace.clone()
    } else {
        spec.replace.replace('$', "$$")
    };

    let mut items: Vec<Planned> = sources
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let display = src.name.to_string_lossy();
            let (stem, ext) = split_ext(&display, src.is_dir);
            let counter = spec.counter_start + spec.counter_step * i as i64;
            let ctx = Context {
                stem,
                ext,
                counter,
                digits: spec.counter_digits,
                parent: &src.parent_name,
                time: src.modified.map(DateTime::<Local>::from),
            };
            let mut name = expand(&spec.name_mask, &ctx);
            let ext_part = expand(&spec.ext_mask, &ctx);
            if !ext_part.is_empty() {
                name.push('.');
                name.push_str(&ext_part);
            }
            if let Some(re) = &search {
                name = re.replace_all(&name, replacement.as_str()).into_owned();
            }
            name = apply_case(&name, spec.case);
            let problem = if name.is_empty() {
                Some(Problem::Empty)
            } else if name.contains('/') || name == "." || name == ".." {
                Some(Problem::Invalid)
            } else {
                None
            };
            Planned {
                old: src.name.clone(),
                new: name,
                problem,
            }
        })
        .collect();

    // Collisions: two items ending with the same name, or one item taking the
    // unchanged name of another. Names are compared exactly (Linux is case-sensitive).
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut collide = vec![false; items.len()];
    for (i, item) in items.iter().enumerate() {
        if item.problem.is_some() {
            continue;
        }
        if let Some(&first) = seen.get(item.new.as_str()) {
            collide[i] = true;
            collide[first] = true;
        } else {
            seen.insert(item.new.as_str(), i);
        }
    }
    for (item, hit) in items.iter_mut().zip(collide) {
        if hit {
            item.problem = Some(Problem::Collision);
        }
    }
    Ok(Plan { items })
}

/// Apply `plan` inside `dir`. Every changing item is first moved to a unique
/// temporary name, then to its final name, so cycles resolve. Returns the
/// `(old, new)` pairs actually renamed, in order.
pub fn execute(dir: &Path, plan: &Plan) -> Result<Vec<(OsString, String)>, RenameError> {
    if plan.problems() > 0 {
        return Err(RenameError::Problems);
    }
    let changing: Vec<&Planned> = plan.items.iter().filter(|p| p.changes()).collect();
    let vacating: std::collections::HashSet<&std::ffi::OsStr> =
        changing.iter().map(|p| p.old.as_os_str()).collect();
    for item in &changing {
        let target = dir.join(&item.new);
        if std::fs::symlink_metadata(&target).is_ok()
            && !vacating.contains(Path::new(&item.new).as_os_str())
        {
            return Err(RenameError::TargetExists(item.new.clone()));
        }
    }

    let pid = std::process::id();
    let mut temps: Vec<(PathBuf, &Planned)> = Vec::with_capacity(changing.len());
    for (i, item) in changing.iter().enumerate() {
        let mut temp_name = OsString::from(format!(".fc-rename-{pid}-{i}-"));
        temp_name.push(&item.old);
        let temp = dir.join(temp_name);
        rename_io(&dir.join(&item.old), &temp)?;
        temps.push((temp, item));
    }
    let mut applied = Vec::with_capacity(temps.len());
    for (temp, item) in temps {
        rename_io(&temp, &dir.join(&item.new))?;
        applied.push((item.old.clone(), item.new.clone()));
    }
    Ok(applied)
}

/// The inverse of an `execute` result, ready for `execute` again.
pub fn undo_plan(applied: &[(OsString, String)]) -> Plan {
    Plan {
        items: applied
            .iter()
            .map(|(old, new)| Planned {
                old: OsString::from(new),
                new: old.to_string_lossy().into_owned(),
                problem: None,
            })
            .collect(),
    }
}

fn rename_io(from: &Path, to: &Path) -> Result<(), RenameError> {
    std::fs::rename(from, to).map_err(|source| RenameError::Io {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        source,
    })
}

// ---- mask expansion ----------------------------------------------------------

struct Context<'a> {
    stem: &'a str,
    ext: &'a str,
    counter: i64,
    digits: usize,
    parent: &'a str,
    time: Option<DateTime<Local>>,
}

/// Folders and dotfiles have no extension; `a.tar.gz` splits at the last dot.
fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn expand(mask: &str, ctx: &Context) -> String {
    let mut out = String::new();
    let mut rest = mask;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find(']') {
            Some(end) => {
                let token = &after[..end];
                match expand_token(token, ctx) {
                    Some(text) => out.push_str(&text),
                    None => {
                        out.push('[');
                        out.push_str(token);
                        out.push(']');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn expand_token(token: &str, ctx: &Context) -> Option<String> {
    if let Some(range) = token.strip_prefix('N') {
        return slice(ctx.stem, range);
    }
    if let Some(range) = token.strip_prefix('E') {
        return slice(ctx.ext, range);
    }
    match token {
        "C" => Some(if ctx.digits > 1 {
            format!("{:0width$}", ctx.counter, width = ctx.digits)
        } else {
            ctx.counter.to_string()
        }),
        "P" => Some(ctx.parent.to_owned()),
        "Y" => ctx.time.map(|t| format!("{:04}", t.year())),
        "M" => ctx.time.map(|t| format!("{:02}", t.month())),
        "D" => ctx.time.map(|t| format!("{:02}", t.day())),
        "h" => ctx.time.map(|t| format!("{:02}", t.hour())),
        "m" => ctx.time.map(|t| format!("{:02}", t.minute())),
        "s" => ctx.time.map(|t| format!("{:02}", t.second())),
        _ => None,
    }
}

/// `""` whole, `"2-5"`, `"3-"`, `"-3"` (= 1-3), `"3"` single char; 1-based, inclusive.
fn slice(text: &str, range: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if range.is_empty() {
        return Some(text.to_owned());
    }
    let (from, to) = match range.split_once('-') {
        Some((a, b)) => {
            let from = if a.is_empty() {
                1
            } else {
                a.parse::<usize>().ok()?
            };
            let to = if b.is_empty() {
                chars.len()
            } else {
                b.parse::<usize>().ok()?
            };
            (from, to)
        }
        None => {
            let n = range.parse::<usize>().ok()?;
            (n, n)
        }
    };
    if from == 0 || from > to {
        return Some(String::new());
    }
    let end = to.min(chars.len());
    if from > end {
        return Some(String::new());
    }
    Some(chars[from - 1..end].iter().collect())
}

fn apply_case(name: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Unchanged => name.to_owned(),
        CaseMode::Lower => name.to_lowercase(),
        CaseMode::Upper => name.to_uppercase(),
        CaseMode::FirstUpper => {
            let mut chars = name.chars();
            match chars.next() {
                Some(c) => c
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        }
        CaseMode::Title => {
            let mut out = String::with_capacity(name.len());
            let mut at_word_start = true;
            for c in name.chars() {
                if c.is_alphanumeric() {
                    if at_word_start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    at_word_start = false;
                } else {
                    out.push(c);
                    at_word_start = true;
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn src(name: &str) -> Source {
        Source {
            name: OsString::from(name),
            is_dir: false,
            modified: None,
            parent_name: "Photos".into(),
        }
    }

    fn names(spec: &RenameSpec, sources: &[&str]) -> Vec<String> {
        let sources: Vec<Source> = sources.iter().map(|s| src(s)).collect();
        plan(spec, &sources)
            .unwrap()
            .items
            .into_iter()
            .map(|p| p.new)
            .collect()
    }

    #[test]
    fn identity_mask_keeps_names() {
        let spec = RenameSpec::default();
        assert_eq!(
            names(&spec, &["a.txt", "archive.tar.gz", ".bashrc"]),
            ["a.txt", "archive.tar.gz", ".bashrc"]
        );
    }

    #[test]
    fn counter_parent_and_ranges() {
        let spec = RenameSpec {
            name_mask: "[P]_[C]_[N1-3]".into(),
            counter_start: 10,
            counter_step: 5,
            counter_digits: 3,
            ..Default::default()
        };
        assert_eq!(
            names(&spec, &["holiday.jpg", "beach.png"]),
            ["Photos_010_hol.jpg", "Photos_015_bea.png"]
        );
        let spec = RenameSpec {
            name_mask: "[N3-]-[N-2]-[N2]".into(),
            ext_mask: "[E1-1]".into(),
            ..Default::default()
        };
        assert_eq!(names(&spec, &["abcdef.png"]), ["cdef-ab-b.p"]);
    }

    #[test]
    fn unknown_tokens_and_unclosed_brackets_stay_literal() {
        let spec = RenameSpec {
            name_mask: "[X]-[N]-[".into(),
            ..Default::default()
        };
        assert_eq!(names(&spec, &["f.txt"]), ["[X]-f-[.txt"]);
    }

    #[test]
    fn date_tokens_use_modified_time() {
        let modified = DateTime::parse_from_rfc3339("2026-09-26T09:05:07+00:00").unwrap();
        let sources = [Source {
            modified: Some(modified.into()),
            ..src("shot.jpg")
        }];
        let spec = RenameSpec {
            name_mask: "[Y]-[M]-[D]".into(),
            ..Default::default()
        };
        let planned = plan(&spec, &sources).unwrap();
        let local: DateTime<Local> = modified.into();
        assert_eq!(
            planned.items[0].new,
            format!(
                "{:04}-{:02}-{:02}.jpg",
                local.year(),
                local.month(),
                local.day()
            )
        );
    }

    #[test]
    fn search_replace_plain_and_regex() {
        let spec = RenameSpec {
            search: "IMG_".into(),
            replace: "trip $1 ".into(),
            ..Default::default()
        };
        assert_eq!(names(&spec, &["img_001.jpg"]), ["trip $1 001.jpg"]);

        let spec = RenameSpec {
            search: r"^IMG_(\d+)".into(),
            replace: "trip-$1".into(),
            use_regex: true,
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(
            names(&spec, &["IMG_001.jpg", "img_002.jpg"]),
            ["trip-001.jpg", "img_002.jpg"]
        );

        let bad = RenameSpec {
            search: "(".into(),
            use_regex: true,
            ..Default::default()
        };
        assert!(matches!(
            plan(&bad, &[src("a")]),
            Err(RenameError::Regex(_))
        ));
    }

    #[test]
    fn case_modes() {
        let make = |case| RenameSpec {
            case,
            ..Default::default()
        };
        let n = "my HOLIDAY pics.JPG";
        assert_eq!(names(&make(CaseMode::Lower), &[n]), ["my holiday pics.jpg"]);
        assert_eq!(names(&make(CaseMode::Upper), &[n]), ["MY HOLIDAY PICS.JPG"]);
        assert_eq!(
            names(&make(CaseMode::FirstUpper), &[n]),
            ["My holiday pics.jpg"]
        );
        assert_eq!(names(&make(CaseMode::Title), &[n]), ["My Holiday Pics.Jpg"]);
    }

    #[test]
    fn detects_collisions_and_bad_names() {
        let spec = RenameSpec {
            name_mask: "same".into(),
            ..Default::default()
        };
        let planned = plan(&spec, &[src("a.txt"), src("b.txt"), src("c.md")]).unwrap();
        assert_eq!(planned.items[0].problem, Some(Problem::Collision));
        assert_eq!(planned.items[1].problem, Some(Problem::Collision));
        assert_eq!(planned.items[2].problem, None);
        assert_eq!(planned.problems(), 2);

        let spec = RenameSpec {
            name_mask: "".into(),
            ext_mask: "".into(),
            ..Default::default()
        };
        assert_eq!(
            plan(&spec, &[src("a")]).unwrap().items[0].problem,
            Some(Problem::Empty)
        );
        let spec = RenameSpec {
            name_mask: "x/y".into(),
            ..Default::default()
        };
        assert_eq!(
            plan(&spec, &[src("a")]).unwrap().items[0].problem,
            Some(Problem::Invalid)
        );
    }

    #[test]
    fn execute_swaps_cycles_and_undoes() {
        let tmp = tempfile::tempdir().unwrap();
        for (n, body) in [("a", "A"), ("b", "B"), ("c", "C")] {
            fs::write(tmp.path().join(n), body).unwrap();
        }
        // a→b, b→c, c→a
        let plan = Plan {
            items: vec![
                Planned {
                    old: "a".into(),
                    new: "b".into(),
                    problem: None,
                },
                Planned {
                    old: "b".into(),
                    new: "c".into(),
                    problem: None,
                },
                Planned {
                    old: "c".into(),
                    new: "a".into(),
                    problem: None,
                },
            ],
        };
        let applied = execute(tmp.path(), &plan).unwrap();
        assert_eq!(applied.len(), 3);
        assert_eq!(fs::read(tmp.path().join("b")).unwrap(), b"A");
        assert_eq!(fs::read(tmp.path().join("c")).unwrap(), b"B");
        assert_eq!(fs::read(tmp.path().join("a")).unwrap(), b"C");
        assert!(
            fs::read_dir(tmp.path()).unwrap().count() == 3,
            "no temp files left"
        );

        execute(tmp.path(), &undo_plan(&applied)).unwrap();
        assert_eq!(fs::read(tmp.path().join("a")).unwrap(), b"A");
        assert_eq!(fs::read(tmp.path().join("c")).unwrap(), b"C");
    }

    #[test]
    fn execute_refuses_existing_targets_and_problems() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a"), "").unwrap();
        fs::write(tmp.path().join("taken"), "").unwrap();
        let plan = Plan {
            items: vec![Planned {
                old: "a".into(),
                new: "taken".into(),
                problem: None,
            }],
        };
        assert!(matches!(
            execute(tmp.path(), &plan),
            Err(RenameError::TargetExists(_))
        ));
        assert!(tmp.path().join("a").exists());

        let plan = Plan {
            items: vec![Planned {
                old: "a".into(),
                new: "".into(),
                problem: Some(Problem::Empty),
            }],
        };
        assert!(matches!(
            execute(tmp.path(), &plan),
            Err(RenameError::Problems)
        ));
    }
}
