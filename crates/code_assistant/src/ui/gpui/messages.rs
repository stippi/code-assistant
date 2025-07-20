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
    current_session_activity_state:
        Arc<Mutex<Option<crate::session::instance::SessionActivityState>>>,
    current_pending_message: Arc<Mutex<Option<String>>>,
    focus_handle: FocusHandle,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        current_session_activity_state: Arc<
            Mutex<Option<crate::session::instance::SessionActivityState>>,
        >,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message_queue,
            current_session_activity_state,
            current_pending_message: Arc::new(Mutex::new(None)),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Update the pending message for the current session
    pub fn update_pending_message(&self, message: Option<String>) {
        *self.current_pending_message.lock().unwrap() = message;
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

        // Collect all message elements first
        let message_elements: Vec<_> = messages
            .into_iter()
            .map(|msg| {
                // Create message container with appropriate styling based on role
                let mut message_container = div().p_3();

                if msg.read(cx).is_user_message() {
                    message_container = message_container
                        .m_3()
                        .bg(cx.theme().muted.opacity(0.3)) // Use theme muted color with opacity
                        .border_1()
                        .border_color(cx.theme().border)
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

                // Render all block elements (but no longer render waiting content here)
                let elements = msg.read(cx).elements();
                let container_children: Vec<_> = elements
                    .into_iter()
                    .map(|element| element.into_any_element())
                    .collect();

                message_container.children(container_children)
            })
            .collect();

        // Create the base messages container
        let mut messages_container = v_flex()
            .id("messages")
            .p_2()
            .bg(cx.theme().card)
            .gap_2()
            .text_size(px(16.))
            .children(message_elements);

        // Add pending message display if there is one
        if let Some(pending_message) = self.current_pending_message.lock().unwrap().clone() {
            if !pending_message.is_empty() {
                // Create a pending message container styled like a user message but with different visual cues
                messages_container = messages_container.child(
                    div()
                        .m_3()
                        .bg(cx.theme().muted.opacity(0.2)) // Lighter than regular user messages
                        .border_1()
                        .border_color(cx.theme().warning) // Use warning color to indicate pending
                        .rounded_md()
                        .shadow_sm()
                        .p_3()
                        .child(
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
                                        cx.theme().warning,
                                        "👤",
                                    )
                                    .into_any_element(),
                                    div()
                                        .font_weight(gpui::FontWeight(600.0))
                                        .text_color(cx.theme().warning)
                                        .child("Pending")
                                        .into_any_element(),
                                ]),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_color(cx.theme().foreground.opacity(0.8))
                                .child(gpui_component::text::TextView::markdown(
                                    "pending-message",
                                    pending_message,
                                )),
                        ),
                );
            }
        }

        // Add waiting UI based on current session activity state (below all messages)
        let current_activity_state = self.current_session_activity_state.lock().unwrap().clone();
        if let Some(activity_state) = current_activity_state {
            if matches!(
                activity_state,
                crate::session::instance::SessionActivityState::WaitingForResponse
                    | crate::session::instance::SessionActivityState::RateLimited { .. }
            ) {
                let (message_text, icon_color) = match activity_state {
                    crate::session::instance::SessionActivityState::RateLimited {
                        seconds_remaining,
                    } => (
                        format!("Rate limited - retrying in {}s...", seconds_remaining),
                        cx.theme().warning,
                    ),
                    crate::session::instance::SessionActivityState::WaitingForResponse => {
                        ("Waiting for response...".to_string(), cx.theme().info)
                    }
                    _ => unreachable!(),
                };

                messages_container = messages_container.child(
                    div()
                        .p_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .size(px(16.))
                                .path(SharedString::from("icons/arrow_circle.svg"))
                                .text_color(icon_color)
                                .with_animation(
                                    "loading_indicator",
                                    Animation::new(std::time::Duration::from_secs(2))
                                        .repeat()
                                        .with_easing(bounce(ease_in_out)),
                                    |svg, delta| {
                                        svg.with_transformation(Transformation::rotate(percentage(
                                            delta,
                                        )))
                                    },
                                ),
                        )
                        .child(
                            div()
                                .text_color(icon_color)
                                .text_size(px(12.))
                                .child(message_text),
                        ),
                );
            }
        }

        messages_container
    }
}
