use super::branch_switcher::BranchSwitcherElement;
use super::elements::MessageContainer;
use gpui::{
    div, list, prelude::*, px, rgb, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    ListAlignment, ListState, SharedString, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon};
use std::sync::{Arc, Mutex};
use tracing::trace;

/// MessagesView - Component responsible for displaying the message history.
///
/// Uses GPUI's virtualized `list()` element to only render messages that are
/// currently visible (plus an overdraw buffer). This is critical for performance
/// in long sessions with many messages — off-screen messages skip render/layout/paint
/// entirely.
pub struct MessagesView {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    current_pending_message: Arc<Mutex<Option<String>>>,
    /// Current project (used to detect cross-project tool calls)
    #[allow(dead_code)]
    current_project: Arc<Mutex<String>>,
    current_session_id: Arc<Mutex<Option<String>>>,
    focus_handle: FocusHandle,
    /// The virtualized list state that tracks item count, scroll position,
    /// and cached item heights.
    list_state: ListState,
    /// Whether auto-scroll to bottom is active (user is following the tail).
    pub follow_tail: bool,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_count = message_queue.lock().unwrap().len();
        // Overdraw of 1024px means items within ~1024px outside the visible viewport
        // are pre-rendered, avoiding flicker when scrolling.
        let list_state = ListState::new(initial_count, ListAlignment::Top, px(1024.));

        // Install scroll handler to detect when user scrolls away from bottom
        let entity = cx.entity().downgrade();
        list_state.set_scroll_handler(move |_event, _window, cx| {
            // Must defer to avoid double-borrow of ListState's internal RefCell
            let entity = entity.clone();
            cx.defer(move |cx| {
                let _ = entity.update(cx, |this, _cx| {
                    this.update_follow_tail();
                });
            });
        });

        Self {
            message_queue,
            current_pending_message: Arc::new(Mutex::new(None)),
            current_project: Arc::new(Mutex::new(String::new())),
            current_session_id: Arc::new(Mutex::new(None)),
            focus_handle: cx.focus_handle(),
            list_state,
            follow_tail: true, // Start following tail by default
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

    /// Notify that messages have been added. Call this after pushing to message_queue.
    /// Splices new items into the ListState so it knows about the new count.
    pub fn messages_spliced(&mut self, old_len: usize, new_len: usize) {
        if new_len > old_len {
            // Insert new items at the end
            self.list_state.splice(old_len..old_len, new_len - old_len);
            trace!(
                "ListState spliced: added {} items ({}→{})",
                new_len - old_len,
                old_len,
                new_len
            );

            // Auto-scroll to bottom if following tail
            if self.follow_tail {
                self.scroll_to_bottom();
            }
        }
    }

    /// Notify that all messages have been cleared and replaced.
    /// Resets the ListState with the new count.
    pub fn messages_reset(&mut self, new_count: usize) {
        self.list_state.reset(new_count);
        self.follow_tail = true;
        if new_count > 0 {
            self.scroll_to_bottom();
        }
        trace!("ListState reset with {} items", new_count);
    }

    /// Scroll to the bottom of the list
    pub fn scroll_to_bottom(&self) {
        let count = self.list_state.item_count();
        if count > 0 {
            self.list_state.scroll_to_reveal_item(count - 1);
        }
    }

    /// Check if we're near the bottom and update follow_tail state
    fn update_follow_tail(&mut self) {
        // Use the scrollbar offset to determine if we're near the bottom
        let offset = self.list_state.scroll_px_offset_for_scrollbar();
        let max_offset = self.list_state.max_offset_for_scrollbar();

        // If max scroll is very small (content fits in viewport), always follow
        if max_offset.height <= px(50.0) {
            self.follow_tail = true;
            return;
        }

        // Check if we're within ~50px of the bottom
        let distance_from_bottom = max_offset.height + offset.y; // offset.y is negative
        self.follow_tail = distance_from_bottom < px(50.0);
    }

    /// Get the list state (for scrollbar integration)
    #[allow(dead_code)]
    pub fn list_state(&self) -> &ListState {
        &self.list_state
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

    /// Render a single message at the given index.
    /// Called by the list's render callback — only for visible items.
    fn render_message(
        &self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let messages = self.message_queue.lock().unwrap();
        let Some(msg) = messages.get(index) else {
            // Index out of bounds — might be the pending message slot
            // or a race condition. Return empty.
            return div().into_any_element();
        };
        let msg = msg.clone();
        drop(messages); // Release lock before reading entity

        let current_project = self.current_project.lock().unwrap().clone();
        let current_session_id = self.get_current_session_id();

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
        // w_full() is required because list items don't inherit parent flex width.
        // p_3 provides interior padding, pb_2 at the end provides gap between items.
        let mut message_container = div().w_full().p_3().flex().flex_col().gap(px(10.));

        if is_user_message {
            message_container = message_container
                .m_3()
                .bg(cx.theme().muted)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .shadow_xs();
        }

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
                                    if let Some(sender) = cx.try_global::<super::UiEventSender>() {
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

        // Render all block elements
        let elements = msg.read(cx).elements();

        let message_container = if is_user_message {
            let container_children = Self::group_user_message_elements(elements, cx);
            message_container.children(container_children)
        } else {
            let container_children: Vec<_> = elements
                .into_iter()
                .map(|element| element.into_any_element())
                .collect();
            message_container.children(container_children)
        };

        // Add branch switcher if applicable
        if is_user_message {
            if let (Some(branch_info), Some(session_id)) = (branch_info, current_session_id.clone())
            {
                if branch_info.sibling_ids.len() > 1 {
                    return message_container
                        .child(BranchSwitcherElement::new(branch_info, session_id))
                        .into_any_element();
                }
            }
        }

        message_container.into_any_element()
    }
}

impl Focusable for MessagesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessagesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total_items = self.message_queue.lock().unwrap().len();

        // The pending message is rendered as an extra item after all messages
        let has_pending = self
            .current_pending_message
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|m| !m.is_empty());

        let item_count = total_items + if has_pending { 1 } else { 0 };

        // Build the virtualized list. The render callback is only invoked for
        // items that are within the visible viewport + overdraw zone.
        let message_list = list(
            self.list_state.clone(),
            cx.processor(move |this: &mut Self, index: usize, window, cx| {
                if index < total_items {
                    this.render_message(index, window, cx)
                } else {
                    // Render pending message
                    this.render_pending_message(window, cx)
                }
            }),
        )
        .flex_grow();

        // Sync item count with ListState — the list element needs the correct count
        // for its internal SumTree. We do a reset-style sync if count diverged.
        let current_count = self.list_state.item_count();
        if current_count != item_count {
            self.list_state.reset(item_count);
        }

        div()
            .id("messages")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().popover)
            .text_size(px(16.))
            .child(message_list)
            .vertical_scrollbar(&self.list_state)
    }
}

impl MessagesView {
    /// Render the pending message indicator
    fn render_pending_message(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let pending_message = self.current_pending_message.lock().unwrap().clone();
        let Some(pending_message) = pending_message else {
            return div().into_any_element();
        };
        if pending_message.is_empty() {
            return div().into_any_element();
        }

        div()
            .w_full()
            .m_3()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().warning)
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
            )
            .into_any_element()
    }
}
