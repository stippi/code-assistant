//! The Code tab: the changed files with their diffs and review threads,
//! per commit or for all of them.

use super::PullRequestIntent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{Context, EventEmitter, Render, Window, div, prelude::*};

pub struct CodeTab;

impl EventEmitter<PullRequestIntent> for CodeTab {}

impl Render for CodeTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .px_4()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("The changed files are not shown yet.")
    }
}
