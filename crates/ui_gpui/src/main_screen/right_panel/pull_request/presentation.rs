//! How pull requests look across the view: the colour and glyph of a state,
//! of checks and of a review decision, line counts (`+1,933 −179`) and
//! relative times ("9h ago"). Colours follow T3 Code: open emerald, draft
//! zinc, closed red, merged violet; approved emerald, changes requested
//! amber, pending checks amber.

use crate::diff::render::{added_row_colors, deleted_row_colors};
use crate::shared::theme::merged_violet;
use chrono::{DateTime, Utc};
use github::model::{
    Check, CheckStatus, ChecksState, MergeMethod, Mergeability, PullRequestState, ReviewDecision,
};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{AnyElement, App, Hsla, Pixels, SharedString, div, prelude::*, px, svg};

/// What a pull request's state is shown as: a draft is open but looks apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Open,
    Draft,
    Closed,
    Merged,
}

impl Tone {
    pub fn of(state: PullRequestState, is_draft: bool) -> Self {
        match state {
            PullRequestState::Open if is_draft => Tone::Draft,
            PullRequestState::Open => Tone::Open,
            PullRequestState::Closed => Tone::Closed,
            PullRequestState::Merged => Tone::Merged,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tone::Open => "Open",
            Tone::Draft => "Draft",
            Tone::Closed => "Closed",
            Tone::Merged => "Merged",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Tone::Open => "icons/git_pull_request.svg",
            Tone::Draft => "icons/git_pull_request_draft.svg",
            Tone::Closed => "icons/git_pull_request_closed.svg",
            Tone::Merged => "icons/git_merge.svg",
        }
    }

    pub fn color(self, cx: &App) -> Hsla {
        match self {
            Tone::Open => cx.theme().success,
            Tone::Draft => cx.theme().muted_foreground,
            Tone::Closed => cx.theme().danger,
            Tone::Merged => merged_violet(),
        }
    }
}

/// An icon with its colour and what it means (the tooltip).
#[derive(Clone, Debug)]
pub struct Glyph {
    pub icon: &'static str,
    pub color: Hsla,
    pub label: SharedString,
}

impl Glyph {
    pub fn render(&self, size: Pixels) -> impl IntoElement + use<> {
        svg()
            .flex_none()
            .size(size)
            .path(self.icon)
            .text_color(self.color)
    }
}

/// The warning on an open pull request that conflicts with its base.
pub fn conflict_glyph(
    state: PullRequestState,
    is_draft: bool,
    mergeability: Mergeability,
    base_branch: &str,
    cx: &App,
) -> Option<Glyph> {
    (state == PullRequestState::Open && !is_draft && mergeability == Mergeability::Conflicting)
        .then(|| Glyph {
            icon: "icons/triangle_alert.svg",
            color: cx.theme().danger,
            label: format!("Conflicts with {base_branch}").into(),
        })
}

pub fn checks_glyph(state: ChecksState, cx: &App) -> Glyph {
    let (icon, color, label) = match state {
        ChecksState::Passing => (
            "icons/circle_check.svg",
            cx.theme().success,
            "All checks have passed",
        ),
        ChecksState::Failing => (
            "icons/circle_x.svg",
            cx.theme().danger,
            "Some checks were not successful",
        ),
        ChecksState::Pending => (
            "icons/circle_dot.svg",
            cx.theme().warning,
            "Some checks haven't completed yet",
        ),
    };
    Glyph {
        icon,
        color,
        label: label.into(),
    }
}

pub fn check_status_glyph(check: &Check, cx: &App) -> Glyph {
    let muted = cx.theme().muted_foreground.opacity(0.7);
    let (icon, color) = match check.status {
        CheckStatus::Pending => ("icons/loader_circle.svg", cx.theme().warning),
        CheckStatus::ActionRequired => ("icons/circle_dot.svg", cx.theme().warning),
        CheckStatus::Success => ("icons/circle_check.svg", cx.theme().success),
        CheckStatus::Failure | CheckStatus::Cancelled => ("icons/circle_x.svg", cx.theme().danger),
        CheckStatus::Skipped => ("icons/circle_slash.svg", muted),
        CheckStatus::Neutral => ("icons/circle_dashed.svg", muted),
    };
    Glyph {
        icon,
        color,
        label: check_status_label(check).into(),
    }
}

pub fn review_glyph(decision: ReviewDecision, cx: &App) -> Glyph {
    let (icon, color, label) = match decision {
        ReviewDecision::Approved => ("icons/user_check.svg", cx.theme().success, "Approved"),
        ReviewDecision::ChangesRequested => (
            "icons/user_round_x.svg",
            cx.theme().warning,
            "Changes requested",
        ),
        ReviewDecision::ReviewRequired => (
            "icons/user_round.svg",
            cx.theme().muted_foreground.opacity(0.6),
            "Awaiting review",
        ),
    };
    Glyph {
        icon,
        color,
        label: label.into(),
    }
}

pub fn merge_method_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "Merge",
        MergeMethod::Squash => "Squash and merge",
        MergeMethod::Rebase => "Rebase and merge",
    }
}

/// A workflow run of an outside contributor waits for a maintainer to let
/// it run.
fn is_workflow_approval(check: &Check) -> bool {
    check.status == CheckStatus::ActionRequired
        && check
            .url
            .as_deref()
            .and_then(|url| url.split_once("/actions/runs/"))
            .is_some_and(|(_, rest)| {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                digits > 0 && matches!(rest.as_bytes().get(digits), None | Some(b'/'))
            })
}

pub fn check_status_label(check: &Check) -> &'static str {
    if is_workflow_approval(check) {
        return "Awaiting approval";
    }
    match check.status {
        CheckStatus::Pending => "Running",
        CheckStatus::ActionRequired => "Awaiting action",
        CheckStatus::Success => "Passed",
        CheckStatus::Failure => "Failed",
        CheckStatus::Cancelled => "Cancelled",
        CheckStatus::Skipped => "Skipped",
        CheckStatus::Neutral => "Neutral",
    }
}

/// The verdict over `checks`: any failure fails, anything unfinished is
/// pending; `None` without checks or without a single success.
pub fn checks_state(checks: &[Check]) -> Option<ChecksState> {
    let any = |statuses: &[CheckStatus]| checks.iter().any(|c| statuses.contains(&c.status));
    if any(&[CheckStatus::Failure, CheckStatus::Cancelled]) {
        Some(ChecksState::Failing)
    } else if any(&[CheckStatus::Pending, CheckStatus::ActionRequired]) {
        Some(ChecksState::Pending)
    } else if any(&[CheckStatus::Success]) {
        Some(ChecksState::Passing)
    } else {
        None
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// "5 of 6 passing", "1 of 6 failing", "2 of 6 running" and the like.
pub fn summarize_checks(checks: &[Check]) -> String {
    let total = checks.len();
    if total == 0 {
        return "No checks reported".into();
    }
    let count = |statuses: &[CheckStatus]| {
        checks
            .iter()
            .filter(|c| statuses.contains(&c.status))
            .count()
    };
    let failed = count(&[CheckStatus::Failure, CheckStatus::Cancelled]);
    let action_required = count(&[CheckStatus::ActionRequired]);
    let approvals = checks.iter().filter(|c| is_workflow_approval(c)).count();
    let other_actions = action_required - approvals;
    let pending = count(&[CheckStatus::Pending]);
    let passed = count(&[CheckStatus::Success]);
    if failed > 0 {
        format!("{failed} of {total} failing")
    } else if approvals > 0 && other_actions > 0 {
        format!(
            "{} and {} awaiting action",
            plural(approvals, "workflow", "workflows"),
            plural(other_actions, "check", "checks")
        )
    } else if approvals > 0 {
        format!(
            "{} awaiting approval",
            plural(approvals, "workflow", "workflows")
        )
    } else if other_actions > 0 {
        format!(
            "{} awaiting action",
            plural(other_actions, "check", "checks")
        )
    } else if pending > 0 {
        format!("{pending} of {total} running")
    } else if passed == total {
        "All checks passed".into()
    } else {
        format!("{passed} of {total} passing")
    }
}

/// "just now", "12m ago", "9h ago", "185d ago".
pub fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - then).num_seconds();
    if seconds < 60 {
        return "just now".into();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// `1933` → `1,933`.
pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `+1,933 −179` in the diff colours and the mono font; nothing while both
/// are zero (not counted yet).
pub fn diff_stat(additions: u64, deletions: u64, cx: &App) -> Option<AnyElement> {
    if additions == 0 && deletions == 0 {
        return None;
    }
    let theme = cx.theme();
    Some(
        div()
            .flex_none()
            .flex()
            .items_baseline()
            .gap_1()
            .font_family(theme.mono_font_family.clone())
            .child(
                div()
                    .text_color(added_row_colors(theme).1)
                    .child(format!("+{}", format_count(additions))),
            )
            .child(
                div()
                    .text_color(deleted_row_colors(theme).1)
                    .child(format!("−{}", format_count(deletions))),
            )
            .into_any_element(),
    )
}

/// A small glyph: `svg` sized for a row.
pub const GLYPH_SIZE: Pixels = px(14.);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{check, hours_ago, now};
    use chrono::Duration;

    #[test]
    fn relative_times() {
        assert_eq!(
            relative_time(now() - Duration::seconds(59), now()),
            "just now"
        );
        assert_eq!(
            relative_time(now() - Duration::minutes(12), now()),
            "12m ago"
        );
        assert_eq!(relative_time(hours_ago(9), now()), "9h ago");
        assert_eq!(relative_time(hours_ago(24 * 185 + 5), now()), "185d ago");
        // A clock running behind the host's.
        assert_eq!(
            relative_time(now() + Duration::minutes(3), now()),
            "just now"
        );
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(179), "179");
        assert_eq!(format_count(1933), "1,933");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn checks_summaries() {
        use CheckStatus::*;
        let of = |statuses: &[CheckStatus]| {
            let checks: Vec<Check> = statuses.iter().map(|s| check("ci", *s)).collect();
            (summarize_checks(&checks), checks_state(&checks))
        };
        assert_eq!(of(&[]), ("No checks reported".into(), None));
        assert_eq!(
            of(&[Success, Success, Skipped]),
            ("2 of 3 passing".into(), Some(ChecksState::Passing))
        );
        assert_eq!(
            of(&[Success, Success]),
            ("All checks passed".into(), Some(ChecksState::Passing))
        );
        assert_eq!(
            of(&[Success, Failure, Pending]),
            ("1 of 3 failing".into(), Some(ChecksState::Failing))
        );
        assert_eq!(
            of(&[Success, Pending, Pending]),
            ("2 of 3 running".into(), Some(ChecksState::Pending))
        );
        assert_eq!(
            of(&[ActionRequired]),
            ("1 check awaiting action".into(), Some(ChecksState::Pending))
        );
        assert_eq!(of(&[Neutral, Skipped]).1, None);
    }

    #[test]
    fn workflow_runs_awaiting_approval_are_told_apart() {
        let mut run = check("build", CheckStatus::ActionRequired);
        run.url = Some("https://github.com/o/r/actions/runs/123/job/4".into());
        assert_eq!(check_status_label(&run), "Awaiting approval");
        assert_eq!(
            summarize_checks(&[run.clone()]),
            "1 workflow awaiting approval"
        );
        let other = check("deploy", CheckStatus::ActionRequired);
        assert_eq!(
            summarize_checks(&[run.clone(), run, other]),
            "2 workflows and 1 check awaiting action"
        );
    }

    #[test]
    fn a_draft_is_its_own_tone() {
        assert_eq!(Tone::of(PullRequestState::Open, true), Tone::Draft);
        assert_eq!(Tone::of(PullRequestState::Open, false), Tone::Open);
        // Closed and merged drafts show their state.
        assert_eq!(Tone::of(PullRequestState::Merged, true), Tone::Merged);
    }
}
