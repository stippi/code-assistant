//! One pull request: the header, the tab row and the shown tab's body.
//! What the page knows lives in [`PullRequestData`], which the header and
//! each tab body ([`SummaryTab`], [`TimelineTab`], [`CodeTab`]) observe;
//! the view passes their intents on and carries out the writes on the
//! pull request (actions, updates, refresh).

use super::PullRequestIntent;
use super::code::CodeTab;
use super::composer::Composer;
use super::data::PullRequestData;
use super::header::DetailHeader;
use super::summary::SummaryTab;
use super::tab_bar::{checks_slot, render_tab_row, timeline_slot};
use super::timeline::TimelineTab;
use github::PullRequestRef;
use github::model::{Action, ListEntry, PullRequestUpdate};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, button::Button, button::ButtonVariants};
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, Render, SharedString, Subscription, Window, div,
    prelude::*,
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
    data: Entity<PullRequestData>,
    tab: DetailTab,
    header: Entity<DetailHeader>,
    summary: Entity<SummaryTab>,
    timeline: Entity<TimelineTab>,
    code: Entity<CodeTab>,
    composer: Entity<Composer>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestIntent> for DetailView {}

impl DetailView {
    /// `seed` is the listing row it was opened from; `show_back` whether
    /// there is a list to go back to. Reads the pull request and polls its
    /// checks while shown.
    pub fn new(
        pr: PullRequestRef,
        seed: Option<ListEntry>,
        show_back: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let data = cx.new(|cx| PullRequestData::new(pr.clone(), cx));
        data.update(cx, |data, cx| {
            data.load(cx);
            data.set_polling(true, cx);
        });
        let header = cx.new(|_| DetailHeader::new(pr.clone(), seed, show_back));
        let summary = cx.new(|cx| SummaryTab::new(data.clone(), cx));
        let timeline = cx.new(|_| TimelineTab::new(data.clone()));
        let code = cx.new(|cx| CodeTab::new(data.clone(), cx));
        let composer = cx.new(|cx| Composer::new(data.clone(), cx));
        let subscriptions = vec![
            cx.observe(&data, |this, _, cx| this.sync_header(cx)),
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
            data,
            tab: DetailTab::default(),
            header,
            summary,
            timeline,
            code,
            composer,
            _subscriptions: subscriptions,
        }
    }

    pub fn pr(&self) -> &PullRequestRef {
        &self.pr
    }

    #[cfg(test)]
    pub fn data(&self) -> &Entity<PullRequestData> {
        &self.data
    }

    fn sync_header(&mut self, cx: &mut Context<Self>) {
        let (detail, checks_state, refreshing) = {
            let data = self.data.read(cx);
            (data.detail.clone(), data.checks_state(), data.refreshing)
        };
        self.header.update(cx, |header, cx| {
            header.set_data(detail, checks_state, refreshing, cx)
        });
        cx.notify();
    }

    /// Shown again (read anew) or hidden (the polling pauses).
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.data.update(cx, |data, cx| {
            if visible {
                data.load(cx);
            }
            data.set_polling(visible, cx);
        });
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

    /// Merge, close, … the pull request; the primary control waits meanwhile.
    pub fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        self.header
            .update(cx, |header, cx| header.set_pending(true, cx));
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service.pull_request_action(pr, action).await
        });
        cx.spawn(async move |this, cx| {
            let _ = write.await;
            this.update(cx, |this, cx| {
                this.header
                    .update(cx, |header, cx| header.set_pending(false, cx))
            })
            .ok();
        })
        .detach();
    }

    /// Rewrite the title, body or maintainer edits; a refused title is
    /// taken back.
    pub fn update(&mut self, update: PullRequestUpdate, cx: &mut Context<Self>) {
        let retitles = update.title.is_some();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service.pull_request_update(pr, update).await
        });
        cx.spawn(async move |this, cx| {
            if write.await.is_err() && retitles {
                this.update(cx, |this, cx| {
                    this.header
                        .update(cx, |header, cx| header.title_rejected(cx))
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        PullRequestData::refresh(&self.data, cx);
    }

    /// What the tab row shows on its right for the shown tab.
    fn tab_slot(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.tab {
            DetailTab::Summary => {
                let data = self.data.read(cx);
                let checks = data.current_checks().to_vec();
                let state = data.checks_state();
                checks_slot(&checks, state, cx.entity().downgrade(), cx)
            }
            DetailTab::Timeline => {
                let activity = self.data.read(cx).activity.clone();
                Some(timeline_slot(activity.as_deref(), &self.timeline, cx))
            }
            DetailTab::Code => None,
        }
    }

    /// The last failure, until it is dismissed or something succeeds.
    fn render_error(&self, error: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let danger = cx.theme().danger;
        div()
            .debug_selector(|| "pr-error".into())
            .flex_none()
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .bg(danger.opacity(0.08))
            .border_b_1()
            .border_color(danger.opacity(0.25))
            .text_xs()
            .text_color(danger)
            .child(div().flex_1().min_w_0().child(error))
            .child(
                Button::new("pr-error-dismiss")
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().path("icons/close.svg"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.data.update(cx, |data, cx| data.dismiss_error(cx))
                    })),
            )
            .into_any_element()
    }
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            DetailTab::Summary => self.summary.clone().into_any_element(),
            DetailTab::Timeline => self.timeline.clone().into_any_element(),
            DetailTab::Code => self.code.clone().into_any_element(),
        };
        let error = self.data.read(cx).error.clone();
        let slot = self.tab_slot(cx);
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(self.header.clone())
            .children(error.map(|error| self.render_error(error, cx)))
            .child(render_tab_row(self.tab, slot, window, cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(
                div()
                    .absolute()
                    .bottom_4()
                    .right_4()
                    .child(self.composer.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{detail, repo};
    use github::model::MergeMethod;
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::sync::Arc;

    /// A detail page for #290 as if read; no session service behind it,
    /// so every write fails.
    fn page(cx: &mut TestAppContext) -> (Entity<DetailView>, &mut VisualTestContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| DetailView::new(repo().pull_request(290), None, true, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let data = view.read_with(cx, |view, _| view.data().clone());
        data.update(cx, |data, cx| {
            data.detail = Some(Arc::new(detail(290)));
            cx.notify();
        });
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui_kit::test]
    fn a_failed_action_shows_and_frees_the_controls(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        assert!(cx.debug_bounds("pr-error").is_none());
        view.update(cx, |view, cx| {
            view.act(Action::Merge(MergeMethod::Merge), cx)
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-error").is_some());
        let header = view.read_with(cx, |view, _| view.header.clone());
        header.read_with(cx, |header, _| assert!(!header.pending()));
    }

    #[gpui_kit::test]
    fn a_refused_title_is_taken_back(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        let header = view.read_with(cx, |view, _| view.header.clone());
        header.update(cx, |header, cx| header.save_title("Renamed".into(), cx));
        cx.run_until_parked();
        // The header's intent reached the view as the panel would pass it.
        view.update(cx, |view, cx| {
            view.update(
                PullRequestUpdate {
                    title: Some("Renamed".into()),
                    ..PullRequestUpdate::default()
                },
                cx,
            )
        });
        cx.run_until_parked();
        header.read_with(cx, |header, _| {
            assert_eq!(header.shown_title().as_ref(), detail(290).title)
        });
    }
}
