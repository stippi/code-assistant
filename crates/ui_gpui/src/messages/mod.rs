mod activity_indicator;
mod branch_switcher;
mod message_item;
mod scroll;

use crate::Gpui;

use code_assistant_core::session::instance::SessionActivityState;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, ListAlignment, ListOffset, ListState,
    MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Task, Window, div, list,
    prelude::*, px, rems,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use scroll::{
    ANIMATION_FRAME_MS, ANIMATION_IDLE_MS, DAMPING_C, EDGE_SCROLL_FRAME_MS, MIN_DISTANCE_TO_STOP,
    MIN_SPEED_TO_STOP, SPRING_K, edge_scroll_velocity,
};

use super::blocks::MessageContainer;

/// Braille spinner frames for the activity indicator.
const BRAILLE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Maximum width of the message content area. On wide viewports, messages
/// stay centered rather than stretching edge-to-edge for comfortable reading.
const MAX_MESSAGE_WIDTH: f32 = 720.0;

/// Persisted scroll state for a single session.
///
/// The `anchor` is the list's *logical* scroll position (an item index plus a
/// pixel offset within that item), which survives remeasuring far better than a
/// raw pixel offset. `follow_tail` records whether the user was pinned to the
/// bottom, so a session that was following the tail keeps doing so on return.
#[derive(Clone, Copy)]
struct SavedScroll {
    anchor: ListOffset,
    follow_tail: bool,
}

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
    /// Whether the currently displayed session is "resumable" — i.e. its
    /// last message is a user message or an assistant message with un-
    /// answered tool calls. Written from MainScreen render based on the
    /// cached ChatMetadata.
    is_resumable: Cell<bool>,
    focus_handle: FocusHandle,
    /// The virtualized list state that tracks item count, scroll position,
    /// and cached item heights.
    list_state: ListState,
    /// Whether auto-scroll to bottom is active (user is following the tail).
    pub follow_tail: bool,

    // -- Smooth-scroll animation state --
    /// Running animation task. Dropping it cancels the animation.
    smooth_scroll_task: Option<Task<()>>,
    /// Short-lived task that remeasures list rows after markdown parsing catches up.
    height_cache_refresh_task: Option<Task<()>>,
    /// Shared flag read by the animation loop — set to `false` to request stop.
    animation_active: Rc<Cell<bool>>,
    /// The pixel offset we last wrote via the animation loop. Kept in sync
    /// so the animation can track its own state across frames.
    last_animation_offset: Rc<Cell<f32>>,
    /// Task that ticks the braille spinner animation.
    _spinner_task: Option<Task<()>>,

    // -- Edge auto-scroll state (drag-to-select near a viewport edge) --
    /// Bounds of the message list viewport in window coordinates, captured
    /// during prepaint. Used to detect when a held drag reaches an edge.
    list_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Desired edge auto-scroll velocity in px/frame (sign matches
    /// `set_offset_from_scrollbar`: negative scrolls down, positive up, 0 =
    /// inactive). Written by the mouse-move handler, read by the edge task.
    edge_scroll_velocity: Rc<Cell<f32>>,
    /// Whether the left button is currently held (a drag may be in progress).
    /// Cleared on mouse-up so a parked pointer stops scrolling on release.
    edge_drag_active: Rc<Cell<bool>>,
    /// Running edge auto-scroll task. Dropping it cancels the loop.
    edge_scroll_task: Option<Task<()>>,

    // -- Per-session scroll persistence --
    /// The session id whose scroll position the `list_state` currently
    /// reflects. Distinct from `current_session_id` (which the app sets
    /// eagerly on switch): this is updated only when the list is actually
    /// rebuilt, so a reset can compare the two to tell a genuine session
    /// switch apart from a same-session resync.
    displayed_session_id: Option<String>,
    /// Last known scroll anchor + follow-tail flag per session, captured
    /// whenever we switch away. Restored when the session is shown again so
    /// the user sees it exactly as they left it.
    saved_scroll: HashMap<String, SavedScroll>,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        activity_state: Arc<Mutex<Option<SessionActivityState>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_count = message_queue.lock().unwrap().len();
        // Overdraw of 1024px means items within ~1024px outside the visible viewport
        // are pre-rendered, avoiding flicker when scrolling. Measure all rows after
        // resets so upward scrolling from the bottom does not discover zero-height
        // history rows above the viewport and remap the visual scroll offset.
        let list_state = ListState::new(initial_count, ListAlignment::Top, px(1024.)).measure_all();

        let animation_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let last_animation_offset: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));

        // Install scroll handler to detect manual user scrolling.
        scroll::install_scroll_handler(&list_state, &animation_active, cx);

        // Spawn a periodic task that triggers a repaint every 80ms while the
        // activity indicator is visible. This drives the braille spinner.
        let activity_for_tick = activity_state.clone();

        let spinner_task = cx.spawn(async move |weak_entity, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
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
            is_resumable: Cell::new(false),
            focus_handle: cx.focus_handle(),
            list_state,
            follow_tail: true,
            smooth_scroll_task: None,
            height_cache_refresh_task: None,
            animation_active,
            last_animation_offset,
            _spinner_task: Some(spinner_task),
            list_bounds: Rc::new(Cell::new(Bounds::default())),
            edge_scroll_velocity: Rc::new(Cell::new(0.0)),
            edge_drag_active: Rc::new(Cell::new(false)),
            edge_scroll_task: None,
            displayed_session_id: None,
            saved_scroll: HashMap::new(),
        }
    }

    /// Update the current session ID
    pub fn set_current_session_id(&self, session_id: Option<String>) {
        *self.current_session_id.lock().unwrap() = session_id;
    }

    /// Get the current session ID
    #[cfg(test)]
    fn get_current_session_id(&self) -> Option<String> {
        self.current_session_id.lock().unwrap().clone()
    }

    /// Notify that messages have been added. Call this after pushing to message_queue.
    /// Splices new items into the ListState so it knows about the new count.
    pub fn messages_spliced(&mut self, old_len: usize, new_len: usize, cx: &mut Context<Self>) {
        if new_len > old_len {
            // Insert new items at the end
            self.list_state.splice(old_len..old_len, new_len - old_len);

            tracing::trace!(
                "ListState spliced: added {} items ({}→{})",
                new_len - old_len,
                old_len,
                new_len
            );

            // Auto-scroll to bottom if following tail
            if self.follow_tail {
                self.scroll_to_bottom();
            }

            self.schedule_height_cache_refresh(cx);
        }
    }

    /// Reset the message list after its contents were rebuilt from scratch,
    /// making the scroll behavior session-aware.
    ///
    /// The message queue is rebuilt from scratch on two very different
    /// occasions, and they want opposite scroll behavior:
    ///
    /// * **Session switch** (`session_id` differs from the currently displayed
    ///   one): save the outgoing session's scroll position, then either restore
    ///   the incoming session's remembered position or — if it was never shown —
    ///   follow the tail.
    /// * **Same-session resync** (`session_id` unchanged): a stream lag, a file
    ///   watcher refresh, or a structural edit rebuilt the identical transcript.
    ///   The visible viewport must not jump, so we snapshot the scroll anchor
    ///   before the reset and restore it afterwards (freeze the offset).
    pub fn messages_reset_for_session(
        &mut self,
        session_id: Option<String>,
        new_count: usize,
        cx: &mut Context<Self>,
    ) {
        let is_switch = session_id != self.displayed_session_id;

        if is_switch {
            // Persist where we were in the session we are leaving.
            self.save_current_scroll(cx);

            self.list_state.reset(new_count);
            self.stop_animation();

            // Restore the target session's remembered scroll, or follow the
            // tail if we've never displayed it before. In-memory state (from
            // an earlier switch this run) takes precedence over the on-disk
            // position from a previous run.
            let restored = session_id.as_ref().and_then(|id| {
                self.saved_scroll
                    .get(id)
                    .copied()
                    .or_else(|| self.load_persisted_scroll(id))
            });

            self.displayed_session_id = session_id;

            match restored {
                Some(saved) if !saved.follow_tail => {
                    self.follow_tail = false;
                    if new_count > 0 {
                        self.list_state.scroll_to(saved.anchor);
                        self.schedule_height_cache_refresh(cx);
                    }
                }
                // Either following the tail previously, or a fresh session.
                _ => {
                    self.follow_tail = true;
                    if new_count > 0 {
                        self.scroll_to_bottom_instant();
                        self.schedule_height_cache_refresh(cx);
                    }
                }
            }
            tracing::trace!(
                "ListState reset for session switch → {} items, follow_tail={}",
                new_count,
                self.follow_tail
            );
        } else {
            // Same-session resync: freeze the visible offset. Snapshot the
            // anchor, rebuild, and restore it (unless we were following the
            // tail, in which case stay pinned to the bottom).
            let anchor = self.list_state.logical_scroll_top();
            let was_following = self.follow_tail;

            self.list_state.reset(new_count);
            self.stop_animation();

            if new_count == 0 {
                self.follow_tail = true;
            } else if was_following {
                self.follow_tail = true;
                self.scroll_to_bottom_instant();
                self.schedule_height_cache_refresh(cx);
            } else {
                self.follow_tail = false;
                self.list_state.scroll_to(anchor);
                self.schedule_height_cache_refresh(cx);
            }
            tracing::trace!(
                "ListState reset for same-session resync → {} items, follow_tail={}",
                new_count,
                self.follow_tail
            );
        }
    }

    /// Snapshot the current scroll anchor + follow-tail flag for the session
    /// the list currently reflects, so it can be restored on return (both
    /// in-memory this run and on disk across restarts).
    fn save_current_scroll(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.displayed_session_id.clone() {
            let anchor = self.list_state.logical_scroll_top();
            self.saved_scroll.insert(
                id.clone(),
                SavedScroll {
                    anchor,
                    follow_tail: self.follow_tail,
                },
            );
            // Mark the on-disk state dirty and schedule the debounced flush so
            // the position survives a restart even if the user never wheel-
            // scrolled (e.g. only dragged the scrollbar) before switching.
            if self.write_persisted_scroll(&id, anchor, self.follow_tail)
                && let Some(sender) = cx.try_global::<crate::UiEventSender>()
            {
                let _ = sender
                    .0
                    .try_send(code_assistant_core::ui::UiEvent::PersistUiState);
            }
        }
    }

    /// Persist the current scroll position to the per-session UI-state store
    /// and schedule a debounced flush to disk. Called on real user scrolling
    /// (not on programmatic restores), so the write is throttled to the last
    /// position after the user settles.
    fn persist_current_scroll(&self, cx: &mut Context<Self>) {
        let Some(id) = self.displayed_session_id.clone() else {
            return;
        };
        let anchor = self.list_state.logical_scroll_top();
        if self.write_persisted_scroll(&id, anchor, self.follow_tail)
            && let Some(sender) = cx.try_global::<crate::UiEventSender>()
        {
            let _ = sender
                .0
                .try_send(code_assistant_core::ui::UiEvent::PersistUiState);
        }
    }

    /// Write a scroll position into the global UI-state store. Returns `true`
    /// if the value changed (and the session was therefore marked dirty).
    fn write_persisted_scroll(
        &self,
        session_id: &str,
        anchor: gpui::ListOffset,
        follow_tail: bool,
    ) -> bool {
        let pos = crate::shared::ui_state::ScrollPosition {
            item_ix: anchor.item_ix,
            offset_in_item: anchor.offset_in_item.into(),
            follow_tail,
        };
        if let Some(store) = crate::shared::ui_state::UiStateStore::try_global()
            && let Ok(mut store) = store.lock()
        {
            return store.set_scroll(session_id, pos);
        }
        false
    }

    /// Load a previously persisted scroll position for a session from the
    /// global UI-state store (from a prior app run).
    fn load_persisted_scroll(&self, session_id: &str) -> Option<SavedScroll> {
        let store = crate::shared::ui_state::UiStateStore::try_global()?;
        let pos = store.lock().ok()?.get_scroll(session_id)?;
        Some(SavedScroll {
            anchor: gpui::ListOffset {
                item_ix: pos.item_ix,
                offset_in_item: px(pos.offset_in_item),
            },
            follow_tail: pos.follow_tail,
        })
    }

    /// Forget any remembered scroll for a session (e.g. after deletion).
    pub fn forget_session_scroll(&mut self, session_id: &str) {
        self.saved_scroll.remove(session_id);
        if self.displayed_session_id.as_deref() == Some(session_id) {
            self.displayed_session_id = None;
        }
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
    ///
    /// Uses [`ListState::scroll_to_end`] (anchoring past the last item) rather
    /// than `scroll_to_reveal_item`: right after a `reset()` the list items are
    /// still unmeasured, and `scroll_to_reveal_item` would compute a pixel
    /// target from those zero heights and land at the top. `scroll_to_end`
    /// instead lets the layout pass walk backwards from the end, which reveals
    /// the true bottom regardless of measurement state.
    fn scroll_to_bottom_instant(&self) {
        if self.list_state.item_count() > 0 {
            self.list_state.scroll_to_end();
        }
    }

    /// Stop any running smooth-scroll animation.
    fn stop_animation(&mut self) {
        self.animation_active.set(false);
        self.smooth_scroll_task = None;
    }

    fn remeasure_preserving_anchor(&self) {
        let count = self.list_state.item_count();
        if count == 0 {
            return;
        }

        if self.follow_tail {
            // A following session stays pinned to the bottom across remeasures
            // (e.g. markdown reflow settling after a restore). Anchor past the
            // last item so the layout re-reveals the true bottom.
            self.list_state.reset(count);
            self.list_state.scroll_to_end();
            return;
        }

        let anchor = self.list_state.logical_scroll_top();
        self.list_state.reset(count);
        self.list_state.scroll_to(anchor);
    }

    fn schedule_height_cache_refresh(&mut self, cx: &mut Context<Self>) {
        self.height_cache_refresh_task = None;

        let task = cx.spawn(async move |weak_entity, cx| {
            for delay_ms in [50_u64, 150, 350, 750, 1500] {
                cx.background_executor()
                    .timer(Duration::from_millis(delay_ms))
                    .await;

                let ok = weak_entity.update(cx, |this, cx| {
                    this.remeasure_preserving_anchor();
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        });

        self.height_cache_refresh_task = Some(task);
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
                cx.background_executor()
                    .timer(Duration::from_millis(ANIMATION_FRAME_MS))
                    .await;

                if !active.get() {
                    break;
                }

                // Compute current position (negative) and target.
                let current_offset: f32 = list_state.scroll_px_offset_for_scrollbar().y.into();

                let max: f32 = list_state.max_offset_for_scrollbar().y.into();

                // Safety: if max_offset is tiny or zero, the list likely has many
                // unmeasured items. Using set_offset_from_scrollbar in this state
                // would scroll to a wrong position. Fall back to reveal_item.
                if max < 100.0 {
                    let count = list_state.item_count();
                    if count > 0 {
                        list_state.scroll_to_reveal_item(count - 1);
                    }
                    let _ = weak_entity.update(cx, |_this, cx| {
                        cx.notify();
                    });
                    idle_elapsed_ms += ANIMATION_FRAME_MS;
                    if idle_elapsed_ms >= ANIMATION_IDLE_MS {
                        break;
                    }
                    continue;
                }

                // Target offset is -max (fully scrolled to bottom). The
                // scrollbar API returns offset.y as negative.
                let target: f32 = -max;

                // displacement > 0 means we're above the target (need to scroll down).
                let displacement = current_offset - target;

                if displacement.abs() < MIN_DISTANCE_TO_STOP && velocity.abs() < MIN_SPEED_TO_STOP {
                    // We've converged. Snap to exact bottom and enter idle phase.
                    if displacement.abs() > 0.01 {
                        // set_offset_from_scrollbar expects negative y (same
                        // convention as scroll_px_offset_for_scrollbar).
                        list_state.set_offset_from_scrollbar(gpui::Point {
                            x: px(0.),
                            y: px(-max.round()),
                        });
                        last_offset.set(-max.round());
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

                // new_y is the new scroll offset (negative, like
                // scroll_px_offset_for_scrollbar returns).
                // set_offset_from_scrollbar expects this same sign convention.
                let new_y = current_offset + delta;
                list_state.set_offset_from_scrollbar(gpui::Point {
                    x: px(0.),
                    y: px(new_y.min(0.0)),
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
    pub fn activate_follow_tail(&mut self) {
        self.follow_tail = true;
        self.scroll_to_bottom();
    }

    // -----------------------------------------------------------------
    // Edge auto-scroll (drag-to-select near a viewport edge)
    // -----------------------------------------------------------------

    /// Update the desired edge auto-scroll velocity from a pointer position.
    ///
    /// Called from the mouse-move handler. It only sets the *target* velocity
    /// (and starts the self-firing task); the actual scrolling happens
    /// independently in [`Self::spawn_edge_scroll_task`], so a pointer that is
    /// parked beyond the edge (no further move events) keeps scrolling until
    /// the button is released or the pointer moves back into the neutral zone.
    fn update_edge_scroll(&mut self, pointer_y: f32, cx: &mut Context<Self>) {
        let bounds = self.list_bounds.get();
        let top: f32 = bounds.origin.y.into();
        let height: f32 = bounds.size.height.into();
        let bottom = top + height;

        // Ignore until we have a measured viewport.
        if height <= 0.0 {
            self.edge_scroll_velocity.set(0.0);
            return;
        }

        let velocity = edge_scroll_velocity(pointer_y, top, bottom);
        self.edge_scroll_velocity.set(velocity);

        if velocity != 0.0 && self.edge_scroll_task.is_none() {
            self.spawn_edge_scroll_task(cx);
        }
    }

    /// Stop edge auto-scrolling (drag ended or pointer returned inside).
    fn stop_edge_scroll(&mut self) {
        self.edge_drag_active.set(false);
        self.edge_scroll_velocity.set(0.0);
        self.edge_scroll_task = None;
    }

    /// Spawn the self-firing edge auto-scroll task. It ticks every frame and
    /// applies the current `edge_scroll_velocity` to the list offset, clamped
    /// to the scrollable range. The task shuts itself down once the drag ends
    /// (`edge_drag_active` is false) and the velocity has settled to zero.
    fn spawn_edge_scroll_task(&mut self, cx: &mut Context<Self>) {
        self.edge_scroll_task = None;

        let list_state = self.list_state.clone();
        let velocity = self.edge_scroll_velocity.clone();
        let drag_active = self.edge_drag_active.clone();

        let task = cx.spawn(async move |weak_entity, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(EDGE_SCROLL_FRAME_MS))
                    .await;

                let v = velocity.get();

                // Stop condition: no velocity AND the drag is no longer active.
                // (If the drag is still active but velocity is 0, the pointer
                // is in the neutral zone — keep the task alive so we resume
                // instantly when it re-enters an edge band.)
                if v == 0.0 && !drag_active.get() {
                    break;
                }

                if v == 0.0 {
                    continue;
                }

                let max: f32 = list_state.max_offset_for_scrollbar().y.into();
                if max <= 0.0 {
                    // Content fits; nothing to scroll.
                    continue;
                }

                let current: f32 = list_state.scroll_px_offset_for_scrollbar().y.into();
                // Offset is negative (0 = top, -max = bottom). Positive
                // velocity scrolls up (toward 0); negative scrolls down.
                let new_y = (current + v).clamp(-max, 0.0);

                if (new_y - current).abs() < f32::EPSILON {
                    // Already pinned at the edge in the requested direction.
                    continue;
                }

                list_state.set_offset_from_scrollbar(gpui::Point {
                    x: px(0.),
                    y: px(new_y),
                });

                let ok = weak_entity.update(cx, |this, cx| {
                    // Auto-scrolling away from the bottom means the user is no
                    // longer following the tail.
                    if this.follow_tail && new_y > -max + 1.0 {
                        this.follow_tail = false;
                        // Persist the new (non-following) position on this drag
                        // transition; subsequent frames are no-ops.
                        this.persist_current_scroll(cx);
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        });

        self.edge_scroll_task = Some(task);
    }

    /// Get the list state (for scrollbar integration)
    #[allow(dead_code)]
    pub fn list_state(&self) -> &ListState {
        &self.list_state
    }

    /// Update the pending message for the current session
    pub fn update_pending_message(&self, message: Option<String>) {
        *self.current_pending_message.lock().unwrap() = message;
    }

    /// Update the current project for parameter filtering
    pub fn set_current_project(&self, project: String) {
        *self.current_project.lock().unwrap() = project;
    }

    /// Update whether the currently displayed session is "resumable" — i.e.
    /// the agent ended in a state that suggests an unfinished iteration
    /// (e.g. crashed mid-tool-call). When true and the session is idle, the
    /// MessagesView shows a floating Resume button above the input area.
    pub fn set_is_resumable(&self, is_resumable: bool) {
        self.is_resumable.set(is_resumable);
    }
}

impl Focusable for MessagesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessagesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // If no session is connected, show a centered hint instead of the
        // message list.
        let has_session = self.current_session_id.lock().unwrap().is_some();
        if !has_session {
            return div()
                .id("messages")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .size_full()
                .bg(cx.theme().popover)
                .child(
                    div().flex().flex_col().items_center().gap_2().child(
                        div()
                            .text_size(rems(1.125))
                            .text_color(cx.theme().muted_foreground)
                            .child("Select a session or start a new one from the sidebar"),
                    ),
                )
                .into_any();
        }

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
                    message_item::render_message(this, index, window, cx)
                } else if index == pending_index && has_pending {
                    activity_indicator::render_pending_message(this, cx)
                } else {
                    // Activity indicator (last item)
                    activity_indicator::render_activity_indicator(this, cx)
                };
                let scale = cx.theme().font_size / px(16.0);
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .max_w(px(MAX_MESSAGE_WIDTH * scale))
                            .w_full()
                            .child(inner),
                    )
                    .into_any_element()
            }),
        )
        .flex_grow(1.0)
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

            tracing::trace!("ListState count sync: {} → {}", current_count, item_count);
        }

        // If the animation flag was set but the task hasn't been spawned yet,
        // spawn it now (we need a Context<Self> to call cx.spawn).
        if self.animation_active.get() && self.smooth_scroll_task.is_none() {
            self.spawn_animation_task(cx);
        }

        let show_scroll_button = !self.follow_tail && total_items > 0;

        // Resume button: shown when the session ended in a "stuck" state
        // (last message is a user message or an assistant message with
        // un-answered tool calls) AND the agent is currently idle/errored
        // and not running externally. Indicates that clicking will retry
        // the agent against the existing message history.
        let activity_state_snapshot = self.activity_state.lock().ok().and_then(|g| g.clone());
        let agent_is_terminal = activity_state_snapshot
            .as_ref()
            .is_none_or(SessionActivityState::is_terminal);
        let externally_locked = activity_state_snapshot
            .as_ref()
            .is_some_and(SessionActivityState::is_running_externally);
        let show_resume_button =
            self.is_resumable.get() && agent_is_terminal && !externally_locked && total_items > 0;

        div()
            .on_children_prepainted({
                let list_bounds = self.list_bounds.clone();
                move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                    if let Some(first) = bounds_vec.first()
                        && list_bounds.get() != *first
                    {
                        list_bounds.set(*first);
                    }
                }
            })
            .id("messages")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .size_full()
            .overflow_hidden() // Prevent width fluctuation that invalidates list item heights
            .bg(cx.theme().popover)
            .text_size(rems(1.0))
            // Edge auto-scroll: while the left button is held and the pointer
            // reaches the top/bottom edge band, ramp up an auto-scroll speed.
            // The actual scrolling runs in a self-firing task, so parking the
            // pointer beyond the edge keeps scrolling until release. The list
            // viewport bounds are captured above via on_children_prepainted.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.edge_drag_active.set(true);
                    this.update_edge_scroll(event.position.y.into(), cx);
                } else if this.edge_drag_active.get() {
                    // Button released elsewhere but we missed the up event —
                    // moving without a held button ends the drag.
                    this.stop_edge_scroll();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.stop_edge_scroll();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.stop_edge_scroll();
                }),
            )
            .child(message_list)
            .when(show_scroll_button, |el| {
                let is_dark = cx.theme().is_dark();
                let btn_bg = cx.theme().muted;
                // Hover must be fully opaque — opacity() sets alpha and causes
                // see-through artifacts. Use a solid theme token instead.
                let btn_hover_bg = cx.theme().secondary;
                let btn_border = if is_dark {
                    cx.theme().muted_foreground.opacity(0.4)
                } else {
                    cx.theme().border
                };
                let btn_hover_border = cx.theme().muted_foreground;
                el.child(
                    // Full-width absolute wrapper to center the button horizontally
                    div()
                        .absolute()
                        .bottom_4()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("scroll-to-bottom")
                                .flex()
                                .items_center()
                                .justify_center()
                                .w_8()
                                .h_8()
                                .rounded_full()
                                .bg(btn_bg)
                                .border_1()
                                .border_color(btn_border)
                                .shadow_md()
                                .cursor(gpui::CursorStyle::PointingHand)
                                .hover(move |s| s.bg(btn_hover_bg).border_color(btn_hover_border))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/chevron_down.svg"))
                                        .text_color(cx.theme().muted_foreground)
                                        .size(px(16.)),
                                )
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.activate_follow_tail();
                                    this.persist_current_scroll(cx);
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(show_resume_button, |el| {
                // When the scroll-to-bottom button is also visible, lift the
                // resume button so they don't overlap. Both are anchored to
                // the bottom of the messages area.
                let bottom_offset = if show_scroll_button { px(56.) } else { px(16.) };
                let btn_bg = cx.theme().primary;
                let btn_hover_bg = cx.theme().primary_hover;
                let btn_fg = cx.theme().primary_foreground;
                let session_id = self.current_session_id.lock().unwrap().clone();
                el.child(
                    // Full-width absolute wrapper to center the button horizontally
                    div()
                        .absolute()
                        .bottom(bottom_offset)
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("resume-session")
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap_1p5()
                                .h_8()
                                .px_3()
                                .rounded_full()
                                .bg(btn_bg)
                                .shadow_md()
                                .cursor(gpui::CursorStyle::PointingHand)
                                .hover(move |s| s.bg(btn_hover_bg))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/rotate_ccw.svg"))
                                        .text_color(btn_fg)
                                        .size(px(14.)),
                                )
                                .child(
                                    div()
                                        .text_color(btn_fg)
                                        .text_size(rems(0.875))
                                        .child("Resume"),
                                )
                                .on_click(cx.listener(move |_this, _event, _window, cx| {
                                    let Some(session_id) = session_id.clone() else {
                                        return;
                                    };
                                    if let Some(gpui) = cx.try_global::<Gpui>() {
                                        gpui.cmd_resume_session(session_id);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
            })
            .vertical_scrollbar(&self.list_state)
            .into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    /// Initialize globals needed for tests (theme).
    fn init_test_globals(cx: &mut gpui::App) {
        gpui_component::theme::init(cx);
    }

    #[gpui::test]
    fn test_messages_view_starts_with_follow_tail(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            init_test_globals(cx);
            let queue = Arc::new(Mutex::new(Vec::new()));
            let activity = Arc::new(Mutex::new(None));
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, _| {
                assert!(view.follow_tail);
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_messages_spliced_updates_list_state(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));
        let queue_clone = queue.clone();

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue_clone, activity, cx))
            })
            .unwrap()
        });

        // Initially 0 items
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.list_state.item_count(), 0);
            })
            .unwrap();

        // Simulate adding 3 messages externally
        window
            .update(cx, |view, _, cx| {
                view.messages_spliced(0, 3, cx);
            })
            .unwrap();

        window
            .update(cx, |view, _, _| {
                assert_eq!(view.list_state.item_count(), 3);
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_messages_spliced_no_op_when_no_growth(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        // Splice with same old/new should be no-op
        window
            .update(cx, |view, _, cx| {
                view.messages_spliced(5, 5, cx);
                // item_count stays at 0 (initial) because we never actually spliced
                assert_eq!(view.list_state.item_count(), 0);
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_messages_reset_resets_list_state(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        // First add some items
        window
            .update(cx, |view, _, cx| {
                view.messages_spliced(0, 5, cx);
                assert_eq!(view.list_state.item_count(), 5);
            })
            .unwrap();

        // Reset to 2 items (same-session clear/reload path)
        window
            .update(cx, |view, _, cx| {
                view.messages_reset_for_session(None, 2, cx);
                assert_eq!(view.list_state.item_count(), 2);
                // follow_tail should be re-enabled on reset
                assert!(view.follow_tail);
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_messages_reset_to_zero(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                view.messages_spliced(0, 10, cx);
                view.messages_reset_for_session(None, 0, cx);
                assert_eq!(view.list_state.item_count(), 0);
                assert!(view.follow_tail);
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_activate_follow_tail(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                // Add items so scroll_to_bottom does something
                view.messages_spliced(0, 5, cx);
                // Disable follow_tail
                view.follow_tail = false;
                assert!(!view.follow_tail);
                // Activate
                view.activate_follow_tail();
                assert!(view.follow_tail);
                // Animation should be flagged
                assert!(view.animation_active.get());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_scroll_to_bottom_no_op_when_empty(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, _| {
                view.scroll_to_bottom();
                // Animation should NOT be active because there are no items
                assert!(!view.animation_active.get());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_stop_animation(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                view.messages_spliced(0, 5, cx);
                view.ensure_animation_running();
                assert!(view.animation_active.get());
                view.stop_animation();
                assert!(!view.animation_active.get());
                assert!(view.smooth_scroll_task.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_set_current_session_id(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, _| {
                assert!(view.get_current_session_id().is_none());
                view.set_current_session_id(Some("session-123".to_string()));
                assert_eq!(
                    view.get_current_session_id(),
                    Some("session-123".to_string())
                );
                view.set_current_session_id(None);
                assert!(view.get_current_session_id().is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_update_pending_message(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, _| {
                assert!(view.current_pending_message.lock().unwrap().is_none());
                view.update_pending_message(Some("Hello world".to_string()));
                assert_eq!(
                    *view.current_pending_message.lock().unwrap(),
                    Some("Hello world".to_string())
                );
                view.update_pending_message(None);
                assert!(view.current_pending_message.lock().unwrap().is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_messages_spliced_triggers_animation_when_following(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                assert!(view.follow_tail);
                view.messages_spliced(0, 3, cx);
                // Animation should be flagged because follow_tail is true
                assert!(view.animation_active.get());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_messages_spliced_no_animation_when_not_following(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                view.follow_tail = false;
                view.messages_spliced(0, 3, cx);
                // Animation should NOT be triggered because follow_tail is false
                assert!(!view.animation_active.get());
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_same_session_reset_preserves_scroll_offset(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                // Show a session with some messages, scrolled away from the bottom.
                view.messages_reset_for_session(Some("s1".to_string()), 10, cx);
                view.follow_tail = false;
                view.list_state.scroll_to(ListOffset {
                    item_ix: 4,
                    offset_in_item: px(7.0),
                });

                // A same-session resync arrives (e.g. lagged stream, file watcher).
                view.messages_reset_for_session(Some("s1".to_string()), 10, cx);

                // The scroll anchor must be preserved, not jumped to bottom.
                let anchor = view.list_state.logical_scroll_top();
                assert_eq!(anchor.item_ix, 4);
                assert_eq!(anchor.offset_in_item, px(7.0));
                // And follow_tail stays disabled.
                assert!(!view.follow_tail);
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_restoring_bottom_session_anchors_to_end(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                // A long session the user scrolled to the very end (follow_tail
                // stays true near the bottom).
                view.messages_reset_for_session(Some("long".to_string()), 100, cx);
                assert!(view.follow_tail);

                // Switch away, then back.
                view.messages_reset_for_session(Some("other".to_string()), 3, cx);
                view.messages_reset_for_session(Some("long".to_string()), 100, cx);

                // A following session must anchor to the END sentinel so the
                // layout can walk backwards and reveal the bottom — even though
                // the freshly reset items are still unmeasured. Anchoring to a
                // concrete item index (scroll_to_reveal_item) would land at the
                // top here.
                assert!(view.follow_tail);
                let anchor = view.list_state.logical_scroll_top();
                assert_eq!(
                    anchor.item_ix,
                    view.list_state.item_count(),
                    "follow-tail restore must anchor past the last item (scroll_to_end)"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_session_switch_saves_and_restores_scroll(cx: &mut TestAppContext) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(Mutex::new(None));

        let window = cx.update(|cx| {
            init_test_globals(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| MessagesView::new(queue, activity, cx))
            })
            .unwrap()
        });

        window
            .update(cx, |view, _, cx| {
                // Session s1: scroll to a specific anchor and stop following.
                view.messages_reset_for_session(Some("s1".to_string()), 10, cx);
                view.follow_tail = false;
                view.list_state.scroll_to(ListOffset {
                    item_ix: 3,
                    offset_in_item: px(12.0),
                });

                // Switch to s2 — s1's scroll must be saved; s2 (unseen) defaults
                // to following the tail.
                view.messages_reset_for_session(Some("s2".to_string()), 8, cx);
                assert!(view.follow_tail, "unseen session should follow the tail");

                // Scroll s2 somewhere too.
                view.follow_tail = false;
                view.list_state.scroll_to(ListOffset {
                    item_ix: 1,
                    offset_in_item: px(0.0),
                });

                // Back to s1 — the saved anchor and follow_tail=false are restored.
                view.messages_reset_for_session(Some("s1".to_string()), 10, cx);
                assert!(!view.follow_tail, "returning session restores follow state");
                let anchor = view.list_state.logical_scroll_top();
                assert_eq!(anchor.item_ix, 3);
                assert_eq!(anchor.offset_in_item, px(12.0));
            })
            .unwrap();
    }
}
