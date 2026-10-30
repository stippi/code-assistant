//! The "Pull request" view of the right panel (`docs/pull-request-view.md`):
//! the session's pull request, or the pull requests of its repository to
//! pick one from; a back arrow returns to the list.
//!
//! The views ([`list::PullRequestList`], [`detail::DetailView`] with its
//! header and tab bodies) render model values from the `github` crate and
//! say what the user asks for as [`PullRequestIntent`]s. The panel carries
//! them out: navigation itself, copying and opening links, everything that
//! reads or writes GitHub through the `SessionService`.

mod actions;
mod avatar;
mod code;
mod detail;
#[cfg(test)]
mod fixtures;
mod header;
mod list;
mod list_logic;
mod markdown;
mod presentation;
mod summary;
mod tab_bar;
mod timeline;

use detail::DetailView;
use github::RepoRef;
use github::model::{Action, ListQuery, PullRequestUpdate};
use gpui_kit::{ClipboardItem, Context, Entity, Render, Subscription, Window, div, prelude::*};
use list::PullRequestList;

/// What the user asks of the pull request view.
#[derive(Clone, Debug, PartialEq)]
pub enum PullRequestIntent {
    /// Show a pull request of the listed repository.
    Open(u64),
    /// Back to the list.
    Back,
    /// List the repository's pull requests anew.
    List(ListQuery),
    /// Read what is shown again, past the cache.
    Refresh,
    /// Merge, close, reopen, draft or ready, auto-merge, update or delete
    /// the branch of the shown pull request.
    Act(Action),
    /// Rewrite the shown pull request's title, body or maintainer edits.
    Update(PullRequestUpdate),
    OpenUrl(String),
    Copy(String),
}

pub struct PullRequestPanel {
    session_id: Option<String>,
    repo: Option<RepoRef>,
    list: Entity<PullRequestList>,
    detail: Option<Entity<DetailView>>,
    _list_subscription: Subscription,
    _detail_subscription: Option<Subscription>,
}

impl PullRequestPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| PullRequestList::new(window, cx));
        let list_subscription = cx.subscribe(&list, |this, _, intent: &PullRequestIntent, cx| {
            this.handle(intent, cx)
        });
        Self {
            session_id: None,
            repo: None,
            list,
            detail: None,
            _list_subscription: list_subscription,
            _detail_subscription: None,
        }
    }

    /// Follow `session_id` while the tab is shown (`None` while hidden,
    /// which keeps what is shown for its return).
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        let Some(session_id) = session_id else {
            return;
        };
        if self.session_id.as_deref() == Some(session_id.as_str()) {
            return;
        }
        self.session_id = Some(session_id);
        self.set_repository(None, None, cx);
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
        if let Some(number) = own {
            self.open(number, cx);
        }
        cx.notify();
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
            PullRequestIntent::List(_)
            | PullRequestIntent::Refresh
            | PullRequestIntent::Act(_)
            | PullRequestIntent::Update(_) => {
                tracing::debug!(?intent, "pull request intent: no service connected yet");
            }
        }
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
        let detail = cx.new(|cx| DetailView::new(pr, seed, true, cx));
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
        detail.update(cx, |view, cx| view.set_detail(fixtures::detail(290), cx));
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
        detail.update(cx, |view, cx| view.set_detail(fixtures::detail(290), cx));
        cx.run_until_parked();
        click(cx, "pr-tab-Code");
        detail.read_with(cx, |view, _| assert_eq!(view.tab(), DetailTab::Code));
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
