//! One session row in the sidebar.
//!
//! Two lines: the title, then the project and, when the session is not
//! simply ready, what it is doing. Colour is reserved for rows that need the
//! user now (approval), that broke (failed), or that move (working). A ready
//! session the user has not looked at since it changed is unread: its title
//! stands out and a dot marks it. Everything else recedes.

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::lifecycle::{SessionLifecycle, SessionStatus};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::{
    Animation, AnimationExt, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement, SharedString, StatefulInteractiveElement, Styled, Transformation, Window,
    div, percentage, prelude::*, px,
};
use std::time::SystemTime;

/// Events emitted by individual SessionListItem components
#[derive(Clone, Debug)]
pub enum SessionListItemEvent {
    /// User clicked to select this session
    SessionClicked { session_id: String },
    /// User clicked to delete this session
    DeleteClicked { session_id: String },
    /// User moved the session into the settled shelf
    SettleClicked { session_id: String },
    /// User pulled the session back into the inbox
    UnsettleClicked { session_id: String },
}

pub struct SessionListItem {
    pub(super) metadata: ChatMetadata,
    pub(super) lifecycle: SessionLifecycle,
    is_selected: bool,
    is_hovered: bool,
    activity_state: SessionActivityState,
    awaiting_permission: bool,
    focus_handle: FocusHandle,
}

impl SessionListItem {
    pub fn new(
        metadata: ChatMetadata,
        lifecycle: SessionLifecycle,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            metadata,
            lifecycle,
            is_selected,
            is_hovered: false,
            activity_state: SessionActivityState::Idle,
            awaiting_permission: false,
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

    pub fn update_lifecycle(&mut self, lifecycle: SessionLifecycle, cx: &mut Context<Self>) {
        if self.lifecycle != lifecycle {
            self.lifecycle = lifecycle;
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

    pub fn set_awaiting_permission(&mut self, awaiting: bool, cx: &mut Context<Self>) {
        if self.awaiting_permission != awaiting {
            self.awaiting_permission = awaiting;
            cx.notify();
        }
    }

    pub(super) fn status(&self) -> SessionStatus {
        SessionStatus::resolve(&self.activity_state, self.awaiting_permission)
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

    fn on_toggle_settled(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let session_id = self.metadata.id.clone();
        if self.lifecycle.is_settled() {
            cx.emit(SessionListItemEvent::UnsettleClicked { session_id });
        } else {
            cx.emit(SessionListItemEvent::SettleClicked { session_id });
        }
    }

    /// A small icon button shown in the row's right column on hover.
    fn action_button(
        &self,
        id: String,
        icon: &'static str,
        tooltip: &'static str,
        color: Hsla,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(id))
            .flex_none()
            .size(px(18.))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |s| s.bg(color.opacity(0.15)))
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .child(gpui_kit::svg().size(px(12.)).path(icon).text_color(color))
            .on_click(cx.listener(on_click))
    }
}

impl EventEmitter<SessionListItemEvent> for SessionListItem {}

impl Focusable for SessionListItem {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionListItem {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = if self.metadata.name.is_empty() {
            "Unnamed chat".to_string()
        } else {
            self.metadata.name.clone()
        };
        let status = self.status();
        let settled = self.lifecycle.is_settled();
        let unread = !settled && self.lifecycle.is_unread(self.metadata.updated_at);
        let recede = settled || status.should_recede(unread, self.is_selected);
        let date = Self::format_relative_date(match self.lifecycle.settled {
            Some(settlement) => settlement.at,
            None => self.metadata.updated_at,
        });

        let (status_label, status_color): (Option<&'static str>, Hsla) = match status {
            SessionStatus::NeedsApproval => (Some("Needs approval"), cx.theme().warning),
            SessionStatus::Working => (Some("Working"), cx.theme().muted_foreground),
            SessionStatus::RateLimited => (Some("Rate limited"), cx.theme().warning),
            SessionStatus::Failed => (Some("Failed"), cx.theme().danger),
            SessionStatus::RunningElsewhere => {
                (Some("Running elsewhere"), cx.theme().muted_foreground)
            }
            SessionStatus::Ready => (None, cx.theme().muted_foreground),
        };
        let project = self.metadata.initial_project.clone();
        let subtitle = match (project.is_empty(), status_label) {
            (false, Some(label)) => format!("{project} · {label}"),
            (false, None) => project,
            (true, Some(label)) => label.to_string(),
            (true, None) => String::new(),
        };

        // Left column width: aligned with the section headers' icons.
        let left_col_width = px(24.);
        // Fixed-width right column so actions and the date don't shift the title.
        let date_col_width = px(50.);

        let title_color = if self.is_selected || unread {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        };

        div()
            .id(SharedString::from(format!(
                "chat-item-{}",
                self.metadata.id
            )))
            .mx(px(2.))
            .pr_1()
            .py(px(4.))
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
            .when(recede && !self.is_hovered, |el| el.opacity(0.55))
            .on_hover(cx.listener(Self::on_hover))
            .when(!self.is_selected, |el| {
                el.hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
            })
            .on_click(cx.listener(Self::on_session_click))
            // Left column: what the session is doing, or that it is unread.
            .child(
                div()
                    .flex_none()
                    .w(left_col_width)
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|el| match status {
                        SessionStatus::NeedsApproval => el.child(
                            gpui_kit::svg()
                                .size(px(12.))
                                .path("icons/info.svg")
                                .text_color(status_color),
                        ),
                        SessionStatus::Failed => el.child(
                            gpui_kit::svg()
                                .size(px(12.))
                                .path("icons/circle_stop.svg")
                                .text_color(status_color),
                        ),
                        SessionStatus::RunningElsewhere => el.child(
                            gpui_kit::svg()
                                .size(px(12.))
                                .path("icons/lock.svg")
                                .text_color(status_color),
                        ),
                        SessionStatus::Working | SessionStatus::RateLimited => el.child(
                            gpui_kit::svg()
                                .size(px(12.))
                                .path("icons/arrow_circle.svg")
                                .text_color(if status == SessionStatus::Working {
                                    cx.theme().info
                                } else {
                                    status_color
                                })
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
                        ),
                        SessionStatus::Ready => el.when(unread, |el| {
                            el.child(div().size(px(7.)).rounded_full().bg(cx.theme().primary))
                        }),
                    }),
            )
            // Title and subtitle
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(title_color)
                            .when(unread, |el| el.font_semibold())
                            .when(!unread, |el| el.font_medium())
                            .child(SharedString::from(name)),
                    )
                    .when(!subtitle.is_empty(), |el| {
                        el.child(
                            div()
                                .w_full()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(11.))
                                .text_color(if status_label.is_some() {
                                    status_color
                                } else {
                                    cx.theme().muted_foreground.opacity(0.7)
                                })
                                .child(SharedString::from(subtitle)),
                        )
                    }),
            )
            // Right column: actions on hover, date otherwise
            .child(
                div()
                    .flex_none()
                    .w(date_col_width)
                    .ml_2()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(2.))
                    .map(|el| {
                        if self.is_hovered {
                            let (icon, tooltip) = if settled {
                                ("icons/rotate_ccw.svg", "Un-settle")
                            } else {
                                ("icons/check.svg", "Settle")
                            };
                            el.child(self.action_button(
                                format!("settle-{}", self.metadata.id),
                                icon,
                                tooltip,
                                cx.theme().muted_foreground,
                                Self::on_toggle_settled,
                                cx,
                            ))
                            .child(self.action_button(
                                format!("delete-{}", self.metadata.id),
                                "icons/trash.svg",
                                "Delete",
                                cx.theme().danger,
                                Self::on_session_delete,
                                cx,
                            ))
                        } else {
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
