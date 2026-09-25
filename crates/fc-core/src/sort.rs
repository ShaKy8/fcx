//! Name ordering for file listings.

use std::cmp::Ordering;
use std::ffi::OsStr;

/// Precomputed sort key for a filename: lossy UTF-8, lowercased.
/// Build it once per entry so sorting large directories doesn't allocate per comparison.
pub fn name_key(name: &OsStr) -> String {
    name.to_string_lossy().to_lowercase()
}

/// Natural ("human") ordering: digit runs compare by numeric value, so
/// `file2` < `file10`. Callers pass keys from [`name_key`] for case-insensitivity.
/// Ties on numeric value (`007` vs `7`) fall back to the shorter run first.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let (da, ra) = split_digits(a);
                let (db, rb) = split_digits(b);
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let ord = ta
                    .len()
                    .cmp(&tb.len())
                    .then_with(|| ta.cmp(tb))
                    .then_with(|| da.len().cmp(&db.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
                (a, b) = (ra, rb);
            }
            (Some(ca), Some(cb)) => {
                if ca != cb {
                    return ca.cmp(&cb);
                }
                (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
            }
        }
    }
}

fn split_digits(s: &str) -> (&str, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut keys: Vec<String> = names.iter().map(|n| name_key(OsStr::new(n))).collect();
        keys.sort_by(|a, b| natural_cmp(a, b));
        keys
    }

    #[test]
    fn numbers_sort_by_value() {
        assert_eq!(
            sorted(&["file10", "file2", "file1", "file20"]),
            ["file1", "file2", "file10", "file20"]
        );
    }

    #[test]
    fn case_insensitive_via_key() {
        assert_eq!(
            sorted(&["Beta", "alpha", "Gamma"]),
            ["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn leading_zeros_and_prefixes() {
        assert_eq!(natural_cmp("7", "007"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
        assert_eq!(natural_cmp("img99.png", "img100.png"), Ordering::Less);
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
    }

    #[test]
    fn huge_digit_runs_do_not_overflow() {
        assert_eq!(
            natural_cmp("v99999999999999999999999", "v100000000000000000000000"),
            Ordering::Less
        );
    }
}
