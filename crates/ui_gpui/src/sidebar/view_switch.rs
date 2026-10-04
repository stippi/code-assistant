//! The header's switch between the active sessions and the project folders.
//!
//! A segmented control like gpui-component's, drawn here because that one
//! paints its active segment from the theme's global background token and
//! cannot stand out on the sidebar header in dark mode. The active pill
//! slides on the same spring as gpui-component's tab indicator.

use super::SessionSidebar;
use crate::shared::settings::SidebarView;
use gpui_kit::base::spring;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    BoxShadow, Context, InteractiveElement, Rems, SharedString, StatefulInteractiveElement, Styled,
    Window, div, hsla, prelude::*, px, rems,
};

const SEGMENTS: [(SidebarView, &str); 2] = [
    (SidebarView::Inbox, "Active"),
    (SidebarView::Projects, "Projects"),
];
/// Segments share one width, so the pill's target needs no measuring.
const SEGMENT_WIDTH: Rems = rems(4.75);
const SEGMENT_HEIGHT: Rems = rems(1.625);
const SEGMENT_GAP: Rems = rems(0.125);
const TRACK_PADDING: Rems = rems(0.1875);

impl SessionSidebar {
    pub(super) fn render_view_switch(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
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
        let selected = SEGMENTS
            .iter()
            .position(|(view, _)| *view == self.view)
            .unwrap_or_default();
        let target = rems(selected as f32 * (SEGMENT_WIDTH.0 + SEGMENT_GAP.0));
        let pill_left = spring(
            ("sidebar-view-switch", "left"),
            target,
            cx.theme().motion_tokens().spring_move,
            window,
            cx,
        );

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
            .children(
                SEGMENTS
                    .into_iter()
                    .enumerate()
                    .map(|(index, (view, label))| {
                        let is_selected = index == selected;
                        div()
                            .id(SharedString::from(format!("sidebar-view-{label}")))
                            .w(SEGMENT_WIDTH)
                            .h(SEGMENT_HEIGHT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(rems(0.8125))
                            .map(|el| {
                                if is_selected {
                                    el.text_color(cx.theme().foreground)
                                } else {
                                    el.cursor_pointer()
                                        .text_color(cx.theme().muted_foreground)
                                        .hover(|s| s.text_color(cx.theme().foreground))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_view(view, cx)
                                        }))
                                }
                            })
                            .child(label)
                    }),
            )
    }
}
