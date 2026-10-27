//! The running agents above the input: the sub-agents of the viewed
//! session, foreground and background, each with what it does, how long it
//! runs and buttons to open its transcript or cancel it. Background agents
//! go on after the turn that started them, so without this panel an idle
//! session would look finished. It lives in the footer, outside the
//! scrolling conversation.

use crate::messages::{BRAILLE_FRAMES, MessagesView, WAKEUP_SPINNER_FRAME_MS, wall_clock_millis};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    Context, Entity, IntoElement, Render, Subscription, Task, Window, div, prelude::*, px, rems,
};
use std::time::Duration;

/// Reads what it shows from the [`MessagesView`], which the event loop
/// keeps up to date for the viewed session.
pub struct AgentsPanel {
    messages_view: Entity<MessagesView>,
    /// Renders the next spinner frame.
    refresh: Option<Task<()>>,
    _messages: Subscription,
}

impl AgentsPanel {
    pub fn new(messages_view: Entity<MessagesView>, cx: &mut Context<Self>) -> Self {
        let _messages = cx.observe(&messages_view, |_, _, cx| cx.notify());
        Self {
            messages_view,
            refresh: None,
            _messages,
        }
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let agents = self.messages_view.read(cx).running_sub_agents().to_vec();
        if agents.is_empty() {
            self.refresh = None;
            return div().into_any_element();
        }
        if self.refresh.is_none() {
            let next_frame = Duration::from_millis(
                (WAKEUP_SPINNER_FRAME_MS - wall_clock_millis() % WAKEUP_SPINNER_FRAME_MS) as u64,
            );
            self.refresh = Some(cx.spawn(async move |panel, cx| {
                cx.background_executor().timer(next_frame).await;
                let _ = panel.update(cx, |panel, cx| {
                    panel.refresh = None;
                    cx.notify();
                });
            }));
        }
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let frame = (wall_clock_millis() / WAKEUP_SPINNER_FRAME_MS) as usize % BRAILLE_FRAMES.len();
        let header = match agents.len() {
            1 => "1 sub-agent running".to_string(),
            n => format!("{n} sub-agents running"),
        };

        let mut list = div().flex().flex_col().py_1().child(
            div()
                .px_3()
                .py_0p5()
                .text_size(rems(0.75))
                .text_color(muted)
                .child(header),
        );
        for agent in agents {
            list = list.child(sub_agent_row(agent, frame, cx));
        }

        div()
            .id("running-sub-agents")
            .w_full()
            .max_h(px(180.))
            .overflow_y_scroll()
            .px_2()
            .border_b_1()
            .border_color(border)
            .child(list)
            .into_any_element()
    }
}

/// What a running sub-agent is doing, in a few words.
fn sub_agent_activity_text(
    agent: &code_assistant_core::agent::sub_agent::RunningSubAgentInfo,
) -> String {
    use code_assistant_core::agent::sub_agent::SubAgentActivity;
    if let Some(tool) = &agent.current_tool {
        return tool.clone();
    }
    match agent.activity {
        Some(SubAgentActivity::Streaming) => "Responding…".into(),
        Some(SubAgentActivity::ExecutingTools) => "Running tools…".into(),
        _ => "Thinking…".into(),
    }
}

/// `42s`, `3m 05s`, `1h 02m`.
fn elapsed_text(started_at: chrono::DateTime<chrono::Utc>) -> String {
    let seconds = (chrono::Utc::now() - started_at).num_seconds().max(0);
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// One running sub-agent: what it is doing, for how long, and buttons to
/// open its transcript or cancel it.
fn sub_agent_row(
    agent: code_assistant_core::agent::sub_agent::RunningSubAgentInfo,
    frame: usize,
    cx: &mut Context<AgentsPanel>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    use gpui_kit::{ClickEvent, SharedString, px};

    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let foreground = theme.foreground;
    let danger = theme.danger;
    let tool_id = agent.tool_id.clone();
    div()
        .id(SharedString::from(format!(
            "running-sub-agent-{}",
            agent.tool_id
        )))
        .debug_selector(|| "running-sub-agent".into())
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .py_0p5()
        .px_3()
        .child(
            div()
                .w(px(14.))
                .flex()
                .justify_center()
                .text_size(rems(0.875))
                .text_color(muted)
                .child(BRAILLE_FRAMES[frame]),
        )
        .child(
            div()
                .flex_none()
                .text_size(rems(0.8125))
                .text_color(muted)
                .child(format!("#{}", agent.id)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(rems(0.8125))
                .text_color(foreground)
                .child(agent.title.clone()),
        )
        .when(agent.background, |row| {
            row.child(
                div()
                    .flex_none()
                    .px_1p5()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(muted.opacity(0.4))
                    .text_size(rems(0.6875))
                    .text_color(muted)
                    .child("background"),
            )
        })
        .child(
            div()
                .flex_none()
                .max_w(px(220.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(rems(0.75))
                .text_color(muted)
                .child(sub_agent_activity_text(&agent)),
        )
        .child(
            div()
                .flex_none()
                .text_size(rems(0.75))
                .text_color(muted)
                .child(elapsed_text(agent.started_at)),
        )
        .when_some(agent.session_id.clone(), |row, session_id| {
            row.child(
                div()
                    .id(SharedString::from(format!(
                        "open-running-{}",
                        agent.tool_id
                    )))
                    .debug_selector(|| "open-running-sub-agent".into())
                    .flex_none()
                    .px_2()
                    .py(px(1.))
                    .rounded(px(4.))
                    .text_size(rems(0.6875))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(|s| s.bg(foreground.opacity(0.1)).text_color(foreground))
                    .on_click(cx.listener(move |_view, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        if let Some(gpui) = cx.try_global::<crate::Gpui>() {
                            gpui.cmd_load_session(session_id.clone(), None);
                        }
                    }))
                    .child("Open"),
            )
        })
        .child(
            div()
                .id(SharedString::from(format!(
                    "cancel-running-{}",
                    agent.tool_id
                )))
                .flex_none()
                .px_2()
                .py(px(1.))
                .rounded(px(4.))
                .text_size(rems(0.6875))
                .text_color(muted)
                .cursor_pointer()
                .hover(|s| s.bg(danger.opacity(0.15)).text_color(danger))
                .on_click(cx.listener(move |_view, _event: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    if let Some(gpui) = cx.try_global::<crate::Gpui>() {
                        gpui.cmd_cancel_sub_agent(tool_id.clone());
                    }
                }))
                .child("Cancel"),
        )
}
