//! The row under the header: Summary · Timeline · Code, and on its right
//! what belongs to the shown tab: the checks ("5 of 6 passing", a popover
//! lists them) on Summary, the comment and commit counts and the order on
//! Timeline.

use super::PullRequestIntent;
use super::detail::{DetailTab, DetailView};
use super::presentation::{
    GLYPH_SIZE, Glyph, check_status_glyph, checks_glyph, group_checks, summarize_checks,
};
use super::timeline::TimelineTab;
use crate::shared::segmented_switch::segmented_switch;
use github::model::{Activity, Check, ChecksState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, App, Context, Entity, SharedString, WeakEntity, Window, div, prelude::*, px, svg,
};

pub fn render_tab_row(
    tab: DetailTab,
    right: Option<AnyElement>,
    window: &mut Window,
    cx: &mut Context<DetailView>,
) -> impl IntoElement {
    let view = cx.entity().downgrade();
    div()
        .flex_none()
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(segmented_switch(
            "pr-tab",
            DetailTab::ALL,
            tab,
            move |tab, _, cx| {
                view.update(cx, |view, cx| view.select_tab(tab, cx)).ok();
            },
            window,
            cx,
        ))
        .child(div().flex_1())
        .children(right)
}

/// "✓ 5 of 6 passing"; a click lists the checks: those needing attention
/// first, then the running ones, the completed ones behind "Show all"
/// while there are others (T3 Code's `PullRequestChecksPopover`).
pub fn checks_slot(
    checks: &[Check],
    state: Option<ChecksState>,
    view: WeakEntity<DetailView>,
    cx: &App,
) -> Option<AnyElement> {
    if checks.is_empty() {
        return None;
    }
    let glyph = state.map(|state| checks_glyph(state, cx));
    let summary = summarize_checks(checks);
    let checks = checks.to_vec();
    Some(
        Popover::new("pr-checks")
            .anchor(gpui_kit::Anchor::TopRight)
            .trigger(
                Button::new("pr-checks-trigger")
                    .ghost()
                    .xsmall()
                    .when_some(glyph.clone(), |button, glyph| {
                        button.icon(Icon::default().path(glyph.icon).text_color(glyph.color))
                    })
                    .label(summary.clone()),
            )
            .content(move |_, _, cx| checks_list(&checks, glyph.as_ref(), &summary, &view, cx))
            .into_any_element(),
    )
}

fn checks_list(
    checks: &[Check],
    glyph: Option<&Glyph>,
    summary: &str,
    view: &WeakEntity<DetailView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let show_all = view
        .upgrade()
        .is_some_and(|view| view.read(cx).checks_show_all());
    let groups = group_checks(checks);
    let collapsible = !groups.attention.is_empty() || !groups.running.is_empty();
    let collapsible = collapsible && !groups.completed.is_empty();
    let shown = groups.attention.into_iter().chain(groups.running).chain(
        groups
            .completed
            .into_iter()
            .filter(|_| show_all || !collapsible),
    );
    let rows = shown.enumerate().map(|(i, check)| {
        let glyph = check_status_glyph(check, cx);
        let url = check.url.clone();
        let tooltip: SharedString = check
            .description
            .clone()
            .unwrap_or_else(|| check.name.clone())
            .into();
        let view = view.clone();
        div()
            .id(("pr-check", i))
            .py_0p5()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .child(glyph.render(GLYPH_SIZE))
            .child(
                div()
                    .id(("pr-check-name", i))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .child(check.name.clone())
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                            .build(window, cx)
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .child(glyph.label.clone()),
            )
            .when_some(url, |row, url| {
                row.child(
                    div()
                        .id(("pr-check-details", i))
                        .flex_none()
                        .text_color(theme.link)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .child("Details")
                        .on_click(move |_, _, cx| {
                            view.update(cx, |_, cx| {
                                cx.emit(PullRequestIntent::OpenUrl(url.clone()))
                            })
                            .ok();
                        }),
                )
            })
    });
    let toggle = view.clone();
    div()
        .w(px(360.))
        .flex()
        .flex_col()
        .gap_1()
        .when_some(glyph, |d, glyph| {
            d.child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(glyph.label.clone()),
            )
        })
        .child(
            div()
                .mb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(summary.to_owned()),
        )
        .child(
            div()
                .id("pr-checks-list")
                .max_h(px(256.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
        .when(collapsible, |d| {
            d.child(
                Button::new("pr-checks-show-all")
                    .ghost()
                    .xsmall()
                    .label(if show_all { "Show less" } else { "Show all" })
                    .on_click(move |_, _, cx| {
                        toggle
                            .update(cx, |view, cx| view.toggle_checks_show_all(cx))
                            .ok();
                    }),
            )
        })
        .into_any_element()
}

/// "▷ Approve workflows to run" in place of the checks while runs of a
/// fork wait for the viewer: the first click arms it ("Confirm
/// approve?"), the second approves.
pub fn approve_workflows_slot(
    armed: bool,
    approving: bool,
    view: WeakEntity<DetailView>,
    cx: &mut Context<DetailView>,
) -> AnyElement {
    let label = match (approving, armed) {
        (true, _) => "Approving...",
        (false, true) => "Confirm approve?",
        (false, false) => "Approve workflows to run",
    };
    let button = Button::new("pr-approve-workflows")
        .warning()
        .outline()
        .xsmall()
        .loading(approving)
        .icon(Icon::default().path(if armed {
            "icons/check.svg"
        } else {
            "icons/play.svg"
        }))
        .label(label)
        .on_click(move |_, _, cx| {
            view.update(cx, |view, cx| view.click_approve_workflows(cx))
                .ok();
        })
        .when(armed, |button| {
            button.on_mouse_down_out(cx.listener(|view, _, _, cx| view.disarm(cx)))
        });
    div()
        .debug_selector(|| "pr-approve-workflows".into())
        .child(button)
        .into_any_element()
}

/// "💬 3 · ⟜ 1 · ⇅ Newest first"; the order toggles on a click.
pub fn timeline_slot(
    activity: Option<&Activity>,
    timeline: &Entity<TimelineTab>,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let count = |icon: &'static str, count: usize| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(svg().size(px(13.)).path(icon).text_color(muted))
            .child(count.to_string())
    };
    let newest_first = timeline.read(cx).newest_first();
    let timeline = timeline.downgrade();
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(muted)
        .when_some(activity, |row, activity| {
            row.child(count(
                "icons/message_square.svg",
                activity.comment_count as usize,
            ))
            .child("·")
            .child(count(
                "icons/git_commit_horizontal.svg",
                activity.commits.len(),
            ))
        })
        .child(
            Button::new("pr-timeline-order")
                .ghost()
                .xsmall()
                .icon(Icon::default().path("icons/arrow_up_down.svg"))
                .label(SharedString::from(if newest_first {
                    "Newest first"
                } else {
                    "Oldest first"
                }))
                .on_click(move |_, _, cx| {
                    timeline.update(cx, |t, cx| t.toggle_order(cx)).ok();
                }),
        )
        .into_any_element()
}
