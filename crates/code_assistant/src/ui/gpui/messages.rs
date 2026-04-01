use super::branch_switcher::BranchSwitcherElement;
use super::elements::MessageContainer;
use crate::session::instance::SessionActivityState;
use gpui::{
    div, list, prelude::*, px, rems, rgb, App, Context, CursorStyle, Entity, FocusHandle,
    Focusable, ListAlignment, ListState, Point, SharedString, Task, Timer, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::trace;

/// Braille spinner frames for the activity indicator.
const BRAILLE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Maximum width of the message content area. On wide viewports, messages
/// stay centered rather than stretching edge-to-edge for comfortable reading.
const MAX_MESSAGE_WIDTH: f32 = 720.0;

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
/// How long the animation idles at the bottom before shutting down.
/// While idling, it keeps checking for new content growth and instantly
/// resumes scrolling — this avoids the race where content arrives between
/// animation stop and the next `scroll_to_bottom()` call.
const ANIMATION_IDLE_MS: u64 = 2000;

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
    /// Current session activity state (shared with Gpui).
    activity_state: Arc<Mutex<Option<SessionActivityState>>>,
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
    /// The pixel offset we last wrote via the animation loop. Kept in sync
    /// so the animation can track its own state across frames.
    last_animation_offset: Rc<Cell<f32>>,
    /// Task that ticks the braille spinner animation.
    _spinner_task: Option<Task<()>>,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        activity_state: Arc<Mutex<Option<SessionActivityState>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_count = message_queue.lock().unwrap().len();
        // Overdraw of 1024px means items within ~1024px outside the visible viewport
        // are pre-rendered, avoiding flicker when scrolling.
        let list_state = ListState::new(initial_count, ListAlignment::Top, px(1024.));

        let animation_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let last_animation_offset: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));

        // Install scroll handler to detect manual user scrolling.
        //
        // This handler is ONLY called on real ScrollWheelEvent (mouse/trackpad),
        // never for programmatic offset changes (set_offset_from_scrollbar).
        //
        // Strategy: we only care about *direction*. If the user scrolls upward
        // (away from bottom), we disable follow_tail and stop the animation.
        // If they scroll downward, we check if they reached the bottom and
        // re-enable follow_tail. We track the previous offset to compute the
        // direction.
        let entity = cx.entity().downgrade();
        let anim_active_for_handler = animation_active.clone();
        let prev_scroll_offset: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));
        list_state.set_scroll_handler(move |_event, _window, cx| {
            let entity = entity.clone();
            let anim_active = anim_active_for_handler.clone();
            let prev_offset = prev_scroll_offset.clone();
            cx.defer(move |cx| {
                let _ = entity.update(cx, |this, _cx| {
                    // offset.y is negative: 0 = top, -max = bottom
                    let current: f32 = this.list_state.scroll_px_offset_for_scrollbar().y.into();
                    let previous = prev_offset.get();
                    prev_offset.set(current);

                    // delta > 0 means offset.y moved toward 0 (= scrolled UP)
                    let delta = current - previous;

                    if delta > 0.5 {
                        // User scrolled UP → disable follow
                        trace!(
                            "User scrolled up (delta={:.1}px) — disabling follow_tail",
                            delta
                        );
                        this.follow_tail = false;
                        anim_active.set(false);
                        this.smooth_scroll_task = None;
                    } else if delta < -0.5 {
                        // User scrolled DOWN → check if near bottom, re-enable follow
                        let max: f32 = this.list_state.max_offset_for_scrollbar().height.into();
                        let distance_from_bottom = max + current; // current is negative
                        if distance_from_bottom < 50.0 && !this.follow_tail {
                            trace!("User scrolled to bottom — re-enabling follow_tail");
                            this.follow_tail = true;
                            // Kick off smooth scroll to snap to exact bottom
                            this.ensure_animation_running();
                        }
                    }
                });
            });
        });

        // Spawn a periodic task that triggers a repaint every 80ms while the
        // activity indicator is visible. This drives the braille spinner.
        let activity_for_tick = activity_state.clone();
        let spinner_task = cx.spawn(async move |weak_entity, cx| {
            loop {
                Timer::after(Duration::from_millis(80)).await;
                // Only notify when there's an active activity state
                let should_tick = activity_for_tick
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .is_some_and(|s| !matches!(s, SessionActivityState::Idle));
                if should_tick {
                    let ok = weak_entity.update(cx, |_this, cx| {
                        cx.notify();
                    });
                    if ok.is_err() {
                        break;
                    }
                }
            }
        });

        Self {
            message_queue,
            current_pending_message: Arc::new(Mutex::new(None)),
            current_project: Arc::new(Mutex::new(String::new())),
            current_session_id: Arc::new(Mutex::new(None)),
            activity_state,
            focus_handle: cx.focus_handle(),
            list_state,
            follow_tail: true,
            smooth_scroll_task: None,
            animation_active,
            last_animation_offset,
            _spinner_task: Some(spinner_task),
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
        // Flag for restart. Clear the old (completed) task so the render
        // check `animation_active && smooth_scroll_task.is_none()` will
        // trigger a new spawn.
        self.animation_active.set(true);
        self.smooth_scroll_task = None;
    }

    /// Actually spawn the animation task. Must be called with a `Context<Self>`.
    ///
    /// The animation loop has two phases:
    /// 1. **Active**: spring-damper physics scrolling toward the bottom.
    /// 2. **Idle**: we've converged to the bottom. Instead of stopping, we
    ///    keep polling at a lower rate. If new content pushes the bottom
    ///    further away, we seamlessly resume the spring animation. After
    ///    `ANIMATION_IDLE_MS` of idling without new movement, we shut down.
    ///    (A new `scroll_to_bottom()` call will restart us.)
    fn spawn_animation_task(&mut self, cx: &mut Context<Self>) {
        // Cancel any prior task.
        self.smooth_scroll_task = None;

        if !self.animation_active.get() {
            return;
        }

        let list_state = self.list_state.clone();
        let active = self.animation_active.clone();
        let last_offset = self.last_animation_offset.clone();

        let task = cx.spawn(async move |weak_entity, cx| {
            let mut velocity: f32 = 0.0;
            let mut idle_elapsed_ms: u64 = 0;

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
                    // We've converged. Snap to exact bottom and enter idle phase.
                    if displacement.abs() > 0.01 {
                        let max_rounded = max.round();
                        list_state.set_offset_from_scrollbar(Point {
                            x: px(0.),
                            y: px(max_rounded),
                        });
                        last_offset.set(-max_rounded);
                        let _ = weak_entity.update(cx, |_this, cx| {
                            cx.notify();
                        });
                    }

                    velocity = 0.0;
                    idle_elapsed_ms += ANIMATION_FRAME_MS;

                    if idle_elapsed_ms >= ANIMATION_IDLE_MS {
                        // Idle timeout — shut down. Will be restarted by
                        // the next scroll_to_bottom() call.
                        break;
                    }
                    continue;
                }

                // We have work to do — reset idle timer.
                idle_elapsed_ms = 0;

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
                let abs_offset = (-new_y).max(0.0).round();
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

    /// Force-enable follow_tail and kick off smooth scrolling.
    /// Called when the user explicitly wants to go to the bottom (e.g. button click).
    #[allow(dead_code)]
    pub fn activate_follow_tail(&mut self) {
        self.follow_tail = true;
        self.scroll_to_bottom();
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
        // p_3 provides interior padding. max_w is applied via the centering
        // wrapper returned at the end of this function.
        let mut message_container = div().w_full().p_3().flex().flex_col().gap(px(10.));

        if is_user_message {
            message_container = message_container
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

        // Wrap user messages in a padding container so the card (with its
        // border + shadow) sits inset from the max-width boundary without
        // overflowing via margin.
        if is_user_message {
            div()
                .w_full()
                .p_3()
                .child(message_container)
                .into_any_element()
        } else {
            message_container.into_any_element()
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
        let total_items = self.message_queue.lock().unwrap().len();

        // The pending message is rendered as an extra item after all messages
        let has_pending = self
            .current_pending_message
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|m| !m.is_empty());

        // Activity indicator is rendered as the very last list item, but only
        // for WaitingForResponse and RateLimited. During AgentRunning the
        // streaming content itself shows activity.
        let has_activity = self
            .activity_state
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .is_some_and(|s| {
                matches!(
                    s,
                    SessionActivityState::WaitingForResponse
                        | SessionActivityState::RateLimited { .. }
                )
            });

        let pending_index = total_items;
        let activity_index = total_items + if has_pending { 1 } else { 0 };
        let item_count = activity_index + if has_activity { 1 } else { 0 };

        // Build the virtualized list. The render callback is only invoked for
        // items that are within the visible viewport + overdraw zone.
        let message_list = list(
            self.list_state.clone(),
            cx.processor(move |this: &mut Self, index: usize, window, cx| {
                // Wrap every rendered item in a centering container so the
                // list element itself spans the full parent width (= its
                // hitbox receives scroll-wheel events everywhere, not only
                // over the narrow content column).
                let inner = if index < total_items {
                    this.render_message(index, window, cx)
                } else if index == pending_index && has_pending {
                    this.render_pending_message(window, cx)
                } else {
                    // Activity indicator (last item)
                    this.render_activity_indicator(cx)
                };
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(div().max_w(px(MAX_MESSAGE_WIDTH)).w_full().child(inner))
                    .into_any_element()
            }),
        )
        .flex_grow()
        .w_full();

        // Sync item count with ListState if it diverged (e.g. pending message
        // appeared/disappeared). Use splice instead of reset to preserve the
        // current scroll position and cached item heights.
        let current_count = self.list_state.item_count();
        if current_count != item_count {
            if item_count > current_count {
                // Items were added at the end (e.g. pending message appeared)
                self.list_state
                    .splice(current_count..current_count, item_count - current_count);
            } else {
                // Items were removed from the end (e.g. pending message disappeared)
                self.list_state.splice(item_count..current_count, 0);
            }
            trace!(
                "ListState count sync: {} → {} (splice, not reset)",
                current_count,
                item_count
            );
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
            .items_center()
            .size_full()
            .bg(cx.theme().popover)
            .text_size(rems(1.0))
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

        pending_card.into_any_element()
    }

    /// Render the inline activity indicator (braille spinner or rate-limit text).
    ///
    /// Only shown for `WaitingForResponse` (pre-stream) and `RateLimited`.
    /// During `AgentRunning` the streaming content itself signals activity.
    fn render_activity_indicator(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let activity = self.activity_state.lock().ok().and_then(|g| g.clone());

        let Some(activity) = activity else {
            return div().into_any_element();
        };

        // Pick the current braille frame based on wall-clock time (~80ms per frame)
        let frame_index = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            / 80) as usize
            % BRAILLE_FRAMES.len();
        let braille_char = BRAILLE_FRAMES[frame_index];

        match activity {
            SessionActivityState::RateLimited { seconds_remaining } => {
                // Orange rate-limit message with spinner
                let color = cx.theme().warning;
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(rems(0.875))
                            .text_color(color)
                            .child(braille_char.to_string()),
                    )
                    .child(
                        div()
                            .text_size(rems(0.8125))
                            .text_color(color)
                            .child(format!(
                                "Rate limited — retrying in {}s…",
                                seconds_remaining
                            )),
                    )
                    .into_any_element()
            }
            SessionActivityState::WaitingForResponse => {
                // Blue braille spinner, no text
                let color = cx.theme().primary;
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_size(rems(0.875))
                            .text_color(color)
                            .child(braille_char.to_string()),
                    )
                    .into_any_element()
            }
            _ => {
                // AgentRunning / Idle — no indicator
                div().into_any_element()
            }
        }
    }
}
