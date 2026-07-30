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

// ---------------------------------------------------------------------------
// Edge auto-scroll configuration (drag-to-select near the viewport edge).
// ---------------------------------------------------------------------------

/// How often the edge auto-scroll loop ticks (~120 FPS).
pub const EDGE_SCROLL_FRAME_MS: u64 = 8;
/// Distance (in px) from the top/bottom viewport edge within which a held
/// drag begins to auto-scroll. Also the band across which the speed ramps
/// from zero (at the inner boundary) to the maximum (at the edge and beyond).
pub const EDGE_SCROLL_MARGIN: f32 = 48.0;
/// Maximum auto-scroll speed in px/frame when the pointer is at (or beyond)
/// the very edge of the viewport.
pub const EDGE_SCROLL_MAX_SPEED: f32 = 22.0;
/// Minimum auto-scroll speed in px/frame once inside the margin, so the
/// scroll feels responsive right at the inner boundary rather than crawling.
pub const EDGE_SCROLL_MIN_SPEED: f32 = 2.0;

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

/// Compute the desired edge auto-scroll velocity (px/frame) from the pointer's
/// vertical position relative to the viewport.
///
/// The sign convention matches `set_offset_from_scrollbar` (offset.y is
/// negative, 0 = top, -max = bottom):
/// - a *negative* return value scrolls the content DOWN (reveals content
///   below — used when the pointer is near the bottom edge),
/// - a *positive* return value scrolls UP,
/// - `0.0` means the pointer is inside the neutral zone: no auto-scroll.
///
/// `pointer_y`, `top` and `bottom` are all in window coordinates.
pub fn edge_scroll_velocity(pointer_y: f32, top: f32, bottom: f32) -> f32 {
    // Near / beyond the top edge → scroll up (offset.y toward 0, i.e. positive
    // delta applied to the negative offset).
    if pointer_y < top + EDGE_SCROLL_MARGIN {
        let depth = ((top + EDGE_SCROLL_MARGIN) - pointer_y).min(EDGE_SCROLL_MARGIN);
        return ramp_speed(depth);
    }

    // Near / beyond the bottom edge → scroll down (offset.y more negative).
    if pointer_y > bottom - EDGE_SCROLL_MARGIN {
        let depth = (pointer_y - (bottom - EDGE_SCROLL_MARGIN)).min(EDGE_SCROLL_MARGIN);
        return -ramp_speed(depth);
    }

    0.0
}

/// Map a penetration depth into the edge margin (0..=EDGE_SCROLL_MARGIN) to a
/// speed magnitude, ramping linearly from the minimum to the maximum speed.
fn ramp_speed(depth: f32) -> f32 {
    let t = (depth / EDGE_SCROLL_MARGIN).clamp(0.0, 1.0);
    EDGE_SCROLL_MIN_SPEED + t * (EDGE_SCROLL_MAX_SPEED - EDGE_SCROLL_MIN_SPEED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_scroll_in_neutral_zone() {
        // Viewport from y=100 to y=500; a point in the middle → no scroll.
        assert_eq!(edge_scroll_velocity(300.0, 100.0, 500.0), 0.0);
        // Just inside the top margin boundary.
        assert_eq!(
            edge_scroll_velocity(100.0 + EDGE_SCROLL_MARGIN, 100.0, 500.0),
            0.0
        );
        // Just inside the bottom margin boundary.
        assert_eq!(
            edge_scroll_velocity(500.0 - EDGE_SCROLL_MARGIN, 100.0, 500.0),
            0.0
        );
    }

    #[test]
    fn scrolls_up_near_top_edge() {
        // At the very top edge → positive (scroll up), at max speed.
        let v = edge_scroll_velocity(100.0, 100.0, 500.0);
        assert!(v > 0.0);
        assert!((v - EDGE_SCROLL_MAX_SPEED).abs() < 1e-3);
    }

    #[test]
    fn scrolls_up_beyond_top_edge() {
        // Parked above the viewport → still max speed, clamped.
        let v = edge_scroll_velocity(20.0, 100.0, 500.0);
        assert!((v - EDGE_SCROLL_MAX_SPEED).abs() < 1e-3);
    }

    #[test]
    fn scrolls_down_near_bottom_edge() {
        // At the very bottom edge → negative (scroll down), at max speed.
        let v = edge_scroll_velocity(500.0, 100.0, 500.0);
        assert!(v < 0.0);
        assert!((v + EDGE_SCROLL_MAX_SPEED).abs() < 1e-3);
    }

    #[test]
    fn scrolls_down_beyond_bottom_edge() {
        // Parked below the viewport → still max speed (clamped), scrolling down.
        let v = edge_scroll_velocity(900.0, 100.0, 500.0);
        assert!((v + EDGE_SCROLL_MAX_SPEED).abs() < 1e-3);
    }

    #[test]
    fn speed_ramps_with_depth() {
        // Halfway into the top margin → roughly the midpoint speed.
        let mid_y = 100.0 + EDGE_SCROLL_MARGIN / 2.0;
        let v = edge_scroll_velocity(mid_y, 100.0, 500.0);
        let expected =
            EDGE_SCROLL_MIN_SPEED + 0.5 * (EDGE_SCROLL_MAX_SPEED - EDGE_SCROLL_MIN_SPEED);
        assert!((v - expected).abs() < 0.5, "v={v} expected≈{expected}");
    }
}
