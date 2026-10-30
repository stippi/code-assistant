//! Where GitHub's line positions sit in a file's diff, both ways: a review
//! thread or pending comment at `(side, line)` goes below the row showing
//! that line, if one is rendered (else it is listed apart, "not on the
//! current diff"); a selection of rows becomes the `(side, line)` range a
//! new comment is anchored to. `LEFT` is the old file, `RIGHT` the new one;
//! a context row shows a line of both.

use github::model::DiffSide;
use similar::ChangeTag;
use std::collections::{BTreeMap, HashMap};

/// A line of one side of a diff, as GitHub addresses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineRef {
    pub side: DiffSide,
    pub line: u64,
}

/// A comment's position: one line, or a range from `start` to `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeRef {
    pub start: Option<LineRef>,
    pub end: LineRef,
}

/// The rows of a file's diff by the lines they show (flat line indices,
/// as [`PreparedFileDiff::row_numbers`] counts them).
///
/// [`PreparedFileDiff::row_numbers`]: crate::diff::prepare::PreparedFileDiff::row_numbers
#[derive(Clone, Debug, Default)]
pub struct LineIndex {
    left: HashMap<u64, usize>,
    right: HashMap<u64, usize>,
}

impl LineIndex {
    pub fn new(numbers: &[(ChangeTag, usize, usize)]) -> Self {
        let mut index = Self::default();
        for (flat, &(tag, old, new)) in numbers.iter().enumerate() {
            if tag != ChangeTag::Insert {
                index.left.insert(old as u64, flat);
            }
            if tag != ChangeTag::Delete {
                index.right.insert(new as u64, flat);
            }
        }
        index
    }

    /// The row showing `at`, when the diff has one.
    pub fn row(&self, at: LineRef) -> Option<usize> {
        match at.side {
            DiffSide::Left => self.left.get(&at.line),
            DiffSide::Right => self.right.get(&at.line),
        }
        .copied()
    }
}

/// The line a row stands for: deleted rows the old file's, the others the
/// new file's.
pub fn line_of((tag, old, new): (ChangeTag, usize, usize)) -> LineRef {
    match tag {
        ChangeTag::Delete => LineRef {
            side: DiffSide::Left,
            line: old as u64,
        },
        _ => LineRef {
            side: DiffSide::Right,
            line: new as u64,
        },
    }
}

/// The position of a comment on rows `lo..=hi`: their last row, from their
/// first one when there are several.
pub fn range_of(numbers: &[(ChangeTag, usize, usize)], lo: usize, hi: usize) -> Option<RangeRef> {
    let end = line_of(*numbers.get(hi)?);
    let start = (lo < hi)
        .then(|| numbers.get(lo).copied().map(line_of))
        .flatten();
    Some(RangeRef { start, end })
}

/// What goes below a diff row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Annotation {
    /// A review thread, by index into the pull request's threads.
    Thread(usize),
    /// A comment of the pending review, by id.
    Pending(u64),
    /// The box a new comment is written in.
    Draft,
}

/// Annotations by the flat line they go below, each line's in the order
/// threads, pending comments, the draft; `None` for the ones whose line no
/// row shows.
pub fn place(
    index: &LineIndex,
    items: impl IntoIterator<Item = (LineRef, Annotation)>,
) -> (BTreeMap<usize, Vec<Annotation>>, Vec<Annotation>) {
    let mut placed: BTreeMap<usize, Vec<Annotation>> = BTreeMap::new();
    let mut off = Vec::new();
    for (at, annotation) in items {
        match index.row(at) {
            Some(row) => placed.entry(row).or_default().push(annotation),
            None => off.push(annotation),
        }
    }
    for annotations in placed.values_mut() {
        annotations.sort();
    }
    (placed, off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChangeTag::*;

    fn left(line: u64) -> LineRef {
        LineRef {
            side: DiffSide::Left,
            line,
        }
    }

    fn right(line: u64) -> LineRef {
        LineRef {
            side: DiffSide::Right,
            line,
        }
    }

    /// `@@ -10,3 +10,4 @@`: ctx, -old, +new, +more, ctx.
    fn numbers() -> Vec<(ChangeTag, usize, usize)> {
        vec![
            (Equal, 10, 10),
            (Delete, 11, 11),
            (Insert, 12, 11),
            (Insert, 12, 12),
            (Equal, 12, 13),
        ]
    }

    #[test]
    fn rows_are_found_by_side_and_line() {
        let index = LineIndex::new(&numbers());
        assert_eq!(index.row(left(11)), Some(1));
        assert_eq!(index.row(right(11)), Some(2));
        // A context row shows a line of both sides.
        assert_eq!(index.row(left(12)), Some(4));
        assert_eq!(index.row(right(13)), Some(4));
        assert_eq!(index.row(left(10)), Some(0));
        assert_eq!(index.row(right(14)), None);
        assert_eq!(index.row(left(13)), None);
    }

    #[test]
    fn a_selection_is_anchored_like_github_wants_it() {
        let numbers = numbers();
        // One deleted row: the old file's line.
        assert_eq!(
            range_of(&numbers, 1, 1),
            Some(RangeRef {
                start: None,
                end: left(11)
            })
        );
        // From the deleted row to the context row below.
        assert_eq!(
            range_of(&numbers, 1, 4),
            Some(RangeRef {
                start: Some(left(11)),
                end: right(13)
            })
        );
        assert_eq!(range_of(&numbers, 2, 9), None);
    }

    #[test]
    fn annotations_go_below_their_rows_in_a_fixed_order() {
        let index = LineIndex::new(&numbers());
        let (placed, off) = place(
            &index,
            [
                (right(12), Annotation::Draft),
                (right(12), Annotation::Pending(7)),
                (right(12), Annotation::Thread(3)),
                (left(11), Annotation::Thread(0)),
                (right(40), Annotation::Thread(1)),
            ],
        );
        assert_eq!(
            placed.into_iter().collect::<Vec<_>>(),
            vec![
                (1, vec![Annotation::Thread(0)]),
                (
                    3,
                    vec![
                        Annotation::Thread(3),
                        Annotation::Pending(7),
                        Annotation::Draft
                    ]
                ),
            ]
        );
        assert_eq!(off, [Annotation::Thread(1)]);
    }
}
