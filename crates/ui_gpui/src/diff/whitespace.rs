//! Hunks without their whitespace-only changes, ported from T3 Code's
//! `hideWhitespaceChanges`: each hunk's two sides are diffed again with all
//! whitespace taken out of every line, and lines that then match show as
//! unchanged (with the new text). Each side keeps its lines in their
//! order, so line numbers and per-hunk syntax stay valid.

use super::model::{DiffHunk, DiffLine, push_replace_block};
use similar::{Algorithm, ChangeTag, DiffOp, capture_diff_slices};

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// `hunks` with lines that differ only in whitespace shown as unchanged.
pub fn hide_whitespace_changes(hunks: &[DiffHunk]) -> Vec<DiffHunk> {
    hunks.iter().map(hide_in_hunk).collect()
}

fn hide_in_hunk(hunk: &DiffHunk) -> DiffHunk {
    if hunk.lines.iter().all(|line| line.tag == ChangeTag::Equal) {
        return hunk.clone();
    }
    let side = |skip: ChangeTag| -> Vec<&str> {
        hunk.lines
            .iter()
            .filter(|line| line.tag != skip)
            .map(|line| line.text.as_ref())
            .collect()
    };
    let (old, new) = (side(ChangeTag::Insert), side(ChangeTag::Delete));
    let stripped = |lines: &[&str]| lines.iter().map(|l| without_whitespace(l)).collect();
    let (old_stripped, new_stripped): (Vec<String>, Vec<String>) = (stripped(&old), stripped(&new));
    let ops = capture_diff_slices(Algorithm::Myers, &old_stripped, &new_stripped);

    let mut lines = Vec::with_capacity(hunk.lines.len());
    let (mut removed, mut added): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    for op in ops {
        match op {
            DiffOp::Equal { new_index, len, .. } => {
                push_replace_block(&removed, &added, &mut lines);
                (removed, added) = (Vec::new(), Vec::new());
                lines.extend(
                    new[new_index..new_index + len]
                        .iter()
                        .map(|text| DiffLine::plain(ChangeTag::Equal, text)),
                );
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => removed.extend(&old[old_index..old_index + old_len]),
            DiffOp::Insert {
                new_index, new_len, ..
            } => added.extend(&new[new_index..new_index + new_len]),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                removed.extend(&old[old_index..old_index + old_len]);
                added.extend(&new[new_index..new_index + new_len]);
            }
        }
    }
    push_replace_block(&removed, &added, &mut lines);
    DiffHunk {
        old_start: hunk.old_start,
        new_start: hunk.new_start,
        header: hunk.header.clone(),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChangeTag::*;

    fn hunk(lines: &[(ChangeTag, &str)]) -> DiffHunk {
        DiffHunk {
            old_start: 10,
            new_start: 12,
            header: Some("fn main() {".into()),
            lines: lines
                .iter()
                .map(|&(tag, text)| DiffLine::plain(tag, text))
                .collect(),
        }
    }

    fn shape(hunk: &DiffHunk) -> Vec<(ChangeTag, String)> {
        hunk.lines
            .iter()
            .map(|line| (line.tag, line.text.to_string()))
            .collect()
    }

    #[test]
    fn reindented_lines_show_unchanged_with_the_new_text() {
        let hunks = [hunk(&[
            (Equal, "a"),
            (Delete, "foo(1,2)"),
            (Delete, "old()"),
            (Insert, "    foo(1, 2)"),
            (Insert, "bar()"),
        ])];
        let hidden = hide_whitespace_changes(&hunks);
        assert_eq!(
            shape(&hidden[0]),
            [
                (Equal, "a".into()),
                (Equal, "    foo(1, 2)".into()),
                (Delete, "old()".into()),
                (Insert, "bar()".into()),
            ]
        );
        assert_eq!((hidden[0].old_start, hidden[0].new_start), (10, 12));
        assert_eq!(hidden[0].header.as_deref(), Some("fn main() {"));
    }

    #[test]
    fn real_changes_and_unchanged_hunks_stay() {
        let unchanged = hunk(&[(Equal, "a"), (Equal, "b")]);
        assert_eq!(
            shape(&hide_whitespace_changes(std::slice::from_ref(&unchanged))[0]),
            shape(&unchanged)
        );
        let changed = hunk(&[(Delete, "x = 1"), (Insert, "x = 2")]);
        assert_eq!(
            shape(&hide_whitespace_changes(&[changed])[0]),
            [(Delete, "x = 1".into()), (Insert, "x = 2".into())]
        );
    }

    #[test]
    fn respaced_lines_become_context_but_added_lines_stay() {
        let hunks = [hunk(&[
            (Delete, "if x {"),
            (Insert, "if x  {"),
            (Insert, ""),
        ])];
        let hidden = hide_whitespace_changes(&hunks);
        // A blank line added is still a line added.
        assert_eq!(
            shape(&hidden[0]),
            [(Equal, "if x  {".into()), (Insert, "".into())]
        );
    }
}
