//! The Summary tab (GitHub's Conversation). For now the description,
//! rendered as GitHub renders it; reviewers, labels, checks and the
//! comments follow.

use super::PullRequestIntent;
use super::markdown;
use github::RepoRef;
use github::model::Detail;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::text::TextView;
use gpui_kit::{Context, EventEmitter, Render, SharedString, Window, div, prelude::*};

pub struct SummaryTab {
    repo: RepoRef,
    /// The description, prepared for the Markdown view.
    body: Option<SharedString>,
}

impl EventEmitter<PullRequestIntent> for SummaryTab {}

impl SummaryTab {
    pub fn new(repo: RepoRef) -> Self {
        Self { repo, body: None }
    }

    pub fn set_detail(&mut self, detail: &Detail, cx: &mut Context<Self>) {
        let body = markdown::prepare(&detail.body, &self.repo);
        if self.body.as_deref() != Some(body.as_str()) {
            self.body = Some(body.into());
            cx.notify();
        }
    }
}

impl Render for SummaryTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        div()
            .id("pr-summary")
            .size_full()
            .overflow_y_scroll()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().text_color(muted).child("Description"))
            .child(match &self.body {
                Some(body) if !body.trim().is_empty() => {
                    TextView::markdown("pr-description", body.clone())
                        .selectable(true)
                        .into_any_element()
                }
                Some(_) => div()
                    .text_sm()
                    .text_color(muted)
                    .child("No description provided.")
                    .into_any_element(),
                None => div().into_any_element(),
            })
    }
}
