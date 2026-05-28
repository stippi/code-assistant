//! Activity indicator rendering for the messages list.
//!
//! Shows braille spinners for loading states and pending message cards.

use super::{MessagesView, BRAILLE_FRAMES};
use crate::session::instance::SessionActivityState;
use gpui::{div, prelude::*, rems, Context};
use gpui_component::ActiveTheme;

/// Render the pending message indicator
pub fn render_pending_message(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui::AnyElement {
    let pending_message = view.current_pending_message.lock().unwrap().clone();
    let Some(pending_message) = pending_message else {
        return div().into_any_element();
    };
    if pending_message.is_empty() {
        return div().into_any_element();
    }

    let pending_card = div()
        .w_full()
        .m_3()
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
                        .font_weight(gpui::FontWeight(600.0))
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
                    gpui_component::text::TextView::markdown("pending-message", pending_message)
                        .selectable(true),
                ),
        );

    pending_card.into_any_element()
}

/// Render the inline activity indicator (braille spinner or rate-limit text).
///
/// Only shown for `WaitingForResponse` (pre-stream) and `RateLimited`.
/// During `AgentRunning` the streaming content itself signals activity.
pub fn render_activity_indicator(
    view: &MessagesView,
    cx: &mut Context<MessagesView>,
) -> gpui::AnyElement {
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

    match activity {
        SessionActivityState::RateLimited { seconds_remaining } => {
            // Orange rate-limit message with spinner
            let color = cx.theme().warning;
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
                .child(
                    div()
                        .text_size(rems(0.8125))
                        .text_color(color)
                        .child(format!(
                            "Rate limited — retrying in {}s…",
                            seconds_remaining
                        )),
                )
                .into_any_element()
        }
        SessionActivityState::WaitingForResponse => {
            // Blue braille spinner, no text
            let color = cx.theme().primary;
            div()
                .w_full()
                .px_3()
                .py_2()
                .child(
                    div()
                        .text_size(rems(0.875))
                        .text_color(color)
                        .child(braille_char.to_string()),
                )
                .into_any_element()
        }
        _ => {
            // AgentRunning / Idle — no indicator
            div().into_any_element()
        }
    }
}
