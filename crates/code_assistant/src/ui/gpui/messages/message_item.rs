//! Single message row rendering logic.

use super::branch_switcher::BranchSwitcherElement;
use super::MessagesView;
use gpui::{div, prelude::*, rems, rgb, Context, CursorStyle, Entity, SharedString, Window};
use gpui_component::{ActiveTheme, Icon};

/// Render a single message at the given index.
/// Called by the list's render callback — only for visible items.
pub fn render_message(
    view: &MessagesView,
    index: usize,
    _window: &mut Window,
    cx: &mut Context<MessagesView>,
) -> gpui::AnyElement {
    let messages = view.message_queue.lock().unwrap();
    let Some(msg) = messages.get(index) else {
        // Index out of bounds — might be the pending message slot
        // or a race condition. Return empty.
        return div().into_any_element();
    };
    let msg = msg.clone();
    drop(messages); // Release lock before reading entity

    let current_project = view.current_project.lock().unwrap().clone();
    let current_session_id = view.current_session_id.lock().unwrap().clone();

    // Update the message container with current project
    msg.read(cx).set_current_project(current_project);

    let is_user_message = msg.read(cx).is_user_message();
    let node_id = msg.read(cx).node_id();
    let branch_info = msg.read(cx).branch_info();

    // Get the theme colors for user messages
    let user_accent = if cx.theme().is_dark() {
        rgb(0x6BD9A8)
    } else {
        rgb(0x0A8A55)
    };

    // Create message container with appropriate styling.
    // max_w is applied via the centering wrapper returned at the end.
    // For user messages: uniform padding + gap between children.
    // For assistant messages: only horizontal padding; each block controls
    // its own vertical margin so inline tools can be tighter than text.
    let message_container = if is_user_message {
        div()
            .w_full()
            .p_3()
            .flex()
            .flex_col()
            .gap(rems(0.625))
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .shadow_xs()
    } else {
        div().w_full().px_3().pb_1().flex().flex_col()
    };

    // Create message container with user badge and edit button if needed
    let message_container = if is_user_message {
        let session_id_for_edit = current_session_id.clone();
        let node_id_for_edit = node_id;

        let header_row = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(vec![
                        super::super::shared::file_icons::render_icon_container(
                            &super::super::shared::file_icons::get()
                                .get_type_icon(super::super::shared::file_icons::TOOL_USER_INPUT),
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
                el.child(
                    div()
                        .id("edit-message-btn")
                        .p_1()
                        .rounded_sm()
                        .cursor(CursorStyle::PointingHand)
                        .hover(|s| s.bg(cx.theme().accent.opacity(0.25)))
                        .on_click(move |_event, _window, cx| {
                            if let (Some(session_id), Some(node_id)) =
                                (session_id_for_edit.clone(), node_id_for_edit)
                            {
                                if let Some(sender) = cx.try_global::<super::super::UiEventSender>()
                                {
                                    let _ =
                                        sender.0.try_send(crate::ui::UiEvent::StartMessageEdit {
                                            session_id,
                                            node_id,
                                        });
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

    // Render all block elements
    let elements = msg.read(cx).elements();

    let message_container = if is_user_message {
        let container_children = group_user_message_elements(elements, cx);
        message_container.children(container_children)
    } else {
        let container_children: Vec<_> = elements
            .into_iter()
            .map(|element| element.into_any_element())
            .collect();
        message_container.children(container_children)
    };

    // Add branch switcher if applicable
    let message_container = if is_user_message {
        if let (Some(branch_info), Some(session_id)) = (branch_info, current_session_id.clone()) {
            if branch_info.sibling_ids.len() > 1 {
                message_container.child(BranchSwitcherElement::new(branch_info, session_id))
            } else {
                message_container
            }
        } else {
            message_container
        }
    } else {
        message_container
    };

    // Wrap user messages in a padding container so the card (with its
    // border + shadow) sits inset from the max-width boundary without
    // overflowing via margin.
    if is_user_message {
        div()
            .id(("message-item", msg.entity_id()))
            .w_full()
            .p_3()
            .child(message_container)
            .into_any_element()
    } else {
        message_container
            .id(("message-item", msg.entity_id()))
            .into_any_element()
    }
}

/// Group consecutive image blocks into horizontal galleries for user messages
fn group_user_message_elements(
    elements: Vec<Entity<super::super::blocks::BlockView>>,
    cx: &Context<MessagesView>,
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
