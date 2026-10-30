//! One pull request: the header, the tab row and the shown tab's body.
//! Each tab body is its own entity ([`SummaryTab`], [`TimelineTab`],
//! [`CodeTab`]); the view hands them the model values and passes their
//! intents on, as it does the header's.

use super::PullRequestIntent;
use super::actions::effective_checks_state;
use super::code::CodeTab;
use super::header::DetailHeader;
use super::summary::SummaryTab;
use super::tab_bar::{checks_slot, render_tab_row, timeline_slot};
use super::timeline::TimelineTab;
use github::PullRequestRef;
use github::model::{Activity, Detail, ListEntry};
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, Render, Subscription, Window, div, prelude::*,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Summary,
    Timeline,
    Code,
}

impl DetailTab {
    pub const ALL: &[(DetailTab, &'static str)] = &[
        (DetailTab::Summary, "Summary"),
        (DetailTab::Timeline, "Timeline"),
        (DetailTab::Code, "Code"),
    ];
}

pub struct DetailView {
    pr: PullRequestRef,
    detail: Option<Detail>,
    activity: Option<Activity>,
    tab: DetailTab,
    header: Entity<DetailHeader>,
    summary: Entity<SummaryTab>,
    timeline: Entity<TimelineTab>,
    code: Entity<CodeTab>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestIntent> for DetailView {}

impl DetailView {
    /// `seed` is the listing row it was opened from; `show_back` whether
    /// there is a list to go back to.
    pub fn new(
        pr: PullRequestRef,
        seed: Option<ListEntry>,
        show_back: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let header = cx.new(|_| DetailHeader::new(pr.clone(), seed, show_back));
        let summary = cx.new(|_| SummaryTab::new(pr.repo.clone()));
        let timeline = cx.new(|_| TimelineTab::new());
        let code = cx.new(|_| CodeTab);
        let subscriptions = vec![
            cx.subscribe(&header, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            cx.subscribe(&summary, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            cx.subscribe(&timeline, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            cx.subscribe(&code, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            // The tab row shows the timeline's order.
            cx.observe(&timeline, |_, _, cx| cx.notify()),
        ];
        Self {
            pr,
            detail: None,
            activity: None,
            tab: DetailTab::default(),
            header,
            summary,
            timeline,
            code,
            _subscriptions: subscriptions,
        }
    }

    pub fn pr(&self) -> &PullRequestRef {
        &self.pr
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "fed by the SessionService once wired")
    )]
    pub fn set_detail(&mut self, detail: Detail, cx: &mut Context<Self>) {
        self.summary
            .update(cx, |summary, cx| summary.set_detail(&detail, cx));
        self.header
            .update(cx, |header, cx| header.set_detail(detail.clone(), cx));
        self.detail = Some(detail);
        cx.notify();
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "fed by the SessionService once wired")
    )]
    pub fn set_activity(&mut self, activity: Activity, cx: &mut Context<Self>) {
        self.activity = Some(activity);
        cx.notify();
    }

    pub fn select_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn tab(&self) -> DetailTab {
        self.tab
    }

    /// What the tab row shows on its right for the shown tab.
    fn tab_slot(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.tab {
            DetailTab::Summary => {
                let detail = self.detail.as_ref()?;
                checks_slot(
                    &detail.checks,
                    effective_checks_state(detail),
                    cx.entity().downgrade(),
                    cx,
                )
            }
            DetailTab::Timeline => Some(timeline_slot(self.activity.as_ref(), &self.timeline, cx)),
            DetailTab::Code => None,
        }
    }
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            DetailTab::Summary => self.summary.clone().into_any_element(),
            DetailTab::Timeline => self.timeline.clone().into_any_element(),
            DetailTab::Code => self.code.clone().into_any_element(),
        };
        let slot = self.tab_slot(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.header.clone())
            .child(render_tab_row(self.tab, slot, window, cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
