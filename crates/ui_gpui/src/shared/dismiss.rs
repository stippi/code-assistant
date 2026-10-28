//! Outside-click dismissal for the popovers drawn by hand (`deferred` +
//! `anchored`); gpui-component's `Popover` and `Select` close themselves.
//!
//! [`dismiss_layer`] is a transparent layer over the window, painted below
//! the popover. It doesn't occlude, so a press anywhere closes the popover
//! and still reaches what lies underneath. Popups that occlude above it —
//! the popover itself, a `Select` list opened from inside it — keep it from
//! being hit, so choosing in them leaves the popover open.
//!
//! [`TriggerBounds`] records where the popover's trigger is: a press on it
//! is left to the trigger's own toggle, which would otherwise reopen the
//! popover the layer just closed.

use gpui_kit::{
    App, Bounds, MouseDownEvent, Pixels, Window, anchored, canvas, deferred, div, point,
    prelude::*, px,
};
use std::cell::Cell;
use std::rc::Rc;

/// Where a popover's trigger was painted last frame.
#[derive(Clone, Default)]
pub struct TriggerBounds(Rc<Cell<Bounds<Pixels>>>);

impl TriggerBounds {
    /// An element filling the trigger (which must be `relative`) that
    /// records its bounds.
    pub fn probe(&self) -> impl IntoElement {
        let bounds = self.0.clone();
        canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
            .absolute()
            .size_full()
    }
}

/// The layer calling `on_dismiss` for a press anywhere except on `trigger`
/// and on what occludes above it. Add it before the popover.
pub fn dismiss_layer(
    trigger: &TriggerBounds,
    window: &Window,
    on_dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let trigger = trigger.0.clone();
    let viewport = window.viewport_size();
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            div()
                .w(viewport.width)
                .h(viewport.height)
                .on_any_mouse_down(move |event, window, cx| {
                    if !trigger.get().contains(&event.position) {
                        on_dismiss(event, window, cx);
                    }
                }),
        ),
    )
}
