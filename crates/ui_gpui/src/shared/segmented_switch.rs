//! A switch between a few views, as in the left sidebar's header (Active |
//! Projects) and the right panel's (Review | Browser).
//!
//! A segmented control like gpui-component's, drawn here because that one
//! paints its active segment from the theme's global background token and
//! cannot stand out on a header in dark mode. The active pill slides on the
//! same spring as gpui-component's tab indicator.

use gpui_kit::base::spring;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, BoxShadow, ClickEvent, InteractiveElement, Rems, SharedString, StatefulInteractiveElement,
    Styled, Window, div, hsla, prelude::*, px, rems,
};
use std::rc::Rc;

/// Segments share one width, so the pill's target needs no measuring.
const SEGMENT_WIDTH: Rems = rems(4.75);
const SEGMENT_HEIGHT: Rems = rems(1.625);
const SEGMENT_GAP: Rems = rems(0.125);
const TRACK_PADDING: Rems = rems(0.1875);

/// A switch over `segments` (value, label) with `selected` active.
/// `on_select` runs when another segment is clicked. `id` keys the pill's
/// spring and the segments' element ids.
pub fn segmented_switch<T: Copy + PartialEq + 'static>(
    id: &'static str,
    segments: &[(T, &'static str)],
    selected: T,
    on_select: impl Fn(T, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let (track, active) = if cx.theme().is_dark() {
        (hsla(0., 0., 0., 0.25), hsla(0., 0., 1., 0.12))
    } else {
        (hsla(0., 0., 0., 0.07), hsla(0., 0., 1., 1.))
    };
    let ink = hsla(0., 0., 0., 0.1);
    let shadow = vec![
        BoxShadow::new(px(0.), px(1.), ink).blur_radius(px(1.5)),
        BoxShadow::new(px(0.), px(1.), ink)
            .blur_radius(px(1.))
            .spread_radius(px(-1.)),
    ];
    let index = segments
        .iter()
        .position(|(value, _)| *value == selected)
        .unwrap_or_default();
    let target = rems(index as f32 * (SEGMENT_WIDTH.0 + SEGMENT_GAP.0));
    let pill_left = spring(
        (id, "left"),
        target,
        cx.theme().motion_tokens().spring_move,
        window,
        cx,
    );
    let on_select = Rc::new(on_select);
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;

    div()
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .gap(SEGMENT_GAP)
        .p(TRACK_PADDING)
        .rounded_lg()
        .bg(track)
        .child(
            div()
                .absolute()
                .top(TRACK_PADDING)
                .left(rems(TRACK_PADDING.0 + pill_left.0))
                .w(SEGMENT_WIDTH)
                .h(SEGMENT_HEIGHT)
                .rounded_md()
                .bg(active)
                .shadow(shadow),
        )
        .children(segments.iter().enumerate().map(|(i, &(value, label))| {
            let on_select = on_select.clone();
            div()
                .id(SharedString::from(format!("{id}-{label}")))
                .w(SEGMENT_WIDTH)
                .h(SEGMENT_HEIGHT)
                .flex()
                .items_center()
                .justify_center()
                .text_size(rems(0.8125))
                .map(|el| {
                    if i == index {
                        el.text_color(foreground)
                    } else {
                        el.cursor_pointer()
                            .text_color(muted)
                            .hover(move |s| s.text_color(foreground))
                            .on_click(move |_: &ClickEvent, window, cx| {
                                on_select(value, window, cx)
                            })
                    }
                })
                .child(label)
        }))
}
