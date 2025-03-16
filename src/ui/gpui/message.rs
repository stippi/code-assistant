use super::elements::MessageContainer;
use super::input::TextInput;
use super::memory_view::MemoryView;
use super::scrollbar::{Scrollbar, ScrollbarState};
use gpui::{
    div, prelude::*, px, rgb, white, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    MouseButton, MouseUpEvent, ScrollHandle,
};
use std::sync::{Arc, Mutex};

// Message View - combines input area and message display
pub struct MessageView {
    pub text_input: Entity<TextInput>,
    memory_view: Entity<MemoryView>,
    recent_keystrokes: Vec<gpui::Keystroke>,
    focus_handle: FocusHandle,
    input_value: Arc<Mutex<Option<String>>>,
    message_queue: Arc<Mutex<Vec<MessageContainer>>>,
    input_requested: Arc<Mutex<bool>>,
    // Add scroll handle for messages
    messages_scroll_handle: ScrollHandle,
}

impl MessageView {
    pub fn new(
        text_input: Entity<TextInput>,
        memory_view: Entity<MemoryView>,
        cx: &mut Context<Self>,
        input_value: Arc<Mutex<Option<String>>>,
        message_queue: Arc<Mutex<Vec<MessageContainer>>>,
        input_requested: Arc<Mutex<bool>>,
    ) -> Self {
        Self {
            text_input,
            memory_view,
            recent_keystrokes: vec![],
            focus_handle: cx.focus_handle(),
            input_value,
            message_queue,
            input_requested,
            // Initialize scroll handle
            messages_scroll_handle: ScrollHandle::new(),
        }
    }

    fn on_reset_click(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.recent_keystrokes.clear();
        self.text_input
            .update(cx, |text_input, _cx| text_input.reset());
        cx.notify();
    }

    fn on_submit_click(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.update(cx, |text_input, _cx| {
            let content = text_input.get_content();
            if !content.is_empty() {
                // Store input in the shared value
                let mut input_value = self.input_value.lock().unwrap();
                *input_value = Some(content);

                // Clear the input field
                text_input.reset();
            }
        });
        cx.notify();
    }
}

impl Focusable for MessageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessageView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get current messages to display
        let messages = {
            let lock = self.message_queue.lock().unwrap();
            lock.clone()
        };

        // Check if input is requested
        let is_input_requested = *self.input_requested.lock().unwrap();

        // Create scrollbar state for messages
        let messages_scrollbar_state =
            ScrollbarState::new(self.messages_scroll_handle.clone()).parent_entity(&cx.entity());

        div()
            .bg(rgb(0x2c2c2c))
            .track_focus(&self.focus_handle(cx))
            .flex()
            .flex_row() // Main container as row layout
            .w_full() // Constrain to window width
            .h_full() // Take full height
            .pt_8() // Leave room for the window title bar
            .child(
                // Left side with messages and input (content area)
                div()
                    .flex()
                    .flex_col()
                    .flex_grow() // Grow to take available space
                    .flex_shrink() // Allow shrinking if needed
                    .overflow_hidden() // Prevent overflow
                    .child(
                        // Messages display area with scrollbar
                        div()
                            .id("messages-container")
                            .flex_1() // Take remaining space in the parent container
                            .min_h(px(100.)) // Minimum height to ensure scrolling works
                            .relative() // For absolute positioning of scrollbar
                            .child(
                                div()
                                    .id("messages")
                                    .size_full() // Fill the parent container
                                    .p_2()
                                    .overflow_y_scroll() // Enable vertical scrolling
                                    .track_scroll(&self.messages_scroll_handle) // Track scrolling
                                    .bg(rgb(0x202020))
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(messages.into_iter().map(|msg| {
                                        div()
                                            .bg(rgb(0x303030))
                                            .p_3()
                                            .rounded_md()
                                            .shadow_sm()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .children(
                                                msg.elements().into_iter().map(|element| element),
                                            )
                                    })),
                            )
                            // Add scrollbar
                            .child(match Scrollbar::vertical(messages_scrollbar_state) {
                                Some(scrollbar) => div()
                                    .absolute()
                                    .right(px(0.))
                                    .top(px(0.))
                                    .h_full()
                                    .w(px(12.))
                                    .child(scrollbar)
                                    .into_any_element(),
                                None => div().w(px(0.)).h(px(0.)).into_any_element(),
                            }),
                    )
                    .child(
                        // Input area - ensure this doesn't get pushed out
                        div()
                            .id("input-area")
                            .flex_none() // Important: don't grow or shrink
                            .bg(rgb(0x303030))
                            .border_t_1()
                            .border_color(rgb(0x404040))
                            .flex()
                            .flex_row()
                            .justify_between()
                            .items_center()
                            .p_2()
                            .gap_2()
                            .child(div().flex_1().child(self.text_input.clone()))
                            .child(
                                div()
                                    .border_1()
                                    .border_color(rgb(0x505050))
                                    .rounded_md()
                                    .px_3()
                                    .py_1()
                                    .bg(if is_input_requested {
                                        rgb(0x3355bb)
                                    } else {
                                        rgb(0xc0c0c0)
                                    })
                                    .cursor(if is_input_requested {
                                        CursorStyle::PointingHand
                                    } else {
                                        CursorStyle::OperationNotAllowed
                                    })
                                    .text_color(white())
                                    .font_weight(gpui::FontWeight(600.0))
                                    .child("Submit")
                                    .when(is_input_requested, |style| {
                                        style.hover(|s| s.bg(rgb(0x4466cc))).on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::on_submit_click),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .border_1()
                                    .border_color(rgb(0x505050))
                                    .rounded_md()
                                    .px_3()
                                    .py_1()
                                    .bg(rgb(0x553333))
                                    .text_color(white())
                                    .cursor_pointer()
                                    .font_weight(gpui::FontWeight(600.0))
                                    .child("Clear")
                                    .hover(|style| style.bg(rgb(0x664444)))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::on_reset_click),
                                    ),
                            ),
                    ),
            )
            .child(
                // Right side with memory view - now using flex_none to ensure it takes its natural width
                div()
                    .h_full()
                    .flex_none() // Don't flex, use exact width
                    .child(self.memory_view.clone()),
            )
    }
}
