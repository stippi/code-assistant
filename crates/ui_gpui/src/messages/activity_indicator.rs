//! Activity indicator rendering for the messages list.
//!
//! Shows braille spinners for loading states and pending message cards.

use super::{BRAILLE_FRAMES, MessagesView, WAKEUP_SPINNER_FRAME_MS, wall_clock_millis};
use code_assistant_core::session::instance::SessionActivityState;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{Context, div, prelude::*, rems};

/// Render the pending message indicator
pub fn render_pending_message(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui_kit::AnyElement {
    let pending_message = view.current_pending_message.lock().unwrap().clone();
    let Some(pending_message) = pending_message else {
        return div().into_any_element();
    };
    if pending_message.is_empty() {
        return div().into_any_element();
    }

    let pending_card = div()
        .w_full()
        .bg(cx.theme().muted)
        .border_1()
        .border_color(cx.theme().warning)
        .rounded_md()
        .shadow_xs()
        .p_3()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .children(vec![
                    super::super::shared::file_icons::render_icon_container(
                        &super::super::shared::file_icons::get()
                            .get_type_icon(super::super::shared::file_icons::TOOL_USER_INPUT),
                        16.0,
                        cx.theme().warning,
                        "👤",
                    )
                    .into_any_element(),
                    div()
                        .font_weight(gpui_kit::FontWeight(600.0))
                        .text_color(cx.theme().warning)
                        .child("Pending")
                        .into_any_element(),
                ]),
        )
        .child(
            div()
                .mt_2()
                .text_color(cx.theme().foreground.opacity(0.8))
                .child(
                    gpui_kit::component::text::TextView::markdown(
                        "pending-message",
                        pending_message,
                    )
                    .selectable(true),
                ),
        );

    // Wrap the card in a padding container so its border + shadow sit inset
    // from the max-width boundary without overflowing via margin.
    div().w_full().p_3().child(pending_card).into_any_element()
}

/// Render the inline activity indicator: a braille spinner, labelled while
/// the model thinks, the session is rate limited or the request is retried.
///
/// Only shown for `WaitingForResponse` (pre-stream), `Thinking`,
/// `RateLimited` and `Retrying`.
/// During `AgentRunning` the streaming content itself signals activity.
pub fn render_activity_indicator(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui_kit::AnyElement {
    let activity = view.activity_state.lock().ok().and_then(|g| g.clone());

    let Some(activity) = activity else {
        return div().into_any_element();
    };

    // Pick the current braille frame based on wall-clock time (~80ms per frame)
    let frame_index = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 80) as usize
        % BRAILLE_FRAMES.len();
    let braille_char = BRAILLE_FRAMES[frame_index];

    let (color, label) = match activity {
        SessionActivityState::RateLimited { seconds_remaining } => (
            cx.theme().warning,
            Some(format!("Rate limited — retrying in {seconds_remaining}s…")),
        ),
        SessionActivityState::Retrying {
            reason,
            attempt,
            max_attempts,
        } => (
            cx.theme().warning,
            Some(format!("{reason} — retrying ({attempt}/{max_attempts})…")),
        ),
        SessionActivityState::Thinking => (cx.theme().primary, Some("Thinking…".to_string())),
        SessionActivityState::WaitingForResponse => (cx.theme().primary, None),
        // AgentRunning / Idle — no indicator
        _ => return div().into_any_element(),
    };

    div()
        .w_full()
        .px_3()
        .py_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_size(rems(0.875))
                .text_color(color)
                .child(braille_char.to_string()),
        )
        .when_some(label, |d, label| {
            d.child(div().text_size(rems(0.8125)).text_color(color).child(label))
        })
        .into_any_element()
}

/// Render the wakeups still to fire in the viewed session, with a slow
/// spinner: without it, an idle session that goes on later would look
/// finished once its `schedule_wakeup` block scrolled out of view.
pub fn render_armed_wakeups(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui_kit::AnyElement {
    use crate::tool_cards::wakeup_renderer::countdown;

    let wakeups = view.armed_wakeups();
    let Some(next) = wakeups.first() else {
        return div().into_any_element();
    };
    let now = chrono::Local::now();
    let countdown = countdown(next.fires_at.with_timezone(&chrono::Local), now);
    let text = match wakeups.len() {
        1 => format!("Wakeup {countdown}"),
        n => format!("{n} wakeups · next {countdown}"),
    };
    let color = cx.theme().muted_foreground;
    let frame = (wall_clock_millis() / WAKEUP_SPINNER_FRAME_MS) as usize % BRAILLE_FRAMES.len();
    div()
        .id("armed-wakeups")
        .w_full()
        .px_3()
        .pb_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .py_1p5()
                .px_3()
                .child(
                    div()
                        .w(gpui_kit::px(14.))
                        .flex()
                        .justify_center()
                        .text_size(rems(0.875))
                        .text_color(color)
                        .child(BRAILLE_FRAMES[frame]),
                )
                .child(div().text_size(rems(0.8125)).text_color(color).child(text)),
        )
        .into_any_element()
}

/// What a running sub-agent is doing, in a few words.
pub(crate) fn sub_agent_activity_text(
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
pub(crate) fn elapsed_text(started_at: chrono::DateTime<chrono::Utc>) -> String {
    let seconds = (chrono::Utc::now() - started_at).num_seconds().max(0);
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Render the sub-agents running in the viewed session, one row each with
/// what it is doing and a Cancel button. Background agents keep running
/// after the turn that started them, so without this list an idle session
/// would look finished.
pub fn render_running_sub_agents(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui_kit::AnyElement {
    use gpui_kit::{ClickEvent, SharedString, px};

    let agents = view.running_sub_agents().to_vec();
    if agents.is_empty() {
        return div().into_any_element();
    }
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let foreground = theme.foreground;
    let danger = theme.danger;
    let frame = (wall_clock_millis() / WAKEUP_SPINNER_FRAME_MS) as usize % BRAILLE_FRAMES.len();
    let header = match agents.len() {
        1 => "1 sub-agent running".to_string(),
        n => format!("{n} sub-agents running"),
    };

    let rows = agents.into_iter().map(|agent| {
        let tool_id = agent.tool_id.clone();
        div()
            .id(SharedString::from(format!(
                "running-sub-agent-{}",
                agent.tool_id
            )))
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
    });

    div()
        .id("running-sub-agents")
        .w_full()
        .px_3()
        .pb_1()
        .child(
            div()
                .flex()
                .flex_col()
                .py_1()
                .child(
                    div()
                        .px_3()
                        .py_0p5()
                        .text_size(rems(0.75))
                        .text_color(muted)
                        .child(header),
                )
                .children(rows),
        )
        .into_any_element()
}
