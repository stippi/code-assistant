//! The draft's comments on the shown files: a card below the line each is
//! on (edit, remove), their rows marked; a comment revealed from the
//! composer shows its rows and opens the comment box on it. New ones are
//! written in the shared comment box and go to the agent's draft.

use super::ReviewView;
use crate::comments::CommentChange;
use crate::main_screen::right_panel::code_view::anchors::{LineRef, RangeRef};
use crate::main_screen::right_panel::code_view::draft::{annotation, lines_label, open_comment};
use crate::main_screen::right_panel::code_view::file::LoadedFile;
use crate::main_screen::right_panel::code_view::list::{self as code_list, CodeHost};
use crate::main_screen::right_panel::code_view::rows::CodeRow;
use code_assistant_core::line_comments::LineComment;
use github::model::DiffSide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{AnyElement, Context, Window, div, prelude::*};
use similar::ChangeTag;
use std::ops::Range;

fn side(comment: &LineComment) -> DiffSide {
    match comment.old_side {
        true => DiffSide::Left,
        false => DiffSide::Right,
    }
}

/// Where `comment` goes below the rows: its last line.
pub(super) fn line_ref(comment: &LineComment) -> LineRef {
    LineRef {
        side: side(comment),
        line: comment.end_line as u64,
    }
}

/// Whether `comment` covers the row with `(tag, old, new)` numbers: a line
/// of its side within its range.
fn covers(comment: &LineComment, (tag, old, new): (ChangeTag, usize, usize)) -> bool {
    let (on_side, line) = match comment.old_side {
        true => (tag != ChangeTag::Insert, old),
        false => (tag != ChangeTag::Delete, new),
    };
    on_side && (comment.start_line..=comment.end_line).contains(&line)
}

impl ReviewView {
    pub fn set_comments(&mut self, comments: Vec<LineComment>, cx: &mut Context<Self>) {
        if self.comments != comments {
            self.comments = comments;
            self.place_annotations(cx);
        }
    }

    /// The draft's comments on `file`.
    fn comments_on<'a>(&'a self, file: &LoadedFile) -> impl Iterator<Item = &'a LineComment> {
        let path = self.comment_file(&file.path);
        self.comments
            .iter()
            .filter(move |c| c.in_diff && c.file == path)
    }

    /// Flat lines `lines` of `file` a comment covers.
    pub(super) fn comment_rows(&self, file: &LoadedFile, lines: Range<usize>) -> Vec<Range<usize>> {
        let comments: Vec<&LineComment> = self.comments_on(file).collect();
        if comments.is_empty() {
            return Vec::new();
        }
        lines
            .filter(|&row| {
                file.numbers
                    .get(row)
                    .is_some_and(|&n| comments.iter().any(|c| covers(c, n)))
            })
            .map(|row| row..row + 1)
            .collect()
    }

    /// The card of the draft's comment `id`.
    pub(super) fn render_comment(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(comment) = self.comments.iter().find(|c| c.id == id).cloned() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let at = RangeRef {
            start: (comment.start_line != comment.end_line).then(|| LineRef {
                side: side(&comment),
                line: comment.start_line as u64,
            }),
            end: line_ref(&comment),
        };
        let edited = comment.clone();
        let card = div()
            .debug_selector(move || format!("review-comment-{id}"))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_lg()
            .border_1()
            .border_dashed()
            .border_color(theme.warning.opacity(0.6))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .px_1p5()
                            .rounded_sm()
                            .bg(theme.warning.opacity(0.15))
                            .text_color(theme.warning)
                            .child("For the agent"),
                    )
                    .child(format!("Line {}", lines_label(&at)))
                    .child(div().flex_1())
                    .child(
                        Button::new(("review-comment-edit", id))
                            .ghost()
                            .xsmall()
                            .icon(Icon::default().path("icons/pencil.svg"))
                            .tooltip("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_comment(&edited, window, cx);
                            })),
                    )
                    .child(
                        Button::new(("review-comment-delete", id))
                            .ghost()
                            .xsmall()
                            .icon(Icon::default().path("icons/trash.svg"))
                            .tooltip("Remove from the draft")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                crate::comments::report(CommentChange::Remove(id), cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(comment.text.clone()),
            );
        annotation(card.into_any_element())
    }

    /// Select `comment`'s rows and open the comment box on them with its
    /// text. `false` while its file's diff is not read.
    fn edit_comment(
        &mut self,
        comment: &LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(root) = self.repo.clone() else {
            return false;
        };
        let Some(ix) = (0..self.shown.len()).find(|&ix| {
            let file = self.shown_file(ix);
            file.loaded && root.join(&file.path) == comment.file
        }) else {
            return false;
        };
        let file = self.shown_file(ix);
        let path = file.path.clone();
        let covered: Vec<usize> = file
            .numbers
            .iter()
            .enumerate()
            .filter(|&(_, &n)| covers(comment, n))
            .map(|(row, _)| row)
            .collect();
        let (Some(&first), Some(&last)) = (covered.first(), covered.last()) else {
            return true;
        };
        let selection = &mut self.list.selection;
        selection.begin(path.clone(), first, false);
        selection.extend(&path, last);
        selection.end();
        if self.list.folds.is_folded(&path) {
            self.list.folds.set(&path, false);
        }
        open_comment(self, Some(comment), window, cx);
        // Scroll to the box below the rows.
        code_list::sync_rows(self);
        if let Some(row) = self
            .list
            .rows
            .iter()
            .position(|row| *row == CodeRow::Draft { file: ix })
        {
            self.list.list_state.scroll_to_reveal_item(row);
        }
        true
    }

    /// Show where `comment` was made and open it for editing. Waits for
    /// the listing and the file's diff.
    pub fn reveal_comment(&mut self, comment: LineComment, cx: &mut Context<Self>) {
        self.pending_reveal = Some(comment);
        cx.notify();
    }

    /// Take a pending reveal as far as the data allows; called in `render`.
    pub(super) fn apply_pending_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.pending_reveal.clone() else {
            return;
        };
        if !self.has_listing {
            return;
        }
        // The repository holding the file, shown.
        let Some(root) = self
            .repos
            .iter()
            .map(|r| r.repo_root.clone())
            .filter(|root| comment.file.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            self.pending_reveal = None;
            return;
        };
        if self.repo.as_ref() != Some(&root) {
            self.show_repo(Some(root.clone()), cx);
        }
        let Some(path) = comment
            .file
            .strip_prefix(&root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
        else {
            self.pending_reveal = None;
            return;
        };
        let Some(file) = self.review_file(&path) else {
            // Not among the changed files (any more), once they are known.
            if self.repo_data().is_some_and(|d| {
                d.scan_state == code_assistant_core::session::ReviewScanState::Done
            }) {
                self.pending_reveal = None;
            }
            return;
        };
        if !file.file.loaded {
            if self.list.folds.is_folded(&path) {
                self.list.folds.set(&path, false);
                self.list.dirty = true;
                self.ensure_diff_request(cx);
            }
            return;
        }
        self.pending_reveal = None;
        self.edit_comment(&comment, window, cx);
    }
}
