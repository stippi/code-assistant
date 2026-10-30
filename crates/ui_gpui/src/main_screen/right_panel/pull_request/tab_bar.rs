//! The row under the header: Summary · Timeline · Code, and on its right
//! what belongs to the shown tab: the checks ("5 of 6 passing", a popover
//! lists them) on Summary, the comment and commit counts and the order on
//! Timeline.

use super::PullRequestIntent;
use super::detail::{DetailTab, DetailView};
use super::presentation::{GLYPH_SIZE, check_status_glyph, checks_glyph, summarize_checks};
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

/// "✓ 5 of 6 passing"; a click lists the checks.
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
                    .when_some(glyph, |button, glyph| {
                        button.icon(Icon::default().path(glyph.icon).text_color(glyph.color))
                    })
                    .label(summary),
            )
            .content(move |_, _, cx| checks_list(&checks, &view, cx))
            .into_any_element(),
    )
}

fn checks_list(checks: &[Check], view: &WeakEntity<DetailView>, cx: &App) -> AnyElement {
    div()
        .w(px(320.))
        .max_h(px(360.))
        .flex()
        .flex_col()
        .gap_0p5()
        .children(checks.iter().enumerate().map(|(i, check)| {
            let glyph = check_status_glyph(check, cx);
            let url = check.url.clone();
            let view = view.clone();
            div()
                .id(("pr-check", i))
                .px_1()
                .py_0p5()
                .rounded_sm()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(glyph.render(GLYPH_SIZE))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().foreground)
                        .child(check.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(cx.theme().muted_foreground)
                        .child(glyph.label.clone()),
                )
                .when_some(url, |row, url| {
                    row.cursor_pointer()
                        .hover(|s| s.bg(cx.theme().muted))
                        .on_click(move |_, _, cx| {
                            view.update(cx, |_, cx| {
                                cx.emit(PullRequestIntent::OpenUrl(url.clone()))
                            })
                            .ok();
                        })
                })
        }))
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
