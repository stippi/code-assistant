use crate::persistence::BranchInfo;
use gpui::{div, prelude::*, px, App, CursorStyle, MouseButton, SharedString, Window};
use gpui_component::{ActiveTheme, Icon};

/// A stateless branch navigation component styled as a bubble on the message border.
/// Displayed at the bottom-right corner of user messages where branches exist.
#[derive(Clone, IntoElement)]
pub struct BranchSwitcherElement {
    branch_info: BranchInfo,
    session_id: String,
}

impl BranchSwitcherElement {
    pub fn new(branch_info: BranchInfo, session_id: String) -> Self {
        Self {
            branch_info,
            session_id,
        }
    }
}

impl RenderOnce for BranchSwitcherElement {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let has_prev = self.branch_info.active_index > 0;
        let has_next = self.branch_info.active_index + 1 < self.branch_info.sibling_ids.len();
        let current = self.branch_info.active_index + 1;
        let total = self.branch_info.sibling_ids.len();

        let muted_color = cx.theme().muted_foreground;
        let active_color = cx.theme().foreground;

        // Get sibling IDs for navigation
        let prev_node_id = if has_prev {
            self.branch_info
                .sibling_ids
                .get(self.branch_info.active_index - 1)
                .copied()
        } else {
            None
        };

        let next_node_id = if has_next {
            self.branch_info
                .sibling_ids
                .get(self.branch_info.active_index + 1)
                .copied()
        } else {
            None
        };

        let session_id = self.session_id.clone();
        let session_id_for_next = session_id.clone();

        // Outer container for positioning
        div().flex().w_full().justify_end().child(
            // The bubble itself - positioned to overlap the border
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.))
                .px_2()
                .py(px(2.))
                .mr_2()
                .mt(px(-12.)) // Pull up to sit on the border
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_full()
                .shadow_xs()
                .text_xs()
                .children(vec![
                    // Previous button
                    {
                        let base = div()
                            .id("branch-prev")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded_full()
                            .cursor(if has_prev {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::default()
                            })
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/chevron_left.svg"))
                                    .text_color(if has_prev { active_color } else { muted_color })
                                    .size(px(14.)),
                            );

                        if has_prev {
                            base.hover(|s| s.bg(cx.theme().accent.opacity(0.15)))
                                .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                                    if let Some(node_id) = prev_node_id {
                                        if let Some(sender) =
                                            cx.try_global::<super::UiEventSender>()
                                        {
                                            let _ = sender.0.try_send(
                                                crate::ui::UiEvent::SwitchBranch {
                                                    session_id: session_id.clone(),
                                                    new_node_id: node_id,
                                                },
                                            );
                                        }
                                    }
                                })
                                .into_any_element()
                        } else {
                            base.into_any_element()
                        }
                    },
                    // Current position display
                    div()
                        .px_1()
                        .text_color(muted_color)
                        .child(format!("{}/{}", current, total))
                        .into_any_element(),
                    // Next button
                    {
                        let base = div()
                            .id("branch-next")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded_full()
                            .cursor(if has_next {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::default()
                            })
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/chevron_right.svg"))
                                    .text_color(if has_next { active_color } else { muted_color })
                                    .size(px(14.)),
                            );

                        if has_next {
                            base.hover(|s| s.bg(cx.theme().accent.opacity(0.25)))
                                .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                                    if let Some(node_id) = next_node_id {
                                        if let Some(sender) =
                                            cx.try_global::<super::UiEventSender>()
                                        {
                                            let _ = sender.0.try_send(
                                                crate::ui::UiEvent::SwitchBranch {
                                                    session_id: session_id_for_next.clone(),
                                                    new_node_id: node_id,
                                                },
                                            );
                                        }
                                    }
                                })
                                .into_any_element()
                        } else {
                            base.into_any_element()
                        }
                    },
                ]),
        )
    }
}
