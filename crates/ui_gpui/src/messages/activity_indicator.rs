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

/// Render the wakeups still to fire and the monitors running in the viewed
/// session, with a slow spinner: without it, an idle session that goes on
/// later would look finished once its `schedule_wakeup` / `monitor` block
/// scrolled out of view.
pub fn render_armed_wakeups(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui_kit::AnyElement {
    use crate::tool_cards::wakeup_renderer::countdown;

    let color = cx.theme().muted_foreground;
    let frame = (wall_clock_millis() / WAKEUP_SPINNER_FRAME_MS) as usize % BRAILLE_FRAMES.len();
    let line = |id: gpui_kit::ElementId, text: String| {
        div()
            .id(id)
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
            .child(div().text_size(rems(0.8125)).text_color(color).child(text))
    };

    let mut lines = Vec::new();
    let wakeups = view.armed_wakeups();
    if let Some(next) = wakeups.first() {
        let now = chrono::Local::now();
        let countdown = countdown(next.fires_at.with_timezone(&chrono::Local), now);
        let text = match wakeups.len() {
            1 => format!("Wakeup {countdown}"),
            n => format!("{n} wakeups · next {countdown}"),
        };
        lines.push(line("armed-wakeup".into(), text).into_any_element());
    }
    for monitor in view.active_monitors() {
        let monitor_id = monitor.id;
        let stop = div()
            .id(("stop-monitor", monitor_id as usize))
            .text_size(rems(0.75))
            .text_color(color)
            .cursor_pointer()
            .hover(|style| style.text_color(cx.theme().foreground))
            .child("Stop")
            .on_click(move |_event, _window, cx| {
                cx.stop_propagation();
                if let Some(gpui) = cx.try_global::<crate::Gpui>().cloned()
                    && let Some(session_id) = gpui.get_current_session_id()
                {
                    gpui.cmd_stop_monitor(session_id, monitor_id);
                }
            });
        lines.push(
            line(
                ("active-monitor", monitor_id as usize).into(),
                format!("Monitor #{monitor_id} · {}", monitor.description),
            )
            .child(stop)
            .into_any_element(),
        );
    }

    div()
        .id("armed-wakeups")
        .w_full()
        .px_3()
        .pb_1()
        .children(lines)
        .into_any_element()
}
