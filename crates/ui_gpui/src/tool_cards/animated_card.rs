//! Animated card body helper for tool block collapse/expand transitions.

use gpui::prelude::FluentBuilder;
use gpui::{div, px, Bounds, Div, IntoElement, ParentElement, Pixels, Styled};
use std::cell::Cell;
use std::rc::Rc;

/// Wrap the card body in an animated height container.
///
/// Uses the persistent `content_height` from `CardRenderContext` so the
/// measured natural height survives across frames.  The wrapper constrains
/// the visible height to `natural_height × animation_scale`.
pub fn animated_card_body(
    body_content: impl IntoElement,
    animation_scale: f32,
    content_height: Rc<Cell<Pixels>>,
) -> Div {
    let height_for_render = content_height.clone();

    div()
        .overflow_hidden()
        .when(animation_scale < 1.0, move |d| {
            let h = height_for_render.get();
            if h > px(0.0) {
                d.h(h * animation_scale)
            } else {
                // Height not yet measured — constrain to zero so content
                // stays hidden until on_children_prepainted provides the
                // real value on the next frame.  Without this the content
                // flashes at full height for one frame.
                d.h(px(0.0))
            }
        })
        .on_children_prepainted({
            move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                if let Some(first) = bounds_vec.first() {
                    let new_h = first.size.height;
                    if content_height.get() != new_h {
                        content_height.set(new_h);
                    }
                }
            }
        })
        .child(body_content)
}
