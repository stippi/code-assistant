//! Inline renderer for `schedule_wakeup` / `cancel_wakeup`.
//!
//! A scheduled wakeup counts down to when it fires. From the call on, the
//! line starts the same ("Wakeup in 20 min"); the fire time is appended once
//! known. Once it fired — the stored result says so, or for results recorded
//! before that, the wakeup message arrived in the same turn
//! (`ToolUseBlock::wakeup_fired`) — the line says when. A wakeup armed by an
//! earlier process that never fired was lost with it. Expanding the line
//! shows the prompt.

use super::inline_renderer::{render_inline_output, render_inline_output_text};
use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use crate::shared::file_icons;
use chrono::{DateTime, Local};
use code_assistant_core::session::wakeup::process_instance;
use code_assistant_core::tools::impls::wakeup::ScheduleWakeupOutput;
use gpui_kit::{AnyElement, Context, SharedString, Window};
use std::time::Duration;

pub struct WakeupToolRenderer;

impl ToolBlockRenderer for WakeupToolRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["schedule_wakeup".to_string(), "cancel_wakeup".to_string()]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        if tool.name == "cancel_wakeup" {
            return match param(tool, "wakeup_id") {
                Some(id) => format!("Cancel wakeup #{id}"),
                None => "Cancel wakeup".to_string(),
            };
        }
        describe_schedule(tool, Local::now(), process_instance())
    }

    fn icon(&self, tool: &ToolUseBlock) -> Option<SharedString> {
        let icon = match state(tool, process_instance()) {
            WakeupState::Fired { .. } => file_icons::TOOL_WAKEUP_FIRED,
            WakeupState::Lost { .. } => file_icons::TOOL_CANCEL_WAKEUP,
            WakeupState::Requested | WakeupState::Armed { .. } => return None,
        };
        file_icons::get().get_type_icon(icon)
    }

    fn refresh_after(&self, tool: &ToolUseBlock) -> Option<Duration> {
        let WakeupState::Armed {
            fires_at: Some(fires_at),
            ..
        } = state(tool, process_instance())
        else {
            return None;
        };
        countdown_changes_after(fires_at)
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_kit::component::theme::Theme,
        _card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        match param(tool, "prompt") {
            Some(prompt) if tool.name == "schedule_wakeup" => {
                render_inline_output_text(prompt.trim(), false, tool, theme)
            }
            _ => render_inline_output(tool, theme),
        }
    }
}

fn param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

/// Where a scheduled wakeup stands. Fire times are absent in results
/// recorded before they carried them.
enum WakeupState {
    /// Called, not executed yet (or failed).
    Requested,
    Armed {
        fires_at: Option<DateTime<Local>>,
        delay_seconds: u64,
    },
    Fired {
        at: Option<DateTime<Local>>,
    },
    /// Armed by an earlier process, which exited before it fired.
    Lost {
        fires_at: Option<DateTime<Local>>,
    },
}

fn state(tool: &ToolUseBlock, this_process: &str) -> WakeupState {
    let Some(output) = tool
        .output
        .as_deref()
        .and_then(ScheduleWakeupOutput::from_ui_output)
    else {
        return WakeupState::Requested;
    };
    let local = |time: chrono::DateTime<chrono::Utc>| time.with_timezone(&Local);
    let fires_at = output.fires_at.map(local);
    if output.fired_at.is_some() || tool.wakeup_fired {
        WakeupState::Fired {
            at: output.fired_at.map(local).or(fires_at),
        }
    } else if output
        .armed_by
        .is_some_and(|armed_by| armed_by != this_process)
    {
        WakeupState::Lost { fires_at }
    } else {
        WakeupState::Armed {
            fires_at,
            delay_seconds: output.delay_seconds,
        }
    }
}

fn describe_schedule(tool: &ToolUseBlock, now: DateTime<Local>, this_process: &str) -> String {
    match state(tool, this_process) {
        WakeupState::Requested => match param(tool, "delay_seconds").and_then(|d| d.parse().ok()) {
            Some(delay) => {
                format!("Wakeup in {}", format_remaining(Duration::from_secs(delay)))
            }
            None => "Wakeup".to_string(),
        },
        WakeupState::Armed {
            fires_at: None,
            delay_seconds,
        } => format!(
            "Wakeup in {}",
            format_remaining(Duration::from_secs(delay_seconds))
        ),
        WakeupState::Armed {
            fires_at: Some(fires_at),
            ..
        } => format!("Wakeup {}", countdown(fires_at, now)),
        WakeupState::Fired { at: Some(at) } => format!("Woke up {}", format_when(at, now)),
        WakeupState::Fired { at: None } => "Woke up".to_string(),
        WakeupState::Lost {
            fires_at: Some(fires_at),
        } => format!(
            "Wakeup lost with a restart · was due {}",
            format_when(fires_at, now)
        ),
        WakeupState::Lost { fires_at: None } => "Wakeup lost with a restart".to_string(),
    }
}

/// "in 20 min · at 14:31" until `fires_at`, "at 14:31" once it is due (but
/// not fired yet: it is on its way, or was cancelled).
pub(crate) fn countdown(fires_at: DateTime<Local>, now: DateTime<Local>) -> String {
    let when = format_when(fires_at, now);
    match (fires_at - now).to_std() {
        Ok(remaining) if !remaining.is_zero() => {
            format!("in {} · {when}", format_remaining(remaining))
        }
        _ => when,
    }
}

/// How long until [`countdown`] for `fires_at` shows a different text;
/// `None` once it no longer changes.
pub(crate) fn countdown_changes_after(fires_at: DateTime<Local>) -> Option<Duration> {
    let remaining = (fires_at - Local::now()).to_std().ok()?;
    Some(until_display_changes(remaining))
}

/// "at 14:32" today, "on Oct 13 at 09:00" on any other day.
fn format_when(time: DateTime<Local>, now: DateTime<Local>) -> String {
    if time.date_naive() == now.date_naive() {
        format!("at {}", time.format("%H:%M"))
    } else {
        format!("on {}", time.format("%b %-d at %H:%M"))
    }
}

/// A countdown: seconds in the last minute, whole minutes rounded up before
/// that — "2 min" until one minute is left, then "60 s".
fn format_remaining(remaining: Duration) -> String {
    let seconds = remaining.as_secs_f64().ceil() as u64;
    if seconds <= 60 {
        format!("{seconds} s")
    } else {
        format_minutes(seconds.div_ceil(60))
    }
}

fn format_minutes(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// How long until [`format_remaining`] shows a different value.
fn until_display_changes(remaining: Duration) -> Duration {
    // A little past the boundary, so the next render is on its far side.
    const MARGIN: Duration = Duration::from_millis(20);
    let millis = remaining.as_millis() as u64;
    if millis <= 60_000 {
        return Duration::from_millis(millis % 1000) + MARGIN;
    }
    // Minutes round up: the value drops when crossing a full minute.
    let into_minute = millis % 60_000;
    let next = if into_minute == 0 {
        60_000
    } else {
        into_minute
    };
    Duration::from_millis(next) + MARGIN
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_cards::tests::make_tool;
    use chrono::TimeZone;

    const HERE: &str = "this-process";

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 12, 14, 12, 0).unwrap()
    }

    fn scheduled(delay_seconds: u64, fires_at: DateTime<Local>) -> ToolUseBlock {
        with_output(delay_seconds, fires_at, |_| {})
    }

    /// A `schedule_wakeup` block armed by this process, its result adjusted
    /// by `adjust`.
    fn with_output(
        delay_seconds: u64,
        fires_at: DateTime<Local>,
        adjust: impl FnOnce(&mut ScheduleWakeupOutput),
    ) -> ToolUseBlock {
        let mut output = ScheduleWakeupOutput {
            wakeup_id: 1,
            delay_seconds,
            fires_at: Some(fires_at.to_utc()),
            armed_by: Some(HERE.to_string()),
            fired_at: None,
        };
        adjust(&mut output);
        let mut tool = make_tool(
            "schedule_wakeup",
            &[
                ("delay_seconds", &delay_seconds.to_string()),
                ("prompt", "check the build"),
            ],
        );
        tool.output = Some(serde_json::to_string(&output).unwrap());
        tool
    }

    #[test]
    fn before_execution_shows_the_requested_delay() {
        let tool = make_tool("schedule_wakeup", &[("delay_seconds", "1200")]);
        assert_eq!(describe_schedule(&tool, now(), HERE), "Wakeup in 20 min");
        let bare = make_tool("schedule_wakeup", &[]);
        assert_eq!(describe_schedule(&bare, now(), HERE), "Wakeup");
    }

    #[test]
    fn armed_wakeup_counts_down() {
        let tool = scheduled(1200, now() + chrono::Duration::seconds(1190));
        assert_eq!(
            describe_schedule(&tool, now(), HERE),
            "Wakeup in 20 min · at 14:31"
        );
        let tool = scheduled(30, now() + chrono::Duration::seconds(30));
        assert_eq!(
            describe_schedule(&tool, now(), HERE),
            "Wakeup in 30 s · at 14:12"
        );
    }

    #[test]
    fn other_days_show_the_date() {
        let tool = scheduled(86_400, now() + chrono::Duration::days(1));
        assert_eq!(
            describe_schedule(&tool, now(), HERE),
            "Wakeup in 24 h · on Oct 13 at 14:12"
        );
    }

    #[test]
    fn executing_keeps_the_start_of_the_line() {
        for delay in [30, 90, 1200, 5400] {
            let requested = make_tool("schedule_wakeup", &[("delay_seconds", &delay.to_string())]);
            // Executed a moment after the call.
            let armed = scheduled(delay, now() + chrono::Duration::seconds(delay as i64));
            let before = describe_schedule(&requested, now(), HERE);
            let after =
                describe_schedule(&armed, now() + chrono::Duration::milliseconds(200), HERE);
            assert!(after.starts_with(&before), "{before:?} → {after:?}");
        }
    }

    #[test]
    fn due_wakeup_without_message_claims_nothing() {
        let tool = scheduled(60, now() - chrono::Duration::seconds(5));
        assert_eq!(describe_schedule(&tool, now(), HERE), "Wakeup at 14:11");
    }

    #[test]
    fn fired_wakeup_says_when() {
        let fired_at = now() - chrono::Duration::seconds(3);
        let tool = with_output(60, now() - chrono::Duration::seconds(5), |output| {
            output.fired_at = Some(fired_at.to_utc());
        });
        assert_eq!(describe_schedule(&tool, now(), HERE), "Woke up at 14:11");
        // Also when it was armed by an earlier process.
        assert_eq!(describe_schedule(&tool, now(), "later"), "Woke up at 14:11");
    }

    #[test]
    fn wakeup_message_of_the_turn_marks_older_results_fired() {
        let mut tool = with_output(60, now() - chrono::Duration::seconds(5), |output| {
            output.armed_by = None;
        });
        tool.wakeup_fired = true;
        assert_eq!(describe_schedule(&tool, now(), HERE), "Woke up at 14:11");
    }

    #[test]
    fn wakeup_of_an_earlier_process_was_lost() {
        let tool = scheduled(1200, now() + chrono::Duration::seconds(600));
        assert_eq!(
            describe_schedule(&tool, now(), "later"),
            "Wakeup lost with a restart · was due at 14:22"
        );
    }

    #[test]
    fn results_without_process_count_as_armed() {
        let tool = with_output(1200, now() + chrono::Duration::seconds(600), |output| {
            output.armed_by = None;
        });
        assert_eq!(
            describe_schedule(&tool, now(), "later"),
            "Wakeup in 10 min · at 14:22"
        );
    }

    #[test]
    fn results_without_fire_time_fall_back_to_the_delay() {
        let mut tool = make_tool("schedule_wakeup", &[]);
        tool.output = Some(r#"{"wakeup_id":1,"delay_seconds":5400}"#.to_string());
        assert_eq!(
            describe_schedule(&tool, now(), HERE),
            "Wakeup in 1 h 30 min"
        );
    }

    #[test]
    fn cancel_names_the_wakeup() {
        let tool = make_tool("cancel_wakeup", &[("wakeup_id", "3")]);
        assert_eq!(WakeupToolRenderer.describe(&tool), "Cancel wakeup #3");
    }

    #[test]
    fn minutes_round_up_until_the_seconds_take_over() {
        let s = Duration::from_secs;
        assert_eq!(format_remaining(s(120)), "2 min");
        assert_eq!(format_remaining(s(61)), "2 min");
        assert_eq!(format_remaining(s(60)), "60 s");
        assert_eq!(format_remaining(s(1190)), "20 min");
        assert_eq!(format_remaining(s(1210)), "21 min");
    }

    #[test]
    fn refresh_follows_the_displayed_unit() {
        let ms = Duration::from_millis;
        assert_eq!(until_display_changes(ms(30_400)), ms(420));
        // 20 min 10 s shows "21 min" until 20 min remain.
        assert_eq!(until_display_changes(ms(1_210_000)), ms(10_020));
        assert_eq!(until_display_changes(ms(1_200_000)), ms(60_020));
        // 1 min 10 s shows "2 min" until the seconds take over.
        assert_eq!(until_display_changes(ms(70_000)), ms(10_020));
    }
}
