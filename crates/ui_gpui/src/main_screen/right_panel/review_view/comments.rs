//! Line comments on the Review view's diffs.
//!
//! A selection of diff rows becomes a comment on the file's new-side lines
//! (or old-side lines when only deleted rows are selected), with the rows as
//! excerpt, `+`/`-` prefixed. Rows covered by any comment on the file are
//! marked, whether the comment was made here or in the Files view.

use super::*;
use crate::comments::CommentChange;
use crate::comments::editor::{CommentEditor, CommentEditorEvent};
use code_assistant_core::line_comments::LineComment;
use similar::ChangeTag;
use std::ops::Range;

/// One diff row's change tag and its 1-based line numbers in the old and
/// new file, for every row of the file's hunks in render order.
pub(super) fn row_numbers(prepared: &PreparedReviewDiff) -> Vec<(ChangeTag, usize, usize)> {
    let mut rows = Vec::new();
    for hunk in &prepared.hunks {
        let (mut old, mut new) = (hunk.old_start, hunk.new_start);
        for line in &hunk.lines {
            rows.push((line.tag, old, new));
            if line.tag != ChangeTag::Insert {
                old += 1;
            }
            if line.tag != ChangeTag::Delete {
                new += 1;
            }
        }
    }
    rows
}

/// Whether `comment` covers a row with these numbers.
fn covers(comment: &LineComment, (tag, old, new): (ChangeTag, usize, usize)) -> bool {
    let lines = comment.start_line..=comment.end_line;
    if comment.old_side {
        tag == ChangeTag::Delete && lines.contains(&old)
    } else {
        tag != ChangeTag::Delete && lines.contains(&new)
    }
}

impl ReviewView {
    fn file_of(key: &FileKey) -> PathBuf {
        key.0.join(&key.1)
    }

    pub fn set_comments(&mut self, comments: Vec<LineComment>, cx: &mut Context<Self>) {
        if self.comments != comments {
            self.comments = comments;
            cx.notify();
        }
    }

    /// Rows of a chunk (relative to it) covered by a comment.
    pub(super) fn chunk_marks(
        &self,
        key: &FileKey,
        prepared: &PreparedReviewDiff,
        base_line: usize,
        row_count: usize,
    ) -> Vec<Range<usize>> {
        let file = Self::file_of(key);
        let comments: Vec<&LineComment> = self.comments.iter().filter(|c| c.file == file).collect();
        if comments.is_empty() {
            return Vec::new();
        }
        let numbers = row_numbers(prepared);
        (0..row_count)
            .filter(|&row| {
                numbers
                    .get(base_line + row)
                    .is_some_and(|&n| comments.iter().any(|c| covers(c, n)))
            })
            .map(|row| row..row + 1)
            .collect()
    }

    /// The comment for the current selection: an existing one on those rows,
    /// else a new one.
    fn selection_comment(&self) -> Option<LineComment> {
        let sel = self.selection.get()?;
        let prepared = &self.file_diffs.get(&sel.key)?.prepared;
        let numbers = row_numbers(prepared);
        let lines: Vec<_> = prepared.hunks.iter().flat_map(|h| h.lines.iter()).collect();
        let (lo, hi) = sel.range();
        let hi = hi.min(numbers.len().checked_sub(1)?);
        if lo > hi {
            return None;
        }
        let file = Self::file_of(&sel.key);
        let rows = &numbers[lo..=hi];
        if let Some(existing) = self
            .comments
            .iter()
            .find(|c| c.file == file && rows.iter().any(|&n| covers(c, n)))
        {
            return Some(existing.clone());
        }
        let new_side: Vec<usize> = rows
            .iter()
            .filter(|(tag, ..)| *tag != ChangeTag::Delete)
            .map(|&(_, _, new)| new)
            .collect();
        let (old_side, numbers_used) = if new_side.is_empty() {
            (true, rows.iter().map(|&(_, old, _)| old).collect())
        } else {
            (false, new_side)
        };
        let excerpt = lines[lo..=hi]
            .iter()
            .map(|l| {
                let prefix = match l.tag {
                    ChangeTag::Insert => '+',
                    ChangeTag::Delete => '-',
                    ChangeTag::Equal => ' ',
                };
                format!("{prefix}{}", l.text)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(LineComment {
            id: 0,
            file,
            start_line: *numbers_used.iter().min()?,
            end_line: *numbers_used.iter().max()?,
            old_side,
            in_diff: true,
            on_message: false,
            excerpt,
            text: String::new(),
        })
    }

    /// Whether the chunk of `key` starting at flat line `base_line` holds the
    /// selection's last row.
    pub(super) fn anchors_selection_end(
        &self,
        key: &FileKey,
        base_line: usize,
        rows: usize,
    ) -> bool {
        self.selection.get().is_some_and(|sel| {
            let (_, hi) = sel.range();
            &sel.key == key && (base_line..base_line + rows).contains(&hi)
        })
    }

    /// The selection pill under the selection's last row, or the comment
    /// card while one is open.
    pub(super) fn render_floating(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        use crate::comments::{floating, selection_pill};
        use gpui_kit::point;
        // Written by the last paint; cleared so a scrolled-away row hides it.
        let row = self.selection_anchor.take();
        if let Some(editor) = &self.comment_editor {
            let position = row
                .map(|b| point(b.left() + px(28.), b.bottom() + px(2.)))
                .or(self.anchor.last());
            self.anchor.track(position, window);
            return position.map(|p| floating(p, editor.clone()));
        }
        let dismissed = self.selection.get() == self.pill_dismissed.as_ref();
        let position = row
            .filter(|_| {
                self.selection.get().is_some() && !self.selection.is_dragging() && !dismissed
            })
            .map(|b| point(b.right() - px(72.), b.bottom() + px(2.)));
        self.anchor.track(position, window);
        let view = cx.entity().downgrade();
        let view_for_comment = view.clone();
        // A press elsewhere hides the pill until the selection changes.
        let dismiss = cx.listener(|this, _: &gpui_kit::MouseDownEvent, _, cx| {
            this.pill_dismissed = this.selection.get().cloned();
            cx.notify();
        });
        Some(floating(
            position?,
            div().on_mouse_down_out(dismiss).child(selection_pill(
                "review-selection",
                move |_, cx| {
                    view.update(cx, |view, cx| view.copy_selection(cx)).ok();
                },
                move |window, cx| {
                    view_for_comment
                        .update(cx, |view, cx| view.start_comment(window, cx))
                        .ok();
                },
                cx,
            )),
        ))
    }

    pub(super) fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(comment) = self.selection_comment() {
            self.open_comment_editor(comment, window, cx);
        }
    }

    fn open_comment_editor(
        &mut self,
        comment: LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| CommentEditor::new(comment, window, cx));
        self._comment_editor_subscription = Some(cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &CommentEditorEvent, window, cx| {
                match event {
                    CommentEditorEvent::Save(comment) => {
                        cx.emit(CommentChange::Upsert(comment.clone()))
                    }
                    CommentEditorEvent::Delete(id) => cx.emit(CommentChange::Remove(*id)),
                    CommentEditorEvent::Cancel => {}
                }
                this.comment_editor = None;
                this._comment_editor_subscription = None;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            },
        ));
        self.comment_editor = Some(editor);
        cx.notify();
    }

    /// Show `comment`'s rows: expand its repo and file, select the rows,
    /// scroll to them and open the comment. Waits for the listing and the
    /// file's diff.
    pub fn reveal_comment(&mut self, comment: LineComment, cx: &mut Context<Self>) {
        self.pending_reveal = Some(comment);
        cx.notify();
    }

    /// Advance a pending reveal as far as the loaded data allows; called
    /// from `render`.
    pub(super) fn apply_pending_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.pending_reveal.clone() else {
            return;
        };
        let found = self.repos.iter().enumerate().find_map(|(repo, section)| {
            section
                .files
                .iter()
                .position(|f| section.repo_root.join(&f.path) == comment.file)
                .map(|file| (repo, file))
        });
        let Some((repo, file)) = found else {
            if self.has_listing {
                // Not among the changed files (any more).
                self.pending_reveal = None;
            }
            return;
        };
        let key: FileKey = (
            self.repos[repo].repo_root.clone(),
            self.repos[repo].files[file].path.clone(),
        );
        if self.repos[repo].collapsed || self.collapsed_files.contains(&key) {
            self.repos[repo].collapsed = false;
            self.collapsed_files.remove(&key);
            self.ensure_diff_request(cx);
            cx.notify();
        }
        let Some(loaded) = self.file_diffs.get(&key) else {
            return;
        };
        self.pending_reveal = None;
        let numbers = row_numbers(&loaded.prepared);
        let covered: Vec<usize> = numbers
            .iter()
            .enumerate()
            .filter(|&(_, &n)| covers(&comment, n))
            .map(|(ix, _)| ix)
            .collect();
        if let (Some(&first), Some(&last)) = (covered.first(), covered.last()) {
            self.selection.begin(key.clone(), first, false);
            self.selection.extend(&key, last);
            self.selection.end();
            // Scroll to the chunk holding the first row.
            let chunks = &loaded.prepared.chunked.chunks;
            let mut base = 0;
            let mut chunk_ix = None;
            for (ix, chunk) in chunks.iter().enumerate() {
                let start: usize = loaded.prepared.hunks[..chunk.hunk]
                    .iter()
                    .map(|h| h.lines.len())
                    .sum::<usize>()
                    + chunk.lines.start;
                if start <= first {
                    chunk_ix = Some(ix);
                    base = start;
                }
            }
            let _ = base;
            self.sync_rows();
            if let Some(chunk) = chunk_ix
                && let Some(row) = self.rows.iter().position(|r| {
                    matches!(r, ReviewRow::Chunk { repo: r_repo, file: r_file, chunk: c, .. }
                        if *r_repo == repo && *r_file == file && *c == chunk)
                })
            {
                self.list_state.scroll_to_reveal_item(row);
            }
        }
        self.open_comment_editor(comment, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(lines: (usize, usize), old_side: bool) -> LineComment {
        LineComment {
            id: 1,
            file: PathBuf::from("/r/a.rs"),
            start_line: lines.0,
            end_line: lines.1,
            old_side,
            in_diff: true,
            on_message: false,
            excerpt: String::new(),
            text: String::new(),
        }
    }

    #[test]
    fn row_numbers_count_each_side() {
        let prepared = PreparedReviewDiff::from_content(
            "a.rs",
            &git::FileDiffContent {
                old_text: Some("a\nb\nc\n".into()),
                new_text: Some("a\nB\nc\nd\n".into()),
                is_binary: false,
                too_large: false,
            },
        );
        let numbers = row_numbers(&prepared);
        let tags: Vec<_> = numbers.iter().map(|n| n.0).collect();
        assert_eq!(
            tags,
            vec![
                ChangeTag::Equal,
                ChangeTag::Delete,
                ChangeTag::Insert,
                ChangeTag::Equal,
                ChangeTag::Insert
            ]
        );
        assert_eq!(numbers[1], (ChangeTag::Delete, 2, 2));
        assert_eq!(numbers[2], (ChangeTag::Insert, 3, 2));
        assert_eq!(numbers[4], (ChangeTag::Insert, 4, 4));
        assert!(covers(&comment((2, 2), false), numbers[2]));
        assert!(!covers(&comment((2, 2), false), numbers[1]));
        assert!(covers(&comment((2, 2), true), numbers[1]));
    }
}
