use super::elements::MessageContainer;
use gpui::{
    div, prelude::*, px, rgb, App, Context, FocusHandle, Focusable, 
    MouseButton, MouseUpEvent, Window,
};
use gpui_component::{scroll::ScrollbarAxis, v_flex, StyledExt};
use std::sync::{Arc, Mutex};

/// MessagesView - Component responsible for displaying the message history
pub struct MessagesView {
    message_queue: Arc<Mutex<Vec<MessageContainer>>>,
    thinking_block_count: usize,
    focus_handle: FocusHandle,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<MessageContainer>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message_queue,
            thinking_block_count: 0,
            focus_handle: cx.focus_handle(),
        }
    }

    fn on_thinking_toggle(
        &mut self,
        index: usize,
        _: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Get access to the message queue
        let mut updated = false;
        let mut queue = self.message_queue.lock().unwrap();

        // Only update if we have messages
        if !queue.is_empty() {
            // Get the last message container
            let last_message = queue.last_mut().unwrap();

            // Toggle the specified thinking block
            updated = last_message.toggle_thinking_collapsed(index);
        }

        // Notify the UI to update if needed
        if updated {
            cx.notify();
        }
    }
}

impl Focusable for MessagesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessagesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get current messages to display
        let messages = {
            let lock = self.message_queue.lock().unwrap();
            lock.clone()
        };

        // Messages display area with scrollbar
        div()
            .id("messages-container")
            .flex_1() // Take remaining space in the parent container
            .min_h_0() // Minimum height to ensure scrolling works
            .relative() // For absolute positioning of scrollbar
            .child(
                v_flex()
                    .id("messages")
                    .flex_1()
                    .p_2()
                    .scrollable(cx.entity().entity_id(), ScrollbarAxis::Vertical)
                    .bg(rgb(0x303030))
                    .gap_2()
                    .text_size(px(16.))
                    .children(messages.into_iter().map(|msg| {
                        // Count thinking blocks for click handlers
                        let elements = msg.elements();
                        let thinking_blocks = elements.iter().filter(|e| {
                            matches!(e, super::elements::MessageElement::ThinkingBlock(_))
                        }).count();

                        self.thinking_block_count = thinking_blocks;

                        // Create message container with appropriate styling based on role
                        let mut message_container = div()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2();

                        if msg.is_user_message() {
                            message_container = message_container
                                .m_3()
                                .bg(rgb(0x202020))
                                .rounded_md()
                                .shadow_sm();
                        }

                        // Create message container with user badge if needed
                        let message_container = if msg.is_user_message() {
                            message_container.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .children(vec![
                                        super::file_icons::render_icon_container(
                                            &super::file_icons::get().get_type_icon(super::file_icons::TOOL_USER_INPUT),
                                            16.0,
                                            rgb(0x6BD9A8), // Greenish color for user icon
                                            "👤",
                                        )
                                        .into_any_element(),
                                        div()
                                            .font_weight(gpui::FontWeight(600.0))
                                            .text_color(rgb(0x6BD9A8))
                                            .child("You")
                                            .into_any_element(),
                                    ])
                            )
                        } else {
                            message_container
                        };

                        // Process elements and add click handlers for thinking blocks
                        let mut thinking_index = 0;

                        let elements_with_handlers = elements.into_iter().map(|element| {
                            match &element {
                                super::elements::MessageElement::ThinkingBlock(_) => {
                                    // Create a closure for this specific thinking block
                                    let current_index = thinking_index;
                                    thinking_index += 1;

                                    // Wrap the element in a div with a click handler
                                    div()
                                        .child(element)
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |view, event, window, cx| {
                                                view.on_thinking_toggle(current_index, event, window, cx);
                                            })
                                        )
                                        .into_any_element()
                                }
                                _ => {
                                    // Regular element, no special handling
                                    element.into_any_element()
                                }
                            }
                        }).collect::<Vec<_>>();

                        // Add all message elements
                        message_container.children(elements_with_handlers)
                    }))
            )
    }
}