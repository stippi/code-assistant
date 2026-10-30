//! The Timeline tab: the pull request's history as a rail (opened,
//! commits, comments, reviews, merged or closed). Its order is the one
//! setting the tab row toggles; the rail follows.

use super::PullRequestIntent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{Context, EventEmitter, Render, Window, div, prelude::*};

pub struct TimelineTab {
    newest_first: bool,
}

impl EventEmitter<PullRequestIntent> for TimelineTab {}

impl TimelineTab {
    pub fn new() -> Self {
        Self { newest_first: true }
    }

    pub fn newest_first(&self) -> bool {
        self.newest_first
    }

    pub fn toggle_order(&mut self, cx: &mut Context<Self>) {
        self.newest_first = !self.newest_first;
        cx.notify();
    }
}

impl Render for TimelineTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .px_4()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("The timeline is not shown yet.")
    }
}
