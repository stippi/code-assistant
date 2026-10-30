//! Hunks side by side: the old file's lines on the left, the new file's on
//! the right. Inside a hunk, a run of removed lines and the run of added
//! lines right after it are paired line by line; the longer run's rest
//! faces an empty cell. Context lines show on both sides.
//!
//! A split row stands for one or two flat lines (the line indices the
//! stacked layout counts), so the chunks of a split diff are cut by rows:
//! a chunk ends after the row holding a break, so whatever goes below a
//! line (a review thread) goes below the row showing it.

use super::model::DiffHunk;
use similar::ChangeTag;
use std::ops::Range;

/// One row: indices into its hunk's lines, `None` for an empty cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
}

/// The rows of one hunk's lines (by their tags), paired.
pub fn pair_lines(tags: impl IntoIterator<Item = ChangeTag>) -> Vec<SplitRow> {
    let tags: Vec<ChangeTag> = tags.into_iter().collect();
    let run = |from: usize, tag: ChangeTag| {
        from..tags[from..]
            .iter()
            .position(|&t| t != tag)
            .map_or(tags.len(), |len| from + len)
    };
    let mut rows = Vec::new();
    let mut ix = 0;
    while ix < tags.len() {
        if tags[ix] == ChangeTag::Equal {
            rows.push(SplitRow {
                left: Some(ix),
                right: Some(ix),
            });
            ix += 1;
            continue;
        }
        let removed = run(ix, ChangeTag::Delete);
        let added = run(removed.end, ChangeTag::Insert);
        ix = added.end;
        for k in 0..removed.len().max(added.len()) {
            rows.push(SplitRow {
                left: removed.clone().nth(k),
                right: added.clone().nth(k),
            });
        }
    }
    rows
}

/// A slice of one hunk's split rows: an item of a virtualized list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitChunk {
    pub hunk: usize,
    pub rows: Range<usize>,
}

/// A file's hunks side by side, cut into chunks.
#[derive(Clone, Debug, Default)]
pub struct SplitHunks {
    /// Per hunk, its rows.
    pub rows: Vec<Vec<SplitRow>>,
    /// Per hunk, the flat index of its first line.
    pub flat_starts: Vec<usize>,
    pub chunks: Vec<SplitChunk>,
}

impl SplitHunks {
    /// Pair `hunks` and cut them into chunks of at most `max_rows` rows,
    /// ending one after every row showing a flat line in `breaks`.
    pub fn new(hunks: &[DiffHunk], max_rows: usize, breaks: &[usize]) -> Self {
        let max_rows = max_rows.max(1);
        let mut split = Self::default();
        let mut flat = 0;
        for (hunk_ix, hunk) in hunks.iter().enumerate() {
            let rows = pair_lines(hunk.lines.iter().map(|line| line.tag));
            let mut start = 0;
            for (ix, row) in rows.iter().enumerate() {
                let breaks_here = [row.left, row.right]
                    .into_iter()
                    .flatten()
                    .any(|line| breaks.contains(&(flat + line)));
                if breaks_here || ix + 1 - start == max_rows || ix + 1 == rows.len() {
                    split.chunks.push(SplitChunk {
                        hunk: hunk_ix,
                        rows: start..ix + 1,
                    });
                    start = ix + 1;
                }
            }
            split.flat_starts.push(flat);
            split.rows.push(rows);
            flat += hunk.lines.len();
        }
        split
    }

    /// The rows of `chunk`.
    pub fn chunk_rows(&self, chunk: &SplitChunk) -> &[SplitRow] {
        self.rows
            .get(chunk.hunk)
            .and_then(|rows| rows.get(chunk.rows.clone()))
            .unwrap_or_default()
    }

    /// The flat lines `chunk`'s last row shows: what goes below it.
    pub fn last_lines(&self, chunk: &SplitChunk) -> Vec<usize> {
        let flat = self.flat_starts.get(chunk.hunk).copied().unwrap_or(0);
        let Some(row) = self.chunk_rows(chunk).last() else {
            return Vec::new();
        };
        let mut lines: Vec<usize> = [row.left, row.right]
            .into_iter()
            .flatten()
            .map(|line| flat + line)
            .collect();
        lines.dedup();
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::model::DiffLine;
    use ChangeTag::*;

    fn row(left: Option<usize>, right: Option<usize>) -> SplitRow {
        SplitRow { left, right }
    }

    #[test]
    fn removed_and_added_runs_face_each_other() {
        assert_eq!(
            pair_lines([Equal, Delete, Delete, Insert, Equal]),
            [
                row(Some(0), Some(0)),
                row(Some(1), Some(3)),
                row(Some(2), None),
                row(Some(4), Some(4)),
            ]
        );
        assert_eq!(
            pair_lines([Delete, Insert, Insert, Insert]),
            [
                row(Some(0), Some(1)),
                row(None, Some(2)),
                row(None, Some(3)),
            ]
        );
    }

    #[test]
    fn lone_runs_face_empty_cells() {
        assert_eq!(
            pair_lines([Insert, Insert]),
            [row(None, Some(0)), row(None, Some(1))]
        );
        assert_eq!(
            pair_lines([Delete, Equal, Delete]),
            [
                row(Some(0), None),
                row(Some(1), Some(1)),
                row(Some(2), None)
            ]
        );
    }

    #[test]
    fn removed_lines_after_added_ones_start_a_new_block() {
        assert_eq!(
            pair_lines([Delete, Insert, Delete, Insert, Insert]),
            [
                row(Some(0), Some(1)),
                row(Some(2), Some(3)),
                row(None, Some(4)),
            ]
        );
    }

    fn hunk(tags: &[ChangeTag]) -> DiffHunk {
        DiffHunk {
            old_start: 1,
            new_start: 1,
            header: None,
            lines: tags.iter().map(|&tag| DiffLine::plain(tag, "x")).collect(),
        }
    }

    #[test]
    fn chunks_are_cut_by_rows_and_end_at_breaks() {
        let hunks = [
            hunk(&[Delete, Delete, Delete, Insert, Insert, Insert]),
            hunk(&[Equal, Equal, Equal]),
        ];
        // A thread on the first removed line: below the row pairing it.
        let split = SplitHunks::new(&hunks, 2, &[0]);
        let shape: Vec<_> = split
            .chunks
            .iter()
            .map(|c| (c.hunk, c.rows.clone()))
            .collect();
        assert_eq!(shape, [(0, 0..1), (0, 1..3), (1, 0..2), (1, 2..3)]);
        assert_eq!(split.flat_starts, [0, 6]);
        assert_eq!(split.last_lines(&split.chunks[0]), [0, 3]);
        assert_eq!(split.last_lines(&split.chunks[3]), [8]);
        assert_eq!(
            split.chunk_rows(&split.chunks[1]),
            [row(Some(1), Some(4)), row(Some(2), Some(5))]
        );
    }
}
