//! The "Pull request" view of the right panel (`docs/pull-request-view.md`):
//! the session's pull request, or the pull requests of its repository to
//! pick one from; a back arrow returns to the list.
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
mod summary;
mod tab_bar;
mod timeline;

use bus::{PullRequestEvent, bus};
use code_assistant_core::pull_requests::{CheckoutPlace, PullRequestCheckout};
use data::session_service;
use detail::DetailView;
use github::RepoRef;
use github::model::{Action, ListQuery, PullRequestUpdate};
use gpui_kit::{
    ClipboardItem, Context, Entity, Render, Subscription, Task, Window, div, prelude::*,
};
use list::PullRequestList;

/// How often relative times ("9h ago") are drawn anew.
const CLOCK_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// What the user asks of the pull request view.
#[derive(Clone, Debug, PartialEq)]
pub enum PullRequestIntent {
    /// Show a pull request of the listed repository.
    Open(u64),
    /// Back to the list.
    Back,
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
    detail: Option<Entity<DetailView>>,
    /// Finding the session's repository and pull request.
    resolve: Option<Task<()>>,
    list_read: Option<Task<()>>,
    more_read: Option<Task<()>>,
    viewer_read: Option<Task<()>>,
    /// Redraws once a minute while shown, so "9h ago" stays true.
    _clock: Option<Task<()>>,
    _list_subscription: Subscription,
    _detail_subscription: Option<Subscription>,
    _bus: Subscription,
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
            detail: None,
            resolve: None,
            list_read: None,
            more_read: None,
            viewer_read: None,
            _clock: None,
            _list_subscription: list_subscription,
            _detail_subscription: None,
            _bus: bus,
        }
    }

    /// Follow `session_id` while the tab is shown (`None` while hidden,
    /// which keeps what is shown for its return).
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        let visible = session_id.is_some();
        if visible != self.visible {
            self.visible = visible;
            if let Some(detail) = &self.detail {
                detail.update(cx, |detail, cx| detail.set_visible(visible, cx));
            }
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
                    if let Some(detail) = &this.detail {
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

    /// Where the session's pull requests live and which one is its own,
    /// which is shown first.
    pub fn set_repository(
        &mut self,
        repo: Option<RepoRef>,
        own: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        self.repo = repo.clone();
        self.close_detail();
        self.list.update(cx, |list, cx| list.set_repo(repo, cx));
        self.load_list(cx);
        self.read_viewer(cx);
        if let Some(number) = own {
            self.open(number, cx);
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
        match &self.detail {
            Some(detail) => detail.update(cx, |detail, cx| detail.set_visible(true, cx)),
            None => self.load_list(cx),
        }
    }

    #[cfg(test)]
    pub fn list(&self) -> &Entity<PullRequestList> {
        &self.list
    }

    #[cfg(test)]
    pub fn detail(&self) -> Option<&Entity<DetailView>> {
        self.detail.as_ref()
    }

    fn handle(&mut self, intent: &PullRequestIntent, cx: &mut Context<Self>) {
        match intent {
            PullRequestIntent::Open(number) => self.open(*number, cx),
            PullRequestIntent::Back => {
                self.close_detail();
                cx.notify();
            }
            PullRequestIntent::OpenUrl(url) => cx.open_url(url),
            PullRequestIntent::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
            }
            PullRequestIntent::List(_) => self.load_list(cx),
            PullRequestIntent::Refresh => match &self.detail {
                Some(detail) => detail.update(cx, |detail, cx| detail.refresh(cx)),
                None => self.refresh_list(cx),
            },
            PullRequestIntent::Act(action) => {
                if let Some(detail) = self.detail.clone() {
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
                if let Some(detail) = &self.detail {
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
        let (Some(detail), Some(session_id), Some(service)) = (
            self.detail.clone(),
            self.session_id.clone(),
            session_service(cx),
        ) else {
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
                            CheckoutPlace::Worktree => gpui.open_session(to),
                            CheckoutPlace::Here => gpui.cmd_list_branches_and_worktrees(to),
                        }
                    }
                });
            }
        })
        .detach();
    }

    fn open(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(repo) = &self.repo else {
            return;
        };
        let pr = repo.pull_request(number);
        if self.detail.as_ref().is_some_and(|d| *d.read(cx).pr() == pr) {
            return;
        }
        let seed = self.list.read(cx).entry(number).cloned();
        let visible = self.visible;
        let detail = cx.new(|cx| {
            let mut detail = DetailView::new(pr, seed, true, cx);
            if !visible {
                detail.set_visible(false, cx);
            }
            detail
        });
        self._detail_subscription = Some(
            cx.subscribe(&detail, |this, _, intent: &PullRequestIntent, cx| {
                this.handle(intent, cx)
            }),
        );
        self.detail = Some(detail);
        cx.notify();
    }

    fn close_detail(&mut self) {
        self.detail = None;
        self._detail_subscription = None;
    }
}

impl Render for PullRequestPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.detail {
            Some(detail) => detail.clone().into_any_element(),
            None => self.list.clone().into_any_element(),
        };
        div().size_full().child(body)
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
        let detail = panel.read_with(cx, |panel, _| panel.detail().cloned());
        let detail = detail.expect("the detail is shown");
        show(&detail, fixtures::detail(290), cx);
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-primary").is_some());

        click(cx, "pr-back");
        panel.read_with(cx, |panel, _| assert!(panel.detail().is_none()));
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
            .read_with(cx, |panel, _| panel.detail().cloned())
            .expect("the row opened");
        assert!(cx.debug_bounds("pr-title").is_some());
        detail.read_with(cx, |view, _| {
            assert_eq!(view.pr().number, 281);
            assert_eq!(view.tab(), DetailTab::Summary);
        });
    }

    #[gpui_kit::test]
    fn the_tab_row_switches_tabs(cx: &mut TestAppContext) {
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_repository(Some(repo()), Some(290), cx)
        });
        let detail = panel
            .read_with(cx, |panel, _| panel.detail().cloned())
            .unwrap();
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
