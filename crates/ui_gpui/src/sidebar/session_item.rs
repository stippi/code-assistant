//! Individual session list item component for the sidebar.

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use gpui::{
    div, percentage, prelude::*, px, Animation, AnimationExt, ClickEvent, Context, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, SharedString, StatefulInteractiveElement, Styled,
    Transformation, Window,
};
use gpui_component::{ActiveTheme, StyledExt};
use std::time::SystemTime;

/// Events emitted by individual SessionListItem components
#[derive(Clone, Debug)]
pub enum SessionListItemEvent {
    /// User clicked to select this session
    SessionClicked { session_id: String },
    /// User clicked to delete this session
    DeleteClicked { session_id: String },
}

/// Individual session list item component — simplified to title + date.
pub struct SessionListItem {
    pub(super) metadata: ChatMetadata,
    is_selected: bool,
    is_hovered: bool,
    activity_state: SessionActivityState,
    focus_handle: FocusHandle,
}

impl SessionListItem {
    pub fn new(metadata: ChatMetadata, is_selected: bool, cx: &mut Context<Self>) -> Self {
        Self {
            metadata,
            is_selected,
            is_hovered: false,
            activity_state: SessionActivityState::Idle,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn update_selection(&mut self, is_selected: bool, cx: &mut Context<Self>) {
        if self.is_selected != is_selected {
            self.is_selected = is_selected;
            cx.notify();
        }
    }

    pub fn update_metadata(&mut self, metadata: ChatMetadata, cx: &mut Context<Self>) {
        if self.metadata != metadata {
            self.metadata = metadata;
            cx.notify();
        }
    }

    pub fn update_activity_state(
        &mut self,
        activity_state: SessionActivityState,
        cx: &mut Context<Self>,
    ) {
        if self.activity_state != activity_state {
            self.activity_state = activity_state;
            cx.notify();
        }
    }

    pub(super) fn format_relative_date(timestamp: SystemTime) -> String {
        match timestamp.elapsed() {
            Ok(duration) => {
                let secs = duration.as_secs();
                if secs < 60 {
                    "Just now".to_string()
                } else if secs < 3600 {
                    format!("{}m", secs / 60)
                } else if secs < 86400 {
                    format!("{}h", secs / 3600)
                } else if secs < 86400 * 30 {
                    format!("{}d", secs / 86400)
                } else if secs < 86400 * 365 {
                    format!("{}mo", secs / (86400 * 30))
                } else {
                    format!("{}y", secs / (86400 * 365))
                }
            }
            Err(_) => "?".to_string(),
        }
    }

    fn on_hover(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if *hovered != self.is_hovered {
            self.is_hovered = *hovered;
            cx.notify();
        }
    }

    fn on_session_click(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(SessionListItemEvent::SessionClicked {
            session_id: self.metadata.id.clone(),
        });
    }

    fn on_session_delete(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        cx.emit(SessionListItemEvent::DeleteClicked {
            session_id: self.metadata.id.clone(),
        });
    }
}

impl EventEmitter<SessionListItemEvent> for SessionListItem {}

impl Focusable for SessionListItem {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionListItem {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = if self.metadata.name.is_empty() {
            "Unnamed chat".to_string()
        } else {
            self.metadata.name.clone()
        };
        let date = Self::format_relative_date(self.metadata.updated_at);

        let is_active = !matches!(self.activity_state, SessionActivityState::Idle);
        let is_errored = matches!(self.activity_state, SessionActivityState::Errored { .. });
        let is_externally_locked =
            matches!(self.activity_state, SessionActivityState::RunningExternally);
        let activity_color = match &self.activity_state {
            SessionActivityState::AgentRunning => cx.theme().info,
            SessionActivityState::RunningExternally => cx.theme().warning,
            SessionActivityState::WaitingForResponse => cx.theme().primary,
            SessionActivityState::RateLimited { .. } => cx.theme().warning,
            SessionActivityState::Errored { .. } => cx.theme().danger,
            SessionActivityState::Idle => cx.theme().muted,
        };

        // Left column width: folder icon area (aligned with project headers)
        let left_col_width = px(24.);

        // Fixed-width right column so trash + date don't shift around
        let date_col_width = px(50.);

        div()
            .id(SharedString::from(format!(
                "chat-item-{}",
                self.metadata.id
            )))
            .mx(px(2.))
            .pr_1()
            .py(px(5.))
            .flex()
            .items_center()
            .cursor_pointer()
            .rounded_md()
            .border_1()
            .border_color(if self.is_selected {
                cx.theme().primary.opacity(0.3)
            } else {
                cx.theme().transparent
            })
            .bg(if self.is_selected {
                cx.theme().primary.opacity(0.1)
            } else {
                cx.theme().transparent
            })
            .on_hover(cx.listener(Self::on_hover))
            .when(!self.is_selected, |el| {
                el.hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
            })
            .on_click(cx.listener(Self::on_session_click))
            // Left column: fixed width, shows spinning icon when active or error icon when errored
            .child(
                div()
                    .flex_none()
                    .w(left_col_width)
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(is_errored, |el| {
                        el.child(
                            gpui::svg()
                                .size(px(12.))
                                .path("icons/circle_stop.svg")
                                .text_color(activity_color),
                        )
                    })
                    .when(is_externally_locked, |el| {
                        el.child(
                            gpui::svg()
                                .size(px(12.))
                                .path("icons/lock.svg")
                                .text_color(activity_color),
                        )
                    })
                    .when(is_active && !is_errored && !is_externally_locked, |el| {
                        el.child(
                            gpui::svg()
                                .size(px(12.))
                                .path("icons/arrow_circle.svg")
                                .text_color(activity_color)
                                .with_animation(
                                    SharedString::from(format!(
                                        "activity-spin-{}",
                                        self.metadata.id
                                    )),
                                    Animation::new(std::time::Duration::from_secs(2)).repeat(),
                                    |svg, delta| {
                                        svg.with_transformation(Transformation::rotate(percentage(
                                            delta,
                                        )))
                                    },
                                ),
                        )
                    }),
            )
            // Session name (truncated — shrinks when trash icon appears)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(if self.is_selected {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .font_medium()
                    .child(SharedString::from(name)),
            )
            // Right column: fixed width, shows delete button on hover, date otherwise
            .child(
                div()
                    .flex_none()
                    .w(date_col_width)
                    .ml_2()
                    .flex()
                    .items_center()
                    .justify_end()
                    .map(|el| {
                        if self.is_hovered {
                            // Trash icon replaces date on hover
                            el.child(
                                div()
                                    .id(SharedString::from(format!("delete-{}", self.metadata.id)))
                                    .flex_none()
                                    .size(px(18.))
                                    .rounded_sm()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().danger.opacity(0.15)))
                                    .child(
                                        gpui::svg()
                                            .size(px(12.))
                                            .path("icons/trash.svg")
                                            .text_color(cx.theme().danger),
                                    )
                                    .on_click(cx.listener(Self::on_session_delete)),
                            )
                        } else {
                            // Date shown when not hovered
                            el.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                                    .child(SharedString::from(date)),
                            )
                        }
                    }),
            )
    }
}
