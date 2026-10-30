//! The Code tab as a flat list of rows, the items of its virtualized list:
//! per file a header, then (unless folded) the hunks as chunks, each hunk
//! after a gap row ("30 unmodified lines"), threads and comments right
//! below the row they are on, and the loading footer at the end. Rows that
//! stay the same compare equal, so a change reaches the list as one splice
//! ([`changed_span`](crate::main_screen::right_panel::review_rows::changed_span))
//! and the scroll position survives.

use super::anchors::Annotation;
use crate::diff::chunks::DiffChunk;
use crate::diff::model::DiffHunk;
use similar::ChangeTag;
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
    /// The unchanged lines before hunk `hunk`, and its section.
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
    /// Below the last hunk of a file that has lines past it.
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

/// What a file contributes below its header.
pub enum FileBody<'a> {
    /// Binary, too large or no content change: a note says which.
    Note,
    Diff {
        hunks: &'a [DiffHunk],
        chunks: &'a [DiffChunk],
        /// Annotations by the flat line they go below; chunks end there.
        annotations: &'a BTreeMap<usize, Vec<Annotation>>,
        /// Versions of the threads, by index.
        thread_version: &'a dyn Fn(usize) -> u64,
        tail: bool,
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
                hunks,
                chunks,
                annotations,
                thread_version,
                tail,
            } => {
                let mut prev = None;
                for (ix, chunk) in chunks.iter().enumerate() {
                    if chunk.lines.start == 0
                        && let Some(hunk) = hunks.get(chunk.hunk)
                        && gap(prev, hunk).is_some()
                    {
                        rows.push(CodeRow::Gap {
                            file,
                            hunk: chunk.hunk,
                            stamp,
                        });
                    }
                    prev = hunks.get(chunk.hunk);
                    rows.push(CodeRow::Chunk {
                        file,
                        chunk: ix,
                        stamp,
                    });
                    let last = chunk.flat_start + chunk.lines.len().saturating_sub(1);
                    for annotation in annotations.get(&last).into_iter().flatten() {
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
                if *tail && !chunks.is_empty() {
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

/// What separates `hunk` from the one before (`None` for the first): the
/// unchanged lines between them and its section, when there is either.
pub fn gap<'a>(prev: Option<&DiffHunk>, hunk: &'a DiffHunk) -> Option<(usize, Option<&'a str>)> {
    let prev_end = prev.map_or(1, |prev| {
        prev.old_start
            + prev
                .lines
                .iter()
                .filter(|l| l.tag != ChangeTag::Insert)
                .count()
    });
    let lines = hunk.old_start.saturating_sub(prev_end);
    let section = hunk.header.as_deref();
    (lines > 0 || section.is_some()).then_some((lines, section))
}

/// The text of a gap row.
pub fn gap_label(lines: usize, section: Option<&str>) -> String {
    let count = match lines {
        0 => None,
        1 => Some("1 unmodified line".to_owned()),
        n => Some(format!("{n} unmodified lines")),
    };
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
    use crate::diff::chunks::chunk_hunks;
    use crate::diff::model::DiffLine;
    use ChangeTag::*;

    fn hunk(old_start: usize, header: Option<&str>, tags: &[ChangeTag]) -> DiffHunk {
        DiffHunk {
            old_start,
            new_start: old_start,
            header: header.map(|h| h.to_owned().into()),
            lines: tags.iter().map(|&tag| DiffLine::plain(tag, "x")).collect(),
        }
    }

    #[test]
    fn gaps_count_the_old_files_unchanged_lines() {
        let first = hunk(31, None, &[Equal, Insert, Delete, Equal]);
        assert_eq!(gap(None, &first), Some((30, None)));
        // `first` covers old lines 31..=33.
        let second = hunk(40, Some("fn main() {"), &[Equal]);
        assert_eq!(gap(Some(&first), &second), Some((6, Some("fn main() {"))));
        let adjacent = hunk(34, None, &[Equal]);
        assert_eq!(gap(Some(&first), &adjacent), None);
        // A new file's hunk starts at old line 0.
        assert_eq!(gap(None, &hunk(0, None, &[Insert])), None);
        assert_eq!(gap(None, &hunk(1, None, &[Delete])), None);
    }

    #[test]
    fn gap_labels() {
        assert_eq!(gap_label(30, None), "30 unmodified lines");
        assert_eq!(gap_label(1, None), "1 unmodified line");
        assert_eq!(
            gap_label(4, Some("impl A {")),
            "4 unmodified lines · impl A {"
        );
        assert_eq!(gap_label(0, Some("impl A {")), "⋯ impl A {");
    }

    #[test]
    fn files_list_gaps_chunks_annotations_and_tail_in_order() {
        let hunks = vec![
            hunk(5, None, &[Equal, Insert, Equal]),
            hunk(8, None, &[Equal, Delete]),
        ];
        let mut annotations = BTreeMap::new();
        annotations.insert(1, vec![Annotation::Thread(4), Annotation::Draft]);
        annotations.insert(4, vec![Annotation::Pending(9)]);
        let chunked = chunk_hunks(&hunks, 40, &[1]);
        let version = |thread: usize| thread as u64 * 10;
        let files = [
            FileOutline {
                folded: false,
                stamp: 7,
                body: FileBody::Diff {
                    hunks: &hunks,
                    chunks: &chunked.chunks,
                    annotations: &annotations,
                    thread_version: &version,
                    tail: true,
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
                // Hunk 0 ends at old line 6, hunk 1 starts at 8.
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
