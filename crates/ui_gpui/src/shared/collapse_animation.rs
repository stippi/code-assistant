//! The open/close animation of a collapsible region: the sidebars, the plan
//! banner, a tool block's output.
//!
//! The view owning a [`CollapseAnimator`] advances it once per drawn frame
//! from `render` with [`CollapseAnimator::tick`], which asks the window for
//! the next frame while the animation runs. So there is one step per frame
//! the display shows, whatever its rate, and the frame after the last step
//! draws the resting state. The view applies [`CollapseAnimator::scale`] to
//! the region's size.
//!
//! Time comes from the background executor's clock, which tests advance
//! by hand (`advance_clock`) and deliver frames to (`simulate_next_frame`).

use gpui_kit::{App, Window};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
enum State {
    Idle,
    Animating {
        scale: f32,
        /// 0.0 = collapsing, 1.0 = expanding
        target: f32,
        start_time: Instant,
    },
}

/// Eases the scale of a collapsible region between 0.0 (collapsed) and 1.0
/// (expanded) over a fixed duration, reversing smoothly when told to go the
/// other way mid-animation.
#[derive(Clone, Debug)]
pub struct CollapseAnimator {
    duration: Duration,
    state: State,
}

impl CollapseAnimator {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            state: State::Idle,
        }
    }

    /// Begins animating towards expanded (`true`) or collapsed (`false`),
    /// or jumps there when the system asks for reduced motion.
    pub fn start(&mut self, expand: bool, cx: &App) {
        if cx.reduce_motion() {
            self.stop();
        } else {
            self.start_at(expand, cx.background_executor().now());
        }
    }

    pub fn start_at(&mut self, expand: bool, now: Instant) {
        let target = if expand { 1.0 } else { 0.0 };
        match &self.state {
            State::Animating {
                scale,
                target: current_target,
                ..
            } if *current_target != target => {
                // Reverse: keep the current scale and backdate the start so
                // the easing continues from there.
                let progress = if expand { *scale } else { 1.0 - *scale };
                self.state = State::Animating {
                    scale: *scale,
                    target,
                    start_time: now - self.duration.mul_f32(progress),
                };
            }
            _ => {
                self.state = State::Animating {
                    scale: if expand { 0.0 } else { 1.0 },
                    target,
                    start_time: now,
                };
            }
        }
    }

    /// Ends an animation in flight; [`Self::scale`] then gives the resting
    /// value.
    pub fn stop(&mut self) {
        self.state = State::Idle;
    }

    /// Advances by one drawn frame and asks for the next one while the
    /// animation still runs. Returns `true` in that case.
    pub fn tick(&mut self, window: &Window, cx: &App) -> bool {
        let running = self.tick_at(cx.background_executor().now());
        if running {
            window.request_animation_frame();
        }
        running
    }

    pub fn tick_at(&mut self, now: Instant) -> bool {
        let State::Animating {
            scale,
            target,
            start_time,
        } = &mut self.state
        else {
            return false;
        };
        let elapsed = now.saturating_duration_since(*start_time);
        let progress = (elapsed.as_secs_f32() / self.duration.as_secs_f32()).min(1.0);
        // ease-out cubic
        let eased = 1.0 - (1.0 - progress).powi(3);
        *scale = if *target == 1.0 { eased } else { 1.0 - eased };
        if progress >= 1.0 {
            self.state = State::Idle;
            false
        } else {
            true
        }
    }

    pub fn is_animating(&self) -> bool {
        matches!(self.state, State::Animating { .. })
    }

    /// The region's current scale: 0.0 = collapsed, 1.0 = expanded.
    /// `collapsed` supplies the resting value while nothing animates.
    pub fn scale(&self, collapsed: bool) -> f32 {
        match &self.state {
            State::Animating { scale, .. } => *scale,
            State::Idle if collapsed => 0.0,
            State::Idle => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DURATION: Duration = Duration::from_millis(250);

    fn ms(start: Instant, n: u64) -> Instant {
        start + Duration::from_millis(n)
    }

    #[test]
    fn eases_out_and_rests_at_the_target() {
        let start = Instant::now();
        let mut animator = CollapseAnimator::new(DURATION);
        assert!(!animator.is_animating());
        assert_eq!(animator.scale(true), 0.0);
        assert_eq!(animator.scale(false), 1.0);

        animator.start_at(true, start);
        assert!(animator.is_animating());
        assert_eq!(animator.scale(true), 0.0);

        assert!(animator.tick_at(ms(start, 125)));
        let half = animator.scale(true);
        assert!(
            half > 0.5 && half < 1.0,
            "ease-out is past half way: {half}"
        );
        assert!(animator.tick_at(ms(start, 200)));
        assert!(animator.scale(true) > half);

        assert!(!animator.tick_at(ms(start, 250)));
        assert!(!animator.is_animating());
        assert_eq!(animator.scale(false), 1.0);
    }

    #[test]
    fn reversing_keeps_the_current_scale() {
        let start = Instant::now();
        let mut animator = CollapseAnimator::new(DURATION);
        animator.start_at(false, start);
        animator.tick_at(ms(start, 100));
        let scale = animator.scale(true);
        assert!(scale > 0.0 && scale < 1.0);

        animator.start_at(true, ms(start, 100));
        assert_eq!(animator.scale(true), scale);
        animator.tick_at(ms(start, 101));
        assert!(animator.scale(true) >= scale);
        assert!(!animator.tick_at(ms(start, 400)));
        assert_eq!(animator.scale(false), 1.0);
    }

    #[test]
    fn stop_rests_right_away() {
        let mut animator = CollapseAnimator::new(DURATION);
        animator.start_at(true, Instant::now());
        animator.stop();
        assert!(!animator.is_animating());
        assert_eq!(animator.scale(true), 0.0);
    }
}
