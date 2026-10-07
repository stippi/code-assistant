//! The right sidebar's view switcher.
//!
//! The sidebar hosts the [`review_view::ReviewView`] and, with the
//! `browser-panel` feature, the agent's browser ([`browser::BrowserPanel`]),
//! switched in a header above them. The shown view is remembered per session.

#[cfg(feature = "browser-panel")]
mod browser;
mod review_rows;
pub mod review_view;

use gpui_kit::{Context, Entity, FocusHandle, Focusable, Render, Window, div, prelude::*};
use review_view::ReviewView;

/// Which view the right panel is currently showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPanelView {
    Review,
    #[cfg(feature = "browser-panel")]
    Browser,
}

impl RightPanelView {
    /// Stable string used for persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            RightPanelView::Review => "review",
            #[cfg(feature = "browser-panel")]
            RightPanelView::Browser => "browser",
        }
    }

    /// Parse a persisted string back into a view (defaults to `Review`).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            #[cfg(feature = "browser-panel")]
            "browser" => RightPanelView::Browser,
            _ => RightPanelView::Review,
        }
    }
}

pub struct RightPanel {
    active_view: RightPanelView,
    review_view: Entity<ReviewView>,
    #[cfg(feature = "browser-panel")]
    browser: Entity<browser::BrowserPanel>,
    #[cfg(feature = "browser-panel")]
    session_id: Option<String>,
    #[cfg(feature = "browser-panel")]
    _browser_events: gpui_kit::Subscription,
    focus_handle: FocusHandle,
}

impl RightPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let review_view = cx.new(|cx| ReviewView::new(window, cx));
        #[cfg(feature = "browser-panel")]
        let browser = cx.new(browser::BrowserPanel::new);
        Self {
            active_view: RightPanelView::Review,
            review_view,
            #[cfg(feature = "browser-panel")]
            _browser_events: cx.subscribe(&browser, |this, _, event, cx| match event {
                // The agent started browsing: show it.
                browser::BrowserPanelEvent::BrowserOpened => {
                    this.set_active_view(RightPanelView::Browser, cx)
                }
            }),
            #[cfg(feature = "browser-panel")]
            browser,
            #[cfg(feature = "browser-panel")]
            session_id: None,
            focus_handle: cx.focus_handle(),
        }
    }

    #[allow(dead_code)]
    pub fn active_view(&self) -> RightPanelView {
        self.active_view
    }

    pub fn set_active_view(&mut self, view: RightPanelView, cx: &mut Context<Self>) {
        if self.active_view == view {
            return;
        }
        self.active_view = view;
        #[cfg(feature = "browser-panel")]
        if let Some(session_id) = &self.session_id {
            crate::shared::ui_state::update(cx, |store| {
                store.set_right_panel_view(session_id, view.as_str())
            });
        }
        cx.notify();
    }

    /// Point the active view(s) at a session.
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        #[cfg(feature = "browser-panel")]
        {
            self.active_view = session_id
                .as_deref()
                .and_then(|id| {
                    crate::shared::ui_state::read(cx, |store| store.get_right_panel_view(id))
                        .flatten()
                })
                .map_or(RightPanelView::Review, |view| {
                    RightPanelView::from_str(&view)
                });
            self.session_id = session_id.clone();
            self.browser.update(cx, |browser, cx| {
                browser.set_session(session_id.clone(), cx)
            });
            cx.notify();
        }
        self.review_view
            .update(cx, |v, cx| v.set_session(session_id, cx));
    }

    /// Re-request data for the active view.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.review_view.update(cx, |v, cx| v.reload(cx));
    }

    #[cfg(feature = "browser-panel")]
    fn render_header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme;
        let panel = cx.entity().downgrade();
        div()
            .flex_none()
            .flex()
            .items_center()
            .px_2()
            .py(gpui_kit::px(8.))
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(crate::shared::segmented_switch::segmented_switch(
                "right-panel-view",
                &[
                    (RightPanelView::Review, "Review"),
                    (RightPanelView::Browser, "Browser"),
                ],
                self.active_view,
                move |view, _, cx| {
                    panel
                        .update(cx, |panel, cx| panel.set_active_view(view, cx))
                        .ok();
                },
                window,
                cx,
            ))
    }
}

impl Focusable for RightPanel {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RightPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.active_view {
            RightPanelView::Review => self.review_view.clone().into_any_element(),
            #[cfg(feature = "browser-panel")]
            RightPanelView::Browser => self.browser.clone().into_any_element(),
        };
        #[cfg(feature = "browser-panel")]
        let panel = div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_header(_window, _cx))
            .child(div().flex_1().min_h_0().child(body));
        #[cfg(not(feature = "browser-panel"))]
        let panel = div().size_full().child(body);
        panel
    }
}
