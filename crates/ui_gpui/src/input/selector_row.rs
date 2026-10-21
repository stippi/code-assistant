//! The selector row below the composer: model | effort | worktree |
//! sandbox | permissions | MCP | context ring.
//!
//! When the row is too narrow for all of them (a wide right panel), the
//! secondary selectors collapse behind a "⋯" button and only the model
//! and effort selectors stay inline. The row measures itself after layout; a change
//! of fit re-renders on the next frame.

use super::InputArea;
use crate::shared::context_breakdown::ContextBreakdown;
use crate::shared::context_indicator::ContextIndicator;
use gpui_kit::component::{ActiveTheme, Icon, ThemeStyled};
use gpui_kit::{
    Anchor, AnyElement, Context, Edges, Pixels, SharedString, anchored, canvas, deferred, div,
    prelude::*, px,
};

/// Room kept for the model selector before the other selectors collapse.
const MODEL_SELECTOR_MIN_WIDTH: Pixels = px(200.);
/// Room for the "⋯" button, the context ring and the gaps between them.
const SELECTOR_ROW_CHROME_WIDTH: Pixels = px(48.);
/// Assumed width of the secondary selectors until they were measured once.
pub(super) const DEFAULT_SECONDARY_SELECTORS_WIDTH: Pixels = px(560.);

impl InputArea {
    pub(super) fn render_selector_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.selectors_collapsed;

        // Measures the space the row gets (independent of its content, which
        // may overflow) and flips `selectors_collapsed` when that changes the fit.
        let fit_probe = {
            let secondary_width = self.secondary_selectors_width.clone();
            let this = cx.entity().downgrade();
            canvas(
                move |bounds, _, cx| {
                    let fits = bounds.size.width
                        >= MODEL_SELECTOR_MIN_WIDTH
                            + secondary_width.get()
                            + SELECTOR_ROW_CHROME_WIDTH;
                    if fits == collapsed {
                        cx.defer(move |cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.selectors_collapsed = !fits;
                                this.selectors_menu_open = false;
                                cx.notify();
                            });
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };

        div()
            .relative()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(fit_probe)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.model_selector.clone())
                    .child(self.effort_selector.clone()),
            )
            .map(|row| {
                if collapsed {
                    row.child(self.render_selectors_menu(cx))
                } else {
                    row.child(self.render_inline_secondary_selectors())
                }
            })
            .child(self.render_context_indicator(cx))
    }

    /// Worktree, sandbox, permissions and MCP selectors, each in a
    /// non-shrinking wrapper.
    fn secondary_selectors(&self) -> [AnyElement; 4] {
        [
            div()
                .flex_none()
                .flex()
                .child(self.worktree_selector.clone())
                .into_any_element(),
            div()
                .flex_none()
                .flex()
                .child(self.sandbox_selector.clone())
                .into_any_element(),
            div()
                .flex_none()
                .flex()
                .child(self.permission_selector.clone())
                .into_any_element(),
            div()
                .flex_none()
                .flex()
                .child(self.mcp_selector.clone())
                .into_any_element(),
        ]
    }

    fn render_inline_secondary_selectors(&self) -> impl IntoElement {
        let secondary_width = self.secondary_selectors_width.clone();
        div()
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(
                // Remembers the natural width, which decides the fit while
                // the selectors are collapsed.
                canvas(
                    move |bounds, _, _| secondary_width.set(bounds.size.width),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(self.secondary_selectors())
    }

    fn render_selectors_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_open = self.selectors_menu_open;
        div()
            .flex_none()
            .flex()
            .child(
                div()
                    .id("selectors-menu-btn")
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_open, |el| el.bg(cx.theme().muted))
                    .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                    .child(
                        Icon::default()
                            .path(SharedString::from("icons/ellipsis.svg"))
                            .text_color(cx.theme().muted_foreground)
                            .size(px(14.)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selectors_menu_open = !this.selectors_menu_open;
                        cx.notify();
                    })),
            )
            .when(is_open, |el| {
                // No outside-click dismissal: the selectors' own dropdowns
                // paint outside this panel, and a click in them would close
                // it before the choice lands. Picking a value closes it.
                el.child(deferred(
                    anchored()
                        .anchor(Anchor::BottomRight)
                        .snap_to_window_with_margin(Edges::all(px(8.)))
                        .child(
                            div()
                                .id("selectors-menu")
                                .occlude()
                                .popover_style(cx)
                                .shadow_md()
                                .p_2()
                                .mb_1()
                                .flex()
                                .flex_col()
                                // Same width for all, so their chevrons line up.
                                .items_stretch()
                                .gap_1()
                                .children(self.secondary_selectors()),
                        ),
                ))
            })
    }

    fn render_context_indicator(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let usage = self.context_usage;
        let total_usage = self.context_total_usage;
        let ratio = usage.and_then(|u| u.context_ratio()).unwrap_or(0.0);
        let progress_color = if ratio >= 0.8 {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        };
        let scale = cx.theme().font_size / px(16.0);
        div()
            .id("context-indicator")
            .flex_none()
            .flex()
            .items_center()
            .ml_1()
            .when_some(usage, |el, usage| {
                el.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::element(move |_window, _cx| {
                        ContextBreakdown::new(usage).session_total(total_usage)
                    })
                    .build(window, cx)
                })
            })
            .child(
                ContextIndicator::new(ratio)
                    .size(px(16.0 * scale))
                    .stroke_width(px(2.5 * scale))
                    .bg_color(cx.theme().muted_foreground.opacity(0.25))
                    .progress_color(progress_color),
            )
    }
}
