//! Shared collapse/expand state with animation for card renderers.
//!
//! Cards toggle collapsed/expanded on header click. The body area animates
//! its height using an ease-out cubic curve over 250ms.
//!
//! Usage in a card renderer:
//! ```ignore
//! let anim = card_collapse::get_state(&tool.id);
//! // ... render header with on_click that calls card_collapse::toggle(...)
//! if anim.body_scale > 0.0 {
//!     // render body wrapped in card_collapse::animated_body(...)
//! }
//! ```

use gpui::prelude::FluentBuilder;
use gpui::{div, px, Bounds, IntoElement, ParentElement, Pixels, Styled};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Animation parameters
// ---------------------------------------------------------------------------

/// Animation duration in milliseconds.
const DURATION_MS: f32 = 250.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct CollapseEntry {
    collapsed: bool,
    /// `None` when idle, `Some((start, from, to))` when animating.
    animation: Option<(Instant, f32, f32)>,
}

static STATE: OnceLock<Mutex<HashMap<String, CollapseEntry>>> = OnceLock::new();

fn state_map() -> &'static Mutex<HashMap<String, CollapseEntry>> {
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Snapshot returned to the renderer each frame.
#[derive(Clone, Copy)]
pub struct CollapseSnapshot {
    pub collapsed: bool,
    /// 0.0 = fully collapsed, 1.0 = fully expanded.
    /// During animation this is an intermediate value.
    pub body_scale: f32,
    /// `true` if an animation is still running and the caller should arrange
    /// for another repaint (e.g. via `cx.notify()`).
    pub animating: bool,
}

/// Read (and advance) the animation state for the given tool id.
pub fn get_state(tool_id: &str) -> CollapseSnapshot {
    let mut map = state_map().lock().unwrap();
    let entry = map.entry(tool_id.to_string()).or_insert(CollapseEntry {
        collapsed: false,
        animation: None,
    });

    if let Some((start, from, to)) = entry.animation {
        let elapsed = start.elapsed().as_millis() as f32;
        let progress = (elapsed / DURATION_MS).min(1.0);
        // ease-out cubic
        let eased = 1.0 - (1.0 - progress).powi(3);
        let scale = from + (to - from) * eased;

        if progress >= 1.0 {
            entry.animation = None;
            CollapseSnapshot {
                collapsed: entry.collapsed,
                body_scale: to,
                animating: false,
            }
        } else {
            CollapseSnapshot {
                collapsed: entry.collapsed,
                body_scale: scale,
                animating: true,
            }
        }
    } else {
        CollapseSnapshot {
            collapsed: entry.collapsed,
            body_scale: if entry.collapsed { 0.0 } else { 1.0 },
            animating: false,
        }
    }
}

/// Toggle collapsed state and start the animation.
pub fn toggle(tool_id: &str) {
    let mut map = state_map().lock().unwrap();
    let entry = map.entry(tool_id.to_string()).or_insert(CollapseEntry {
        collapsed: false,
        animation: None,
    });

    // Compute current scale (so reverse-animation starts from the right point).
    let current_scale = if let Some((start, from, to)) = entry.animation {
        let elapsed = start.elapsed().as_millis() as f32;
        let progress = (elapsed / DURATION_MS).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        from + (to - from) * eased
    } else if entry.collapsed {
        0.0
    } else {
        1.0
    };

    entry.collapsed = !entry.collapsed;
    let target = if entry.collapsed { 0.0 } else { 1.0 };
    entry.animation = Some((Instant::now(), current_scale, target));
}

// ---------------------------------------------------------------------------
// Animated body wrapper
// ---------------------------------------------------------------------------

/// Wrap the card body in an animated height container.
///
/// `body_content` is the actual body element. The wrapper measures the
/// natural height via `on_children_prepainted`, then constrains height to
/// `natural_height * body_scale` with `overflow_hidden`.
pub fn animated_body(body_content: impl IntoElement, body_scale: f32) -> gpui::Div {
    use std::cell::Cell;
    use std::rc::Rc;

    let measured_height: Rc<Cell<Pixels>> = Rc::new(Cell::new(px(0.0)));
    let height_for_render = measured_height.clone();

    div()
        .overflow_hidden()
        .when(body_scale < 1.0, move |d| {
            let h = height_for_render.get();
            if h > px(0.0) {
                d.h(h * body_scale)
            } else {
                d
            }
        })
        .on_children_prepainted({
            move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                if let Some(first) = bounds_vec.first() {
                    measured_height.set(first.size.height);
                }
            }
        })
        .child(body_content)
}
