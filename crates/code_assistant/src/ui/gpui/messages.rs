use super::branch_switcher::BranchSwitcherElement;
use super::elements::MessageContainer;
use gpui::{
    div, list, prelude::*, px, rgb, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    ListAlignment, ListState, Point, SharedString, Task, Timer, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::trace;

/// Maximum width of the message content area. On wide viewports, messages
/// stay centered rather than stretching edge-to-edge for comfortable reading.
const MAX_MESSAGE_WIDTH: f32 = 900.0;

// ---------------------------------------------------------------------------
// Smooth-scroll configuration (spring-damper model, same tuning as the old
// AutoScrollContainer).
// ---------------------------------------------------------------------------

/// How often the animation loop ticks (~120 FPS).
const ANIMATION_FRAME_MS: u64 = 8;
/// Spring constant.
const SPRING_K: f32 = 0.035;
/// Damping constant.
const DAMPING_C: f32 = 0.32;
/// Stop threshold: distance in pixels.
const MIN_DISTANCE_TO_STOP: f32 = 0.5;
/// Stop threshold: speed in pixels/frame.
const MIN_SPEED_TO_STOP: f32 = 0.5;

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

    // -- Smooth-scroll animation state --
    /// Running animation task. Dropping it cancels the animation.
    smooth_scroll_task: Option<Task<()>>,
    /// Shared flag read by the animation loop — set to `false` to request stop.
    animation_active: Rc<Cell<bool>>,
    /// The pixel offset we last wrote via the animation loop. Used to detect
    /// whether the user scrolled manually (the scroll handler fires with a
    /// different offset than we set).
    last_animation_offset: Rc<Cell<f32>>,
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

        let animation_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let last_animation_offset: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));

        // Install scroll handler to detect when user scrolls away from bottom.
        // If an animation is running and the user scrolls manually, stop it.
        let entity = cx.entity().downgrade();
        let anim_active_for_handler = animation_active.clone();
        let last_anim_offset_for_handler = last_animation_offset.clone();
        list_state.set_scroll_handler(move |_event, _window, cx| {
            let entity = entity.clone();
            let anim_active = anim_active_for_handler.clone();
            let last_offset = last_anim_offset_for_handler.clone();
            cx.defer(move |cx| {
                let _ = entity.update(cx, |this, _cx| {
                    // If an animation is running, check whether this scroll event
                    // was caused by the user (manual) vs. our animation loop.
                    if anim_active.get() {
                        let current: f32 =
                            this.list_state.scroll_px_offset_for_scrollbar().y.into();
                        let expected = last_offset.get();
                        // Allow small floating-point jitter.
                        if (current - expected).abs() > 1.5 {
                            // User scrolled manually — stop animation.
                            anim_active.set(false);
                            this.smooth_scroll_task = None;
                        }
                    }
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
            follow_tail: true,
            smooth_scroll_task: None,
            animation_active,
            last_animation_offset,
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
        // For a full reset, jump instantly — no need to animate.
        self.stop_animation();
        if new_count > 0 {
            self.scroll_to_bottom_instant();
        }
        trace!("ListState reset with {} items", new_count);
    }

    // -----------------------------------------------------------------
    // Scrolling helpers
    // -----------------------------------------------------------------

    /// Scroll to the bottom smoothly. Always uses the spring animation so
    /// that irregular content growth during streaming (whole lines, tool
    /// blocks, etc.) doesn't jerk the viewport. The animation loop
    /// recalculates the target each frame from `max_offset_for_scrollbar()`,
    /// so it naturally chases a continuously moving bottom.
    pub fn scroll_to_bottom(&mut self) {
        if self.list_state.item_count() == 0 {
            return;
        }
        self.ensure_animation_running();
    }

    /// Jump to the bottom instantly (no animation).
    fn scroll_to_bottom_instant(&self) {
        let count = self.list_state.item_count();
        if count > 0 {
            self.list_state.scroll_to_reveal_item(count - 1);
        }
    }

    /// Stop any running smooth-scroll animation.
    fn stop_animation(&mut self) {
        self.animation_active.set(false);
        self.smooth_scroll_task = None;
    }

    /// Ensure the spring animation task is running. If it's already running
    /// the task will naturally pick up the new target (always: scroll to bottom).
    fn ensure_animation_running(&mut self) {
        if self.animation_active.get() {
            // Already running — it will keep chasing the bottom.
            return;
        }
        // Will be started in the next render cycle when we have a Context.
        // We set a flag and start the task in render() or via a dedicated method.
        self.animation_active.set(true);
    }

    /// Actually spawn the animation task. Must be called with a `Context<Self>`.
    fn spawn_animation_task(&mut self, cx: &mut Context<Self>) {
        // Cancel any prior task.
        self.smooth_scroll_task = None;

        if !self.animation_active.get() {
            return;
        }

        // Seed the expected offset to the current position so the scroll
        // handler doesn't immediately detect a "manual scroll" on the first
        // frame.
        let current: f32 = self.list_state.scroll_px_offset_for_scrollbar().y.into();
        self.last_animation_offset.set(current);

        let list_state = self.list_state.clone();
        let active = self.animation_active.clone();
        let last_offset = self.last_animation_offset.clone();

        let task = cx.spawn(async move |weak_entity, cx| {
            let mut velocity: f32 = 0.0;

            loop {
                Timer::after(Duration::from_millis(ANIMATION_FRAME_MS)).await;

                if !active.get() {
                    break;
                }

                // Compute current position (negative) and target.
                let current_offset: f32 = list_state.scroll_px_offset_for_scrollbar().y.into();
                let max: f32 = list_state.max_offset_for_scrollbar().height.into();

                // Target offset is -max (fully scrolled to bottom). The
                // scrollbar API returns offset.y as negative.
                let target: f32 = -max;

                // displacement > 0 means we're above the target (need to scroll down).
                let displacement = current_offset - target;

                if displacement.abs() < MIN_DISTANCE_TO_STOP && velocity.abs() < MIN_SPEED_TO_STOP {
                    // Close enough — snap to exact bottom and stop.
                    list_state.set_offset_from_scrollbar(Point {
                        x: px(0.),
                        y: px(max),
                    });
                    last_offset.set(-max);
                    active.set(false);

                    // One final notify to paint the snapped position.
                    let _ = weak_entity.update(cx, |_this, cx| {
                        cx.notify();
                    });
                    break;
                }

                // Spring-damper physics.
                let spring_force = -SPRING_K * displacement;
                let damping_force = -DAMPING_C * velocity;
                velocity += spring_force + damping_force;

                let mut delta = velocity;

                // Prevent overshooting past the target.
                if displacement.abs() > f32::EPSILON {
                    let new_offset = current_offset + delta;
                    let new_displacement = new_offset - target;
                    if new_displacement.signum() != displacement.signum() {
                        // Would overshoot — clamp to exactly the target.
                        delta = -displacement;
                    }
                }
                if delta.abs() > displacement.abs() {
                    delta = -displacement;
                }

                // Convert the new offset back to the absolute (positive)
                // offset that `set_offset_from_scrollbar` expects.
                let new_y = current_offset + delta;
                let abs_offset = (-new_y).max(0.0);
                list_state.set_offset_from_scrollbar(Point {
                    x: px(0.),
                    y: px(abs_offset),
                });
                last_offset.set(new_y);

                // Request repaint.
                let ok = weak_entity.update(cx, |_this, cx| {
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }

            active.set(false);
        });

        self.smooth_scroll_task = Some(task);
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

    /// Wrap a message div in a full-width flex column that centers its child
    /// horizontally via `items_center` (cross-axis). The main axis stays
    /// vertical so the list's height measurement works correctly.
    fn centered_list_item(content: gpui::Div) -> gpui::Div {
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(content.max_w(px(MAX_MESSAGE_WIDTH)))
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
        // p_3 provides interior padding. max_w is applied via the centering
        // wrapper returned at the end of this function.
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
        let message_container = if is_user_message {
            if let (Some(branch_info), Some(session_id)) = (branch_info, current_session_id.clone())
            {
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

        // Wrap in a full-width centering container so that on wide viewports
        // the message content stays centered rather than left-aligned.
        Self::centered_list_item(message_container).into_any_element()
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

        // If the animation flag was set but the task hasn't been spawned yet,
        // spawn it now (we need a Context<Self> to call cx.spawn).
        if self.animation_active.get() && self.smooth_scroll_task.is_none() {
            self.spawn_animation_task(cx);
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

        let pending_card = div()
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
            );

        Self::centered_list_item(pending_card).into_any_element()
    }
}
