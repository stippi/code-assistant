//! The right sidebar's view switcher.
//!
//! For now the sidebar hosts a single view — [`review_view::ReviewView`] — but
//! it is structured as a switcher so additional views (e.g. an outline or a
//! terminal) can be added without reworking the [`crate::main_screen::MainScreen`]
//! shell that owns it.

pub mod review_view;

use gpui::{Context, Entity, FocusHandle, Focusable, Render, Window, div, prelude::*};
use review_view::ReviewView;

/// Which view the right panel is currently showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPanelView {
    Review,
}

impl RightPanelView {
    /// Stable string used for persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            RightPanelView::Review => "review",
        }
    }

    /// Parse a persisted string back into a view (defaults to `Review`).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(_s: &str) -> Self {
        // Only one view exists today; everything maps to Review.
        RightPanelView::Review
    }
}

pub struct RightPanel {
    active_view: RightPanelView,
    review_view: Entity<ReviewView>,
    focus_handle: FocusHandle,
}

impl RightPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let review_view = cx.new(|cx| ReviewView::new(window, cx));
        Self {
            active_view: RightPanelView::Review,
            review_view,
            focus_handle: cx.focus_handle(),
        }
    }

    #[allow(dead_code)]
    pub fn active_view(&self) -> RightPanelView {
        self.active_view
    }

    #[allow(dead_code)]
    pub fn set_active_view(&mut self, view: RightPanelView, cx: &mut Context<Self>) {
        if self.active_view != view {
            self.active_view = view;
            cx.notify();
        }
    }

    /// Point the active view(s) at a session.
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.review_view.update(cx, |v, cx| v.set_session(session_id, cx));
    }

    /// Re-request data for the active view.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.review_view.update(cx, |v, cx| v.reload(cx));
    }
}

impl Focusable for RightPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RightPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.active_view {
            RightPanelView::Review => self.review_view.clone().into_any_element(),
        };
        div().size_full().child(body)
    }
}
