use super::attachment::{AttachmentEvent, AttachmentView};
use super::file_icons;
use crate::persistence::DraftAttachment;
use base64::Engine;
use gpui::{
    div, prelude::*, px, ClipboardEntry, Context, CursorStyle, Entity, EventEmitter, FocusHandle,
    Focusable, MouseButton, MouseUpEvent, Render, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState, Paste, TextInput};
use gpui_component::ActiveTheme;

/// Events emitted by the InputArea component
#[derive(Clone, Debug)]
pub enum InputAreaEvent {
    /// Message submitted with content and attachments
    MessageSubmitted {
        content: String,
        attachments: Vec<DraftAttachment>,
    },
    /// Content changed (for draft saving)
    ContentChanged {
        content: String,
        attachments: Vec<DraftAttachment>,
    },
    /// Focus requested on the input
    FocusRequested,
    /// Cancel/stop requested (for agent cancellation)
    CancelRequested,
    /// Clear draft requested (before clearing input)
    ClearDraftRequested,
}

/// Self-contained input area component that handles text input and attachments
pub struct InputArea {
    text_input: Entity<InputState>,
    attachments: Vec<DraftAttachment>,
    attachment_views: Vec<Entity<AttachmentView>>,
    focus_handle: FocusHandle,

    // Agent state for button rendering
    agent_is_running: bool,
    cancel_enabled: bool,
    // Subscriptions
    _input_subscription: Subscription,
}

impl InputArea {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Create the text input
        let text_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line()
                .auto_grow(1, 8)
                .placeholder("Type your message...")
        });

        // Subscribe to text input events
        let input_subscription = cx.subscribe_in(&text_input, window, Self::on_input_event);

        Self {
            text_input,
            attachments: Vec::new(),
            attachment_views: Vec::new(),
            focus_handle: cx.focus_handle(),

            agent_is_running: false,
            cancel_enabled: false,
            _input_subscription: input_subscription,
        }
    }

    /// Set the input value and attachments (for loading drafts)
    pub fn set_content(
        &mut self,
        text: String,
        attachments: Vec<DraftAttachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Update text input
        self.text_input.update(cx, |text_input, cx| {
            text_input.set_value(text, window, cx);
        });

        // Update attachments
        self.attachments = attachments;
        self.rebuild_attachment_views(cx);
    }

    /// Clear the input content
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Clear text input
        self.text_input.update(cx, |text_input, cx| {
            text_input.set_value("", window, cx);
        });

        // Clear attachments
        self.attachments.clear();
        self.attachment_views.clear();
    }

    /// Get current content (text and attachments)
    #[allow(dead_code)]
    pub fn get_content(&self, cx: &Context<Self>) -> (String, Vec<DraftAttachment>) {
        let text = self.text_input.read(cx).value().to_string();
        (text, self.attachments.clone())
    }

    /// Update agent state for button rendering
    pub fn set_agent_state(&mut self, agent_is_running: bool, cancel_enabled: bool) {
        self.agent_is_running = agent_is_running;
        self.cancel_enabled = cancel_enabled;
    }

    /// Handle paste events (for images)
    fn on_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(clipboard_item) = cx.read_from_clipboard() {
            for entry in clipboard_item.into_entries() {
                if let ClipboardEntry::Image(image) = entry {
                    // Create a DraftAttachment from the image
                    let attachment = DraftAttachment::Image {
                        content: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                        mime_type: image.format.mime_type().to_string(),
                    };

                    self.attachments.push(attachment);

                    // Rebuild attachment views
                    self.rebuild_attachment_views(cx);

                    // Emit content changed event
                    self.emit_content_changed(cx);

                    cx.notify();
                }
            }
        }
    }

    /// Remove an attachment by index
    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.attachments.len() {
            self.attachments.remove(index);

            // Rebuild attachment views with updated indices
            self.rebuild_attachment_views(cx);

            // Emit content changed event
            self.emit_content_changed(cx);

            cx.notify();
        }
    }

    /// Rebuild attachment views when attachments change
    fn rebuild_attachment_views(&mut self, cx: &mut Context<Self>) {
        self.attachment_views.clear();

        for (index, attachment) in self.attachments.iter().enumerate() {
            let attachment_view = cx.new(|cx| AttachmentView::new(attachment.clone(), index, cx));

            // Subscribe to attachment events
            cx.subscribe(
                &attachment_view,
                |view, _attachment_view, event: &AttachmentEvent, cx| match event {
                    AttachmentEvent::Remove(index) => {
                        view.remove_attachment(*index, cx);
                    }
                },
            )
            .detach();

            self.attachment_views.push(attachment_view);
        }
    }

    /// Emit content changed event
    fn emit_content_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.text_input.read(cx).value().to_string();
        cx.emit(InputAreaEvent::ContentChanged {
            content: text,
            attachments: self.attachments.clone(),
        });
    }

    /// Handle text input events
    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                // Emit content changed event for draft saving
                self.emit_content_changed(cx);
            }
            InputEvent::Focus => {
                cx.emit(InputAreaEvent::FocusRequested);
            }
            InputEvent::Blur => {}
            InputEvent::PressEnter { secondary } => {
                // Only send message on plain ENTER (not with modifiers)
                if !secondary {
                    // Get current text (this might include the newline that was just added)
                    let current_text = self.text_input.read(cx).value().to_string();
                    // Remove trailing newline if present (from ENTER key press)
                    let cleaned_text = current_text.trim_end_matches('\n').to_string();

                    // FIRST: Clear draft before doing anything else
                    cx.emit(InputAreaEvent::ClearDraftRequested);

                    // Emit event for RootView to handle
                    cx.emit(InputAreaEvent::MessageSubmitted {
                        content: cleaned_text,
                        attachments: self.attachments.clone(),
                    });

                    // Clear the input and attachments
                    self.clear(window, cx);
                }
                // If secondary is true, do nothing - modifiers will be handled by InsertLineBreak action
            }
        }
    }

    /// Handle submit button click
    fn on_submit_click(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.text_input.read(cx).value().to_string();

        if !content.trim().is_empty() || !self.attachments.is_empty() {
            // FIRST: Clear draft before doing anything else
            cx.emit(InputAreaEvent::ClearDraftRequested);

            // Emit event for RootView to handle
            cx.emit(InputAreaEvent::MessageSubmitted {
                content: content.clone(),
                attachments: self.attachments.clone(),
            });

            // Clear the input and attachments
            self.clear(window, cx);
        }
    }

    /// Handle cancel button click
    fn on_cancel_click(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputAreaEvent::CancelRequested);
    }

    /// Render the input area
    fn render_input_area(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_input_handle = self.text_input.read(cx).focus_handle(cx);
        let is_focused = text_input_handle.is_focused(window);
        let has_input_content = !self.text_input.read(cx).value().trim().is_empty();

        div()
            .id("input-area")
            .flex_none() // Important: don't grow or shrink
            .bg(cx.theme().popover)
            .border_t_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col() // Column to accommodate attachments area
            .gap_2()
            // Attachments area - show image previews when available
            .when(!self.attachments.is_empty(), |parent| {
                parent.child(
                    div()
                        .p_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .flex()
                        .flex_row()
                        .gap_2()
                        .flex_wrap()
                        .children(self.attachment_views.iter().cloned()),
                )
            })
            // Main input row
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .p_2()
                    .gap_2()
                    .child({
                        div()
                            .flex_1()
                            .border(if is_focused { px(2.) } else { px(1.) })
                            .p(if is_focused { px(0.) } else { px(1.) })
                            .border_color(if is_focused {
                                cx.theme().primary
                            } else {
                                cx.theme().sidebar_border
                            })
                            .rounded_md()
                            .track_focus(&text_input_handle)
                            .child(TextInput::new(&self.text_input).appearance(false))
                    })
                    .children({
                        let mut buttons = Vec::new();

                        // Show both send and cancel buttons
                        // Send button - enabled when input has content
                        let send_enabled = has_input_content;
                        let mut send_button = div()
                            .size(px(40.))
                            .rounded_sm()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor(if send_enabled {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::OperationNotAllowed
                            })
                            .child(file_icons::render_icon(
                                &file_icons::get().get_type_icon(file_icons::SEND),
                                22.0,
                                if send_enabled {
                                    cx.theme().primary
                                } else {
                                    cx.theme().muted_foreground
                                },
                                ">",
                            ));

                        if send_enabled {
                            send_button = send_button
                                .hover(|s| s.bg(cx.theme().muted))
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_submit_click));
                        }
                        buttons.push(send_button);

                        // Cancel button - always visible, but enabled/disabled based on agent state
                        let mut cancel_button = div()
                            .size(px(40.))
                            .rounded_sm()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor(if self.cancel_enabled {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::OperationNotAllowed
                            })
                            .child(file_icons::render_icon(
                                &file_icons::get().get_type_icon(file_icons::STOP),
                                22.0,
                                if self.cancel_enabled {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                },
                                "⬜",
                            ));

                        if self.cancel_enabled {
                            cancel_button = cancel_button
                                .hover(|s| s.bg(cx.theme().muted))
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_cancel_click));
                        }

                        buttons.push(cancel_button);

                        buttons
                    }),
            )
    }
}

impl Focusable for InputArea {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<InputAreaEvent> for InputArea {}

impl Render for InputArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(cx.listener(Self::on_paste))
            .on_action({
                let text_input_handle = self.text_input.clone();
                move |_: &crate::ui::gpui::InsertLineBreak, window, cx| {
                    // Insert a line break at the current cursor position
                    text_input_handle.update(cx, |input_state, cx| {
                        input_state.insert("\n", window, cx);
                    });
                }
            })
            .track_focus(&self.focus_handle(cx))
            .child(self.render_input_area(window, cx))
    }
}
