//! Stacked pull requests, after T3 Code's `PullRequestStackMenu`: what a
//! stack offers for the shown layer (merging the stack through it,
//! rebasing it from its top), when that is held back, and the parts the
//! header's menu, the list's popover and the confirmation share: the
//! layers top first ("#290 · feat/x · Open"), the base below them.

use super::PullRequestIntent;
use super::actions::{allowed_merge_methods, can};
use super::header::DetailHeader;
use super::presentation::{Glyph, Tone};
use github::model::{Detail, PullRequestAction, PullRequestState, Stack, StackHead, StackLayer};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable};
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, SharedString, WeakEntity, Window, div, prelude::*, px,
    svg,
};
use std::sync::Arc;

/// The stack of the shown pull request as last read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StackView {
    /// It was read at least once.
    pub read: bool,
    pub stack: Option<Arc<Stack>>,
    /// The last read succeeded and nothing newer is on its way: actions
    /// may run on it.
    pub fresh: bool,
    pub error: Option<SharedString>,
}

impl StackView {
    /// Read, and in no stack.
    #[cfg(test)]
    pub fn known_alone() -> Self {
        Self {
            read: true,
            fresh: true,
            ..Self::default()
        }
    }

    /// T3 Code: the single pull request's merge controls wait until the
    /// stack is known not to exist.
    pub fn allows_single_merge(&self) -> bool {
        self.read && self.error.is_none() && self.stack.is_none()
    }

    /// "May be stale" / "Refreshing…" beside the stack's name.
    pub fn notice(&self) -> Option<&'static str> {
        self.stack.as_ref()?;
        match (&self.error, self.fresh) {
            (Some(_), _) => Some("Stack data may be stale. We couldn't refresh it."),
            (None, false) => Some("Refreshing stack… Showing saved data."),
            (None, true) => None,
        }
    }
}

/// Merging or rebasing a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackActionKind {
    Merge,
    Rebase,
}

/// What a stack offers for the layer `number`.
#[derive(Debug, PartialEq)]
pub struct StackPlan<'a> {
    /// 1-based, merged layers counted.
    pub position: usize,
    pub selected: &'a StackLayer,
    pub top: &'a StackLayer,
    /// The unmerged layers up to the selected one: what merging it merges.
    pub merge_layers: Vec<&'a StackLayer>,
    /// Every unmerged layer: what a rebase moves.
    pub unmerged: Vec<&'a StackLayer>,
}

impl<'a> StackPlan<'a> {
    pub fn of(stack: &'a Stack, number: u64) -> Option<Self> {
        let ix = stack.layers.iter().position(|l| l.number == number)?;
        let unmerged = |layers: &'a [StackLayer]| {
            layers
                .iter()
                .filter(|l| l.state != PullRequestState::Merged)
                .collect::<Vec<_>>()
        };
        Some(Self {
            position: ix + 1,
            selected: &stack.layers[ix],
            top: stack.layers.last()?,
            merge_layers: unmerged(&stack.layers[..=ix]),
            unmerged: unmerged(&stack.layers),
        })
    }

    pub fn merge_disabled(&self) -> bool {
        self.selected.state != PullRequestState::Open
            || self.merge_layers.is_empty()
            || self.merge_layers.iter().any(|l| {
                l.head_sha.is_none()
                    || l.state == PullRequestState::Closed
                    || l.is_draft == Some(true)
            })
    }

    pub fn rebase_disabled(&self) -> bool {
        self.unmerged.is_empty()
            || self
                .unmerged
                .iter()
                .any(|l| l.head_sha.is_none() || l.state != PullRequestState::Open)
    }

    /// "Every layer being merged must be open and ready for review."
    pub fn merge_hint(&self) -> bool {
        self.merge_layers
            .iter()
            .any(|l| l.state == PullRequestState::Closed || l.is_draft == Some(true))
    }

    /// The layer an action runs on and the heads it expects.
    pub fn target(&self, kind: StackActionKind) -> (u64, Vec<StackHead>) {
        let (target, layers) = match kind {
            StackActionKind::Merge => (self.selected, &self.merge_layers),
            StackActionKind::Rebase => (self.top, &self.unmerged),
        };
        let heads = layers
            .iter()
            .filter_map(|l| {
                Some(StackHead {
                    number: l.number,
                    head_sha: l.head_sha.clone()?,
                })
            })
            .collect();
        (target.number, heads)
    }

    /// The layers an action works on.
    pub fn layers(&self, kind: StackActionKind) -> &[&'a StackLayer] {
        match kind {
            StackActionKind::Merge => &self.merge_layers,
            StackActionKind::Rebase => &self.unmerged,
        }
    }
}

pub fn pull_requests(count: usize) -> String {
    match count {
        1 => "1 pull request".into(),
        n => format!("{n} pull requests"),
    }
}

/// A layer's state glyph: open, draft, closed, merged.
pub fn layer_glyph(layer: &StackLayer, cx: &App) -> Glyph {
    let tone = Tone::of(layer.state, layer.is_draft == Some(true));
    Glyph::new(tone.icon(), tone.color(cx), tone.label())
}

/// One layer: its glyph, its title (else branch) and "#n · branch · State"
/// (`compact` leaves the branch out); a check on the `current` one.
pub fn layer_row(layer: &StackLayer, current: bool, compact: bool, cx: &App) -> gpui_kit::Div {
    let theme = cx.theme();
    let glyph = layer_glyph(layer, cx);
    let title = layer
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| layer.head_branch.clone());
    let detail = match compact {
        true => format!("#{} · {}", layer.number, glyph.label),
        false => format!(
            "#{} · {} · {}",
            layer.number, layer.head_branch, glyph.label
        ),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_none().child(glyph.render(px(16.))))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.foreground)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(detail),
                ),
        )
        .when(current, |row| {
            row.child(
                svg()
                    .flex_none()
                    .size(px(14.))
                    .path("icons/check.svg")
                    .text_color(theme.foreground),
            )
        })
}

/// "↳ main" under the layers.
pub fn base_row(base: &str, cx: &App) -> AnyElement {
    div()
        .px_2()
        .pt_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(format!("↳ {base}"))
        .into_any_element()
}

/// The layers glyph and "2/3" of a stack trigger.
pub fn position_label(position: usize, total: usize) -> String {
    format!("{position}/{total}")
}

pub const LAYERS_ICON: &str = "icons/layers.svg";

#[cfg(test)]
mod tests {
    use super::*;
    use PullRequestState::*;

    fn layer(number: u64, state: PullRequestState, draft: bool) -> StackLayer {
        StackLayer {
            number,
            title: None,
            is_draft: Some(draft),
            head_sha: Some(format!("sha{number}")),
            head_branch: format!("b{number}"),
            state,
        }
    }

    fn stack(layers: Vec<StackLayer>) -> Stack {
        Stack {
            id: "1".into(),
            number: 3,
            url: String::new(),
            base: "main".into(),
            layers,
        }
    }

    #[test]
    fn merging_takes_the_unmerged_layers_up_to_the_shown_one() {
        let stack = stack(vec![
            layer(1, Merged, false),
            layer(2, Open, false),
            layer(3, Open, false),
            layer(4, Open, false),
        ]);
        let plan = StackPlan::of(&stack, 3).unwrap();
        assert_eq!(plan.position, 3);
        let numbers = |layers: &[&StackLayer]| layers.iter().map(|l| l.number).collect::<Vec<_>>();
        assert_eq!(numbers(&plan.merge_layers), [2, 3]);
        assert_eq!(numbers(&plan.unmerged), [2, 3, 4]);
        assert!(!plan.merge_disabled() && !plan.rebase_disabled());
        let (target, heads) = plan.target(StackActionKind::Merge);
        assert_eq!(target, 3);
        assert_eq!(heads.iter().map(|h| h.number).collect::<Vec<_>>(), [2, 3]);
        // A rebase runs from the top over every unmerged layer.
        let (target, heads) = plan.target(StackActionKind::Rebase);
        assert_eq!((target, heads.len()), (4, 3));
    }

    #[test]
    fn a_draft_or_closed_layer_holds_the_merge_back() {
        let drafted = stack(vec![layer(1, Open, true), layer(2, Open, false)]);
        let plan = StackPlan::of(&drafted, 2).unwrap();
        assert!(plan.merge_disabled() && plan.merge_hint());
        assert!(!plan.rebase_disabled());
        let closed = stack(vec![layer(1, Closed, false), layer(2, Open, false)]);
        let plan = StackPlan::of(&closed, 2).unwrap();
        assert!(plan.merge_disabled() && plan.rebase_disabled());
        assert!(StackPlan::of(&closed, 9).is_none());
    }

    #[test]
    fn a_stack_holds_single_merges_back_until_it_is_known() {
        let mut view = StackView::default();
        assert!(!view.allows_single_merge(), "not read yet");
        view.read = true;
        view.fresh = true;
        assert!(view.allows_single_merge());
        view.stack = Some(Arc::new(stack(Vec::new())));
        assert!(!view.allows_single_merge());
        assert_eq!(view.notice(), None);
        view.fresh = false;
        assert_eq!(view.notice(), Some("Refreshing stack… Showing saved data."));
        view.error = Some("rate limited".into());
        assert!(
            view.notice()
                .unwrap()
                .starts_with("Stack data may be stale")
        );
    }
}

/// What the header asks of the detail view about the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackRequest {
    /// Read the stack again.
    Retry,
    /// Ask to confirm merging or rebasing it.
    Confirm(StackActionKind),
}

/// The header's stack controls: the stack's menu ("⧉ 2/3") and "Merge
/// stack"; "Retry stack lookup" when reading it failed.
pub fn render_stack_controls(
    header: &DetailHeader,
    detail: &Detail,
    cx: &mut Context<DetailHeader>,
) -> Vec<AnyElement> {
    let view = header.stack().clone();
    let request = |request: StackRequest| {
        move |this: &mut DetailHeader,
              _: &ClickEvent,
              _: &mut Window,
              cx: &mut Context<DetailHeader>| { this.request_stack(request, cx) }
    };
    let Some(stack) = view.stack.clone() else {
        return match view.error {
            Some(_) => vec![
                Button::new("pr-stack-retry")
                    .ghost()
                    .xsmall()
                    .label("Retry stack lookup")
                    .on_click(cx.listener(request(StackRequest::Retry)))
                    .into_any_element(),
            ],
            None => Vec::new(),
        };
    };
    let Some(plan) = StackPlan::of(&stack, detail.number) else {
        return Vec::new();
    };
    let writer = can(detail, PullRequestAction::Merge);
    let can_merge = view.fresh && writer && !allowed_merge_methods(detail).is_empty();
    let can_rebase = view.fresh && writer;
    let (position, total) = (plan.position, stack.layers.len());
    let tooltip: SharedString = match view.notice() {
        Some(notice) => format!(
            "View stack #{}, layer {position} of {total} · {notice}",
            stack.number
        ),
        None => format!("View stack #{}, layer {position} of {total}", stack.number),
    }
    .into();
    let warning = view.error.is_some();
    let trigger = Button::new("pr-stack-trigger")
        .ghost()
        .xsmall()
        .icon(match warning {
            true => Icon::default()
                .path("icons/triangle_alert.svg")
                .text_color(cx.theme().warning),
            false => Icon::default().path(LAYERS_ICON),
        })
        .label(position_label(position, total))
        .tooltip(tooltip);
    let menu = {
        let weak = cx.entity().downgrade();
        let stack = stack.clone();
        let number = detail.number;
        let notice = view.notice();
        Popover::new("pr-stack")
            .anchor(gpui_kit::Anchor::TopRight)
            .trigger(trigger)
            .content(move |state, window, cx| {
                stack_menu(
                    &stack,
                    number,
                    StackMenuOptions {
                        notice,
                        retry: warning,
                        can_merge,
                        can_rebase,
                    },
                    &weak,
                    state,
                    window,
                    cx,
                )
            })
            .into_any_element()
    };
    let mut controls = vec![
        div()
            .debug_selector(|| "pr-stack".into())
            .child(menu)
            .into_any_element(),
    ];
    if can_merge && plan.selected.state == PullRequestState::Open {
        let count = plan.merge_layers.len();
        controls.push(
            div()
                .debug_selector(|| "pr-merge-stack".into())
                .child(
                    Button::new("pr-merge-stack")
                        .primary()
                        .xsmall()
                        .icon(Icon::default().path("icons/git_merge.svg"))
                        .label("Merge stack")
                        .disabled(plan.merge_disabled())
                        .tooltip(format!(
                            "Merge stack through #{} into {} ({})",
                            detail.number,
                            stack.base,
                            pull_requests(count)
                        ))
                        .on_click(
                            cx.listener(request(StackRequest::Confirm(StackActionKind::Merge))),
                        ),
                )
                .into_any_element(),
        );
    }
    controls
}

#[derive(Clone, Copy)]
struct StackMenuOptions {
    notice: Option<&'static str>,
    retry: bool,
    can_merge: bool,
    can_rebase: bool,
}

/// The stack's menu: "Stack #3" (stale or refreshing), the layers top
/// first (a click shows that pull request), the base, and the actions.
#[allow(clippy::too_many_arguments)]
fn stack_menu(
    stack: &Stack,
    number: u64,
    options: StackMenuOptions,
    header: &WeakEntity<DetailHeader>,
    _state: &mut PopoverState,
    _window: &mut Window,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, hover) = (theme.muted_foreground, theme.accent);
    let popover = cx.entity().downgrade();
    let send = |intent: Option<PullRequestIntent>, request: Option<StackRequest>| {
        let (header, popover) = (header.clone(), popover.clone());
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            popover
                .update(cx, |state, cx| state.dismiss(window, cx))
                .ok();
            header
                .update(cx, |header, cx| {
                    if let Some(intent) = intent.clone() {
                        header.emit(intent, cx);
                    }
                    if let Some(request) = request {
                        header.request_stack(request, cx);
                    }
                })
                .ok();
        }
    };
    let short = options
        .notice
        .map(|notice| match notice.starts_with("Stack data") {
            true => "May be stale",
            false => "Refreshing…",
        });
    let mut menu = div().w(px(320.)).flex().flex_col().gap_0p5().child(
        div()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .text_xs()
            .text_color(muted)
            .child(div().flex_1().child(format!("Stack #{}", stack.number)))
            .children(short),
    );
    if options.retry {
        menu = menu.child(
            div()
                .id("pr-stack-refresh")
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(|d| d.bg(hover))
                .child("Retry stack refresh")
                .on_click(send(None, Some(StackRequest::Retry))),
        );
    }
    let layers = div()
        .id("pr-stack-layers")
        .max_h(px(320.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .children(stack.layers.iter().rev().map(|layer| {
            let current = layer.number == number;
            div()
                .id(SharedString::from(format!(
                    "pr-stack-layer-{}",
                    layer.number
                )))
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(hover))
                .child(layer_row(layer, current, false, cx))
                .when(!current, |row| {
                    row.on_click(send(Some(PullRequestIntent::Open(layer.number)), None))
                })
        }));
    menu = menu.child(layers).child(base_row(&stack.base, cx));
    if (options.can_merge || options.can_rebase)
        && let Some(plan) = StackPlan::of(stack, number)
    {
        let action = |id: &'static str,
                      label: String,
                      icon: &'static str,
                      disabled: bool,
                      kind: StackActionKind| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .when(disabled, |d| d.opacity(0.5))
                .when(!disabled, |d| {
                    d.cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .on_click(send(None, Some(StackRequest::Confirm(kind))))
                })
                .child(svg().size(px(14.)).path(icon).text_color(muted))
                .child(label)
        };
        menu = menu.child(div().my_1().h(px(1.)).bg(theme.border));
        if options.can_merge {
            menu = menu.child(action(
                "pr-stack-merge",
                format!("Merge stack ({})", plan.merge_layers.len()),
                "icons/git_merge.svg",
                plan.merge_disabled(),
                StackActionKind::Merge,
            ));
        }
        if options.can_rebase {
            menu = menu.child(action(
                "pr-stack-rebase",
                "Rebase stack".into(),
                "icons/refresh_cw.svg",
                plan.rebase_disabled(),
                StackActionKind::Rebase,
            ));
        }
        if plan.merge_hint() {
            menu = menu.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted)
                    .child("Every layer being merged must be open and ready for review."),
            );
        }
    }
    menu.into_any_element()
}

/// The confirmation's title and description, in T3 Code's words.
pub fn confirmation(
    kind: StackActionKind,
    plan: &StackPlan,
    base: &str,
    method: &str,
    host: &str,
) -> (String, String) {
    let count = plan.layers(kind).len();
    match kind {
        StackActionKind::Merge => (
            format!("Merge {}?", pull_requests(count)),
            format!(
                "Merge #{} and its unmerged layers below into {base} using {method}. {host} \
                 checks their rules before merging or queueing them and rebases the remaining \
                 stack after merging.",
                plan.selected.number
            ),
        ),
        StackActionKind::Rebase => (
            format!("Rebase {}?", pull_requests(count)),
            format!(
                "Rebase the remote branches from bottom to top onto {base}. This rewrites \
                 branch history and may restart checks. If a layer fails, earlier updates \
                 remain."
            ),
        ),
    }
}
