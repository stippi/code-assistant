//! The Code tab as a flat list of rows, the items of its virtualized list:
//! per file a header, then (unless folded) the hunks as chunks (stacked or
//! side by side), each hunk after its gap row ("30 unmodified lines") when
//! it has one, threads and comments right below the row they are on, and
//! the loading footer at the end. Rows that stay the same compare equal,
//! so a change reaches the list as one splice
//! ([`changed_span`](crate::main_screen::right_panel::review_rows::changed_span))
//! and the scroll position survives.

use super::anchors::Annotation;
use super::context::ShownGap;
use std::collections::BTreeMap;

/// One item of the list. `file` indexes the shown files; `stamp` names
/// the cut of a file's diff a row was built from (a new diff or new cut
/// yields new rows); `version` changes with what a row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeRow {
    File {
        file: usize,
    },
    /// Binary, too large or no content change.
    Note {
        file: usize,
        stamp: u64,
    },
    /// The unchanged lines before shown hunk `hunk`, and its section.
    Gap {
        file: usize,
        hunk: usize,
        stamp: u64,
    },
    Chunk {
        file: usize,
        chunk: usize,
        stamp: u64,
    },
    /// Below the last hunk of a file that may have lines past it.
    Tail {
        file: usize,
        stamp: u64,
    },
    Thread {
        file: usize,
        thread: usize,
        version: u64,
    },
    Pending {
        file: usize,
        id: u64,
    },
    Draft {
        file: usize,
    },
    /// More pages follow: "Loading more files…" or a retry.
    Footer,
}

/// A chunk as the rows need it.
pub struct ChunkOutline {
    pub hunk: usize,
    /// The chunk opens its hunk.
    pub starts_hunk: bool,
    /// The flat lines its last row shows: what goes below it.
    pub last_lines: Vec<usize>,
}

/// What a file contributes below its header.
pub enum FileBody<'a> {
    /// Binary, too large or no content change: a note says which.
    Note,
    Diff {
        hunk_count: usize,
        chunks: Vec<ChunkOutline>,
        gaps: &'a [ShownGap],
        /// Annotations by the flat line they go below; chunks end there.
        annotations: &'a BTreeMap<usize, Vec<Annotation>>,
        /// Versions of the threads, by index.
        thread_version: &'a dyn Fn(usize) -> u64,
    },
}

pub struct FileOutline<'a> {
    pub folded: bool,
    pub stamp: u64,
    pub body: FileBody<'a>,
}

pub fn flatten(files: &[FileOutline], footer: bool) -> Vec<CodeRow> {
    let mut rows = Vec::new();
    for (file, outline) in files.iter().enumerate() {
        rows.push(CodeRow::File { file });
        if outline.folded {
            continue;
        }
        let stamp = outline.stamp;
        match &outline.body {
            FileBody::Note => rows.push(CodeRow::Note { file, stamp }),
            FileBody::Diff {
                hunk_count,
                chunks,
                gaps,
                annotations,
                thread_version,
            } => {
                let gap_before = |hunk: usize| gaps.iter().any(|gap| gap.before == hunk);
                for (ix, chunk) in chunks.iter().enumerate() {
                    if chunk.starts_hunk && gap_before(chunk.hunk) {
                        rows.push(CodeRow::Gap {
                            file,
                            hunk: chunk.hunk,
                            stamp,
                        });
                    }
                    rows.push(CodeRow::Chunk {
                        file,
                        chunk: ix,
                        stamp,
                    });
                    for &line in &chunk.last_lines {
                        for annotation in annotations.get(&line).into_iter().flatten() {
                            rows.push(match *annotation {
                                Annotation::Thread(thread) => CodeRow::Thread {
                                    file,
                                    thread,
                                    version: thread_version(thread),
                                },
                                Annotation::Pending(id) => CodeRow::Pending { file, id },
                                Annotation::Draft => CodeRow::Draft { file },
                            });
                        }
                    }
                }
                if !chunks.is_empty() && gap_before(*hunk_count) {
                    rows.push(CodeRow::Tail { file, stamp });
                }
            }
        }
    }
    if footer {
        rows.push(CodeRow::Footer);
    }
    rows
}

impl CodeRow {
    /// The shown file a row belongs to (`None` for the footer).
    pub fn file(&self) -> Option<usize> {
        match *self {
            CodeRow::File { file }
            | CodeRow::Note { file, .. }
            | CodeRow::Gap { file, .. }
            | CodeRow::Chunk { file, .. }
            | CodeRow::Tail { file, .. }
            | CodeRow::Thread { file, .. }
            | CodeRow::Pending { file, .. }
            | CodeRow::Draft { file } => Some(file),
            CodeRow::Footer => None,
        }
    }
}

/// The shown files (`0..files`) in the order to highlight them when
/// `top_row` is the first row in view: from the file at the top of the
/// viewport downwards, then the ones above it, nearest first.
pub fn files_by_proximity(rows: &[CodeRow], files: usize, top_row: usize) -> Vec<usize> {
    let top = rows
        .get(top_row)
        .and_then(CodeRow::file)
        .unwrap_or_else(|| files.saturating_sub(1))
        .min(files.saturating_sub(1));
    (top..files).chain((0..top).rev()).collect()
}

/// The text of a gap row; `None` lines past the last hunk while the file's
/// length is unknown.
pub fn gap_label(lines: Option<usize>, section: Option<&str>) -> String {
    let count = match lines {
        None => return "More unchanged context may be available".to_owned(),
        Some(0) => None,
        Some(1) => Some("1 unmodified line".to_owned()),
        Some(n) => Some(format!("{n} unmodified lines")),
    };
    // A section is the raw `@@` context: one line, its spacing collapsed.
    let section = section
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty());
    match (count, section) {
        (Some(count), Some(section)) => format!("{count} · {section}"),
        (Some(count), None) => count,
        (None, Some(section)) => format!("⋯ {section}"),
        (None, None) => "⋯".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_highlight_from_the_top_of_the_view_outwards() {
        use CodeRow::*;
        let rows = vec![
            File { file: 0 },
            File { file: 1 },
            Chunk {
                file: 1,
                chunk: 0,
                stamp: 1,
            },
            File { file: 2 },
            File { file: 3 },
            Footer,
        ];
        assert_eq!(files_by_proximity(&rows, 4, 0), [0, 1, 2, 3]);
        // Inside file 1's rows: it first, then below, then above.
        assert_eq!(files_by_proximity(&rows, 4, 2), [1, 2, 3, 0]);
        // The footer or a stale index past the end: from the last file up.
        assert_eq!(files_by_proximity(&rows, 4, 5), [3, 2, 1, 0]);
        assert_eq!(files_by_proximity(&rows, 4, 99), [3, 2, 1, 0]);
        assert!(files_by_proximity(&[], 0, 0).is_empty());
    }

    #[test]
    fn gap_labels() {
        assert_eq!(gap_label(Some(30), None), "30 unmodified lines");
        assert_eq!(gap_label(Some(1), None), "1 unmodified line");
        assert_eq!(
            gap_label(Some(4), Some("impl A {")),
            "4 unmodified lines · impl A {"
        );
        assert_eq!(gap_label(Some(0), Some("impl A {")), "⋯ impl A {");
        assert_eq!(
            gap_label(Some(2), Some("  Layer 5:\t   code   — CLI ")),
            "2 unmodified lines · Layer 5: code — CLI"
        );
        assert_eq!(gap_label(Some(2), Some("   ")), "2 unmodified lines");
        assert_eq!(
            gap_label(None, None),
            "More unchanged context may be available"
        );
    }

    #[test]
    fn files_list_gaps_chunks_annotations_and_tail_in_order() {
        let mut annotations = BTreeMap::new();
        annotations.insert(1, vec![Annotation::Thread(4), Annotation::Draft]);
        annotations.insert(4, vec![Annotation::Pending(9)]);
        let gap = |before, hidden| ShownGap {
            gap: before,
            before,
            hidden,
            section: None,
        };
        let gaps = [gap(0, Some(4)), gap(1, Some(1)), gap(2, None)];
        let chunk = |hunk, starts_hunk, last_lines: &[usize]| ChunkOutline {
            hunk,
            starts_hunk,
            last_lines: last_lines.to_vec(),
        };
        let version = |thread: usize| thread as u64 * 10;
        let files = [
            FileOutline {
                folded: false,
                stamp: 7,
                body: FileBody::Diff {
                    hunk_count: 2,
                    // A split chunk's last row shows two lines.
                    chunks: vec![
                        chunk(0, true, &[1]),
                        chunk(0, false, &[2]),
                        chunk(1, true, &[3, 4]),
                    ],
                    gaps: &gaps,
                    annotations: &annotations,
                    thread_version: &version,
                },
            },
            FileOutline {
                folded: true,
                stamp: 8,
                body: FileBody::Note,
            },
            FileOutline {
                folded: false,
                stamp: 9,
                body: FileBody::Note,
            },
        ];
        use CodeRow::*;
        assert_eq!(
            flatten(&files, true),
            vec![
                File { file: 0 },
                Gap {
                    file: 0,
                    hunk: 0,
                    stamp: 7
                },
                Chunk {
                    file: 0,
                    chunk: 0,
                    stamp: 7
                },
                Thread {
                    file: 0,
                    thread: 4,
                    version: 40
                },
                Draft { file: 0 },
                Chunk {
                    file: 0,
                    chunk: 1,
                    stamp: 7
                },
                Gap {
                    file: 0,
                    hunk: 1,
                    stamp: 7
                },
                Chunk {
                    file: 0,
                    chunk: 2,
                    stamp: 7
                },
                Pending { file: 0, id: 9 },
                Tail { file: 0, stamp: 7 },
                File { file: 1 },
                File { file: 2 },
                Note { file: 2, stamp: 9 },
                Footer,
            ]
        );
    }
}
