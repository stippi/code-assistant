//! Comments on passages of chat messages.
//!
//! Releasing the mouse over selected text in a message shows the selection
//! pill there; its comment button opens the comment card at the same spot.
//! A comment carries the selected passage (Markdown source) as its quote.
//! A message containing the quote of a pending comment gets a border and a
//! badge; the badge opens the comment again.

use super::BlockView;
use crate::comments::editor::{CommentEditor, CommentEditorEvent};
use crate::comments::{CommentChange, floating, message_comments, report, selection_pill};
use code_assistant_core::line_comments::LineComment;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Pixels, Point, SharedString, Window, div, point,
    prelude::*, px,
};

/// Where a card opened from the badge floats, relative to the block.
const BADGE_CARD_OFFSET: Point<Pixels> = Point {
    x: px(16.),
    y: px(24.),
};

impl BlockView {
    /// The text of a text block.
    pub(crate) fn text_content(&self) -> Option<&str> {
        match &*self.block {
            super::BlockData::TextBlock(text) => Some(&text.content),
            _ => None,
        }
    }

    /// The selected passage of this block's text, if any.
    fn current_selection(&self, cx: &gpui_kit::App) -> Option<String> {
        let text = self.markdown_state.as_ref()?.read(cx).selected_text();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// The mouse was released over the text: show the pill where it was
    /// released when text is selected now.
    pub(super) fn on_text_mouse_up(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let passage = self.current_selection(cx);
        let offset = self
            .block_origin
            .get()
            .filter(|_| passage.is_some())
            .map(|origin| position - origin);
        if offset != self.selection_offset || passage != self.selected_passage {
            self.selection_offset = offset;
            self.selected_passage = passage;
            cx.notify();
        }
    }

    /// Pending comments whose quote is in `content`.
    pub(super) fn comments_on(content: &str, cx: &gpui_kit::App) -> Vec<LineComment> {
        message_comments(cx)
            .into_iter()
            .filter(|c| !c.excerpt.trim().is_empty() && content.contains(c.excerpt.trim()))
            .collect()
    }

    /// Open `comment` (new or existing) in a card at `offset` from the block.
    pub(crate) fn open_comment_editor(
        &mut self,
        comment: LineComment,
        offset: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| CommentEditor::new(comment, window, cx));
        self._comment_editor_subscription = Some(cx.subscribe(
            &editor,
            |this, _, event: &CommentEditorEvent, cx| {
                match event {
                    CommentEditorEvent::Save(comment) => {
                        report(CommentChange::Upsert(comment.clone()), cx)
                    }
                    CommentEditorEvent::Delete(id) => report(CommentChange::Remove(*id), cx),
                    CommentEditorEvent::Cancel => {}
                }
                this.comment_editor = None;
                this._comment_editor_subscription = None;
                this.selection_offset = None;
                cx.notify();
            },
        ));
        self.comment_editor = Some((editor, offset));
        cx.notify();
    }

    /// Open `comment` from outside (the composer's list).
    pub(crate) fn reveal_comment(
        &mut self,
        comment: LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_comment_editor(comment, BADGE_CARD_OFFSET, window, cx);
    }

    /// The pill or the card, floating at its place in the block.
    pub(super) fn render_text_floating(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let origin = self.block_origin.get();
        if let Some((editor, offset)) = &self.comment_editor {
            let position = origin.map(|o| o + *offset + point(px(0.), px(6.)));
            self.anchor.track(position, window);
            return position.map(|p| floating(p, editor.clone()));
        }
        let position = origin
            .zip(self.selection_offset)
            .map(|(o, offset)| o + offset + point(px(0.), px(6.)));
        self.anchor.track(position, window);
        let view = cx.entity().downgrade();
        let view_for_comment = view.clone();
        // A press anywhere else dismisses the pill. It does not follow the
        // window's text selection, which a press on the pill itself may clear
        // before its click lands.
        let dismiss = cx.listener(|this, _: &gpui_kit::MouseDownEvent, _, cx| {
            this.selection_offset = None;
            this.selected_passage = None;
            cx.notify();
        });
        Some(floating(
            position?,
            div().on_mouse_down_out(dismiss).child(selection_pill(
                SharedString::from(format!("message-selection-{}", cx.entity_id())),
                move |_, cx| {
                    view.update(cx, |view, cx| {
                        if let Some(text) = view.selected_passage.clone() {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    })
                    .ok();
                },
                move |window, cx| {
                    view_for_comment
                        .update(cx, |view, cx| {
                            let (Some(text), Some(offset)) =
                                (view.selected_passage.clone(), view.selection_offset)
                            else {
                                return;
                            };
                            view.open_comment_editor(
                                LineComment::on_message(text),
                                offset,
                                window,
                                cx,
                            );
                        })
                        .ok();
                },
                cx,
            )),
        ))
    }

    /// A small badge for the comments on this block; opens the first.
    pub(super) fn render_comment_badge(
        &self,
        comments: &[LineComment],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let first = comments.first()?.clone();
        let theme = cx.theme();
        Some(
            div()
                .id("message-comment-badge")
                .absolute()
                .top_1()
                .right(px(30.))
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .h(px(22.))
                .rounded(px(6.))
                .border_1()
                .border_color(theme.warning.opacity(0.6))
                .bg(theme.background.opacity(0.9))
                .cursor_pointer()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    gpui_kit::svg()
                        .size(px(12.))
                        .path("icons/message_bubbles.svg")
                        .text_color(theme.warning),
                )
                .child(comments.len().to_string())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_comment_editor(first.clone(), BADGE_CARD_OFFSET, window, cx);
                }))
                .into_any_element(),
        )
    }
}
