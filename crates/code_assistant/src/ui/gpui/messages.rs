use super::branch_switcher::BranchSwitcherElement;
use super::elements::MessageContainer;
use gpui::{
    div, prelude::*, px, rgb, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    MouseButton, SharedString, Window,
};
use gpui_component::{v_flex, ActiveTheme, Icon};
use std::sync::{Arc, Mutex};

/// MessagesView - Component responsible for displaying the message history
pub struct MessagesView {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    current_pending_message: Arc<Mutex<Option<String>>>,
    current_project: Arc<Mutex<String>>,
    current_session_id: Arc<Mutex<Option<String>>>,
    focus_handle: FocusHandle,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message_queue,
            current_pending_message: Arc::new(Mutex::new(None)),
            current_project: Arc::new(Mutex::new(String::new())),
            current_session_id: Arc::new(Mutex::new(None)),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Update the current session ID
    pub fn set_current_session_id(&self, session_id: Option<String>) {
        *self.current_session_id.lock().unwrap() = session_id;
    }

    /// Get the current session ID
    fn get_current_session_id(&self) -> Option<String> {
        self.current_session_id.lock().unwrap().clone()
    }

    /// Group consecutive image blocks into horizontal galleries for user messages
    fn group_user_message_elements(
        elements: Vec<Entity<super::elements::BlockView>>,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let mut result = Vec::new();
        let mut current_images = Vec::new();

        for element in elements {
            if element.read(cx).is_image_block() {
                // Collect consecutive image blocks
                current_images.push(element);
            } else {
                // If we have accumulated images, create a gallery first
                if !current_images.is_empty() {
                    let image_gallery = div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .mt_2() // Add top margin to separate from text above
                        .children(current_images.drain(..).map(|img| img.into_any_element()));
                    result.push(image_gallery.into_any_element());
                }

                // Add the non-image element
                result.push(element.into_any_element());
            }
        }

        // Handle any remaining images at the end
        if !current_images.is_empty() {
            let image_gallery = div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_2()
                .mt_2() // Add top margin to separate from text above
                .children(current_images.drain(..).map(|img| img.into_any_element()));
            result.push(image_gallery.into_any_element());
        }

        result
    }

    /// Update the pending message for the current session
    pub fn update_pending_message(&self, message: Option<String>) {
        *self.current_pending_message.lock().unwrap() = message;
    }

    /// Update the current project for parameter filtering
    pub fn set_current_project(&self, project: String) {
        *self.current_project.lock().unwrap() = project;
    }

    fn get_current_project(&self) -> String {
        self.current_project.lock().unwrap().clone()
    }
}

impl Focusable for MessagesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessagesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        // Get current project for parameter filtering
        let current_project = self.get_current_project();
        let current_session_id = self.get_current_session_id();

        // Collect all message elements first
        let message_elements: Vec<_> = messages
            .into_iter()
            .map(|msg| {
                // Update the message container with current project
                msg.read(cx).set_current_project(current_project.clone());

                let is_user_message = msg.read(cx).is_user_message();
                let node_id = msg.read(cx).node_id();
                let branch_info = msg.read(cx).branch_info();

                // Create message container with appropriate styling based on role
                let mut message_container = div().p_3();

                if is_user_message {
                    message_container = message_container
                        .m_3()
                        .bg(cx.theme().muted) // Use opaque muted color (darker than card background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .shadow_xs();
                }

                // Create message container with user badge and edit button if needed
                let message_container = if is_user_message {
                    // Build header row with user badge and edit button
                    let session_id_for_edit = current_session_id.clone();
                    let node_id_for_edit = node_id;

                    let header_row = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .child(
                            // Left side: User badge
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
                                        user_accent,
                                        "👤",
                                    )
                                    .into_any_element(),
                                    div()
                                        .font_weight(gpui::FontWeight(600.0))
                                        .text_color(user_accent)
                                        .child("You")
                                        .into_any_element(),
                                ]),
                        )
                        .when(node_id.is_some(), |el| {
                            // Right side: Edit button (only shown when node_id is present)
                            el.child(
                                div()
                                    .id("edit-message-btn")
                                    .p_1()
                                    .rounded_sm()
                                    .cursor(CursorStyle::PointingHand)
                                    .hover(|s| s.bg(cx.theme().accent.opacity(0.25)))
                                    .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                                        if let (Some(session_id), Some(node_id)) =
                                            (session_id_for_edit.clone(), node_id_for_edit)
                                        {
                                            // Send StartMessageEdit event
                                            if let Some(sender) =
                                                cx.try_global::<super::UiEventSender>()
                                            {
                                                let _ = sender.0.try_send(
                                                    crate::ui::UiEvent::StartMessageEdit {
                                                        session_id,
                                                        node_id,
                                                    },
                                                );
                                            }
                                        }
                                    })
                                    .child(
                                        Icon::default()
                                            .path(SharedString::from("icons/pencil.svg"))
                                            .text_color(cx.theme().muted_foreground)
                                            .size_4(),
                                    ),
                            )
                        });

                    message_container.child(header_row)
                } else {
                    message_container
                };

                // Render all block elements with special handling for user messages
                let elements = msg.read(cx).elements();

                let message_container = if is_user_message {
                    // For user messages, group consecutive image blocks into horizontal galleries
                    let container_children = Self::group_user_message_elements(elements, cx);
                    message_container.children(container_children)
                } else {
                    // For assistant messages, render elements normally (vertically)
                    let container_children: Vec<_> = elements
                        .into_iter()
                        .map(|element| element.into_any_element())
                        .collect();
                    message_container.children(container_children)
                };

                // Add branch switcher if branch_info is present (only for user messages)
                if is_user_message {
                    if let (Some(branch_info), Some(session_id)) =
                        (branch_info, current_session_id.clone())
                    {
                        // Only show if there are multiple siblings (actual branches)
                        if branch_info.sibling_ids.len() > 1 {
                            return message_container
                                .child(BranchSwitcherElement::new(branch_info, session_id))
                                .into_any_element();
                        }
                    }
                }

                message_container.into_any_element()
            })
            .collect();

        // Create the base messages container
        let mut messages_container = v_flex()
            .id("messages")
            .p_2()
            .bg(cx.theme().popover)
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
                        .bg(cx.theme().muted) // Same opaque background as regular user messages
                        .border_1()
                        .border_color(cx.theme().warning) // Use warning color to indicate pending
                        .rounded_md()
                        .shadow_xs()
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
                                .child(
                                    gpui_component::text::TextView::markdown(
                                        "pending-message",
                                        pending_message,
                                        window,
                                        cx,
                                    )
                                    .selectable(true),
                                ),
                        ),
                );
            }
        }

        messages_container
    }
}
