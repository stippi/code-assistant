//! Small pieces the cards share: who wrote a comment and when, a review's
//! verdict, where a comment sits in the code, and the one-line form of a
//! settled card.

use super::super::PullRequestIntent;
use super::super::avatar::avatar;
use super::super::presentation::relative_time;
use super::logic::ReviewOutcome;
use chrono::{DateTime, Utc};
use github::model::{Actor, ReviewState};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, FontWeight, Hsla, SharedString, div, prelude::*, px,
    svg,
};

/// Where a comment sits in the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    /// `None` once the line left the diff.
    pub line: Option<u64>,
    pub outdated: bool,
}

impl Location {
    /// `path:line`, or the path alone.
    pub fn label(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

pub fn outcome_color(outcome: ReviewOutcome, cx: &App) -> Hsla {
    match outcome {
        ReviewOutcome::Approved => cx.theme().success,
        ReviewOutcome::ChangesRequested => cx.theme().danger,
        ReviewOutcome::Dismissed => cx.theme().muted_foreground,
    }
}

pub fn outcome_icon(outcome: ReviewOutcome) -> &'static str {
    match outcome {
        ReviewOutcome::Approved => "icons/circle_check.svg",
        ReviewOutcome::ChangesRequested => "icons/circle_x.svg",
        ReviewOutcome::Dismissed => "icons/circle_dashed.svg",
    }
}

/// The profile of `login` on `host`.
pub fn profile_url(host: &str, login: &str) -> String {
    format!("https://{host}/{login}")
}

/// Avatar and login (a link to the profile), then the relative time (a
/// link to the comment, the full date as tooltip).
pub fn identity<V: EventEmitter<PullRequestIntent>>(
    id: &str,
    author: Option<&Actor>,
    host: &str,
    created_at: DateTime<Utc>,
    url: Option<&str>,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let login: SharedString = author.map_or("ghost".into(), |a| a.login.clone().into());
    let profile = author.map(|a| profile_url(host, &a.login));
    let full_date: SharedString = created_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
        .into();
    let url = url.map(str::to_owned);
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .child(avatar(author, px(16.), cx))
        .child(
            div()
                .id(SharedString::from(format!("{id}-author")))
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(login)
                .when_some(profile, |el, profile| {
                    el.cursor_pointer()
                        .hover(|s| s.underline())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(PullRequestIntent::OpenUrl(profile.clone()))
                        }))
                }),
        )
        .child(
            div()
                .id(SharedString::from(format!("{id}-time")))
                .flex_none()
                .text_color(muted)
                .child(relative_time(created_at, Utc::now()))
                .tooltip(move |window, cx| Tooltip::new(full_date.clone()).build(window, cx))
                .when_some(url, |el, url| {
                    el.cursor_pointer()
                        .hover(|s| s.underline())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(PullRequestIntent::OpenUrl(url.clone()))
                        }))
                }),
        )
        .into_any_element()
}

/// "Approved" / "Changes requested" as a coloured pill, a stale one muted
/// with the reason as tooltip; other review states as plain text.
pub fn review_badge(
    id: &str,
    state: Option<ReviewState>,
    stale: bool,
    cx: &App,
) -> Option<AnyElement> {
    let theme = cx.theme();
    let Some(outcome) = ReviewOutcome::of(state) else {
        return match state? {
            ReviewState::Commented => Some(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Commented")
                    .into_any_element(),
            ),
            _ => None,
        };
    };
    let color = if stale {
        theme.muted_foreground
    } else {
        outcome_color(outcome, cx)
    };
    let stale_label: SharedString = outcome.stale_label().into();
    Some(
        div()
            .id(SharedString::from(format!("{id}-verdict")))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_1p5()
            .rounded_full()
            .border_1()
            .border_color(color.opacity(0.4))
            .bg(color.opacity(0.08))
            .text_xs()
            .text_color(color)
            .child(
                svg()
                    .size(px(11.))
                    .path(outcome_icon(outcome))
                    .text_color(color),
            )
            .child(outcome.label())
            .when(stale, |el| {
                el.tooltip(move |window, cx| Tooltip::new(stale_label.clone()).build(window, cx))
            })
            .into_any_element(),
    )
}

/// `path:line` in the mono font, "Outdated" after it.
pub fn location_line(location: &Location, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(muted)
        .child(
            svg()
                .flex_none()
                .size(px(12.))
                .path("icons/file_code.svg")
                .text_color(muted),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(cx.theme().mono_font_family.clone())
                .child(location.label()),
        )
        .when(location.outdated, |el| {
            el.child(div().flex_none().child("Outdated"))
        })
        .into_any_element()
}

/// What the one-line form of a settled card shows.
pub struct Collapsed<'a> {
    pub id: &'a str,
    pub author: Option<&'a Actor>,
    pub host: &'a str,
    pub created_at: DateTime<Utc>,
    pub url: Option<&'a str>,
    /// Replies in the thread, read or not.
    pub replies: u64,
    /// "Resolved", "Review dismissed".
    pub label: &'static str,
    pub preview: String,
    pub location: Option<&'a Location>,
    /// The reaction bar of the leading comment.
    pub reactions: Option<AnyElement>,
}

/// A settled card folded to one line: who and when, the replies, the
/// state (a click unfolds it), the reactions, then where and a preview.
pub fn render_collapsed<V: EventEmitter<PullRequestIntent>>(
    collapsed: Collapsed,
    on_expand: impl Fn(&mut V, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, border, hover_bg, foreground) = (
        theme.muted_foreground,
        theme.border,
        theme.muted,
        theme.foreground,
    );
    let id = collapsed.id;
    let expand = on_expand.clone();
    div()
        .id(SharedString::from(format!("{id}-collapsed")))
        .debug_selector({
            let selector = format!("{id}-collapsed");
            move || selector.clone()
        })
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(border.opacity(0.6))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(identity(
                    id,
                    collapsed.author,
                    collapsed.host,
                    collapsed.created_at,
                    collapsed.url,
                    cx,
                ))
                .when(collapsed.replies > 0, |row| {
                    row.child(div().flex_none().text_xs().text_color(muted).child(format!(
                        "{} {}",
                        collapsed.replies,
                        if collapsed.replies == 1 {
                            "reply"
                        } else {
                            "replies"
                        }
                    )))
                })
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-expand")))
                        .debug_selector({
                            let selector = format!("{id}-expand");
                            move || selector.clone()
                        })
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(muted)
                        .hover(|s| s.bg(hover_bg).text_color(foreground))
                        .child(collapsed.label)
                        .child(
                            svg()
                                .size(px(12.))
                                .path("icons/chevron_down.svg")
                                .text_color(muted),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| expand(this, cx))),
                )
                .child(div().flex_1())
                .children(collapsed.reactions),
        )
        .when_some(collapsed.location, |el, location| {
            el.child(location_line(location, cx))
        })
        .when(!collapsed.preview.is_empty(), |el| {
            el.child(
                div()
                    .id(SharedString::from(format!("{id}-preview")))
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(muted)
                    .hover(|s| s.text_color(foreground))
                    .child(collapsed.preview)
                    .on_click(cx.listener(move |this, _, _, cx| on_expand(this, cx))),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_name_their_line() {
        let mut location = Location {
            path: "src/lib.rs".into(),
            line: Some(12),
            outdated: false,
        };
        assert_eq!(location.label(), "src/lib.rs:12");
        location.line = None;
        assert_eq!(location.label(), "src/lib.rs");
    }
}
