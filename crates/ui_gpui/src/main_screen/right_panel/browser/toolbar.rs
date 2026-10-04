//! The browser view's toolbar: browser and tab strips, navigation, the
//! address and the switch between the agent's and the user's control.

use super::BrowserPanel;
use crate::Gpui;
use code_assistant_core::session::browsers::BrowserKey;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, Size};
use gpui_kit::{
    App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use web::UserInput;

impl BrowserPanel {
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let browser = self.shown_browser();
        let watched_tab = self.watching.as_ref().map(|(_, tab)| tab.as_str());
        let tab = browser.and_then(|b| b.tabs.iter().find(|t| Some(t.id.as_str()) == watched_tab));
        let in_control = self.user_in_control();

        let browser_chips = (self.browsers.len() > 1).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(self.browsers.iter().map(|entry| {
                    let key = entry.key.clone();
                    let selected = browser.is_some_and(|b| b.key == key);
                    chip(
                        SharedString::from(format!("browser-{}", browser_label(&key))),
                        browser_label(&key),
                        selected,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_browser(key.clone(), cx)))
                }))
        });
        let tab_chips = browser.filter(|b| b.tabs.len() > 1).map(|b| {
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(b.tabs.iter().map(|t| {
                    let id = t.id.clone();
                    let label = if t.title.is_empty() { &t.url } else { &t.title };
                    chip(
                        SharedString::from(format!("tab-{}", t.id)),
                        truncate(label, 28),
                        Some(t.id.as_str()) == watched_tab,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_tab(id.clone(), cx)))
                }))
        });

        let nav_button = |id: &'static str, icon: &'static str, tooltip: &'static str| {
            Button::new(id)
                .icon(
                    Icon::default()
                        .path(SharedString::from(icon))
                        .with_size(Size::XSmall),
                )
                .ghost()
                .xsmall()
                .tooltip(tooltip)
                .disabled(!in_control)
        };
        let address: gpui_kit::AnyElement = match &self.address {
            Some((state, _)) => Input::new(state)
                .with_size(Size::XSmall)
                .flex_1()
                .into_any_element(),
            None => div()
                .id("browser-address")
                .flex_1()
                .min_w_0()
                .px_2()
                .py_0p5()
                .rounded(px(4.))
                .bg(theme.muted)
                .text_xs()
                .text_color(theme.foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .when(in_control, |el| el.cursor_text())
                .on_click(cx.listener(|this, _, window, cx| this.edit_address(window, cx)))
                .child(tab.map(|t| t.url.clone()).unwrap_or_default())
                .into_any_element(),
        };
        let control = Button::new("browser-control")
            .label(if in_control {
                "Hand back"
            } else {
                "Take control"
            })
            .xsmall()
            .map(|b| if in_control { b.primary() } else { b.ghost() })
            .tooltip(if in_control {
                "Let the agent use the browser again"
            } else {
                "Use the browser yourself; the agent waits"
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.set_control(!in_control, window, cx)),
            );

        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2()
            .border_b_1()
            .border_color(border)
            .children(browser_chips)
            .children(tab_chips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .child(
                        nav_button("browser-back", "icons/arrow_left.svg", "Back").on_click(
                            cx.listener(|this, _, _, _| this.send(UserInput::History(-1))),
                        ),
                    )
                    .child(
                        nav_button("browser-forward", "icons/arrow_right.svg", "Forward").on_click(
                            cx.listener(|this, _, _, _| this.send(UserInput::History(1))),
                        ),
                    )
                    .child(
                        nav_button("browser-reload", "icons/rotate_ccw.svg", "Reload")
                            .on_click(cx.listener(|this, _, _, _| this.send(UserInput::Reload))),
                    )
                    .child(address)
                    .when(tab.is_some_and(|t| t.loading), |el| {
                        el.child(
                            Icon::default()
                                .path("icons/arrow_circle.svg")
                                .with_size(Size::XSmall)
                                .text_color(muted),
                        )
                    })
                    .child(control),
            )
    }

    /// Forward input to the watched tab (the core drops it unless the user
    /// has control).
    pub(super) fn send(&self, input: UserInput) {
        if let Some(view) = &self.input {
            view.send(input);
        }
    }

    fn set_control(&mut self, user: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session_id), Some(browser)) = (self.session_id.clone(), self.shown_browser())
        else {
            return;
        };
        let key = browser.key.clone();
        if user {
            window.focus(&self.focus_handle, cx);
        } else {
            self.address = None;
        }
        let Some(gpui) = cx.try_global::<Gpui>() else {
            return;
        };
        let Some(service) = gpui.session_service() else {
            return;
        };
        gpui.dispatch(async move {
            if let Err(e) = service.set_browser_control(session_id, key, user).await {
                tracing::warn!("Browser panel: cannot change control: {e:#}");
            }
        });
    }

    /// Turn the address into a field (while the user has control); Enter
    /// goes there.
    fn edit_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.user_in_control() || self.address.is_some() {
            return;
        }
        let url = self
            .shown_browser()
            .zip(self.watching.as_ref())
            .and_then(|(b, (_, tab))| b.tabs.iter().find(|t| t.id == *tab))
            .map(|t| t.url.clone())
            .unwrap_or_default();
        let state = cx.new(|cx| InputState::new(window, cx));
        state.update(cx, |state, cx| {
            state.set_value(url, window, cx);
            state.focus(window, cx);
        });
        let subscription = cx.subscribe_in(&state, window, |this, state, event, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => {
                    let url = state.read(cx).value().to_string();
                    this.send(UserInput::Navigate(with_scheme(url.trim())));
                    this.address = None;
                    window.focus(&this.focus_handle, cx);
                }
                InputEvent::Blur => this.address = None,
                _ => return,
            }
            cx.notify();
        });
        self.address = Some((state, subscription));
        cx.notify();
    }

    fn pick_browser(&mut self, key: BrowserKey, cx: &mut Context<Self>) {
        self.picked_browser = Some(key);
        self.picked_tab = None;
        self.sync_view(cx);
        cx.notify();
    }

    /// Watch tab `id`; picking the active tab follows the agent again.
    fn pick_tab(&mut self, id: String, cx: &mut Context<Self>) {
        let active = self
            .shown_browser()
            .and_then(|b| b.tabs.iter().find(|t| t.active))
            .is_some_and(|t| t.id == id);
        self.picked_tab = (!active).then_some(id);
        self.sync_view(cx);
        cx.notify();
    }
}

/// A small selectable label for the browser and tab strips.
fn chip(
    id: SharedString,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(px(4.))
        .text_xs()
        .cursor_pointer()
        .map(|el| {
            if selected {
                el.bg(theme.muted).text_color(theme.foreground)
            } else {
                el.text_color(theme.muted_foreground)
                    .hover(|s| s.bg(theme.muted))
            }
        })
        .child(label.into())
}

fn browser_label(key: &BrowserKey) -> String {
    match &key.sub_agent {
        None => key.profile.clone(),
        Some(_) => format!("sub-agent · {}", key.profile),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// What the user typed as an address, with `https://` when it has no scheme.
fn with_scheme(typed: &str) -> String {
    if typed.contains("://") || typed.starts_with("about:") || typed.starts_with("data:") {
        typed.to_string()
    } else {
        format!("https://{typed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_addresses_get_a_scheme() {
        assert_eq!(with_scheme("example.com/a"), "https://example.com/a");
        assert_eq!(
            with_scheme("http://localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(with_scheme("about:blank"), "about:blank");
    }
}
