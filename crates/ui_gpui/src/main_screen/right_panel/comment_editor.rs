//! The box in which the user writes or edits one line comment, shown at the
//! bottom of the view the lines were selected in. Cmd/Ctrl-Enter saves,
//! Escape cancels.

use code_assistant_core::line_comments::LineComment;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, Render, Subscription, Window, div,
    prelude::*,
};

/// A view's change to the draft's comments.
#[derive(Clone, Debug)]
pub enum CommentChange {
    /// Add (`id` 0) or replace a comment.
    Upsert(LineComment),
    Remove(u64),
}

pub enum CommentEditorEvent {
    /// The comment with the entered text.
    Save(LineComment),
    /// Remove the (existing) comment.
    Delete(u64),
    Cancel,
}

pub struct CommentEditor {
    /// The comment being written; `id` 0 for a new one.
    comment: LineComment,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

impl EventEmitter<CommentEditorEvent> for CommentEditor {}

impl CommentEditor {
    pub fn new(comment: LineComment, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = comment.text.clone();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Comment on these lines…")
                .default_value(text)
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.save(cx);
            }
        });
        Self {
            comment,
            input,
            _subscription: subscription,
        }
    }

    pub fn comment(&self) -> &LineComment {
        &self.comment
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let mut comment = self.comment.clone();
        comment.text = text;
        cx.emit(CommentEditorEvent::Save(comment));
    }
}

impl Focusable for CommentEditor {
    fn focus_handle(&self, cx: &gpui_kit::App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for CommentEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let existing = self.comment.id != 0;
        let id = self.comment.id;
        let title = format!(
            "{} {}:{}",
            if existing {
                "Edit comment on"
            } else {
                "Comment on"
            },
            self.comment.file_name(),
            self.comment.lines_label()
        );
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .capture_action(cx.listener(|_, _: &Escape, _, cx| {
                cx.emit(CommentEditorEvent::Cancel);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(title),
            )
            .child(Textarea::new(&self.input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(existing, |d| {
                        d.child(
                            Button::new("comment-delete")
                                .label("Delete")
                                .xsmall()
                                .ghost()
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(CommentEditorEvent::Delete(id));
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("comment-cancel")
                            .label("Cancel")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(CommentEditorEvent::Cancel);
                            })),
                    )
                    .child(
                        Button::new("comment-save")
                            .label(if existing { "Update" } else { "Add comment" })
                            .xsmall()
                            .primary()
                            .tooltip("⌘↵")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
