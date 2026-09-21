//! The Review view as a flat list of rows — the items of its virtualized
//! list. [`flatten`] derives the rows from an outline of the view's state;
//! [`changed_span`] turns two generations of rows into the one splice the
//! list needs, so untouched rows keep their measured heights and the scroll
//! position survives.

use std::ops::Range;

/// One item of the Review list. Rows address their data by index into the
/// view's sections; `stamp` identifies the loaded diff a row was built from,
/// so a replaced diff yields different rows even when its shape is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReviewRow {
    RepoHeader {
        repo: usize,
    },
    BaseSelector {
        repo: usize,
    },
    FileHeader {
        repo: usize,
        file: usize,
    },
    /// "No content changes" note below a file header.
    NoChanges {
        repo: usize,
        file: usize,
        stamp: u64,
    },
    /// One chunk of a file's diff (index into its `ChunkedHunks`).
    Chunk {
        repo: usize,
        file: usize,
        chunk: usize,
        stamp: u64,
    },
}

/// What a loaded diff contributes below its file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffBody {
    /// Binary / too large: the header badge says it all.
    Nothing,
    NoChanges,
    Chunks(usize),
}

pub(super) struct FileOutline {
    pub collapsed: bool,
    /// Stamp and body of the loaded diff, if any.
    pub diff: Option<(u64, DiffBody)>,
}

pub(super) struct RepoOutline {
    pub collapsed: bool,
    pub base_selector: bool,
    pub files: Vec<FileOutline>,
}

pub(super) fn flatten(repos: &[RepoOutline]) -> Vec<ReviewRow> {
    let mut rows = Vec::new();
    for (repo, section) in repos.iter().enumerate() {
        rows.push(ReviewRow::RepoHeader { repo });
        if section.collapsed {
            continue;
        }
        if section.base_selector {
            rows.push(ReviewRow::BaseSelector { repo });
        }
        for (file, outline) in section.files.iter().enumerate() {
            rows.push(ReviewRow::FileHeader { repo, file });
            if outline.collapsed {
                continue;
            }
            match outline.diff {
                None | Some((_, DiffBody::Nothing)) => {}
                Some((stamp, DiffBody::NoChanges)) => {
                    rows.push(ReviewRow::NoChanges { repo, file, stamp })
                }
                Some((stamp, DiffBody::Chunks(count))) => {
                    rows.extend((0..count).map(|chunk| ReviewRow::Chunk {
                        repo,
                        file,
                        chunk,
                        stamp,
                    }))
                }
            }
        }
    }
    rows
}

/// The smallest splice turning `old` into `new`: the range of `old` to
/// replace and the number of rows replacing it. `None` when nothing changed.
pub(super) fn changed_span(old: &[ReviewRow], new: &[ReviewRow]) -> Option<(Range<usize>, usize)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    Some((prefix..old.len() - suffix, new.len() - suffix - prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReviewRow::*;

    fn file(collapsed: bool, diff: Option<(u64, DiffBody)>) -> FileOutline {
        FileOutline { collapsed, diff }
    }

    #[test]
    fn flatten_lists_headers_and_diff_chunks_in_order() {
        let rows = flatten(&[
            RepoOutline {
                collapsed: false,
                base_selector: true,
                files: vec![
                    file(false, Some((7, DiffBody::Chunks(2)))),
                    file(false, None),
                    file(false, Some((8, DiffBody::NoChanges))),
                    file(false, Some((9, DiffBody::Nothing))),
                ],
            },
            RepoOutline {
                collapsed: false,
                base_selector: false,
                files: vec![],
            },
        ]);
        assert_eq!(
            rows,
            vec![
                RepoHeader { repo: 0 },
                BaseSelector { repo: 0 },
                FileHeader { repo: 0, file: 0 },
                Chunk {
                    repo: 0,
                    file: 0,
                    chunk: 0,
                    stamp: 7
                },
                Chunk {
                    repo: 0,
                    file: 0,
                    chunk: 1,
                    stamp: 7
                },
                FileHeader { repo: 0, file: 1 },
                FileHeader { repo: 0, file: 2 },
                NoChanges {
                    repo: 0,
                    file: 2,
                    stamp: 8
                },
                FileHeader { repo: 0, file: 3 },
                RepoHeader { repo: 1 },
            ]
        );
    }

    #[test]
    fn flatten_skips_collapsed_repos_and_files() {
        let rows = flatten(&[
            RepoOutline {
                collapsed: true,
                base_selector: true,
                files: vec![file(false, Some((1, DiffBody::Chunks(3))))],
            },
            RepoOutline {
                collapsed: false,
                base_selector: false,
                files: vec![file(true, Some((2, DiffBody::Chunks(3))))],
            },
        ]);
        assert_eq!(
            rows,
            vec![
                RepoHeader { repo: 0 },
                RepoHeader { repo: 1 },
                FileHeader { repo: 1, file: 0 },
            ]
        );
    }

    fn chunk(file: usize, chunk: usize, stamp: u64) -> ReviewRow {
        Chunk {
            repo: 0,
            file,
            chunk,
            stamp,
        }
    }

    #[test]
    fn changed_span_is_none_for_equal_rows() {
        let rows = vec![RepoHeader { repo: 0 }, FileHeader { repo: 0, file: 0 }];
        assert_eq!(changed_span(&rows, &rows), None);
        assert_eq!(changed_span(&[], &[]), None);
    }

    #[test]
    fn changed_span_covers_only_an_arriving_diff() {
        let old = vec![
            RepoHeader { repo: 0 },
            FileHeader { repo: 0, file: 0 },
            FileHeader { repo: 0, file: 1 },
        ];
        let new = vec![
            RepoHeader { repo: 0 },
            FileHeader { repo: 0, file: 0 },
            chunk(0, 0, 1),
            chunk(0, 1, 1),
            FileHeader { repo: 0, file: 1 },
        ];
        assert_eq!(changed_span(&old, &new), Some((2..2, 2)));
        // …and collapsing the file again removes exactly those rows.
        assert_eq!(changed_span(&new, &old), Some((2..4, 0)));
    }

    #[test]
    fn changed_span_replaces_a_reloaded_diff_of_the_same_shape() {
        let old = vec![
            FileHeader { repo: 0, file: 0 },
            chunk(0, 0, 1),
            chunk(0, 1, 1),
        ];
        let new = vec![
            FileHeader { repo: 0, file: 0 },
            chunk(0, 0, 2),
            chunk(0, 1, 2),
        ];
        assert_eq!(changed_span(&old, &new), Some((1..3, 2)));
    }

    #[test]
    fn changed_span_handles_repeated_rows_without_overlap() {
        // Prefix and suffix both match the single shared row; they must not
        // both claim it.
        let one = vec![RepoHeader { repo: 0 }];
        let two = vec![RepoHeader { repo: 0 }, RepoHeader { repo: 0 }];
        assert_eq!(changed_span(&one, &two), Some((1..1, 1)));
        assert_eq!(changed_span(&two, &one), Some((1..2, 0)));
    }
}
