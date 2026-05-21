//! Smooth-scroll configuration and scroll handler for MessagesView.
//!
//! Uses a spring-damper model for natural scrolling animation behavior.

use gpui::{Context, ListState};
use std::cell::Cell;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Smooth-scroll configuration (spring-damper model, same tuning as the old
// AutoScrollContainer).
// ---------------------------------------------------------------------------

/// How often the animation loop ticks (~120 FPS).
pub const ANIMATION_FRAME_MS: u64 = 8;
/// Spring constant.
pub const SPRING_K: f32 = 0.035;
/// Damping constant.
pub const DAMPING_C: f32 = 0.32;
/// Stop threshold: distance in pixels.
pub const MIN_DISTANCE_TO_STOP: f32 = 0.5;
/// Stop threshold: speed in pixels/frame.
pub const MIN_SPEED_TO_STOP: f32 = 0.5;
/// How long the animation idles at the bottom before shutting down.
/// While idling, it keeps checking for new content growth and instantly
/// resumes scrolling — this avoids the race where content arrives between
/// animation stop and the next `scroll_to_bottom()` call.
pub const ANIMATION_IDLE_MS: u64 = 2000;

/// Install the scroll handler on the list state.
///
/// This handler is ONLY called on real ScrollWheelEvent (mouse/trackpad),
/// never for programmatic offset changes (set_offset_from_scrollbar).
///
/// Strategy: we only care about *direction*. If the user scrolls upward
/// (away from bottom), we disable follow_tail and stop the animation.
/// If they scroll downward, we check if they reached the bottom and
/// re-enable follow_tail.
pub fn install_scroll_handler(
    list_state: &ListState,
    animation_active: &Rc<Cell<bool>>,
    cx: &mut Context<super::MessagesView>,
) {
    let entity = cx.entity().downgrade();
    let anim_active_for_handler = animation_active.clone();
    let prev_scroll_offset: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));

    list_state.set_scroll_handler(move |_event, _window, cx| {
        let entity = entity.clone();
        let anim_active = anim_active_for_handler.clone();
        let prev_offset = prev_scroll_offset.clone();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                // offset.y is negative: 0 = top, -max = bottom
                let current: f32 = this.list_state.scroll_px_offset_for_scrollbar().y.into();
                let previous = prev_offset.get();
                prev_offset.set(current);

                let max: f32 = this.list_state.max_offset_for_scrollbar().y.into();

                // delta > 0 means offset.y moved toward 0 (= scrolled UP)
                let delta = current - previous;

                if delta > 0.5 {
                    // User scrolled UP → disable follow
                    if this.follow_tail {
                        this.follow_tail = false;
                        cx.notify();
                    }
                    anim_active.set(false);
                    this.smooth_scroll_task = None;
                } else if delta < -0.5 {
                    // User scrolled DOWN → check if near bottom, re-enable follow.
                    let current_abs: f32 = (-current).max(0.0);
                    if max > 100.0 && current_abs > max * 0.8 {
                        let distance_from_bottom = max - current_abs;
                        if distance_from_bottom < 50.0 && !this.follow_tail {
                            this.follow_tail = true;
                            cx.notify();
                        }
                    }
                }
            });
        });
    });
}
