//! What the detail page knows about one pull request, shared by the header
//! and the tabs, which observe it.
//!
//! Reads go through the session service's cache: a held value comes at
//! once (`refreshing` while a newer one is read), and the core announces
//! the newer one ([`PullRequestEvent::Changed`]), which is read again. The
//! checks are polled while the page is shown and the pull request open.
//! Writes run through [`PullRequestData::write`], which shows a failure.

use super::bus::{PullRequestEvent, bus};
use super::presentation::effective_checks_state;
use crate::Gpui;
use code_assistant_core::pull_requests::{Fetched, PullRequestPart};
use code_assistant_core::session::SessionService;
use github::PullRequestRef;
use github::model::{Activity, Check, Checks, ChecksState, Detail, PullRequestState, Viewer};
use gpui_kit::{App, Context, Entity, SharedString, Subscription, Task};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// How often the checks are read while some are pending, and otherwise.
const PENDING_CHECKS_POLL: Duration = Duration::from_secs(45);
const CHECKS_POLL: Duration = Duration::from_secs(60);

/// The session service, once the app installed it.
pub fn session_service(cx: &App) -> Option<SessionService> {
    cx.try_global::<Gpui>()
        .and_then(|gpui| gpui.session_service())
}

/// What the detail page knows about one pull request; the tabs observe it.
pub struct PullRequestData {
    pub pr: PullRequestRef,
    pub detail: Option<Arc<Detail>>,
    pub activity: Option<Arc<Activity>>,
    pub checks: Option<Arc<Checks>>,
    pub viewer: Option<Viewer>,
    /// A background revalidation is under way for some part.
    pub refreshing: bool,
    /// The last failed read or write, shown by the page until the next success.
    pub error: Option<SharedString>,
    refreshing_parts: HashSet<PullRequestPart>,
    reads: HashMap<PullRequestPart, Task<()>>,
    viewer_read: Option<Task<()>>,
    /// Prefetching what the Code tab reads first.
    warm: Option<Task<()>>,
    poll: Option<Task<()>>,
    _bus: Subscription,
}

impl PullRequestData {
    pub fn new(pr: PullRequestRef, cx: &mut Context<Self>) -> Self {
        let events = bus(cx);
        let bus = cx.subscribe(&events, |this, _, event: &PullRequestEvent, cx| {
            if let PullRequestEvent::Changed { pr, part } = event
                && *pr == this.pr
            {
                this.read(*part, cx);
            }
        });
        Self {
            pr,
            detail: None,
            activity: None,
            checks: None,
            viewer: None,
            refreshing: false,
            error: None,
            refreshing_parts: HashSet::new(),
            reads: HashMap::new(),
            viewer_read: None,
            warm: None,
            poll: None,
            _bus: bus,
        }
    }

    /// Read everything the page shows: detail, conversation, checks and who
    /// is signed in.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        for part in [
            PullRequestPart::Detail,
            PullRequestPart::Activity,
            PullRequestPart::Checks,
        ] {
            self.read(part, cx);
        }
        self.read_viewer(cx);
        self.warm_code(cx);
    }

    /// Read the first diff page and the files viewed into the core's cache,
    /// so the Code tab finds them there when it is opened.
    fn warm_code(&mut self, cx: &mut Context<Self>) {
        let Some(service) = session_service(cx) else {
            return;
        };
        let pr = self.pr.clone();
        self.warm = Some(cx.spawn(async move |_, _| {
            let (diff, viewed) = futures::join!(
                service.pull_request_diff(pr.clone(), None, None),
                service.pull_request_files_viewed(pr),
            );
            if let Err(error) = diff.map(drop).and(viewed.map(drop)) {
                tracing::debug!("pull request code not prefetched: {error:#}");
            }
        }));
    }

    /// Read `part` (held value first). Parts other views keep (diff, files
    /// viewed) are theirs to read.
    pub fn read(&mut self, part: PullRequestPart, cx: &mut Context<Self>) {
        match part {
            PullRequestPart::Detail => {
                self.fetch(part, cx, Self::store_detail, |s, pr| async move {
                    s.pull_request_detail(pr).await
                })
            }
            PullRequestPart::Activity => {
                self.fetch(part, cx, Self::store_activity, |s, pr| async move {
                    s.pull_request_activity(pr).await
                })
            }
            PullRequestPart::Checks => {
                self.fetch(part, cx, Self::store_checks, |s, pr| async move {
                    s.pull_request_checks(pr).await
                })
            }
            PullRequestPart::Diff | PullRequestPart::FilesViewed => {}
        }
    }

    fn store_detail(&mut self, detail: Detail) {
        self.detail = Some(Arc::new(detail));
    }

    fn store_activity(&mut self, activity: Activity) {
        self.activity = Some(Arc::new(activity));
    }

    fn store_checks(&mut self, checks: Checks) {
        self.checks = Some(Arc::new(checks));
    }

    fn fetch<T: 'static, Fut>(
        &mut self,
        part: PullRequestPart,
        cx: &mut Context<Self>,
        store: fn(&mut Self, T),
        read: impl FnOnce(SessionService, PullRequestRef) -> Fut + 'static,
    ) where
        Fut: Future<Output = anyhow::Result<Fetched<T>>> + 'static,
    {
        let Some(service) = session_service(cx) else {
            return;
        };
        let pr = self.pr.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = read(service, pr).await;
            this.update(cx, |this, cx| {
                this.reads.remove(&part);
                match result {
                    Ok(fetched) => {
                        store(this, fetched.value);
                        this.error = None;
                        this.set_part_refreshing(part, fetched.refreshing);
                    }
                    Err(error) => {
                        this.error = Some(format!("{error:#}").into());
                        this.set_part_refreshing(part, false);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.reads.insert(part, task);
    }

    fn set_part_refreshing(&mut self, part: PullRequestPart, refreshing: bool) {
        if refreshing {
            self.refreshing_parts.insert(part);
        } else {
            self.refreshing_parts.remove(&part);
        }
        self.refreshing = !self.refreshing_parts.is_empty();
    }

    fn read_viewer(&mut self, cx: &mut Context<Self>) {
        let Some(service) = session_service(cx) else {
            return;
        };
        let host = self.pr.repo.host.clone();
        self.viewer_read = Some(cx.spawn(async move |this, cx| {
            let viewer = service.github_viewer(host).await;
            this.update(cx, |this, cx| match viewer {
                Ok(viewer) => {
                    this.viewer = Some(viewer);
                    cx.notify();
                }
                Err(error) => tracing::debug!("no GitHub viewer: {error:#}"),
            })
            .ok();
        }));
    }

    /// The checks to show: the polled ones once read, else the detail's.
    pub fn current_checks(&self) -> &[Check] {
        match (&self.checks, &self.detail) {
            (Some(checks), _) => &checks.checks,
            (None, Some(detail)) => &detail.checks,
            (None, None) => &[],
        }
    }

    /// The verdict over [`Self::current_checks`].
    pub fn checks_state(&self) -> Option<ChecksState> {
        let rollup = match (&self.checks, &self.detail) {
            (Some(checks), _) => checks.checks_state,
            (None, Some(detail)) => detail.checks_state,
            (None, None) => None,
        };
        effective_checks_state(rollup, self.current_checks())
    }

    fn poll_interval(&self) -> Duration {
        if self.checks_state() == Some(ChecksState::Pending) {
            PENDING_CHECKS_POLL
        } else {
            CHECKS_POLL
        }
    }

    fn is_open(&self) -> bool {
        self.detail
            .as_ref()
            .is_none_or(|detail| detail.state == PullRequestState::Open)
    }

    /// Poll the checks while `polling` (the page is shown) and the pull
    /// request open: every 45 s while some are pending, else every 60 s.
    pub fn set_polling(&mut self, polling: bool, cx: &mut Context<Self>) {
        if !polling {
            self.poll = None;
            return;
        }
        if self.poll.is_some() {
            return;
        }
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.read_with(cx, |data, _| data.poll_interval()) else {
                    return;
                };
                cx.background_executor().timer(interval).await;
                let polled = this.update(cx, |data, cx| {
                    if data.is_open() {
                        data.read(PullRequestPart::Checks, cx);
                    }
                });
                if polled.is_err() {
                    return;
                }
            }
        }));
    }

    /// Run a write through the session service; on failure sets `error`
    /// (and notifies), returns the result so callers can roll back.
    pub fn write<T: 'static, F, Fut>(
        this: &Entity<Self>,
        cx: &mut App,
        f: F,
    ) -> Task<anyhow::Result<T>>
    where
        F: FnOnce(SessionService, PullRequestRef) -> Fut + 'static,
        Fut: Future<Output = anyhow::Result<T>> + 'static,
    {
        let pr = this.read(cx).pr.clone();
        let data = this.downgrade();
        let service = session_service(cx);
        cx.spawn(async move |cx| {
            let result = match service {
                Some(service) => f(service, pr).await,
                None => Err(anyhow::anyhow!("GitHub is not available")),
            };
            data.update(cx, |data, cx| {
                data.error = result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}").into());
                cx.notify();
            })
            .ok();
            result
        })
    }

    /// Read everything anew, past the cache.
    pub fn refresh(this: &Entity<Self>, cx: &mut App) {
        // The core announces each dropped part, which reads it again.
        PullRequestData::write(this, cx, |service, pr| async move {
            service.pull_request_refresh(pr).await
        })
        .detach();
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{check, detail_with, repo};
    use github::model::CheckStatus;
    use gpui_kit::{AppContext, TestAppContext};

    fn data(cx: &mut TestAppContext) -> Entity<PullRequestData> {
        cx.update(|cx| cx.new(|cx| PullRequestData::new(repo().pull_request(290), cx)))
    }

    #[gpui_kit::test]
    fn polled_checks_replace_the_details(cx: &mut TestAppContext) {
        let data = data(cx);
        data.update(cx, |data, _| {
            data.detail = Some(Arc::new(detail_with(290, |d| {
                d.checks = vec![check("ci", CheckStatus::Pending)];
            })));
        });
        data.read_with(cx, |data, _| {
            assert_eq!(data.checks_state(), Some(ChecksState::Pending))
        });
        data.update(cx, |data, _| {
            data.checks = Some(Arc::new(Checks {
                state: PullRequestState::Open,
                head_sha: None,
                checks: vec![check("ci", CheckStatus::Success)],
                checks_state: None,
                workflow_approvals: None,
            }));
        });
        data.read_with(cx, |data, _| {
            assert_eq!(data.current_checks().len(), 1);
            assert_eq!(data.checks_state(), Some(ChecksState::Passing));
        });
    }

    #[gpui_kit::test]
    fn pending_checks_are_polled_sooner(cx: &mut TestAppContext) {
        let data = data(cx);
        data.read_with(cx, |data, _| assert_eq!(data.poll_interval(), CHECKS_POLL));
        data.update(cx, |data, _| {
            data.detail = Some(Arc::new(detail_with(290, |d| {
                d.checks = vec![check("ci", CheckStatus::Pending)];
            })));
        });
        data.read_with(cx, |data, _| {
            assert_eq!(data.poll_interval(), PENDING_CHECKS_POLL)
        });
    }

    #[gpui_kit::test]
    fn a_failed_write_shows_until_the_next_success(cx: &mut TestAppContext) {
        let data = data(cx);
        cx.update(|cx| {
            PullRequestData::write(&data, cx, |_, _| async { Ok(()) }).detach();
        });
        cx.run_until_parked();
        // Without a session service nothing reaches GitHub.
        let error = data.read_with(cx, |data, _| data.error.clone());
        assert_eq!(error.as_deref(), Some("GitHub is not available"));
        data.update(cx, |data, cx| data.dismiss_error(cx));
        data.read_with(cx, |data, _| assert!(data.error.is_none()));
    }
}
