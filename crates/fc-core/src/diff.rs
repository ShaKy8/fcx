//! Text comparison for FC's "compare files" (Ctrl+Alt+V): a unified diff as
//! tagged lines the viewer can colour.

use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Header,
    Equal,
    Delete,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    pub inserted: usize,
    pub deleted: usize,
}

/// Unified diff of `left` → `right` with `context` unchanged lines around each
/// change. Identical inputs give no lines at all.
pub fn unified(
    left_name: &str,
    left: &str,
    right_name: &str,
    right: &str,
    context: usize,
) -> (Vec<Line>, Summary) {
    let diff = TextDiff::from_lines(left, right);
    let mut lines = Vec::new();
    let mut summary = Summary::default();
    if diff.ratio() >= 1.0 && left == right {
        return (lines, summary);
    }
    lines.push(Line {
        kind: LineKind::Header,
        text: format!("--- {left_name}"),
    });
    lines.push(Line {
        kind: LineKind::Header,
        text: format!("+++ {right_name}"),
    });
    for (i, group) in diff.grouped_ops(context).iter().enumerate() {
        if i > 0 {
            lines.push(Line {
                kind: LineKind::Header,
                text: "…".into(),
            });
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let (kind, sign) = match change.tag() {
                    ChangeTag::Equal => (LineKind::Equal, ' '),
                    ChangeTag::Delete => {
                        summary.deleted += 1;
                        (LineKind::Delete, '-')
                    }
                    ChangeTag::Insert => {
                        summary.inserted += 1;
                        (LineKind::Insert, '+')
                    }
                };
                let text = change.value().trim_end_matches('\n');
                lines.push(Line {
                    kind,
                    text: format!("{sign}{text}"),
                });
            }
        }
    }
    (lines, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_empty() {
        let (lines, summary) = unified("a", "x\ny\n", "b", "x\ny\n", 3);
        assert!(lines.is_empty());
        assert_eq!(summary, Summary::default());
    }

    #[test]
    fn reports_changes_with_context() {
        let left = "one\ntwo\nthree\nfour\nfive\n";
        let right = "one\n2\nthree\nfour\nfive\nsix\n";
        let (lines, summary) = unified("l", left, "r", right, 1);
        assert_eq!(
            summary,
            Summary {
                inserted: 2,
                deleted: 1
            }
        );
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts[0], "--- l");
        assert_eq!(texts[1], "+++ r");
        assert!(texts.contains(&"-two"));
        assert!(texts.contains(&"+2"));
        assert!(texts.contains(&"+six"));
        assert!(texts.contains(&" three"), "context kept: {texts:?}");
        assert_eq!(lines[2].kind, LineKind::Equal);
    }
}
