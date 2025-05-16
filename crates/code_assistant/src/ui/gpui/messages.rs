use super::elements::MessageContainer;
use gpui::{div, prelude::*, px, rgb, App, Context, Entity, FocusHandle, Focusable, Window};
use gpui_component::{scroll::ScrollbarAxis, v_flex, ActiveTheme, StyledExt};
use std::sync::{Arc, Mutex};

/// MessagesView - Component responsible for displaying the message history
pub struct MessagesView {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    focus_handle: FocusHandle,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message_queue,
            focus_handle: cx.focus_handle(),
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

        // Get the theme colors for user messages
        let user_accent = if cx.theme().is_dark() {
            rgb(0x6BD9A8) // Dark mode user accent
        } else {
            rgb(0x0A8A55) // Light mode user accent
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
                    .bg(cx.theme().card)
                    .gap_2()
                    .text_size(px(16.))
                    .children(messages.into_iter().map(|msg| {
                        // Create message container with appropriate styling based on role
                        let mut message_container = div().p_3().flex().flex_col().gap_2();

                        if msg.read(cx).is_user_message() {
                            message_container = message_container
                                .m_3()
                                .bg(cx.theme().muted.opacity(0.3)) // Use theme muted color with opacity
                                .rounded_md()
                                .shadow_sm();
                        }

                        // Create message container with user badge if needed
                        let message_container = if msg.read(cx).is_user_message() {
                            message_container.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .children(vec![
                                        super::file_icons::render_icon_container(
                                            &super::file_icons::get()
                                                .get_type_icon(super::file_icons::TOOL_USER_INPUT),
                                            16.0,
                                            user_accent, // Use themed user accent color
                                            "👤",
                                        )
                                        .into_any_element(),
                                        div()
                                            .font_weight(gpui::FontWeight(600.0))
                                            .text_color(user_accent) // Use themed user accent color
                                            .child("You")
                                            .into_any_element(),
                                    ]),
                            )
                        } else {
                            message_container
                        };

                        // Simply render each block entity
                        let elements = msg.read(cx).elements();
                        message_container.children(elements)
                    })),
            )
    }
}
