//! Hunks cut into chunks: the items of a virtualized diff list.

use super::model::DiffHunk;
use similar::ChangeTag;

/// A slice of one hunk, small enough to be a cheap item of a virtualized
/// list. `old_start` / `new_start` are the old- and new-file line numbers at
/// the slice's first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffChunk {
    pub hunk: usize,
    pub lines: std::ops::Range<usize>,
    /// Flat index of the chunk's first line, counting all lines of all hunks
    /// in order — the index line selection and break points use.
    pub flat_start: usize,
    pub old_start: usize,
    pub new_start: usize,
}

impl DiffChunk {
    /// Whether this chunk opens a hunk other than the first — the place for
    /// the "⋯" separator.
    pub fn starts_later_hunk(&self) -> bool {
        self.hunk > 0 && self.lines.start == 0
    }
}

/// Hunks cut into list-item-sized chunks, plus the gutter widths (in digits)
/// all chunks of the file share.
#[derive(Debug, Clone, Default)]
pub struct ChunkedHunks {
    pub chunks: Vec<DiffChunk>,
    pub old_width: usize,
    pub new_width: usize,
}

/// Cut `hunks` into chunks of at most `max_lines` lines. Small hunks stay
/// whole; a big one (a newly added file is a single hunk) is split so a list
/// only builds the chunks in view. A chunk also ends after every flat line
/// index in `breaks`, so the caller can put rows of its own (a review
/// thread) directly below that line.
pub fn chunk_hunks(hunks: &[DiffHunk], max_lines: usize, breaks: &[usize]) -> ChunkedHunks {
    let max_lines = max_lines.max(1);
    let mut breaks = breaks.to_vec();
    breaks.sort_unstable();
    let mut breaks = breaks.into_iter().peekable();
    let mut chunks = Vec::new();
    let (mut max_old, mut max_new) = (1, 1);
    let mut flat = 0;
    for (hunk_ix, hunk) in hunks.iter().enumerate() {
        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;
        let mut start = 0;
        while start < hunk.lines.len() {
            let mut end = (start + max_lines).min(hunk.lines.len());
            while breaks.next_if(|&b| b < flat + start).is_some() {}
            if let Some(&b) = breaks.peek()
                && b < flat + end
            {
                end = b - flat + 1;
            }
            chunks.push(DiffChunk {
                hunk: hunk_ix,
                lines: start..end,
                flat_start: flat + start,
                old_start: old_line,
                new_start: new_line,
            });
            let lines = &hunk.lines[start..end];
            old_line += lines.iter().filter(|l| l.tag != ChangeTag::Insert).count();
            new_line += lines.iter().filter(|l| l.tag != ChangeTag::Delete).count();
            start = end;
        }
        flat += hunk.lines.len();
        max_old = max_old.max(old_line);
        max_new = max_new.max(new_line);
    }
    ChunkedHunks {
        chunks,
        old_width: max_old.to_string().len(),
        new_width: max_new.to_string().len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::model::DiffLine;

    fn hunk_of(old_start: usize, new_start: usize, tags: &[ChangeTag]) -> DiffHunk {
        DiffHunk {
            old_start,
            new_start,
            header: None,
            lines: tags.iter().map(|&tag| DiffLine::plain(tag, "x")).collect(),
        }
    }

    /// `(hunk, lines, flat_start)` of each chunk.
    fn shape(chunked: &ChunkedHunks) -> Vec<(usize, std::ops::Range<usize>, usize)> {
        chunked
            .chunks
            .iter()
            .map(|c| (c.hunk, c.lines.clone(), c.flat_start))
            .collect()
    }

    #[test]
    fn chunk_hunks_keeps_small_hunks_whole() {
        use ChangeTag::*;
        let hunks = vec![
            hunk_of(1, 1, &[Equal, Delete, Insert, Equal]),
            hunk_of(97, 98, &[Equal, Insert, Insert]),
        ];
        let chunked = chunk_hunks(&hunks, 40, &[]);
        assert_eq!(
            chunked.chunks,
            vec![
                DiffChunk {
                    hunk: 0,
                    lines: 0..4,
                    flat_start: 0,
                    old_start: 1,
                    new_start: 1
                },
                DiffChunk {
                    hunk: 1,
                    lines: 0..3,
                    flat_start: 4,
                    old_start: 97,
                    new_start: 98
                },
            ]
        );
        // Last new-file line is 98 + 3 = 101 → three digits; old: 97 + 1.
        assert_eq!((chunked.old_width, chunked.new_width), (2, 3));
        assert!(!chunked.chunks[0].starts_later_hunk());
        assert!(chunked.chunks[1].starts_later_hunk());
    }

    #[test]
    fn chunk_hunks_splits_large_hunks_with_continuous_numbering() {
        use ChangeTag::*;
        // 2 deletes first: they advance only the old-file numbering.
        let mut tags = vec![Delete, Delete];
        tags.extend(std::iter::repeat_n(Insert, 8));
        let chunked = chunk_hunks(&[hunk_of(20, 10, &tags)], 4, &[]);
        assert_eq!(
            chunked.chunks,
            vec![
                DiffChunk {
                    hunk: 0,
                    lines: 0..4,
                    flat_start: 0,
                    old_start: 20,
                    new_start: 10
                },
                DiffChunk {
                    hunk: 0,
                    lines: 4..8,
                    flat_start: 4,
                    old_start: 22,
                    new_start: 12
                },
                DiffChunk {
                    hunk: 0,
                    lines: 8..10,
                    flat_start: 8,
                    old_start: 22,
                    new_start: 16
                },
            ]
        );
        assert!(chunked.chunks.iter().all(|c| !c.starts_later_hunk()));
    }

    #[test]
    fn chunk_hunks_of_nothing_is_empty() {
        let chunked = chunk_hunks(&[], 40, &[]);
        assert!(chunked.chunks.is_empty());
    }

    #[test]
    fn chunks_end_after_break_points() {
        use ChangeTag::*;
        let hunks = vec![
            hunk_of(1, 1, &[Equal; 10]),
            hunk_of(50, 50, &[Delete, Insert, Equal]),
        ];
        // Unsorted, a duplicate, one at a hunk's end (no extra cut) and one
        // past the end (ignored).
        let chunked = chunk_hunks(&hunks, 4, &[6, 1, 1, 9, 11, 99]);
        assert_eq!(
            shape(&chunked),
            [
                (0, 0..2, 0),
                (0, 2..6, 2),
                (0, 6..7, 6),
                (0, 7..10, 7),
                (1, 0..2, 10),
                (1, 2..3, 12),
            ]
        );
        // Numbering continues across the extra cuts.
        assert_eq!(chunked.chunks[2].new_start, 7);
        assert_eq!(chunked.chunks[5].old_start, 51);
        assert_eq!(chunked.chunks[5].new_start, 51);
    }
}
