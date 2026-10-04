//! One session row in the sidebar.
//!
//! Two lines: the title, then the project (left out inside a project
//! folder) and, when the session is not simply ready, what it is doing. Colour is reserved for rows that need the
//! user now (approval), that broke (failed), or that move (working). A ready
//! session the user has not looked at since it changed is unread: its title
//! stands out and a dot marks it. Everything else recedes.

use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::lifecycle::{SessionLifecycle, SessionStatus};
use code_assistant_core::session::pull_request::{ChecksState, PullRequestState, ReviewDecision};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::{
    Animation, AnimationExt, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement, SharedString, StatefulInteractiveElement, Styled, Transformation, Window,
    div, hsla, percentage, prelude::*, px,
};
use std::time::SystemTime;

/// Where a session's work lives, shown in the left column while the agent
/// is neither busy nor blocked: its pull request, or just its branch.
struct GitGlyph {
    icon: &'static str,
    color: Hsla,
    tooltip: String,
    /// The pull request page; clicking the glyph opens it.
    url: Option<String>,
}

/// The violet GitHub and other tools use for merged work and branches; the
/// theme has no token for it.
fn violet() -> Hsla {
    hsla(0.75, 0.55, 0.65, 1.)
}

/// What a session wants from the user, least urgent first; a collapsed
/// project folder shows the most urgent of its sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Attention {
    Unread,
    Failed,
    NeedsApproval,
}

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
    /// Whether the subtitle names the project; not inside a project folder.
    show_project: bool,
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
            show_project: true,
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

    pub fn set_show_project(&mut self, show_project: bool, cx: &mut Context<Self>) {
        if self.show_project != show_project {
            self.show_project = show_project;
            cx.notify();
        }
    }

    pub(super) fn status(&self) -> SessionStatus {
        SessionStatus::resolve(&self.activity_state, self.awaiting_permission)
    }

    /// A settled session is never unread: the user put it away.
    fn is_unread(&self) -> bool {
        !self.lifecycle.is_settled() && self.lifecycle.is_unread(self.metadata.updated_at)
    }

    pub(super) fn attention(&self) -> Option<Attention> {
        match self.status() {
            SessionStatus::NeedsApproval => Some(Attention::NeedsApproval),
            SessionStatus::Failed => Some(Attention::Failed),
            SessionStatus::Ready if self.is_unread() => Some(Attention::Unread),
            _ => None,
        }
    }

    fn git_glyph(&self, cx: &Context<Self>) -> Option<GitGlyph> {
        let branch = self.metadata.branch.as_deref()?;
        let Some(pr) = &self.lifecycle.pull_request else {
            return Some(GitGlyph {
                icon: "icons/git_branch.svg",
                color: violet().opacity(0.8),
                tooltip: branch.to_string(),
                url: None,
            });
        };
        let (icon, color, label) = match pr.state {
            PullRequestState::Open => ("icons/git_pull_request.svg", cx.theme().success, "open"),
            PullRequestState::Draft => (
                "icons/git_pull_request_draft.svg",
                cx.theme().muted_foreground,
                "draft",
            ),
            PullRequestState::Merged => ("icons/git_merge.svg", violet(), "merged"),
            PullRequestState::Closed => (
                "icons/git_pull_request_closed.svg",
                cx.theme().danger,
                "closed",
            ),
        };
        Some(GitGlyph {
            icon,
            color,
            tooltip: format!("#{} {} ({label})", pr.number, pr.title),
            url: Some(pr.url.clone()),
        })
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
        let unread = self.is_unread();
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
        let git = self.git_glyph(cx);

        // Subtitle: the project, then what outranks the rest — the status
        // while the agent is busy or blocked, otherwise the branch and what
        // its pull request waits for.
        let mut parts: Vec<String> = Vec::new();
        let mut subtitle_color = cx.theme().muted_foreground.opacity(0.7);
        // What the pull request waits for, kept visible when the rest is cut.
        let mut attention: Option<(&'static str, Hsla)> = None;
        if self.show_project && !self.metadata.initial_project.is_empty() {
            parts.push(self.metadata.initial_project.clone());
        }
        if let Some(label) = status_label {
            parts.push(label.to_string());
            subtitle_color = status_color;
        } else if let Some(branch) = &self.metadata.branch {
            match &self.lifecycle.pull_request {
                Some(pr) => parts.push(format!("#{} {branch}", pr.number)),
                None => parts.push(branch.clone()),
            }
            if let Some(pr) = &self.lifecycle.pull_request
                && pr.state != PullRequestState::Merged
                && pr.state != PullRequestState::Closed
            {
                attention = if pr.checks == Some(ChecksState::Failing) {
                    Some(("checks failing", cx.theme().danger))
                } else if pr.review == Some(ReviewDecision::ChangesRequested) {
                    Some(("changes requested", cx.theme().warning))
                } else if pr.review == Some(ReviewDecision::Approved) {
                    Some(("approved", cx.theme().success))
                } else {
                    None
                };
            }
        }
        let subtitle = parts.join(" · ");

        // Left column width: room for the status or git glyph.
        let left_col_width = px(24.);
        // Fixed-width right column so actions and the date don't shift the title.
        let date_col_width = px(56.);

        let title_color = if self.is_selected || unread {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        };
        let show_unread_dot =
            unread && matches!(status, SessionStatus::Ready | SessionStatus::Failed);

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
            // Left column: what the agent is doing while it is busy or
            // blocked; otherwise where the work lives (pull request, branch).
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
                                .size(px(13.))
                                .path("icons/shield_question.svg")
                                .text_color(status_color),
                        ),
                        SessionStatus::Failed => el.child(
                            gpui_kit::svg()
                                .size(px(13.))
                                .path("icons/circle_alert.svg")
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
                        SessionStatus::Ready => el.children(git.map(|glyph| {
                            let tooltip = glyph.tooltip.clone();
                            let url = glyph.url.clone();
                            div()
                                .id(SharedString::from(format!("git-{}", self.metadata.id)))
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(18.))
                                .rounded_sm()
                                .when(url.is_some(), |el| {
                                    el.hover(|s| s.bg(cx.theme().muted)).on_click(cx.listener(
                                        move |_, _, _, cx| {
                                            if let Some(url) = &url {
                                                cx.stop_propagation();
                                                cx.open_url(url);
                                            }
                                        },
                                    ))
                                })
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .child(
                                    gpui_kit::svg()
                                        .size(px(13.))
                                        .path(glyph.icon)
                                        .text_color(glyph.color),
                                )
                        })),
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
                    .when(!subtitle.is_empty() || attention.is_some(), |el| {
                        el.child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .text_size(px(11.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_color(subtitle_color)
                                        .child(SharedString::from(subtitle)),
                                )
                                .children(attention.map(|(text, color)| {
                                    div()
                                        .flex_none()
                                        .text_color(color)
                                        .child(SharedString::from(format!(" · {text}")))
                                })),
                        )
                    }),
            )
            // Right column: actions on hover; otherwise the unread mark and the date
            .child(
                div()
                    .flex_none()
                    .w(date_col_width)
                    .ml_2()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(4.))
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
                            el.when(show_unread_dot, |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(cx.theme().primary),
                                )
                            })
                            .child(
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
