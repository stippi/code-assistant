//! Line comments in the composer.
//!
//! All comments of a draft live in one [`DraftAttachment::LineComments`]
//! among the input's attachments, so they persist and send with the draft.
//! They show as one chip in the attachment row: a short preview for a single
//! comment, a count for several. Clicking the chip lists them; each entry
//! reveals its place in the right panel or removes the comment.

use super::{InputArea, InputAreaEvent};
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::persistence::DraftAttachment;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, ThemeStyled};
use gpui_kit::{
    Anchor, AnyElement, Context, Edges, SharedString, anchored, deferred, div, prelude::*, px,
};
use std::collections::BTreeSet;

/// Characters of a comment's text shown in the chip.
const PREVIEW_CHARS: usize = 40;

impl InputArea {
    /// The draft's line comments, in the order they were made.
    pub fn comments(&self) -> &[LineComment] {
        self.attachments
            .iter()
            .find_map(|a| match a {
                DraftAttachment::LineComments { comments } => Some(comments.as_slice()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn comments_mut(&mut self) -> &mut Vec<LineComment> {
        let ix = match self
            .attachments
            .iter()
            .position(|a| matches!(a, DraftAttachment::LineComments { .. }))
        {
            Some(ix) => ix,
            None => {
                self.attachments.push(DraftAttachment::LineComments {
                    comments: Vec::new(),
                });
                self.attachments.len() - 1
            }
        };
        match &mut self.attachments[ix] {
            DraftAttachment::LineComments { comments } => comments,
            _ => unreachable!("position matched a LineComments attachment"),
        }
    }

    /// Add a comment, or replace the one with the same id.
    pub fn upsert_comment(&mut self, mut comment: LineComment, cx: &mut Context<Self>) {
        let comments = self.comments_mut();
        match comments.iter_mut().find(|c| c.id == comment.id) {
            Some(existing) => *existing = comment,
            None => {
                comment.id = comments.iter().map(|c| c.id).max().unwrap_or(0) + 1;
                comments.push(comment);
            }
        }
        self.comments_changed(cx);
    }

    pub fn remove_comment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.comments_mut().retain(|c| c.id != id);
        self.comments_changed(cx);
    }

    fn clear_comments(&mut self, cx: &mut Context<Self>) {
        self.comments_mut().clear();
        self.comments_changed(cx);
    }

    /// Drop an empty comment list, save the draft and tell the panel.
    fn comments_changed(&mut self, cx: &mut Context<Self>) {
        self.attachments.retain(
            |a| !matches!(a, DraftAttachment::LineComments { comments } if comments.is_empty()),
        );
        if self.comments().is_empty() {
            self.comments_popover_open = false;
        }
        self.rebuild_attachment_views(cx);
        self.emit_content_changed(cx);
        cx.emit(InputAreaEvent::CommentsChanged {
            comments: self.comments().to_vec(),
        });
        cx.notify();
    }

    /// The chip's label: a preview of a single comment, else a count.
    pub(super) fn comments_label(comments: &[LineComment]) -> String {
        match comments {
            [] => String::new(),
            [c] => {
                let text: String = c.text.chars().take(PREVIEW_CHARS).collect();
                let ellipsis = if c.text.chars().count() > PREVIEW_CHARS {
                    "…"
                } else {
                    ""
                };
                format!("{}:{} · {text}{ellipsis}", c.file_name(), c.lines_label())
            }
            _ => {
                let files: BTreeSet<_> = comments.iter().map(|c| &c.file).collect();
                let in_files = if files.len() == 1 {
                    format!("in {}", comments[0].file_name())
                } else {
                    format!("in {} files", files.len())
                };
                format!("{} comments {in_files}", comments.len())
            }
        }
    }

    pub(super) fn render_comments_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let comments = self.comments();
        if comments.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let label = Self::comments_label(comments);
        Some(
            div()
                .relative()
                .child(
                    div()
                        .id("comments-chip")
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .h(px(28.))
                        .max_w(px(360.))
                        .pl_2()
                        .pr_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.muted))
                        .child(
                            Icon::default()
                                .path(SharedString::from("icons/message_bubbles.svg"))
                                .with_size(Size::XSmall)
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(theme.foreground)
                                .child(label),
                        )
                        .child(
                            div()
                                .id("comments-chip-clear")
                                .flex_none()
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .hover(|s| s.bg(theme.danger.opacity(0.1)))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/close.svg"))
                                        .with_size(Size::XSmall)
                                        .text_color(theme.muted_foreground),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.clear_comments(cx);
                                })),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.comments_popover_open = !this.comments_popover_open;
                            cx.notify();
                        })),
                )
                .children(
                    self.comments_popover_open
                        .then(|| self.render_comments_popover(cx)),
                )
                .into_any_element(),
        )
    }

    fn render_comments_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let items: Vec<AnyElement> = self
            .comments()
            .iter()
            .map(|c| {
                let first_line = c
                    .excerpt
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or_default()
                    .to_owned();
                let reveal = c.clone();
                let id = c.id;
                div()
                    .id(("comment-entry", c.id as usize))
                    .flex()
                    .items_start()
                    .gap_2()
                    .p_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{}:{}", c.file_name(), c.lines_label())),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .font_family("Menlo")
                                    .text_color(theme.muted_foreground)
                                    .child(first_line),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.foreground)
                                    .child(c.text.clone()),
                            ),
                    )
                    .child(
                        div()
                            .id(("comment-remove", c.id as usize))
                            .flex_none()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .hover(|s| s.bg(theme.danger.opacity(0.1)))
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/trash.svg"))
                                    .with_size(Size::XSmall)
                                    .text_color(theme.danger),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_comment(id, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.comments_popover_open = false;
                        cx.emit(InputAreaEvent::RevealComment {
                            comment: reveal.clone(),
                        });
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        deferred(
            anchored()
                .anchor(Anchor::BottomLeft)
                .snap_to_window_with_margin(Edges::all(px(8.)))
                .child(
                    div()
                        .id("comments-popover")
                        .occlude()
                        .popover_style(cx)
                        .shadow_md()
                        .p_1()
                        .mb_1()
                        .w(px(380.))
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.comments_popover_open = false;
                            cx.notify();
                        }))
                        .children(items),
                ),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn comment(file: &str, text: &str) -> LineComment {
        LineComment {
            id: 0,
            file: PathBuf::from(file),
            start_line: 3,
            end_line: 5,
            old_side: false,
            in_diff: false,
            excerpt: "x".into(),
            text: text.into(),
        }
    }

    #[test]
    fn label_previews_one_comment_and_counts_several() {
        assert_eq!(
            InputArea::comments_label(&[comment("/p/src/lib.rs", "no unwrap here")]),
            "lib.rs:3-5 · no unwrap here"
        );
        let long = "a".repeat(50);
        assert!(InputArea::comments_label(&[comment("/p/a.rs", &long)]).ends_with("a…"));
        assert_eq!(
            InputArea::comments_label(&[comment("/p/a.rs", "x"), comment("/p/a.rs", "y")]),
            "2 comments in a.rs"
        );
        assert_eq!(
            InputArea::comments_label(&[
                comment("/p/a.rs", "x"),
                comment("/p/b.rs", "y"),
                comment("/p/b.rs", "z")
            ]),
            "3 comments in 2 files"
        );
    }
}
