//! The row of tabs above the panel, after T3 Code's `RightPanelTabs`: the
//! list, then a tab per open pull request ("#290" after its state glyph,
//! which a close button replaces under the pointer). A click shows a
//! tab, a middle click or the context menu closes (others, to the right,
//! all), a drag moves it. Too many to fit scroll sideways, with arrows.

use super::presentation::Tone;
use super::{OpenTab, PullRequestPanel};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, App, Context, IntoElement, MouseButton, Render, SharedString, Window, div, point,
    prelude::*, px, svg,
};

/// A tab being dragged.
#[derive(Clone)]
struct DraggedTab {
    index: usize,
    label: SharedString,
}

/// What follows the pointer while a tab is dragged.
struct DraggedTabView {
    label: SharedString,
}

impl Render for DraggedTabView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .bg(cx.theme().accent)
            .text_color(cx.theme().foreground)
            .shadow_md()
            .child(self.label.clone())
    }
}

/// How far the arrows scroll the strip, of its width.
const ARROW_STEP: f32 = 0.75;

impl PullRequestPanel {
    pub(super) fn render_tab_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let list_active = self.tabs.active().is_none();
        let list_tab = div()
            .id("pr-tab-list")
            .debug_selector(|| "pr-tab-list".into())
            .flex_none()
            .h(px(24.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .when(list_active, |d| {
                d.bg(theme.accent).text_color(theme.foreground)
            })
            .when(!list_active, |d| {
                d.text_color(muted).hover(|d| d.bg(theme.muted))
            })
            .child(
                svg()
                    .size(px(13.))
                    .path("icons/git_pull_request.svg")
                    .text_color(muted),
            )
            .child("Pull requests")
            .on_click(cx.listener(|this, _, _, cx| this.activate(None, cx)));
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, (number, tab))| self.render_tab(ix, number, tab, cx))
            .collect();
        let overflow = self.strip_scroll.max_offset().x > px(0.);
        let arrow =
            |id: &'static str, icon: &'static str, direction: f32, cx: &mut Context<Self>| {
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().path(icon))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let scroll = &this.strip_scroll;
                        let step = scroll.bounds().size.width * ARROW_STEP;
                        let offset = scroll.offset();
                        let max = scroll.max_offset().x;
                        let x = (offset.x + step * direction).clamp(-max, px(0.));
                        scroll.set_offset(point(x, offset.y));
                        cx.notify();
                    }))
            };
        div()
            .flex_none()
            .h(px(32.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(border)
            .child(list_tab)
            .when(overflow, |d| {
                d.child(arrow("pr-tabs-left", "icons/chevron_left.svg", 1., cx))
            })
            .child(
                div()
                    .id("pr-tabs")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_x_scroll()
                    .track_scroll(&self.strip_scroll)
                    .children(tabs),
            )
            .when(overflow, |d| {
                d.child(arrow("pr-tabs-right", "icons/chevron_right.svg", -1., cx))
            })
            .into_any_element()
    }

    fn render_tab(
        &self,
        ix: usize,
        number: u64,
        tab: &OpenTab,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let active = self.tabs.active() == Some(ix);
        let label: SharedString = format!("#{number}").into();
        let (icon, color) = match tab_tone(self, number, tab, cx) {
            Some(tone) => (tone.icon(), tone.color(cx)),
            None => (
                "icons/git_pull_request.svg",
                theme.muted_foreground.opacity(0.6),
            ),
        };
        let group: SharedString = format!("pr-tab-{number}").into();
        let line = theme.drag_border;
        let panel = cx.entity().downgrade();
        let close = Button::new(SharedString::from(format!("pr-tab-close-{number}")))
            .ghost()
            .xsmall()
            .icon(Icon::default().path("icons/close.svg"))
            .tooltip(format!("Close {label}"))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.close_tab(ix, cx)
            }));
        let glyph = div()
            .relative()
            .flex_none()
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .group_hover(group.clone(), |d| d.invisible())
                    .child(svg().size(px(13.)).path(icon).text_color(color)),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .debug_selector({
                        let selector = format!("pr-tab-close-{number}");
                        move || selector
                    })
                    .invisible()
                    .group_hover(group.clone(), |d| d.visible())
                    .child(close),
            );
        div()
            .id(group.clone())
            .group(group)
            .debug_selector(move || format!("pr-tab-{number}"))
            .flex_none()
            .max_w(px(144.))
            .h(px(24.))
            .pl_1()
            .pr_2()
            .flex()
            .items_center()
            .gap_0p5()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .border_l_2()
            .border_color(gpui_kit::transparent_black())
            .when(active, |d| d.bg(theme.accent).text_color(theme.foreground))
            .when(!active, |d| {
                d.text_color(theme.muted_foreground)
                    .hover(|d| d.bg(theme.muted))
            })
            .child(glyph)
            .child(div().min_w_0().truncate().child(label.clone()))
            .on_click(cx.listener(move |this, _, _, cx| this.activate(Some(ix), cx)))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| this.close_tab(ix, cx)),
            )
            .on_drag(
                DraggedTab {
                    index: ix,
                    label: label.clone(),
                },
                |dragged, _, _, cx| {
                    cx.new(|_| DraggedTabView {
                        label: dragged.label.clone(),
                    })
                },
            )
            .drag_over::<DraggedTab>(move |style, _, _, _| style.border_color(line))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                this.move_tab(dragged.index, ix, cx)
            }))
            .context_menu(move |menu, _, _| {
                let act =
                    |change: fn(&mut PullRequestPanel, usize, &mut Context<PullRequestPanel>)| {
                        let panel = panel.clone();
                        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                            panel.update(cx, |this, cx| change(this, ix, cx)).ok();
                        }
                    };
                menu.item(PopupMenuItem::new("Close").on_click(act(Self::close_tab)))
                    .item(PopupMenuItem::new("Close others").on_click(act(Self::close_others)))
                    .item(
                        PopupMenuItem::new("Close to the right")
                            .on_click(act(Self::close_to_the_right)),
                    )
                    .item(
                        PopupMenuItem::new("Close all")
                            .on_click(act(|this, _, cx| this.close_all(cx))),
                    )
            })
            .into_any_element()
    }
}

/// A tab's state glyph: from its page once read, else the list's row.
fn tab_tone(panel: &PullRequestPanel, number: u64, tab: &OpenTab, cx: &App) -> Option<Tone> {
    tab.view.read(cx).tone(cx).or_else(|| {
        let entry = panel.list.read(cx).entry(number)?;
        Some(Tone::of(entry.state, entry.is_draft))
    })
}
