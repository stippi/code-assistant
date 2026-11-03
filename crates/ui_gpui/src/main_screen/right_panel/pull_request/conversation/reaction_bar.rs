//! The reactions under a comment: a pill per emoji with its count (the
//! viewer's own highlighted, a tooltip naming who reacted) and 😊+ to pick
//! one of GitHub's eight. The owner keeps the optimistic overlay
//! ([`PendingReactions`](super::reactions::PendingReactions)) and writes.

use super::reactions::{emoji, name, tooltip};
use github::model::{Reaction, ReactionContent};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Selectable};
use gpui_kit::{
    AnyElement, App, Context, ElementId, Pixels, SharedString, Window, div, prelude::*, px, svg,
};

const PILL_HEIGHT: Pixels = px(22.);

/// What a click asks of the owner `V`: react with the content (`true`) or
/// take the reaction back.
pub type ToggleReaction<V> = fn(&mut V, ReactionContent, bool, &mut Context<V>);

/// The bar for `shown` (the host's reactions with the overlay applied);
/// `None` when there is nothing to show or do.
pub fn reaction_bar<V: 'static>(
    id: &str,
    shown: &[Reaction],
    can_react: bool,
    toggle: ToggleReaction<V>,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    if shown.is_empty() && !can_react {
        return None;
    }
    let theme = cx.theme();
    let (primary, border, muted_bg, muted) = (
        theme.primary,
        theme.border,
        theme.muted,
        theme.muted_foreground,
    );
    let pills = shown.iter().map(|reaction| {
        let content = reaction.content;
        let mine = reaction.viewer_has_reacted;
        let tip: SharedString = tooltip(reaction).into();
        div()
            .id(SharedString::from(format!("{id}-reaction-{content:?}")))
            .debug_selector({
                let selector = format!("{id}-reaction-{content:?}");
                move || selector.clone()
            })
            .flex_none()
            .h(PILL_HEIGHT)
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded_full()
            .border_1()
            .text_xs()
            .map(|pill| {
                if mine {
                    pill.border_color(primary.opacity(0.6))
                        .bg(primary.opacity(0.1))
                        .text_color(theme.foreground)
                } else {
                    pill.border_color(border.opacity(0.7))
                        .bg(muted_bg.opacity(0.4))
                        .text_color(muted)
                }
            })
            .child(emoji(content))
            .child(reaction.count.to_string())
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .when(can_react, |pill| {
                pill.cursor_pointer()
                    .hover(|s| s.border_color(primary.opacity(0.6)))
                    .on_click(cx.listener(move |owner, _, _, cx| toggle(owner, content, !mine, cx)))
            })
            .into_any_element()
    });
    let reacted: Vec<ReactionContent> = shown
        .iter()
        .filter(|r| r.viewer_has_reacted)
        .map(|r| r.content)
        .collect();
    let picker = can_react.then(|| {
        let owner = cx.entity().downgrade();
        Popover::new(SharedString::from(format!("{id}-reaction-picker")))
            .anchor(gpui_kit::Anchor::BottomLeft)
            .trigger(PickerTrigger {
                id: SharedString::from(format!("{id}-react")).into(),
                selected: false,
            })
            .content(move |_, _, cx| {
                let popover = cx.entity().downgrade();
                div().flex().items_center().gap_0p5().children(
                    ReactionContent::ALL.into_iter().map(|content| {
                        let owner = owner.clone();
                        let popover = popover.clone();
                        let already = reacted.contains(&content);
                        div()
                            .id(SharedString::from(format!("pick-{content:?}")))
                            .size_7()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_base()
                            .when(already, |el| el.bg(primary.opacity(0.1)))
                            .hover(|s| s.bg(muted_bg))
                            .tooltip(move |window, cx| {
                                Tooltip::new(name(content)).build(window, cx)
                            })
                            .child(emoji(content))
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |p, cx| p.dismiss(window, cx)).ok();
                                owner
                                    .update(cx, |owner, cx| toggle(owner, content, !already, cx))
                                    .ok();
                            })
                    }),
                )
            })
            .into_any_element()
    });
    Some(
        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children(pills)
            .children(picker)
            .into_any_element(),
    )
}

/// The 😊+ pill that opens the picker.
#[derive(IntoElement)]
struct PickerTrigger {
    id: ElementId,
    selected: bool,
}

impl Selectable for PickerTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for PickerTrigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = if self.selected {
            theme.foreground
        } else {
            theme.muted_foreground
        };
        div()
            .id(self.id)
            .flex_none()
            .h(PILL_HEIGHT)
            .px_1p5()
            .flex()
            .items_center()
            .rounded_full()
            .border_1()
            .border_color(theme.border.opacity(0.7))
            .cursor_pointer()
            .hover(|s| s.border_color(theme.primary.opacity(0.6)))
            .tooltip(|window, cx| Tooltip::new("Add a reaction").build(window, cx))
            .child(
                svg()
                    .size(px(14.))
                    .path("icons/smile_plus.svg")
                    .text_color(color),
            )
    }
}
