//! The rows above the description: Reviewers (overlapping avatars, each
//! ringed in its verdict's colour, faded when the verdict predates the
//! newest commit) and Labels (chips in the label's colour).

use super::super::avatar::avatar;
use super::super::conversation::logic::ReviewerEntry;
use super::super::conversation::{outcome_color, outcome_icon};
use github::model::Label;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{AnyElement, App, Hsla, SharedString, div, prelude::*, px, rgb, svg};

const AVATAR: f32 = 20.;

/// A label's colour from GitHub's hex (`d73a4a`, with or without `#`).
pub fn label_color(hex: Option<&str>) -> Option<Hsla> {
    let hex = hex?.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16)
        .ok()
        .map(|value| rgb(value).into())
}

/// The left column of a meta row: icon and name.
pub fn meta_label(icon: &'static str, label: &'static str, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    div()
        .flex_none()
        .w(px(96.))
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .text_color(muted)
        .child(svg().size(px(13.)).path(icon).text_color(muted))
        .child(label)
}

pub fn none_label(cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("None")
        .into_any_element()
}

/// The reviewers' avatars, overlapping, each with its verdict.
pub fn reviewer_avatars(entries: &[ReviewerEntry], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let background = theme.background;
    div()
        .flex()
        .items_center()
        .children(entries.iter().enumerate().map(|(i, entry)| {
            let login = entry
                .actor
                .as_ref()
                .map_or("ghost".to_owned(), |a| a.login.clone());
            let named = match entry.actor.as_ref().and_then(|a| a.name.as_deref()) {
                Some(name) if name != login => format!("{name} (@{login})"),
                _ => login.clone(),
            };
            let tip: SharedString = match entry.outcome {
                Some(outcome) if entry.stale => format!("{named}: {}", outcome.stale_label()),
                Some(outcome) => format!("{named}: {}", outcome.label()),
                None => named,
            }
            .into();
            let ring = entry.outcome.map(|outcome| {
                let color = outcome_color(outcome, cx);
                if entry.stale {
                    color.opacity(0.35)
                } else {
                    color
                }
            });
            div()
                .id(("pr-reviewer", i))
                .relative()
                .flex_none()
                .when(i > 0, |el| el.ml(px(-4.)))
                .rounded_full()
                .border_2()
                .border_color(ring.unwrap_or(background))
                .child(avatar(entry.actor.as_ref(), px(AVATAR), cx))
                .when_some(entry.outcome, |el, outcome| {
                    el.child(
                        div()
                            .absolute()
                            .right(px(-3.))
                            .bottom(px(-3.))
                            .rounded_full()
                            .bg(background)
                            .child(
                                svg()
                                    .size(px(11.))
                                    .path(outcome_icon(outcome))
                                    .text_color(ring.unwrap_or(background)),
                            ),
                    )
                })
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        }))
        .into_any_element()
}

/// A label as a chip: tinted in its colour, with a dot of it.
pub fn label_chip(label: &Label, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = label_color(label.color.as_deref());
    let tint = color.unwrap_or(theme.muted_foreground);
    div()
        .flex_none()
        .max_w(px(192.))
        .h(px(20.))
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_full()
        .border_1()
        .border_color(tint.opacity(0.45))
        .bg(tint.opacity(0.14))
        .text_xs()
        .text_color(theme.foreground)
        .child(div().flex_none().size_2().rounded_full().bg(tint))
        .child(div().min_w_0().truncate().child(label.name.clone()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_colours_come_from_hex() {
        let red: Hsla = rgb(0xd73a4a).into();
        assert_eq!(label_color(Some("d73a4a")), Some(red));
        assert_eq!(label_color(Some("#D73A4A")), Some(red));
        assert_eq!(label_color(Some("fff")), None);
        assert_eq!(label_color(Some("zzzzzz")), None);
        assert_eq!(label_color(None), None);
    }
}
