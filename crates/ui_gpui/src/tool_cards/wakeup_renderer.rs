//! Inline renderer for `schedule_wakeup` / `cancel_wakeup`.
//!
//! A scheduled wakeup shows when it fires and counts down to it; once the
//! wakeup message arrived in the same turn (`ToolUseBlock::wakeup_fired`),
//! the line says it fired, and expanding it shows the prompt.

use super::inline_renderer::{render_inline_output, render_inline_output_text};
use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use crate::shared::file_icons;
use chrono::{DateTime, Local};
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
        describe_schedule(tool, Local::now())
    }

    fn icon(&self, tool: &ToolUseBlock) -> Option<SharedString> {
        tool.wakeup_fired
            .then(|| file_icons::get().get_type_icon(file_icons::TOOL_WAKEUP_FIRED))
            .flatten()
    }

    fn refresh_after(&self, tool: &ToolUseBlock) -> Option<Duration> {
        if tool.wakeup_fired {
            return None;
        }
        let remaining = (fires_at(tool)? - Local::now()).to_std().ok()?;
        Some(until_display_changes(remaining))
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

fn output(tool: &ToolUseBlock) -> Option<ScheduleWakeupOutput> {
    ScheduleWakeupOutput::from_ui_output(tool.output.as_deref()?)
}

fn fires_at(tool: &ToolUseBlock) -> Option<DateTime<Local>> {
    Some(output(tool)?.fires_at?.with_timezone(&Local))
}

fn describe_schedule(tool: &ToolUseBlock, now: DateTime<Local>) -> String {
    let Some(output) = output(tool) else {
        // Not executed yet (or failed): what was asked for.
        return match param(tool, "delay_seconds").and_then(|d| d.parse().ok()) {
            Some(delay) => format!("Schedule wakeup in {}", format_duration(delay)),
            None => "Schedule wakeup".to_string(),
        };
    };
    let Some(fires_at) = output.fires_at.map(|t| t.with_timezone(&Local)) else {
        // Recorded before results carried the fire time.
        return if tool.wakeup_fired {
            "Woke up".to_string()
        } else {
            format!("Wakeup in {}", format_duration(output.delay_seconds))
        };
    };
    let when = format_when(fires_at, now);
    if tool.wakeup_fired {
        return format!("Woke up {when}");
    }
    match (fires_at - now).to_std() {
        Ok(remaining) if !remaining.is_zero() => {
            format!("Wakeup {when} · in {}", format_remaining(remaining))
        }
        // Due, but no wakeup message (yet): it may still be on its way, or
        // was cancelled or lost with a restart.
        _ => format!("Wakeup {when}"),
    }
}

/// "at 14:32" today, "on Oct 13 at 09:00" on any other day.
fn format_when(time: DateTime<Local>, now: DateTime<Local>) -> String {
    if time.date_naive() == now.date_naive() {
        format!("at {}", time.format("%H:%M"))
    } else {
        format!("on {}", time.format("%b %-d at %H:%M"))
    }
}

/// A delay as asked for: "45 s", "20 min", "2 h 5 min".
fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 && !seconds.is_multiple_of(60) {
        format!("{} min {} s", seconds / 60, seconds % 60)
    } else {
        format_minutes(seconds / 60)
    }
}

/// A countdown: seconds in the last minute, the nearest whole minute before
/// that.
fn format_remaining(remaining: Duration) -> String {
    let seconds = remaining.as_secs_f64().ceil() as u64;
    if seconds <= 60 {
        format!("{seconds} s")
    } else {
        format_minutes((seconds + 30) / 60)
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
    // Minutes round to the nearest: the value drops at every half minute.
    let into_minute = (millis + 30_000) % 60_000;
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

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 12, 14, 12, 0).unwrap()
    }

    fn scheduled(delay_seconds: u64, fires_at: DateTime<Local>) -> ToolUseBlock {
        let mut tool = make_tool(
            "schedule_wakeup",
            &[
                ("delay_seconds", &delay_seconds.to_string()),
                ("prompt", "check the build"),
            ],
        );
        tool.output = Some(
            serde_json::to_string(&ScheduleWakeupOutput {
                wakeup_id: 1,
                delay_seconds,
                fires_at: Some(fires_at.to_utc()),
            })
            .unwrap(),
        );
        tool
    }

    #[test]
    fn before_execution_shows_the_requested_delay() {
        let tool = make_tool("schedule_wakeup", &[("delay_seconds", "1200")]);
        assert_eq!(describe_schedule(&tool, now()), "Schedule wakeup in 20 min");
        let bare = make_tool("schedule_wakeup", &[]);
        assert_eq!(describe_schedule(&bare, now()), "Schedule wakeup");
    }

    #[test]
    fn armed_wakeup_counts_down() {
        let tool = scheduled(1200, now() + chrono::Duration::seconds(1190));
        assert_eq!(
            describe_schedule(&tool, now()),
            "Wakeup at 14:31 · in 20 min"
        );
        let tool = scheduled(30, now() + chrono::Duration::seconds(30));
        assert_eq!(describe_schedule(&tool, now()), "Wakeup at 14:12 · in 30 s");
    }

    #[test]
    fn other_days_show_the_date() {
        let tool = scheduled(86_400, now() + chrono::Duration::days(1));
        assert_eq!(
            describe_schedule(&tool, now()),
            "Wakeup on Oct 13 at 14:12 · in 24 h"
        );
    }

    #[test]
    fn due_wakeup_without_message_claims_nothing() {
        let tool = scheduled(60, now() - chrono::Duration::seconds(5));
        assert_eq!(describe_schedule(&tool, now()), "Wakeup at 14:11");
    }

    #[test]
    fn fired_wakeup_says_so() {
        let mut tool = scheduled(60, now() - chrono::Duration::seconds(5));
        tool.wakeup_fired = true;
        assert_eq!(describe_schedule(&tool, now()), "Woke up at 14:11");
    }

    #[test]
    fn results_without_fire_time_fall_back_to_the_delay() {
        let mut tool = make_tool("schedule_wakeup", &[]);
        tool.output = Some(r#"{"wakeup_id":1,"delay_seconds":5400}"#.to_string());
        assert_eq!(describe_schedule(&tool, now()), "Wakeup in 1 h 30 min");
    }

    #[test]
    fn cancel_names_the_wakeup() {
        let tool = make_tool("cancel_wakeup", &[("wakeup_id", "3")]);
        assert_eq!(WakeupToolRenderer.describe(&tool), "Cancel wakeup #3");
    }

    #[test]
    fn refresh_follows_the_displayed_unit() {
        let ms = Duration::from_millis;
        assert_eq!(until_display_changes(ms(30_400)), ms(420));
        // 20 min 10 s shows "20 min" until 19 min 30 s remain.
        assert_eq!(until_display_changes(ms(1_210_000)), ms(40_020));
        assert_eq!(until_display_changes(ms(1_200_000)), ms(30_020));
    }
}
