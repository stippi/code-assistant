use crate::persistence::{BranchInfo, NodeId};
use gpui::{
    div, prelude::*, px, App, Context, CursorStyle, EventEmitter, FocusHandle, Focusable,
    MouseButton, MouseUpEvent, Render, Window,
};
use gpui_component::ActiveTheme;

/// Events emitted by the BranchSwitcher component
#[derive(Clone, Debug)]
pub enum BranchSwitcherEvent {
    /// User clicked to switch to a different branch
    #[allow(dead_code)] // Will be used when entity-based BranchSwitcher is used
    SwitchToBranch { node_id: NodeId },
}

/// A compact branch navigation component that shows "◀ 2/3 ▶"
/// Displayed below user messages where branches exist
#[allow(dead_code)] // Entity-based component - will be used when needed
pub struct BranchSwitcher {
    branch_info: BranchInfo,
    session_id: String,
    focus_handle: FocusHandle,
}

impl BranchSwitcher {
    #[allow(dead_code)] // Will be used when entity-based BranchSwitcher is used
    pub fn new(branch_info: BranchInfo, session_id: String, cx: &mut Context<Self>) -> Self {
        Self {
            branch_info,
            session_id,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Update the branch info (e.g., after a branch switch)
    #[allow(dead_code)] // Will be used when entity-based BranchSwitcher is used
    pub fn set_branch_info(&mut self, branch_info: BranchInfo) {
        self.branch_info = branch_info;
    }

    fn on_prev_click(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev_node_id) = self.get_previous_sibling() {
            cx.emit(BranchSwitcherEvent::SwitchToBranch {
                node_id: prev_node_id,
            });
        }
    }

    fn on_next_click(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(next_node_id) = self.get_next_sibling() {
            cx.emit(BranchSwitcherEvent::SwitchToBranch {
                node_id: next_node_id,
            });
        }
    }

    fn get_previous_sibling(&self) -> Option<NodeId> {
        if self.branch_info.active_index > 0 {
            self.branch_info
                .sibling_ids
                .get(self.branch_info.active_index - 1)
                .copied()
        } else {
            None
        }
    }

    fn get_next_sibling(&self) -> Option<NodeId> {
        if self.branch_info.active_index + 1 < self.branch_info.sibling_ids.len() {
            self.branch_info
                .sibling_ids
                .get(self.branch_info.active_index + 1)
                .copied()
        } else {
            None
        }
    }

    fn has_previous(&self) -> bool {
        self.branch_info.active_index > 0
    }

    fn has_next(&self) -> bool {
        self.branch_info.active_index + 1 < self.branch_info.sibling_ids.len()
    }

    /// Get the session ID this switcher belongs to
    #[allow(dead_code)]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl EventEmitter<BranchSwitcherEvent> for BranchSwitcher {}

impl Focusable for BranchSwitcher {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BranchSwitcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_prev = self.has_previous();
        let has_next = self.has_next();
        let current = self.branch_info.active_index + 1;
        let total = self.branch_info.sibling_ids.len();

        let text_color = cx.theme().muted_foreground;
        let active_color = cx.theme().foreground;
        let hover_bg = cx.theme().muted;

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_xs()
            .children(vec![
                // Previous button
                div()
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
                    .when(has_prev, |el| {
                        el.hover(|s| s.bg(hover_bg))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_prev_click))
                    })
                    .child("◀")
                    .into_any_element(),
                // Current position display
                div()
                    .px_1()
                    .text_color(text_color)
                    .child(format!("{}/{}", current, total))
                    .into_any_element(),
                // Next button
                div()
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
                    .when(has_next, |el| {
                        el.hover(|s| s.bg(hover_bg))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_next_click))
                    })
                    .child("▶")
                    .into_any_element(),
            ])
    }
}

/// A stateless version of BranchSwitcher that can be rendered directly in elements
/// without needing an Entity. Used when rendering message lists.
#[derive(Clone, IntoElement)]
pub struct BranchSwitcherElement {
    branch_info: BranchInfo,
    session_id: String,
    #[allow(dead_code)] // Reserved for future use
    node_id: NodeId,
}

impl BranchSwitcherElement {
    pub fn new(branch_info: BranchInfo, session_id: String, node_id: NodeId) -> Self {
        Self {
            branch_info,
            session_id,
            node_id,
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
