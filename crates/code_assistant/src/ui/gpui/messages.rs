use super::elements::MessageContainer;
use gpui::{
    bounce, div, ease_in_out, percentage, prelude::*, px, rgb, svg, Animation, AnimationExt, App,
    Context, Entity, FocusHandle, Focusable, SharedString, Transformation, Window,
};
use gpui_component::{v_flex, ActiveTheme};
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

        // Render the messages content (this will be wrapped by AutoScrollContainer)
        v_flex()
            .id("messages")
            .p_2()
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

                // Render all block elements
                let elements = msg.read(cx).elements();
                let mut container_children = vec![];

                // Add all existing blocks
                for element in elements {
                    container_children.push(element.into_any_element());
                }

                // Add loading indicator if waiting for content
                if msg.read(cx).is_waiting_for_content() {
                    container_children.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .p_2()
                            .child(
                                svg()
                                    .size(px(16.))
                                    .path(SharedString::from("icons/arrow_circle.svg"))
                                    .text_color(cx.theme().info)
                                    .with_animation(
                                        "loading_indicator",
                                        Animation::new(std::time::Duration::from_secs(2))
                                            .repeat()
                                            .with_easing(bounce(ease_in_out)),
                                        |svg, delta| {
                                            svg.with_transformation(Transformation::rotate(
                                                percentage(delta),
                                            ))
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().info)
                                    .text_size(px(12.))
                                    .child("Waiting for response..."),
                            )
                            .into_any_element(),
                    );
                }

                message_container.children(container_children)
            }))
    }
}
