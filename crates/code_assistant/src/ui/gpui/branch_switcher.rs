use crate::persistence::BranchInfo;
use gpui::{div, prelude::*, px, App, CursorStyle, MouseButton, Window};
use gpui_component::ActiveTheme;

/// A stateless branch navigation component that shows "◀ 2/3 ▶"
/// Displayed below user messages where branches exist.
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

        let text_color = cx.theme().muted_foreground;
        let active_color = cx.theme().foreground;
        let hover_bg = cx.theme().muted;

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

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_xs()
            .mt_1()
            .children(vec![
                // Previous button
                {
                    let base = div()
                        .id("branch-prev")
                        .px_1()
                        .py(px(2.))
                        .rounded_sm()
                        .cursor(if has_prev {
                            CursorStyle::PointingHand
                        } else {
                            CursorStyle::OperationNotAllowed
                        })
                        .text_color(if has_prev { active_color } else { text_color })
                        .child("◀");

                    if has_prev {
                        base.hover(|s| s.bg(hover_bg))
                            .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                                if let Some(node_id) = prev_node_id {
                                    // Send event through UiEventSender global
                                    if let Some(sender) = cx.try_global::<super::UiEventSender>() {
                                        let _ =
                                            sender.0.try_send(crate::ui::UiEvent::SwitchBranch {
                                                session_id: session_id.clone(),
                                                new_node_id: node_id,
                                            });
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
                    .text_color(text_color)
                    .child(format!("{}/{}", current, total))
                    .into_any_element(),
                // Next button
                {
                    let base = div()
                        .id("branch-next")
                        .px_1()
                        .py(px(2.))
                        .rounded_sm()
                        .cursor(if has_next {
                            CursorStyle::PointingHand
                        } else {
                            CursorStyle::OperationNotAllowed
                        })
                        .text_color(if has_next { active_color } else { text_color })
                        .child("▶");

                    if has_next {
                        base.hover(|s| s.bg(hover_bg))
                            .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                                if let Some(node_id) = next_node_id {
                                    // Send event through UiEventSender global
                                    if let Some(sender) = cx.try_global::<super::UiEventSender>() {
                                        let _ =
                                            sender.0.try_send(crate::ui::UiEvent::SwitchBranch {
                                                session_id: session_id_for_next.clone(),
                                                new_node_id: node_id,
                                            });
                                    }
                                }
                            })
                            .into_any_element()
                    } else {
                        base.into_any_element()
                    }
                },
            ])
    }
}
