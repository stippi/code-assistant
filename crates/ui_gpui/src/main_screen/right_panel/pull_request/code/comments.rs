//! New comments on the diff: rows selected with the shared line selection
//! show the pill (copy, comment); commenting opens a box below the last
//! row, from which the comment goes to the pending review ("Add to
//! review", a dashed card on its line until the review is submitted) or to
//! the agent's draft ("Add to agent", like the Review view's line
//! comments). Only on the whole change: a commit's lines are not the ones
//! GitHub anchors comments to.

use super::CodeTab;
use super::anchors::{RangeRef, range_of};
use super::body::annotation;
use super::pages::LoadedFile;
use crate::comments::{CommentChange, floating, selection_pill};
use crate::diff::model::DiffLine;
use code_assistant_core::line_comments::LineComment;
use github::model::DiffSide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, Subscription, Window, div, point, prelude::*, px,
};
use similar::ChangeTag;
use std::ops::Range;
use std::path::PathBuf;

use super::super::review_draft::PendingComment;

/// The box a new comment is written in, below the last selected row.
pub(super) struct Draft {
    pub path: String,
    /// The commented rows, flat line indices, inclusive.
    pub lo: usize,
    pub hi: usize,
    pub at: RangeRef,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

/// A file's rows in flat order (as the selection counts them).
fn flat_lines(file: &LoadedFile) -> impl Iterator<Item = &DiffLine> {
    file.diff.hunks.iter().flat_map(|hunk| &hunk.lines)
}

/// Rows `lo..=hi` as shown, `+`/`-`/` ` prefixed.
fn excerpt(file: &LoadedFile, lo: usize, hi: usize) -> String {
    flat_lines(file)
        .skip(lo)
        .take(hi + 1 - lo)
        .map(|line| {
            let prefix = match line.tag {
                ChangeTag::Insert => '+',
                ChangeTag::Delete => '-',
                ChangeTag::Equal => ' ',
            };
            format!("{prefix}{}", line.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// "12" or "10-12", the lines of `at`, `L` for the old file's.
fn lines_label(at: &RangeRef) -> String {
    let side = |side: DiffSide| match side {
        DiffSide::Left => "L",
        DiffSide::Right => "",
    };
    match at.start {
        Some(start) if start != at.end => format!(
            "{}{}-{}{}",
            side(start.side),
            start.line,
            side(at.end.side),
            at.end.line
        ),
        _ => format!("{}{}", side(at.end.side), at.end.line),
    }
}

/// The agent's line comment on rows `lo..=hi` of `path`: their new-file
/// lines, or the old file's when only deleted rows are selected.
fn agent_comment(
    path: &str,
    numbers: &[(ChangeTag, usize, usize)],
    lo: usize,
    hi: usize,
    excerpt: String,
    text: String,
) -> Option<LineComment> {
    let rows = numbers.get(lo..=hi)?;
    let new_side: Vec<usize> = rows
        .iter()
        .filter(|(tag, ..)| *tag != ChangeTag::Delete)
        .map(|&(_, _, new)| new)
        .collect();
    let (old_side, lines) = if new_side.is_empty() {
        (true, rows.iter().map(|&(_, old, _)| old).collect())
    } else {
        (false, new_side)
    };
    Some(LineComment {
        id: 0,
        file: PathBuf::from(path),
        start_line: *lines.iter().min()?,
        end_line: *lines.iter().max()?,
        old_side,
        in_diff: true,
        on_message: false,
        on_file: false,
        excerpt,
        text,
    })
}

impl CodeTab {
    fn selected_file(&self) -> Option<(&LoadedFile, usize, usize)> {
        let selected = self.selection.get()?;
        let file = self.file(self.shown[self.shown_ix(&selected.key)?]);
        let (lo, hi) = selected.range();
        let hi = hi.min(file.numbers.len().checked_sub(1)?);
        (lo <= hi).then_some((file, lo, hi))
    }

    pub(super) fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((file, lo, hi)) = self.selected_file() {
            let text = flat_lines(file)
                .skip(lo)
                .take(hi + 1 - lo)
                .map(|line| line.text.as_ref())
                .collect::<Vec<_>>()
                .join("\n");
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Open the comment box below the selection.
    pub(super) fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit.is_some() {
            return;
        }
        let Some((file, lo, hi)) = self.selected_file() else {
            return;
        };
        let Some(at) = range_of(&file.numbers, lo, hi) else {
            return;
        };
        let path = file.path.clone();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Add a comment…")
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.add_to_review(cx);
            }
        });
        self.draft = Some(Draft {
            path,
            lo,
            hi,
            at,
            input,
            _subscription: subscription,
        });
        self.place_annotations(cx);
    }

    fn draft_text(&self, cx: &Context<Self>) -> Option<String> {
        let draft = self.draft.as_ref()?;
        let text = draft.input.read(cx).value().trim().to_owned();
        (!text.is_empty()).then_some(text)
    }

    /// The draft's comment into the pending review.
    pub(super) fn add_to_review(&mut self, cx: &mut Context<Self>) {
        let Some(body) = self.draft_text(cx) else {
            return;
        };
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let Some(file) = self
            .shown_ix(&draft.path)
            .map(|ix| self.file(self.shown[ix]))
        else {
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
            excerpt: excerpt(file, draft.lo, draft.hi),
        };
        let pr = self.pr.clone();
        self.drafts
            .update(cx, |drafts, cx| drafts.add(&pr, comment, cx));
        self.close_draft(cx);
    }

    /// The draft's comment into the agent's draft (the composer).
    fn add_to_agent(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.draft_text(cx) else {
            return;
        };
        let comment = self.draft.as_ref().and_then(|draft| {
            let file = self.file(self.shown[self.shown_ix(&draft.path)?]);
            agent_comment(
                &draft.path,
                &file.numbers,
                draft.lo,
                draft.hi,
                excerpt(file, draft.lo, draft.hi),
                text,
            )
        });
        if let Some(comment) = comment {
            crate::comments::report(CommentChange::Upsert(comment), cx);
        }
        self.close_draft(cx);
    }

    fn close_draft(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        self.selection.clear();
        self.place_annotations(cx);
    }

    /// Rows of a chunk (relative to it) a pending comment is on.
    pub(super) fn pending_marks(
        &self,
        file: &LoadedFile,
        base: usize,
        rows: usize,
        cx: &Context<Self>,
    ) -> Vec<Range<usize>> {
        let chunk = base..base + rows;
        self.drafts
            .read(cx)
            .comments(&self.pr)
            .iter()
            .filter(|c| c.path == file.path)
            .filter_map(|c| {
                let end = file.lines.row(super::anchors::LineRef {
                    side: c.side,
                    line: c.line,
                })?;
                let start = c
                    .start_side
                    .zip(c.start_line)
                    .and_then(|(side, line)| file.lines.row(super::anchors::LineRef { side, line }))
                    .unwrap_or(end);
                let covered = start.max(chunk.start)..(end + 1).min(chunk.end);
                (covered.start < covered.end).then(|| covered.start - base..covered.end - base)
            })
            .collect()
    }

    /// Whether the chunk of `path` from flat line `base` holds the
    /// selection's last row.
    pub(super) fn anchors_selection_end(&self, path: &String, base: usize, rows: usize) -> bool {
        self.selection.get().is_some_and(|sel| {
            let (_, hi) = sel.range();
            &sel.key == path && (base..base + rows).contains(&hi)
        })
    }

    /// The pill under the selection's last row (not while the comment box
    /// is open).
    pub(super) fn render_floating(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Written by the last paint; taken so a row scrolled away hides it.
        let row = self.selection_anchor.take();
        let dismissed = self.selection.get() == self.pill_dismissed.as_ref();
        let position = row
            .filter(|_| {
                self.draft.is_none()
                    && self.selection.get().is_some()
                    && !self.selection.is_dragging()
                    && !dismissed
            })
            .map(|b| point(b.right() - px(72.), b.bottom() + px(2.)));
        self.anchor.track(position, window);
        let view = cx.entity().downgrade();
        let view_for_comment = view.clone();
        let dismiss = cx.listener(|this, _: &gpui_kit::MouseDownEvent, _, cx| {
            this.pill_dismissed = this.selection.get().cloned();
            cx.notify();
        });
        Some(floating(
            position?,
            div().on_mouse_down_out(dismiss).child(selection_pill(
                "pr-code-selection",
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

    /// The comment box row.
    pub(super) fn render_draft(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = &self.draft else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let card = div()
            .id("pr-code-draft")
            .debug_selector(|| "pr-code-draft".into())
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.close_draft(cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "Comment on {}:{}",
                        draft.path,
                        lines_label(&draft.at)
                    )),
            )
            .child(Textarea::new(&draft.input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().flex_1())
                    .child(
                        Button::new("pr-code-draft-cancel")
                            .label("Cancel")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.close_draft(cx))),
                    )
                    .child(
                        Button::new("pr-code-draft-agent")
                            .label("Add to agent")
                            .xsmall()
                            .outline()
                            .on_click(cx.listener(|this, _, _, cx| this.add_to_agent(cx))),
                    )
                    .child(
                        Button::new("pr-code-draft-review")
                            .label("Add to review")
                            .xsmall()
                            .primary()
                            .tooltip("⌘↵")
                            .on_click(cx.listener(|this, _, _, cx| this.add_to_review(cx))),
                    ),
            );
        annotation(card.into_any_element())
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
                .map(|(side, line)| super::anchors::LineRef { side, line }),
            end: super::anchors::LineRef {
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

#[cfg(test)]
mod tests {
    use super::super::anchors::LineRef;
    use super::*;
    use ChangeTag::*;

    #[test]
    fn agent_comments_take_new_lines_or_else_old_ones() {
        let numbers = [
            (Equal, 10, 10),
            (Delete, 11, 11),
            (Insert, 12, 11),
            (Equal, 12, 12),
        ];
        let comment = agent_comment("src/a.rs", &numbers, 0, 2, "x".into(), "why".into()).unwrap();
        assert_eq!(
            (comment.start_line, comment.end_line, comment.old_side),
            (10, 11, false)
        );
        assert_eq!(comment.file, PathBuf::from("src/a.rs"));
        assert!(comment.in_diff);
        let deleted = agent_comment("src/a.rs", &numbers, 1, 1, "x".into(), "why".into()).unwrap();
        assert_eq!(
            (deleted.start_line, deleted.end_line, deleted.old_side),
            (11, 11, true)
        );
        assert!(agent_comment("src/a.rs", &numbers, 2, 7, "x".into(), "y".into()).is_none());
    }

    #[test]
    fn line_labels_name_the_old_side() {
        let right = |line| LineRef {
            side: DiffSide::Right,
            line,
        };
        let left = |line| LineRef {
            side: DiffSide::Left,
            line,
        };
        assert_eq!(
            lines_label(&RangeRef {
                start: None,
                end: right(12)
            }),
            "12"
        );
        assert_eq!(
            lines_label(&RangeRef {
                start: Some(left(11)),
                end: right(13)
            }),
            "L11-13"
        );
    }
}
