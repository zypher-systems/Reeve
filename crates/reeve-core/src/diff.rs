//! Line diffs for approval cards and receipts. Shown to the person, never
//! sent to the model (it knows what it wrote).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// What kind of row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffKind {
    /// Unchanged context.
    Context,
    /// Added.
    Add,
    /// Removed.
    Remove,
    /// Lines skipped between hunks.
    Gap,
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Kind.
    pub kind: DiffKind,
    /// Line number in the new text (old text for removals).
    pub line: Option<usize>,
    /// Text without its newline.
    pub text: String,
}

/// A change to one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// Rows to show, with two lines of context.
    pub lines: Vec<DiffLine>,
    /// More rows existed than were kept.
    pub truncated: bool,
}

/// Diff `old` → `new`, keeping at most `max_rows` rows.
pub fn diff(old: &str, new: &str, max_rows: usize) -> FileDiff {
    let d = TextDiff::configure()
        .deadline(Instant::now() + Duration::from_millis(250))
        .diff_lines(old, new);
    let mut out = FileDiff::default();
    for (gi, group) in d.grouped_ops(2).iter().enumerate() {
        if gi > 0 {
            out.lines.push(DiffLine {
                kind: DiffKind::Gap,
                line: None,
                text: String::new(),
            });
        }
        for op in group {
            for change in d.iter_changes(op) {
                let (kind, line) = match change.tag() {
                    ChangeTag::Equal => (DiffKind::Context, change.new_index()),
                    ChangeTag::Insert => {
                        out.added += 1;
                        (DiffKind::Add, change.new_index())
                    }
                    ChangeTag::Delete => {
                        out.removed += 1;
                        (DiffKind::Remove, change.old_index())
                    }
                };
                if out.lines.len() < max_rows {
                    out.lines.push(DiffLine {
                        kind,
                        line: line.map(|i| i + 1),
                        text: change.value().trim_end_matches(['\n', '\r']).to_string(),
                    });
                } else {
                    out.truncated = true;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\n";
        let new = "a\nb\nC\nd\ne\nf\ng\nh\n";
        let d = diff(old, new, 100);
        assert_eq!((d.added, d.removed), (2, 1));
        assert!(
            d.lines
                .iter()
                .any(|l| l.kind == DiffKind::Add && l.text == "C" && l.line == Some(3))
        );
        assert!(
            d.lines
                .iter()
                .any(|l| l.kind == DiffKind::Remove && l.text == "c")
        );
        let short = diff(old, new, 2);
        assert!(short.truncated && short.lines.len() == 2);
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let d = diff("", "x\ny\n", 10);
        assert_eq!((d.added, d.removed), (2, 0));
    }
}
