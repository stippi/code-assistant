//! The "Pull request" view of the right panel (`docs/pull-request-view.md`):
//! the pull requests of the session's repository, and a tab for each one
//! opened from there ([`tabs`]), the session's own first.
//!
//! The views ([`list::PullRequestList`], [`detail::DetailView`] with its
//! header and tab bodies) render model values from the `github` crate and
//! say what the user asks for as [`PullRequestIntent`]s. The panel carries
//! them out: navigation itself, copying and opening links, reading and
//! writing GitHub through the `SessionService` (the detail's writes through
//! its [`data::PullRequestData`]). The core's announcements arrive on the
//! [`bus`]; the list is read again when its repository's lists changed.

mod actions;
mod avatar;
pub mod bus;
mod code;
mod composer;
mod conversation;
pub mod data;
mod detail;
#[cfg(test)]
mod fixtures;
mod header;
mod list;
mod list_logic;
mod list_overrides;
mod markdown;
pub mod preferences;
mod presentation;
pub mod review_draft;
mod stack;
mod summary;
mod tab_bar;
mod tab_strip;
mod tabs;
mod timeline;

use crate::shared::ui_state::{PullRequestTabState, PullRequestTabsState};
use bus::{PullRequestEvent, bus};
use code_assistant_core::pull_requests::{CheckoutPlace, PullRequestCheckout};
use data::session_service;
use detail::{DetailTab, DetailTabChanged, DetailView};
use github::RepoRef;
use github::model::{Action, ListQuery, PullRequestUpdate};
use gpui_kit::{
    ClipboardItem, Context, Entity, Render, ScrollHandle, Subscription, Task, Window, div,
    prelude::*,
};
use list::PullRequestList;
use tabs::Tabs;

/// How often relative times ("9h ago") are drawn anew.
const CLOCK_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// What the user asks of the pull request view.
#[derive(Clone, Debug, PartialEq)]
pub enum PullRequestIntent {
    /// Show a pull request of the listed repository in its tab.
    Open(u64),
    /// List the repository's pull requests anew.
    List(ListQuery),
    /// Read the next page of the listing.
    ListMore(ListQuery),
    /// Read the line counts of these listed pull requests.
    ListStats(Vec<u64>),
    /// Act on a listed pull request (a quick action); the list shows the
    /// outcome already and takes it back (`token`) if GitHub refuses.
    ActOn {
        number: u64,
        action: Action,
        token: u64,
    },
    /// Read what is shown again, past the cache.
    Refresh,
    /// Merge, close, reopen, draft or ready, auto-merge, update or delete
    /// the branch of the shown pull request.
    Act(Action),
    /// Rewrite the shown pull request's title, body or maintainer edits.
    Update(PullRequestUpdate),
    OpenUrl(String),
    Copy(String),
    /// Check the shown pull request out, here or in a worktree.
    CheckOut(CheckoutPlace),
}

pub struct PullRequestPanel {
    session_id: Option<String>,
    /// The tab is shown (the detail polls only then).
    visible: bool,
    repo: Option<RepoRef>,
    list: Entity<PullRequestList>,
    /// A page per opened pull request; the shown one, or the list.
    tabs: Tabs<OpenTab>,
    strip_scroll: ScrollHandle,
    /// Finding the session's repository and pull request.
    resolve: Option<Task<()>>,
    list_read: Option<Task<()>>,
    more_read: Option<Task<()>>,
    viewer_read: Option<Task<()>>,
    /// Redraws once a minute while shown, so "9h ago" stays true.
    _clock: Option<Task<()>>,
    _list_subscription: Subscription,
    _bus: Subscription,
}

/// An open pull request's page and what the panel hears from it.
pub(super) struct OpenTab {
    view: Entity<DetailView>,
    _subscriptions: [Subscription; 2],
}

impl PullRequestPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| PullRequestList::new(window, cx));
        let list_subscription = cx.subscribe(&list, |this, _, intent: &PullRequestIntent, cx| {
            this.handle(intent, cx)
        });
        let events = bus(cx);
        let bus = cx.subscribe(&events, |this, _, event: &PullRequestEvent, cx| {
            if let PullRequestEvent::ListChanged { repo } = event
                && this.visible
                && this.repo.as_ref() == Some(repo)
            {
                this.load_list(cx);
            }
        });
        Self {
            session_id: None,
            visible: false,
            repo: None,
            list,
            tabs: Tabs::default(),
            strip_scroll: ScrollHandle::new(),
            resolve: None,
            list_read: None,
            more_read: None,
            viewer_read: None,
            _clock: None,
            _list_subscription: list_subscription,
            _bus: bus,
        }
    }

    /// Follow `session_id` while the tab is shown (`None` while hidden,
    /// which keeps what is shown for its return).
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        let visible = session_id.is_some();
        if visible != self.visible {
            self.visible = visible;
            self.sync_visibility(cx);
            self._clock = visible.then(|| self.start_clock(cx));
        }
        let Some(session_id) = session_id else {
            return;
        };
        if self.session_id.as_deref() == Some(session_id.as_str()) {
            // Back from hiding: the listing may have changed meanwhile.
            self.load_list(cx);
            return;
        }
        self.session_id = Some(session_id.clone());
        self.set_repository(None, None, cx);
        self.resolve_repository(session_id, cx);
    }

    fn start_clock(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let ticked = this.update(cx, |this, cx| {
                    this.list.update(cx, |_, cx| cx.notify());
                    if let Some(detail) = this.detail() {
                        detail.update(cx, |_, cx| cx.notify());
                    }
                    cx.notify();
                });
                if ticked.is_err() {
                    return;
                }
            }
        })
    }

    /// Ask the core where the session's pull requests live.
    fn resolve_repository(&mut self, session_id: String, cx: &mut Context<Self>) {
        let Some(service) = session_service(cx) else {
            return;
        };
        self.list
            .update(cx, |list, cx| list.set_resolving(true, cx));
        self.resolve = Some(cx.spawn(async move |this, cx| {
            let found = service.session_pull_requests(session_id.clone()).await;
            this.update(cx, |this, cx| {
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                this.list
                    .update(cx, |list, cx| list.set_resolving(false, cx));
                match found {
                    Ok(Some(found)) => this.set_repository(Some(found.repo), found.own, cx),
                    Ok(None) => {}
                    Err(error) => this.list.update(cx, |list, cx| {
                        list.set_error(Some(format!("{error:#}")), cx)
                    }),
                }
            })
            .ok();
        }));
    }

    /// Where the session's pull requests live and which one is its own.
    /// The tabs the session had open come back; without any, its own pull
    /// request opens.
    pub fn set_repository(
        &mut self,
        repo: Option<RepoRef>,
        own: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        self.repo = repo.clone();
        self.tabs.close_all();
        self.list.update(cx, |list, cx| list.set_repo(repo, cx));
        self.load_list(cx);
        self.read_viewer(cx);
        let remembered = match (&self.session_id, &self.repo) {
            (Some(session_id), Some(_)) => {
                crate::shared::ui_state::read(cx, |store| store.get_pull_request_tabs(session_id))
                    .unwrap_or_default()
            }
            _ => Default::default(),
        };
        if remembered.open.is_empty() {
            if let Some(number) = own {
                self.open(number, cx);
            }
        } else {
            for open in &remembered.open {
                let tab = open.tab.as_deref().and_then(DetailTab::parse);
                self.open_tab(open.number, tab, cx);
            }
            let active = remembered.active.and_then(|n| self.tabs.position(n));
            self.tabs.activate(active);
            self.sync_visibility(cx);
        }
        cx.notify();
    }

    /// Read the listing the list asks for: the held one at once, a newer
    /// one announced by the core.
    fn load_list(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(service)) = (self.repo.clone(), session_service(cx)) else {
            return;
        };
        let query = self.list.read(cx).query();
        self.list_read = Some(cx.spawn(async move |this, cx| {
            let page = service.pull_request_list(repo.clone(), query).await;
            this.update(cx, |this, cx| {
                if this.repo.as_ref() != Some(&repo) {
                    return;
                }
                this.list.update(cx, |list, cx| match page {
                    Ok(fetched) => {
                        list.set_page(fetched.value, cx);
                        list.set_refreshing(fetched.refreshing, cx);
                    }
                    Err(error) => {
                        list.set_error(Some(format!("{error:#}")), cx);
                        list.set_refreshing(false, cx);
                    }
                })
            })
            .ok();
        }));
    }

    /// A quick action on a listed pull request.
    fn act_on(&mut self, number: u64, action: Action, token: u64, cx: &mut Context<Self>) {
        let (Some(repo), Some(service)) = (self.repo.clone(), session_service(cx)) else {
            self.list
                .update(cx, |list, cx| list.revert(number, token, cx));
            return;
        };
        let list = self.list.downgrade();
        cx.spawn(async move |_, cx| {
            let acted = service
                .pull_request_action(repo.pull_request(number), action)
                .await;
            if let Err(error) = acted {
                list.update(cx, |list, cx| {
                    list.revert(number, token, cx);
                    list.set_error(Some(format!("#{number}: {error:#}")), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Take the list's override back when `done` says the action failed.
    fn revert_unless(
        &mut self,
        done: Task<bool>,
        number: u64,
        token: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = token else {
            done.detach();
            return;
        };
        let list = self.list.downgrade();
        cx.spawn(async move |_, cx| {
            if !done.await {
                list.update(cx, |list, cx| list.revert(number, token, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// The next page of the listing.
    fn load_more(&mut self, query: ListQuery, cx: &mut Context<Self>) {
        let (Some(repo), Some(service)) = (self.repo.clone(), session_service(cx)) else {
            return;
        };
        let list = self.list.downgrade();
        self.more_read = Some(cx.spawn(async move |_, cx| {
            let page = service.pull_request_list(repo, query).await;
            list.update(cx, |list, cx| match page {
                Ok(fetched) => list.append_page(fetched.value, cx),
                Err(error) => list.more_failed(format!("{error:#}"), cx),
            })
            .ok();
        }));
    }

    /// Line counts of listed pull requests that came without them.
    fn load_stats(&mut self, numbers: Vec<u64>, cx: &mut Context<Self>) {
        let (Some(repo), Some(service)) = (self.repo.clone(), session_service(cx)) else {
            return;
        };
        let prs = numbers.iter().map(|n| repo.pull_request(*n)).collect();
        let list = self.list.downgrade();
        cx.spawn(
            async move |_, cx| match service.pull_request_list_stats(prs).await {
                Ok(stats) => {
                    list.update(cx, |list, cx| list.set_stats(&stats, cx)).ok();
                }
                Err(error) => tracing::debug!("no line counts: {error:#}"),
            },
        )
        .detach();
    }

    /// Drop the repository's cached lists and read the listing anew.
    fn refresh_list(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(service)) = (self.repo.clone(), session_service(cx)) else {
            return;
        };
        self.list
            .update(cx, |list, cx| list.set_refreshing(true, cx));
        self.list_read = Some(cx.spawn(async move |this, cx| {
            let refreshed = service.pull_request_list_refresh(repo).await;
            this.update(cx, |this, cx| match refreshed {
                Ok(()) => this.load_list(cx),
                Err(error) => this.list.update(cx, |list, cx| {
                    list.set_error(Some(format!("{error:#}")), cx);
                    list.set_refreshing(false, cx);
                }),
            })
            .ok();
        }));
    }

    /// Who is signed in decides the list's groups.
    fn read_viewer(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(service)) = (self.repo.clone(), session_service(cx)) else {
            return;
        };
        self.viewer_read = Some(cx.spawn(async move |this, cx| {
            let viewer = service.github_viewer(repo.host).await;
            this.update(cx, |this, cx| match viewer {
                Ok(viewer) => this
                    .list
                    .update(cx, |list, cx| list.set_viewer(Some(viewer.login), cx)),
                Err(error) => tracing::debug!("no GitHub viewer: {error:#}"),
            })
            .ok();
        }));
    }

    /// Read what is shown again (the window became active): the held value
    /// at once, a newer one where it is past its time.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match self.detail() {
            Some(detail) => detail.update(cx, |detail, cx| detail.set_visible(true, cx)),
            None => self.load_list(cx),
        }
    }

    /// The shown pull request's page.
    pub fn detail(&self) -> Option<Entity<DetailView>> {
        self.tabs.active_item().map(|(_, tab)| tab.view.clone())
    }

    #[cfg(test)]
    pub fn list(&self) -> &Entity<PullRequestList> {
        &self.list
    }

    fn handle(&mut self, intent: &PullRequestIntent, cx: &mut Context<Self>) {
        match intent {
            PullRequestIntent::Open(number) => self.open(*number, cx),
            PullRequestIntent::OpenUrl(url) => cx.open_url(url),
            PullRequestIntent::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
            }
            PullRequestIntent::List(_) => self.load_list(cx),
            PullRequestIntent::Refresh => match self.detail() {
                Some(detail) => detail.update(cx, |detail, cx| detail.refresh(cx)),
                None => self.refresh_list(cx),
            },
            PullRequestIntent::Act(action) => {
                if let Some(detail) = self.detail() {
                    let action = *action;
                    let number = detail.read(cx).pr().number;
                    // The list shows the outcome already, as after a quick action.
                    let token = self
                        .list
                        .update(cx, |list, cx| list.override_row(number, &action, cx));
                    let done = detail.update(cx, |detail, cx| detail.act(action, cx));
                    self.revert_unless(done, number, token, cx);
                }
            }
            PullRequestIntent::ActOn {
                number,
                action,
                token,
            } => self.act_on(*number, *action, *token, cx),
            PullRequestIntent::ListMore(query) => self.load_more(query.clone(), cx),
            PullRequestIntent::ListStats(numbers) => self.load_stats(numbers.clone(), cx),
            PullRequestIntent::Update(update) => {
                if let Some(detail) = self.detail() {
                    let update = update.clone();
                    detail.update(cx, |detail, cx| detail.update(update, cx));
                }
            }
            PullRequestIntent::CheckOut(place) => self.check_out(*place, cx),
        }
    }

    /// Check the shown pull request out; a new worktree's session is
    /// shown at once, a checkout here updates the session's branch.
    fn check_out(&mut self, place: CheckoutPlace, cx: &mut Context<Self>) {
        let (Some(detail), Some(session_id), Some(service)) =
            (self.detail(), self.session_id.clone(), session_service(cx))
        else {
            return;
        };
        let pr = detail.read(cx).pr().clone();
        detail.update(cx, |detail, cx| detail.set_checking_out(true, cx));
        let detail = detail.downgrade();
        cx.spawn(async move |_, cx| {
            let done = service
                .pull_request_checkout(session_id.clone(), pr, place)
                .await;
            detail
                .update(cx, |detail, cx| {
                    detail.set_checking_out(false, cx);
                    detail.checked_out(place, &done, cx);
                })
                .ok();
            if let Ok(PullRequestCheckout { session_id: to, .. }) = done {
                cx.update(|cx| {
                    if let Some(gpui) = cx.try_global::<crate::Gpui>() {
                        match place {
                            CheckoutPlace::Worktree => {
                                // Its pull request shows beside it.
                                crate::shared::ui_state::update(cx, |store| {
                                    store.set_right_panel_open(&to, true);
                                    store.set_right_panel_view(&to, "pull-request");
                                });
                                gpui.open_session(to)
                            }
                            CheckoutPlace::Here => gpui.cmd_list_branches_and_worktrees(to),
                        }
                    }
                });
            }
        })
        .detach();
    }

    /// Show pull request `number` in its tab, opened when it is not.
    fn open(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.open_tab(number, None, cx) else {
            return;
        };
        self.tabs.activate(Some(ix));
        self.strip_scroll.scroll_to_item(ix + 1);
        self.tabs_changed(cx);
    }

    /// The tab of `number`, opened (on `tab`) when it is not; shows it.
    fn open_tab(
        &mut self,
        number: u64,
        tab: Option<DetailTab>,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let repo = self.repo.clone()?;
        let seed = self.list.read(cx).entry(number).cloned();
        let ix = self.tabs.open(number, || {
            let view = cx.new(|cx| {
                let mut detail = DetailView::new(repo.pull_request(number), seed, cx);
                if let Some(tab) = tab {
                    detail.select_tab(tab, cx);
                }
                detail
            });
            OpenTab {
                _subscriptions: [
                    cx.subscribe(&view, |this, _, intent: &PullRequestIntent, cx| {
                        this.handle(intent, cx)
                    }),
                    cx.subscribe(&view, |this, _, _: &DetailTabChanged, cx| {
                        this.persist_tabs(cx)
                    }),
                ],
                view,
            }
        });
        Some(ix)
    }

    /// Show the tab at `ix`, or the list.
    pub(super) fn activate(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        self.tabs.activate(ix);
        if ix.is_none() {
            self.load_list(cx);
        }
        self.tabs_changed(cx);
    }

    pub(super) fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tabs.close(ix);
        self.tabs_changed(cx);
    }

    pub(super) fn close_others(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tabs.close_others(ix);
        self.tabs_changed(cx);
    }

    pub(super) fn close_to_the_right(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tabs.close_to_the_right(ix);
        self.tabs_changed(cx);
    }

    pub(super) fn close_all(&mut self, cx: &mut Context<Self>) {
        self.tabs.close_all();
        self.tabs_changed(cx);
    }

    pub(super) fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.tabs.move_tab(from, to);
        self.tabs_changed(cx);
    }

    fn tabs_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_visibility(cx);
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Only the shown page reads and polls.
    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        let active = self.tabs.active();
        let visible = self.visible;
        let views: Vec<_> = self.tabs.iter().map(|(_, tab)| tab.view.clone()).collect();
        for (ix, view) in views.into_iter().enumerate() {
            let shown = visible && active == Some(ix);
            view.update(cx, |view, cx| {
                if view.visible() != shown {
                    view.set_visible(shown, cx)
                }
            });
        }
    }

    /// Remember the open tabs for the session.
    fn persist_tabs(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let state = PullRequestTabsState {
            open: self
                .tabs
                .iter()
                .map(|(number, tab)| PullRequestTabState {
                    number,
                    tab: Some(tab.view.read(cx).tab().as_str().to_owned()),
                })
                .collect(),
            active: self.tabs.active_item().map(|(number, _)| number),
        };
        crate::shared::ui_state::update(cx, |store| {
            store.set_pull_request_tabs(&session_id, state)
        });
    }
}

impl Render for PullRequestPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.detail() {
            Some(detail) => detail.into_any_element(),
            None => self.list.clone().into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_tab_strip(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{self, entry, repo};
    use super::*;
    use detail::DetailTab;
    use github::model::ListPage;
    use gpui_kit::{TestAppContext, VisualTestContext};

    fn panel(cx: &mut TestAppContext) -> (Entity<PullRequestPanel>, &mut VisualTestContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| PullRequestPanel::new(window, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        view.update(cx, |panel, cx| panel.set_session(Some("a".into()), cx));
        (view, cx)
    }

    /// The detail page as if `detail` was read.
    fn show(view: &Entity<DetailView>, detail: github::model::Detail, cx: &mut VisualTestContext) {
        let data = view.read_with(cx, |view, _| view.data().clone());
        data.update(cx, |data, cx| {
            data.detail = Some(std::sync::Arc::new(detail));
            data.stack = stack::StackView::known_alone();
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing rendered as {selector}"));
        cx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn the_sessions_own_pull_request_shows_first(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), Some(290), cx)
        });
        let detail = panel.read_with(cx, |panel, _| panel.detail());
        let detail = detail.expect("the detail is shown");
        show(&detail, fixtures::detail(290), cx);
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-primary").is_some());

        // The list stays a tab away.
        click(cx, "pr-tab-list");
        panel.read_with(cx, |panel, _| assert!(panel.detail().is_none()));
        click(cx, "pr-tab-290");
        panel.read_with(cx, |panel, _| assert!(panel.detail().is_some()));
    }

    #[gpui_kit::test]
    fn a_row_opens_its_pull_request_seeded_from_the_list(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), None, cx);
            panel.list().update(cx, |list, cx| {
                list.set_page(
                    ListPage {
                        entries: vec![entry(281, "One kind of sub-agent", "ann")],
                        next_cursor: None,
                    },
                    cx,
                )
            });
        });
        cx.run_until_parked();
        click(cx, "pr-row-281");
        let detail = panel
            .read_with(cx, |panel, _| panel.detail())
            .expect("the row opened");
        assert!(cx.debug_bounds("pr-title").is_some());
        detail.read_with(cx, |view, _| {
            assert_eq!(view.pr().number, 281);
            assert_eq!(view.tab(), DetailTab::Summary);
        });
    }

    #[gpui_kit::test]
    fn each_pull_request_keeps_its_tab_and_its_page(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), None, cx);
            panel.list().update(cx, |list, cx| {
                list.set_page(
                    ListPage {
                        entries: vec![entry(281, "Sub-agents", "ann"), entry(290, "Images", "bob")],
                        next_cursor: None,
                    },
                    cx,
                )
            });
        });
        cx.run_until_parked();
        click(cx, "pr-row-281");
        let first = panel.read_with(cx, |panel, _| panel.detail()).unwrap();
        first.update(cx, |view, cx| view.select_tab(DetailTab::Code, cx));
        click(cx, "pr-tab-list");
        click(cx, "pr-row-290");
        let tabs = |panel: &Entity<PullRequestPanel>, cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| panel.tabs.numbers().collect::<Vec<_>>())
        };
        assert_eq!(tabs(&panel, cx), [281, 290]);
        first.read_with(cx, |view, _| {
            assert!(!view.visible(), "a hidden tab does not poll")
        });

        // Back on the first: the same page, still on its Code tab.
        click(cx, "pr-tab-281");
        let shown = panel.read_with(cx, |panel, _| panel.detail()).unwrap();
        assert_eq!(shown.entity_id(), first.entity_id());
        shown.read_with(cx, |view, _| {
            assert_eq!(view.tab(), DetailTab::Code);
            assert!(view.visible());
        });

        // Closing the shown tab shows its right neighbour.
        panel.update(cx, |panel, cx| panel.close_tab(0, cx));
        assert_eq!(tabs(&panel, cx), [290]);
        let shown = panel.read_with(cx, |panel, cx| {
            panel.detail().map(|d| d.read(cx).pr().number)
        });
        assert_eq!(shown, Some(290));
        panel.update(cx, |panel, cx| panel.close_tab(0, cx));
        panel.read_with(cx, |panel, _| {
            assert!(panel.detail().is_none(), "the list shows")
        });
    }

    #[gpui_kit::test]
    fn tabs_move_by_drag_and_drop(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), None, cx);
            for number in [1, 2, 3] {
                panel.open(number, cx);
            }
        });
        cx.run_until_parked();
        let from = cx.debug_bounds("pr-tab-3").unwrap().center();
        let to = cx.debug_bounds("pr-tab-1").unwrap().center();
        cx.simulate_mouse_down(
            from,
            gpui_kit::MouseButton::Left,
            gpui_kit::Modifiers::none(),
        );
        cx.simulate_mouse_move(to, gpui_kit::MouseButton::Left, gpui_kit::Modifiers::none());
        cx.simulate_mouse_up(to, gpui_kit::MouseButton::Left, gpui_kit::Modifiers::none());
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.tabs.numbers().collect::<Vec<_>>(), [3, 1, 2]);
            assert_eq!(panel.tabs.active_item().map(|(n, _)| n), Some(3));
        });
    }

    #[gpui_kit::test]
    fn the_tab_row_switches_tabs(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), Some(290), cx)
        });
        let detail = panel.read_with(cx, |panel, _| panel.detail()).unwrap();
        show(&detail, fixtures::detail(290), cx);
        cx.run_until_parked();
        click(cx, "pr-tab-Code");
        detail.read_with(cx, |view, _| assert_eq!(view.tab(), DetailTab::Code));
    }

    #[gpui_kit::test]
    fn an_action_on_the_shown_pull_request_shows_in_the_list_until_refused(
        cx: &mut TestAppContext,
    ) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), None, cx);
            panel.list().update(cx, |list, cx| {
                list.set_page(
                    ListPage {
                        entries: vec![entry(290, "Raster images", "ann")],
                        next_cursor: None,
                    },
                    cx,
                )
            });
            panel.open(290, cx);
        });
        let listed = |panel: &Entity<PullRequestPanel>, cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, cx| panel.list().read(cx).shows(290))
        };
        assert!(listed(&panel, cx));
        panel.update(cx, |panel, cx| {
            panel.handle(&PullRequestIntent::Act(Action::Close), cx)
        });
        assert!(!listed(&panel, cx), "closed: out of the open ones at once");
        // No session service: GitHub never saw it, the row is taken back.
        cx.run_until_parked();
        assert!(listed(&panel, cx));
    }

    #[gpui_kit::test]
    fn copying_lands_on_the_clipboard(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.handle(&PullRequestIntent::Copy("gh pr checkout 290".into()), cx)
        });
        let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(copied.as_deref(), Some("gh pr checkout 290"));
    }

    #[gpui_kit::test]
    fn another_session_starts_over(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), Some(290), cx)
        });
        // Hiding the tab keeps the pull request shown.
        panel.update(cx, |panel, cx| panel.set_session(None, cx));
        panel.read_with(cx, |panel, _| assert!(panel.detail().is_some()));
        panel.update(cx, |panel, cx| panel.set_session(Some("b".into()), cx));
        panel.read_with(cx, |panel, _| {
            assert!(panel.detail().is_none());
            assert!(panel.repo.is_none());
        });
    }
}
