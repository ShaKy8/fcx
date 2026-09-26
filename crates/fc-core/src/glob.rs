//! FreeCommander-style file masks for "select by pattern": `*` and `?`
//! wildcards, several masks separated by `;` or whitespace, case-insensitive.
//! A mask without a wildcard matches as a substring (`txt` finds `notes.txt`).

/// A parsed mask list such as `*.jpg; *.png`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    patterns: Vec<Vec<char>>,
}

impl Mask {
    pub fn parse(text: &str) -> Mask {
        let patterns = text
            .split([';', ' ', '\t'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| {
                let has_wildcard = p.contains(['*', '?']);
                let lower = p.to_lowercase();
                if has_wildcard {
                    lower.chars().collect()
                } else {
                    format!("*{lower}*").chars().collect()
                }
            })
            .collect();
        Mask { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.to_lowercase().chars().collect();
        self.patterns.iter().any(|p| wildcard(p, &name))
    }
}

/// Iterative `*`/`?` matcher with backtracking on the last `*` — linear in
/// practice and immune to the exponential blowup of the naive recursion.
fn wildcard(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(&c) if c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, name: &str) -> bool {
        Mask::parse(pattern).matches(name)
    }

    #[test]
    fn wildcards() {
        assert!(m("*.txt", "notes.txt"));
        assert!(m("*.TXT", "Notes.txt"));
        assert!(!m("*.txt", "notes.txt.bak"));
        assert!(m("img_????.jpg", "img_0042.jpg"));
        assert!(!m("img_????.jpg", "img_042.jpg"));
        assert!(m("*", "anything"));
        assert!(m("a*b*c", "aXXbYYc"));
        assert!(!m("a*b*c", "aXXbYY"));
        assert!(m("*.tar.*", "backup.tar.gz"));
    }

    #[test]
    fn plain_text_is_a_substring_search() {
        assert!(m("report", "Q3 Report.pdf"));
        assert!(!m("report", "summary.pdf"));
    }

    #[test]
    fn several_masks() {
        let mask = Mask::parse("*.jpg; *.png  *.gif");
        assert!(mask.matches("a.jpg") && mask.matches("b.png") && mask.matches("c.gif"));
        assert!(!mask.matches("d.bmp"));
        assert!(Mask::parse("  ; ").is_empty());
    }

    #[test]
    fn unicode_and_pathological_input() {
        assert!(m("*é*", "café.txt"));
        let long = "a".repeat(5000);
        assert!(!m("*a*a*a*a*a*a*a*a*b", &long));
    }
}
