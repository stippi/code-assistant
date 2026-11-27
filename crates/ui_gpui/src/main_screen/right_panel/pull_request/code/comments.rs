//! The pending review's comments on the diff: the comment box's "Add to
//! review" and the dashed cards on their lines until the review is
//! submitted. Writing them is the shared comment box's
//! ([`draft`](crate::main_screen::right_panel::code_view::draft)).

use super::super::review_draft::PendingComment;
use super::CodeTab;
use crate::main_screen::right_panel::code_view::anchors::{LineRef, RangeRef};
use crate::main_screen::right_panel::code_view::draft::{
    annotation, close_draft, draft_file, draft_text, lines_label,
};
use crate::main_screen::right_panel::code_view::file::LoadedFile;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{AnyElement, App, Context, div, prelude::*};
use std::ops::Range;

impl CodeTab {
    /// The comment box's comment into the pending review.
    pub(super) fn add_to_review(&mut self, cx: &mut Context<Self>) {
        let Some(body) = draft_text(self, cx) else {
            return;
        };
        let (Some(draft), Some((_, excerpt))) = (self.list.draft.as_ref(), draft_file(self)) else {
            return;
        };
        let comment = PendingComment {
            id: 0,
            path: draft.path.clone(),
            side: draft.at.end.side,
            line: draft.at.end.line,
            start_side: draft.at.start.map(|s| s.side),
            start_line: draft.at.start.map(|s| s.line),
            body,
            excerpt,
        };
        let pr = self.pr.clone();
        self.drafts
            .update(cx, |drafts, cx| drafts.add(&pr, comment, cx));
        close_draft(self, cx);
    }

    /// Flat lines of a chunk a pending comment is on.
    pub(super) fn pending_marks(
        &self,
        file: &LoadedFile,
        chunk: Range<usize>,
        cx: &App,
    ) -> Vec<Range<usize>> {
        self.drafts
            .read(cx)
            .comments(&self.pr)
            .iter()
            .filter(|c| c.path == file.path)
            .filter_map(|c| {
                let end = file.lines.row(LineRef {
                    side: c.side,
                    line: c.line,
                })?;
                let start = c
                    .start_side
                    .zip(c.start_line)
                    .and_then(|(side, line)| file.lines.row(LineRef { side, line }))
                    .unwrap_or(end);
                let covered = start.max(chunk.start)..(end + 1).min(chunk.end);
                (covered.start < covered.end).then_some(covered)
            })
            .collect()
    }

    /// A comment of the pending review: dashed, with "Pending" and delete.
    pub(super) fn render_pending(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let drafts = self.drafts.read(cx);
        let Some(comment) = drafts.comments(&self.pr).iter().find(|c| c.id == id) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let at = RangeRef {
            start: comment
                .start_side
                .zip(comment.start_line)
                .map(|(side, line)| LineRef { side, line }),
            end: LineRef {
                side: comment.side,
                line: comment.line,
            },
        };
        let card = div()
            .debug_selector(move || format!("pr-code-pending-{id}"))
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
                            .child("Pending"),
                    )
                    .child(format!("Line {}", lines_label(&at)))
                    .child(div().flex_1())
                    .child(
                        Button::new(("pr-code-pending-delete", id))
                            .ghost()
                            .xsmall()
                            .icon(Icon::default().path("icons/trash.svg"))
                            .tooltip("Remove from the review")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let pr = this.pr.clone();
                                this.drafts
                                    .update(cx, |drafts, cx| drafts.remove(&pr, id, cx));
                            })),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(comment.body.clone()),
            );
        annotation(card.into_any_element())
    }
}
