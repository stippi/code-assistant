//! The browsers of one session, as a browser panel sees them.
//!
//! The session's agent keeps its browsers in one
//! [`web::BrowserSessionManager`]; each running sub-agent has its own (so
//! parallel sub-agents do not fight over the same tab) and registers it here
//! while it runs. [`SessionBrowsers`] lists them all, publishes their metadata
//! — never frames — on the session's [`EventStream`], and opens
//! [`BrowserView`]s for a panel.

use crate::session::event_stream::EventStream;
use crate::ui::UiEvent;
use anyhow::{Result, anyhow};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Notify, broadcast};
use tokio::task::JoinHandle;
use web::{BrowserSession, BrowserSessionManager, LiveFrames, Point, TabInfo, ViewGuard};

/// Which browser of a session: a profile of the agent's or of a sub-agent's.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserKey {
    /// The tool id of the sub-agent that owns the browser; `None` for the
    /// session's own agent.
    pub sub_agent: Option<String>,
    pub profile: String,
}

/// One browser as listed for a panel.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserEntry {
    pub key: BrowserKey,
    pub tabs: Vec<TabInfo>,
    /// The user has taken over; the agent's browser tools refuse to act.
    pub user_control: bool,
}

/// The browsers of one session.
pub struct SessionBrowsers {
    agent: Arc<BrowserSessionManager>,
    sub_agents: Mutex<Vec<(String, Arc<BrowserSessionManager>)>>,
    /// Rings when sub-agents come or go or a browser changes hands.
    changed: Arc<Notify>,
    publisher: Mutex<Option<JoinHandle<()>>>,
}

impl Default for SessionBrowsers {
    fn default() -> Self {
        Self {
            agent: Arc::new(BrowserSessionManager::default()),
            sub_agents: Mutex::default(),
            changed: Arc::new(Notify::new()),
            publisher: Mutex::default(),
        }
    }
}

impl Drop for SessionBrowsers {
    fn drop(&mut self) {
        if let Some(publisher) = self.publisher.lock().unwrap().take() {
            publisher.abort();
        }
    }
}

/// A sub-agent's browsers stay listed while this lives.
pub struct SubAgentBrowsers {
    browsers: Weak<SessionBrowsers>,
    tool_id: String,
}

impl Drop for SubAgentBrowsers {
    fn drop(&mut self) {
        if let Some(browsers) = self.browsers.upgrade() {
            browsers
                .sub_agents
                .lock()
                .unwrap()
                .retain(|(id, _)| *id != self.tool_id);
            browsers.changed.notify_one();
        }
    }
}

impl SessionBrowsers {
    /// The browsers of the session's own agent.
    pub fn agent(&self) -> &Arc<BrowserSessionManager> {
        &self.agent
    }

    /// List the browsers of the sub-agent `tool_id` while the returned guard
    /// lives.
    pub fn register_sub_agent(
        self: &Arc<Self>,
        tool_id: impl Into<String>,
        manager: Arc<BrowserSessionManager>,
    ) -> SubAgentBrowsers {
        let tool_id = tool_id.into();
        self.sub_agents
            .lock()
            .unwrap()
            .push((tool_id.clone(), manager));
        self.changed.notify_one();
        SubAgentBrowsers {
            browsers: Arc::downgrade(self),
            tool_id,
        }
    }

    /// Every manager with the sub-agent that owns it.
    fn managers(&self) -> Vec<(Option<String>, Arc<BrowserSessionManager>)> {
        std::iter::once((None, self.agent.clone()))
            .chain(
                self.sub_agents
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, manager)| (Some(id.clone()), manager.clone())),
            )
            .collect()
    }

    /// Every browser with its key, the agent's first, each owner's sorted by
    /// profile.
    fn sessions(&self) -> Vec<(BrowserKey, Arc<BrowserSession>)> {
        let mut out = Vec::new();
        for (sub_agent, manager) in self.managers() {
            let mut sessions = manager.sessions();
            sessions.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(sessions.into_iter().map(|(profile, session)| {
                (
                    BrowserKey {
                        sub_agent: sub_agent.clone(),
                        profile,
                    },
                    session,
                )
            }));
        }
        out
    }

    fn entry(key: BrowserKey, session: &BrowserSession) -> BrowserEntry {
        BrowserEntry {
            key,
            tabs: session.watch_tabs().borrow().clone(),
            user_control: session.user_in_control(),
        }
    }

    /// The browsers as they are now.
    pub fn listing(&self) -> Vec<BrowserEntry> {
        self.sessions()
            .into_iter()
            .map(|(key, session)| Self::entry(key, &session))
            .collect()
    }

    fn find(&self, key: &BrowserKey) -> Result<(Arc<BrowserSessionManager>, Arc<BrowserSession>)> {
        let manager = self
            .managers()
            .into_iter()
            .find(|(sub_agent, _)| *sub_agent == key.sub_agent)
            .map(|(_, manager)| manager);
        manager
            .and_then(|manager| {
                let session = manager
                    .sessions()
                    .into_iter()
                    .find(|(profile, _)| *profile == key.profile)?
                    .1;
                Some((manager, session))
            })
            .ok_or_else(|| anyhow!("no browser for profile '{}'", key.profile))
    }

    /// Publish the listing on `events` from now on, whenever it changes.
    /// Starts at most one publisher per session; needs a tokio runtime.
    pub fn publish_to(self: &Arc<Self>, events: EventStream, session_id: String) {
        let mut publisher = self.publisher.lock().unwrap();
        if publisher.is_none() {
            *publisher = Some(tokio::spawn(publish_changes(
                Arc::downgrade(self),
                self.changed.clone(),
                events,
                session_id,
            )));
        }
    }

    /// Hand the browser `key` to the user (`true`) or back to the agent.
    pub fn set_user_control(&self, key: &BrowserKey, user: bool) -> Result<()> {
        self.find(key)?.1.set_user_control(user);
        self.changed.notify_one();
        Ok(())
    }

    /// Watch tab `tab_id` (the active tab if `None`) of browser `key`, frames
    /// at most `max_size` large. The browser stays open while the view lives.
    /// Needs a tokio runtime.
    pub fn view(
        &self,
        key: &BrowserKey,
        tab_id: Option<&str>,
        max_size: (u32, u32),
    ) -> Result<BrowserView> {
        let (manager, session) = self.find(key)?;
        let guard = manager
            .view(&key.profile)
            .ok_or_else(|| anyhow!("the browser for '{}' closed", key.profile))?;
        let tab = session.tab(tab_id)?;
        Ok(BrowserView {
            key: key.clone(),
            tab_id: tab.id().to_string(),
            frames: tab.live_frames(max_size),
            presses: tab.watch_agent_presses(),
            _guard: guard,
        })
    }
}

/// Publish the listing whenever it changes, until the browsers are gone.
async fn publish_changes(
    browsers: Weak<SessionBrowsers>,
    changed: Arc<Notify>,
    events: EventStream,
    session_id: String,
) {
    let mut last: Option<Vec<BrowserEntry>> = None;
    loop {
        let Some(this) = browsers.upgrade() else {
            return;
        };
        // Subscribe before reading, so no change slips between the two. (A
        // ring before the wait starts is kept as a permit.)
        let changed = changed.clone();
        let mut waits: Vec<BoxFuture<'static, ()>> =
            vec![async move { changed.notified().await }.boxed()];
        for (_, manager) in this.managers() {
            let mut changes = manager.watch_changes();
            waits.push(async move { changes.changed().await.unwrap_or(()) }.boxed());
        }
        let sessions = this.sessions();
        drop(this);
        for (_, session) in &sessions {
            let mut tabs = session.watch_tabs();
            waits.push(
                async move {
                    if tabs.changed().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
                .boxed(),
            );
        }
        let listing: Vec<BrowserEntry> = sessions
            .into_iter()
            .map(|(key, session)| SessionBrowsers::entry(key, &session))
            .collect();
        if last.as_ref() != Some(&listing) {
            events.publish_ui(
                &session_id,
                UiEvent::BrowsersChanged {
                    browsers: listing.clone(),
                },
            );
            last = Some(listing);
        }
        futures::future::select_all(waits).await;
    }
}

/// A panel's view of one tab: its frames, where the agent clicks, and (while
/// the user has control) a way to act on it. Keeps the browser open.
pub struct BrowserView {
    pub key: BrowserKey,
    pub tab_id: String,
    /// The newest frame of the tab.
    pub frames: LiveFrames,
    /// Where the agent presses the mouse, in CSS pixels.
    pub presses: broadcast::Receiver<Point>,
    _guard: ViewGuard,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event_stream::EventPayload;
    use std::time::Duration;
    use web::BrowserLaunchConfig;

    async fn open(manager: &BrowserSessionManager, profile: &str) -> Arc<BrowserSession> {
        let session = Arc::new(
            BrowserSession::open(BrowserLaunchConfig::default(), profile)
                .await
                .unwrap(),
        );
        manager.register(session.clone(), profile);
        session
    }

    fn agent_key(profile: &str) -> BrowserKey {
        BrowserKey {
            sub_agent: None,
            profile: profile.into(),
        }
    }

    /// The listing covers the agent's browsers and those of running
    /// sub-agents; a sub-agent's leave with it.
    #[tokio::test]
    async fn sub_agent_browsers_are_listed_while_they_run() {
        let browsers = Arc::new(SessionBrowsers::default());
        open(browsers.agent(), "default").await;
        let sub_manager = Arc::new(BrowserSessionManager::default());
        open(&sub_manager, "default").await;
        let registration = browsers.register_sub_agent("tool-7", sub_manager.clone());

        let keys: Vec<BrowserKey> = browsers.listing().into_iter().map(|e| e.key).collect();
        assert_eq!(
            keys,
            [
                agent_key("default"),
                BrowserKey {
                    sub_agent: Some("tool-7".into()),
                    profile: "default".into()
                }
            ]
        );
        drop(registration);
        assert_eq!(browsers.listing().len(), 1);

        browsers.agent().close_all().await;
        sub_manager.close_all().await;
    }

    /// Metadata reaches the event stream: a browser opening, a navigation,
    /// the user taking over.
    #[tokio::test]
    async fn changes_are_published_on_the_stream() {
        let events = EventStream::new();
        let mut subscription = events.subscribe();
        let browsers = Arc::new(SessionBrowsers::default());
        browsers.publish_to(events.clone(), "s1".into());
        let mut next = async || loop {
            let event = tokio::time::timeout(Duration::from_secs(10), subscription.recv())
                .await
                .expect("an event")
                .unwrap();
            assert_eq!(event.session_id.as_deref(), Some("s1"));
            if let EventPayload::Ui(UiEvent::BrowsersChanged { browsers }) = event.payload {
                return browsers;
            }
        };
        assert!(next().await.is_empty(), "the listing as publishing starts");

        let session = open(browsers.agent(), "default").await;
        let mut listing = next().await;
        while listing.first().is_none_or(|e| e.tabs.is_empty()) {
            listing = next().await;
        }
        assert_eq!(listing[0].key, agent_key("default"));

        session
            .active_tab()
            .unwrap()
            .navigate("data:text/html,<title>Hello</title>hi")
            .await
            .unwrap();
        while !next().await[0].tabs[0].title.contains("Hello") {}

        browsers
            .set_user_control(&agent_key("default"), true)
            .unwrap();
        while !next().await[0].user_control {}

        browsers.agent().close_all().await;
        while !next().await.is_empty() {}
    }

    /// A view shows the tab and keeps a throwaway browser open past the turn.
    #[tokio::test]
    async fn a_view_shows_the_tab_and_keeps_the_browser() {
        let browsers = Arc::new(SessionBrowsers::default());
        open(browsers.agent(), "default").await;
        assert!(
            browsers
                .view(&agent_key("other"), None, (640, 400))
                .is_err()
        );

        let mut view = browsers
            .view(&agent_key("default"), None, (640, 400))
            .unwrap();
        assert_eq!(view.tab_id, "t1");
        tokio::time::timeout(Duration::from_secs(5), view.frames.next())
            .await
            .unwrap()
            .expect("a frame");
        browsers.agent().close_ephemeral().await;
        assert_eq!(browsers.listing().len(), 1, "kept while viewed");
        drop(view);
        let closed = async {
            while !browsers.listing().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), closed)
            .await
            .expect("closed once the view is gone");
    }
}
