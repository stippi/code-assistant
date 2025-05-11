use super::elements::MessageContainer;
use super::file_icons;
// Using new TextInput from gpui-component
use super::memory_view::MemoryView;
use super::scrollbar::{Scrollbar, ScrollbarState};
use super::CloseWindow;
use gpui::{
    div, prelude::*, px, rgb, white, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    MouseButton, MouseUpEvent, ScrollHandle,
};
use gpui_component::input::TextInput;
use gpui_component::ContextModal;
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
    // Scroll handle for messages
    messages_scroll_handle: ScrollHandle,
    // Track the number of thinking blocks for click handling
    thinking_block_count: usize,
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
            thinking_block_count: 0,
        }
    }

    fn on_reset_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.recent_keystrokes.clear();
        self.text_input
            .update(cx, |text_input, cx| text_input.set_text("", window, cx));
        cx.notify();
    }

    fn on_submit_click(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.update(cx, |text_input, cx| {
            let content = text_input.text().to_string();
            if !content.is_empty() {
                // Store input in the shared value
                let mut input_value = self.input_value.lock().unwrap();
                *input_value = Some(content);

                // Clear the input field
                text_input.set_text("", window, cx);
            }
        });
        cx.notify();
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

    // Toggle the memory drawer
    fn toggle_memory_drawer(
        &mut self,
        _: &MouseUpEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Check if drawer is already open and close it if necessary
        if window.has_active_drawer(cx) {
            window.close_drawer(cx);
        } else {
            // Klone das memory_view vor der Closure
            let memory_view = self.memory_view.clone();

            // Open a new drawer with the memory view
            window.open_drawer(cx, move |drawer, _window, _cx| {
                drawer
                    .size(px(300.0))
                    .title("Working Memory")
                    .child(memory_view.clone())
            });
        }
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
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
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
                                    .text_size(px(18.))
                                    .children(messages.into_iter().map(|msg| {
                                        // Count thinking blocks for click handlers
                                        let elements = msg.elements();
                                        let thinking_blocks = elements.iter().filter(|e| {
                                            matches!(e, super::elements::MessageElement::ThinkingBlock(_))
                                        }).count();

                                        self.thinking_block_count = thinking_blocks;

                                        // Create message container with appropriate styling based on role
                                        let message_container = div()
                                            .bg(rgb(0x303030))
                                            .p_3()
                                            .rounded_md()
                                            .shadow_sm()
                                            .flex()
                                            .flex_col()
                                            .gap_2();

                                        // Create message container with user badge if needed
                                        let message_container = if msg.is_user_message() {
                                            message_container.child(
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap_2()
                                                    .children(vec![
                                                        file_icons::render_icon_container(
                                                            &file_icons::get().get_type_icon(file_icons::TOOL_USER_INPUT),
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
                                                                // Always trigger the toggle - since we can't easily identify clickable elements
                                                                // The click handler is attached to the entire thinking block anyway
                                                                view.on_thinking_toggle(current_index, event, window, cx);
                                                            })
                                                        )
                                                        .into_any_element()
                                                },
                                                _ => element.into_element(),
                                            }
                                        }).collect::<Vec<_>>();

                                        message_container.children(elements_with_handlers)
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
                // Right side control for memory view drawer
                div()
                    .h_full()
                    .flex_none() // Don't flex, use exact width
                    .w(px(40.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_start()
                    .py_4()
                    .border_l_1()
                    .border_color(rgb(0x404040))
                    .child(
                        div()
                            .rounded_full()
                            .size(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(0x353535)))
                            .child(file_icons::render_icon(
                                &file_icons::get().get_type_icon(file_icons::WORKING_MEMORY),
                                24.0,
                                rgb(0xAAAAAA),
                                "🧠"
                            ))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::toggle_memory_drawer))
                    )
            )
    }
}
