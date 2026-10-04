//! Interactive browser sessions for agent tools.
//!
//! [`crate::WebClient`] covers the one-shot case: fetch a page, extract it,
//! discard it. Browser *agency* needs the opposite — pages the agent drives
//! over many tool calls: navigate, look (screenshot / read), click, type, wait.
//!
//! This mirrors the `pty_session` crate:
//! - [`BrowserSession`] — one launched browser with its tabs ([`Tab`]), kept
//!   across tool calls.
//! - [`BrowserSessionManager`] — a registry with an LRU cap, one per agent
//!   session, so browser sessions survive across tool calls but die with their
//!   agent session.

use crate::browser::LaunchedBrowser;
use crate::tab::{BrowserTimeouts, Tab};
use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chromiumoxide::cdp::browser_protocol::target::{
    EventTargetCreated, EventTargetDestroyed, EventTargetInfoChanged, GetTargetsParams, TargetId,
    TargetInfo,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{Mutex as AsyncMutex, Notify, watch};
use tokio::task::JoinHandle;

/// A tab as listed for the model and shown in a browser panel.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
    /// The page is still loading.
    #[serde(default)]
    pub loading: bool,
}

#[derive(Default)]
struct Tabs {
    list: Vec<Arc<Tab>>,
    /// The tab a call without an explicit tab id targets.
    active: Option<String>,
    next_id: u32,
}

impl Tabs {
    fn mint_id(&mut self) -> String {
        self.next_id += 1;
        format!("t{}", self.next_id)
    }

    fn get(&self, id: &str) -> Option<Arc<Tab>> {
        self.list.iter().find(|t| t.id() == id).cloned()
    }
}

/// What the session and its tab tracker share.
struct Shared {
    /// Kept alive so the browser process outlives individual tool calls;
    /// behind an async mutex only because a graceful close needs `&mut`.
    launched: AsyncMutex<LaunchedBrowser>,
    tabs: Mutex<Tabs>,
    timeouts: Arc<Mutex<BrowserTimeouts>>,
    /// The tabs as last published by the tracker.
    state: watch::Sender<Vec<TabInfo>>,
    /// Rings when the tab list, the active tab or a tab's loading changed.
    changed: Arc<Notify>,
    /// Held while adopting, so the tracker and a tool call do not adopt the
    /// same page twice.
    adopting: AsyncMutex<()>,
    /// Tabs adopted but not yet returned by [`BrowserSession::sync_tabs`],
    /// whoever adopted them.
    unreported: Mutex<Vec<String>>,
}

impl Shared {
    async fn create_tab(&self, foreground: bool) -> Result<Arc<Tab>> {
        let page = self
            .launched
            .lock()
            .await
            .browser
            .new_page("about:blank")
            .await?;
        let id = self.tabs.lock().unwrap().mint_id();
        let tab = Arc::new(
            Tab::new(
                id.clone(),
                page,
                self.timeouts.clone(),
                self.changed.clone(),
            )
            .await?,
        );
        let mut tabs = self.tabs.lock().unwrap();
        tabs.list.push(tab.clone());
        if foreground || tabs.active.is_none() {
            tabs.active = Some(id);
        }
        drop(tabs);
        self.changed.notify_one();
        Ok(tab)
    }

    /// Adopt tabs the page opened and forget closed ones; adopted tabs are
    /// added to `unreported`.
    async fn sync_tabs(&self) -> Result<()> {
        let _adopting = self.adopting.lock().await;
        let (pages, opened) = {
            let launched = self.launched.lock().await;
            let pages = launched.browser.pages().await?;
            let targets = launched
                .browser
                .execute(GetTargetsParams::default())
                .await?
                .result
                .target_infos;
            let opened: Vec<_> = targets
                .into_iter()
                .filter(|t| t.opener_id.is_some())
                .map(|t| t.target_id)
                .collect();
            (pages, opened)
        };
        let known: Vec<_> = {
            let tabs = self.tabs.lock().unwrap();
            tabs.list
                .iter()
                .map(|t| t.page().target_id().clone())
                .collect()
        };
        let mut adopted = Vec::new();
        for page in pages
            .iter()
            .filter(|p| !known.contains(p.target_id()) && opened.contains(p.target_id()))
        {
            let id = self.tabs.lock().unwrap().mint_id();
            let tab = Arc::new(
                Tab::new(
                    id.clone(),
                    page.clone(),
                    self.timeouts.clone(),
                    self.changed.clone(),
                )
                .await?,
            );
            self.tabs.lock().unwrap().list.push(tab);
            adopted.push(id);
        }
        let open: Vec<_> = pages.iter().map(|p| p.target_id().clone()).collect();
        let mut tabs = self.tabs.lock().unwrap();
        let before = tabs.list.len();
        tabs.list.retain(|t| open.contains(t.page().target_id()));
        let active_gone = tabs
            .active
            .as_deref()
            .is_some_and(|id| tabs.get(id).is_none());
        if active_gone {
            tabs.active = tabs.list.last().map(|t| t.id().to_string());
        }
        if !adopted.is_empty() || tabs.list.len() != before {
            self.changed.notify_one();
        }
        self.unreported.lock().unwrap().extend(adopted);
        Ok(())
    }

    /// Publish the tabs, with the addresses and titles last reported for
    /// their targets.
    fn publish(&self, targets: &HashMap<TargetId, (String, String)>) {
        let tabs = self.tabs.lock().unwrap();
        let list: Vec<TabInfo> = tabs
            .list
            .iter()
            .map(|tab| {
                let (url, mut title) = targets
                    .get(tab.page().target_id())
                    .cloned()
                    .unwrap_or_default();
                let loaded_title = tab.loaded_title();
                if !loaded_title.is_empty() {
                    title = loaded_title;
                }
                TabInfo {
                    id: tab.id().to_string(),
                    url,
                    title,
                    active: tabs.active.as_deref() == Some(tab.id()),
                    loading: tab.is_loading(),
                }
            })
            .collect();
        drop(tabs);
        self.state.send_if_modified(|state| {
            let changed = *state != list;
            *state = list;
            changed
        });
    }
}

/// Keep [`Shared::state`] current: follow the browser's target events
/// (address, title, tabs the page opened or closed) and the session's own
/// changes.
async fn track_tabs(shared: Arc<Shared>) -> Result<()> {
    let (mut created, mut info_changed, mut destroyed) = {
        let launched = shared.launched.lock().await;
        let browser = &launched.browser;
        (
            browser.event_listener::<EventTargetCreated>().await?,
            browser.event_listener::<EventTargetInfoChanged>().await?,
            browser.event_listener::<EventTargetDestroyed>().await?,
        )
    };
    let mut targets: HashMap<TargetId, (String, String)> = HashMap::new();
    loop {
        // A page target the session does not know yet may be a tab the page
        // opened: try to adopt it.
        let mut unknown_page = |info: &TargetInfo| {
            targets.insert(
                info.target_id.clone(),
                (info.url.clone(), info.title.clone()),
            );
            info.r#type == "page"
                && info.opener_id.is_some()
                && !shared
                    .tabs
                    .lock()
                    .unwrap()
                    .list
                    .iter()
                    .any(|t| *t.page().target_id() == info.target_id)
        };
        let sync = tokio::select! {
            Some(event) = created.next() => unknown_page(&event.target_info),
            Some(event) = info_changed.next() => unknown_page(&event.target_info),
            Some(event) = destroyed.next() => targets.remove(&event.target_id).is_some(),
            _ = shared.changed.notified() => false,
            else => break,
        };
        if sync && let Err(e) = shared.sync_tabs().await {
            tracing::debug!("browser: cannot sync tabs: {e}");
        }
        shared.publish(&targets);
    }
    Ok(())
}

/// One launched browser and its tabs, driven across many tool calls.
pub struct BrowserSession {
    shared: Arc<Shared>,
    label: String,
    /// Whether this is an ephemeral throwaway browser (no persistent profile).
    /// Ephemeral sessions are dropped at the end of an agent turn (see
    /// [`BrowserSessionManager::close_ephemeral`]) so a forgotten
    /// `browser_navigate` on the default profile can't leak a Chrome process;
    /// persistent named profiles survive across turns on purpose.
    ephemeral: bool,
    /// Follows the tabs for [`watch_tabs`](Self::watch_tabs) (aborted on drop).
    tracker: JoinHandle<()>,
    /// The user has taken over; the agent keeps its hands off.
    user_control: AtomicBool,
    /// The user had control since the agent last asked.
    user_interlude: AtomicBool,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.tracker.abort();
    }
}

impl BrowserSession {
    /// Launch a browser for `config` with one blank tab.
    pub async fn open(
        config: crate::browser::BrowserLaunchConfig,
        label: impl Into<String>,
    ) -> Result<Self> {
        let ephemeral = matches!(config.profile, crate::browser::BrowserProfile::Ephemeral);
        let launched = LaunchedBrowser::launch(config).await?;
        let shared = Arc::new(Shared {
            launched: AsyncMutex::new(launched),
            tabs: Mutex::new(Tabs::default()),
            timeouts: Arc::new(Mutex::new(BrowserTimeouts::default())),
            state: watch::channel(Vec::new()).0,
            changed: Arc::new(Notify::new()),
            adopting: AsyncMutex::new(()),
            unreported: Mutex::default(),
        });
        let tracker = tokio::spawn({
            let shared = shared.clone();
            async move {
                if let Err(e) = track_tabs(shared).await {
                    tracing::warn!("browser: tab tracking stopped: {e}");
                }
            }
        });
        let session = Self {
            shared,
            label: label.into(),
            ephemeral,
            tracker,
            user_control: AtomicBool::new(false),
            user_interlude: AtomicBool::new(false),
        };
        session.create_tab(true).await?;
        Ok(session)
    }

    /// Use other limits than [`BrowserTimeouts::default`], for every tab.
    pub fn with_timeouts(self, timeouts: BrowserTimeouts) -> Self {
        *self.shared.timeouts.lock().unwrap() = timeouts;
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether this is an ephemeral throwaway browser (no persistent profile).
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    /// Hand the browser to the user (`true`) or back to the agent.
    pub fn set_user_control(&self, user: bool) {
        self.user_control.store(user, Ordering::Relaxed);
        if user {
            self.user_interlude.store(true, Ordering::Relaxed);
        }
    }

    /// Whether the user is controlling the browser right now.
    pub fn user_in_control(&self) -> bool {
        self.user_control.load(Ordering::Relaxed)
    }

    /// Whether the user had control since the last call: what the agent saw
    /// before may be stale.
    pub fn take_user_interlude(&self) -> bool {
        !self.user_in_control() && self.user_interlude.swap(false, Ordering::Relaxed)
    }

    /// The tabs as they change: address, title, loading, which is active.
    /// Tabs the page opens join without a tool call.
    pub fn watch_tabs(&self) -> watch::Receiver<Vec<TabInfo>> {
        self.shared.state.subscribe()
    }

    /// Open a new blank tab. `foreground` makes it the tab that calls without
    /// a tab id target.
    pub async fn create_tab(&self, foreground: bool) -> Result<Arc<Tab>> {
        self.shared.create_tab(foreground).await
    }

    /// Adopt tabs the page opened itself (`target=_blank`, `window.open`) and
    /// forget tabs that were closed. Returns the ids of newly adopted tabs.
    ///
    /// Only pages with an opener are adopted: Chrome's own initial blank tab
    /// has none and stays out of the list.
    /// Tabs the session adopted on its own since the last call are
    /// returned too.
    pub async fn sync_tabs(&self) -> Result<Vec<String>> {
        self.shared.sync_tabs().await?;
        Ok(std::mem::take(&mut *self.shared.unreported.lock().unwrap()))
    }

    /// The tab `id`, or the active tab when `id` is `None`.
    pub fn tab(&self, id: Option<&str>) -> Result<Arc<Tab>> {
        let tabs = self.shared.tabs.lock().unwrap();
        let id = match id {
            Some(id) => id,
            None => tabs
                .active
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("this browser has no open tab"))?,
        };
        tabs.get(id)
            .ok_or_else(|| anyhow::anyhow!("no tab '{id}' (list them with browser_tabs_context)"))
    }

    /// The active tab.
    pub fn active_tab(&self) -> Result<Arc<Tab>> {
        self.tab(None)
    }

    /// Make tab `id` the one calls without a tab id target.
    pub fn select_tab(&self, id: &str) -> Result<()> {
        let tab = self.tab(Some(id))?;
        self.shared.tabs.lock().unwrap().active = Some(tab.id().to_string());
        self.shared.changed.notify_one();
        Ok(())
    }

    /// Close tab `id`. Closing the active tab activates the most recent other.
    pub async fn close_tab(&self, id: &str) -> Result<()> {
        let tab = self.tab(Some(id))?;
        {
            let mut tabs = self.shared.tabs.lock().unwrap();
            tabs.list.retain(|t| t.id() != id);
            if tabs.active.as_deref() == Some(id) {
                tabs.active = tabs.list.last().map(|t| t.id().to_string());
            }
        }
        self.shared.changed.notify_one();
        tab.page().clone().close().await?;
        Ok(())
    }

    /// The open tabs with their location.
    pub async fn tabs(&self) -> Vec<TabInfo> {
        let (list, active) = {
            let tabs = self.shared.tabs.lock().unwrap();
            (tabs.list.clone(), tabs.active.clone())
        };
        let mut out = Vec::new();
        for tab in list {
            let (url, title) = tab.location().await;
            out.push(TabInfo {
                active: active.as_deref() == Some(tab.id()),
                id: tab.id().to_string(),
                url,
                title,
                loading: tab.is_loading(),
            });
        }
        out
    }

    /// Export the whole cookie jar (shared by all tabs).
    pub async fn export_cookies(&self) -> Result<Vec<CookieParam>> {
        self.active_tab()?.export_cookies().await
    }

    /// Close the browser gracefully so a persistent profile flushes its cookies
    /// to disk. After this the session is dead. Dropping without calling this
    /// still kills the process (via `kill_on_drop`) but skips the flush.
    pub async fn close(&self) {
        self.tracker.abort();
        self.shared.launched.lock().await.close().await;
    }
}

/// Default cap on concurrently tracked browser sessions. Lower than the PTY cap
/// — each session is a whole browser process.
pub const DEFAULT_MAX_SESSIONS: usize = 8;

/// Info about a tracked session, for listing/UI purposes.
pub struct BrowserSessionInfo {
    pub id: u32,
    pub label: String,
}

struct Entry {
    session: Arc<BrowserSession>,
    label: String,
    last_used: Instant,
    /// Open [`ViewGuard`]s: a watched browser is not closed at the turn end.
    viewers: usize,
    /// A turn ended while it was watched: close it when the last viewer lets
    /// go, unless the agent uses it again first.
    close_deferred: bool,
}

/// Id-keyed registry of live [`BrowserSession`]s, one per agent session.
pub struct BrowserSessionManager {
    max_sessions: usize,
    entries: Mutex<HashMap<u32, Entry>>,
    /// Bumped whenever a session is added or removed.
    changes: watch::Sender<u64>,
}

impl Default for BrowserSessionManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SESSIONS)
    }
}

/// Someone (a browser panel) is watching a session. Dropping the last guard
/// closes a throwaway browser whose turn already ended.
pub struct ViewGuard {
    manager: std::sync::Weak<BrowserSessionManager>,
    id: u32,
    runtime: tokio::runtime::Handle,
}

impl Drop for ViewGuard {
    fn drop(&mut self) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let orphaned = {
            let mut entries = manager.entries.lock().unwrap();
            let Some(entry) = entries.get_mut(&self.id) else {
                return;
            };
            entry.viewers = entry.viewers.saturating_sub(1);
            if entry.viewers == 0 && entry.close_deferred {
                entries.remove(&self.id).map(|entry| entry.session)
            } else {
                None
            }
        };
        if let Some(session) = orphaned {
            manager.changed();
            self.runtime.spawn(async move { session.close().await });
        }
    }
}

impl BrowserSessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            max_sessions: max_sessions.max(1),
            entries: Mutex::new(HashMap::new()),
            changes: watch::channel(0).0,
        }
    }

    /// Changes whenever a session is added or removed.
    pub fn watch_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn changed(&self) {
        self.changes.send_modify(|generation| *generation += 1);
    }

    /// Watch the session labelled `label`, keeping it open past the end of
    /// the turn while the guard lives. Must be called on a tokio runtime.
    pub fn view(self: &Arc<Self>, label: &str) -> Option<ViewGuard> {
        let mut entries = self.entries.lock().unwrap();
        let (id, entry) = entries.iter_mut().find(|(_, entry)| entry.label == label)?;
        entry.viewers += 1;
        Some(ViewGuard {
            manager: Arc::downgrade(self),
            id: *id,
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Track a session and return its id. Ids are random (not sequential) so an
    /// id from a restored transcript never silently aliases a fresh session.
    /// Evicting a session at the cap drops its `Arc`; if nothing else holds it,
    /// the browser process is killed via `kill_on_drop`. Watched sessions are
    /// not evicted.
    pub fn register(&self, session: Arc<BrowserSession>, label: impl Into<String>) -> u32 {
        let mut entries = self.entries.lock().unwrap();

        while entries.len() >= self.max_sessions {
            let Some(victim) = Self::lru_victim(&entries) else {
                break;
            };
            entries.remove(&victim);
        }

        let id = loop {
            let candidate = rand::random_range(1_000..100_000u32);
            if !entries.contains_key(&candidate) {
                break candidate;
            }
        };
        let label = label.into();
        entries.insert(
            id,
            Entry {
                session,
                label,
                last_used: Instant::now(),
                viewers: 0,
                close_deferred: false,
            },
        );
        drop(entries);
        self.changed();
        id
    }

    fn lru_victim(entries: &HashMap<u32, Entry>) -> Option<u32> {
        entries
            .iter()
            .filter(|(_, entry)| entry.viewers == 0)
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(id, _)| *id)
    }

    /// Look up a session, refreshing its LRU timestamp.
    pub fn get(&self, id: u32) -> Option<Arc<BrowserSession>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(&id)?;
        Some(Self::touch(entry))
    }

    /// Look up a session by its label, refreshing its LRU timestamp. Tools key
    /// one live browser per profile name, so this is the primary lookup for
    /// them.
    pub fn get_by_label(&self, label: &str) -> Option<Arc<BrowserSession>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.values_mut().find(|entry| entry.label == label)?;
        Some(Self::touch(entry))
    }

    /// A use: refresh the LRU timestamp; the session is in use again, so a
    /// close deferred from an earlier turn is off.
    fn touch(entry: &mut Entry) -> Arc<BrowserSession> {
        entry.last_used = Instant::now();
        entry.close_deferred = false;
        entry.session.clone()
    }

    /// Stop tracking the session with the given label and return it.
    pub fn remove_by_label(&self, label: &str) -> Option<Arc<BrowserSession>> {
        let mut entries = self.entries.lock().unwrap();
        let id = *entries
            .iter()
            .find(|(_, entry)| entry.label == label)
            .map(|(id, _)| id)?;
        let removed = entries.remove(&id).map(|entry| entry.session);
        drop(entries);
        self.changed();
        removed
    }

    /// Stop tracking a session and return it, so the caller can close it
    /// gracefully before dropping.
    pub fn remove(&self, id: u32) -> Option<Arc<BrowserSession>> {
        let removed = self
            .entries
            .lock()
            .unwrap()
            .remove(&id)
            .map(|entry| entry.session);
        if removed.is_some() {
            self.changed();
        }
        removed
    }

    pub fn list(&self) -> Vec<BrowserSessionInfo> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, entry)| BrowserSessionInfo {
                id: *id,
                label: entry.label.clone(),
            })
            .collect()
    }

    /// The tracked sessions with their labels, without counting as a use.
    pub fn sessions(&self) -> Vec<(String, Arc<BrowserSession>)> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|entry| (entry.label.clone(), entry.session.clone()))
            .collect()
    }

    /// Gracefully close and forget every tracked session (flushing profiles).
    pub async fn close_all(&self) {
        let sessions: Vec<Arc<BrowserSession>> = {
            let mut entries = self.entries.lock().unwrap();
            entries.drain().map(|(_, entry)| entry.session).collect()
        };
        self.changed();
        for session in sessions {
            session.close().await;
        }
    }

    /// Gracefully close and forget every *ephemeral* (throwaway) session,
    /// leaving persistent named profiles open. Called at the end of an agent
    /// turn so a forgotten `browser_navigate` on the default profile can't
    /// leak a Chrome process or spam CDP errors between turns. A watched
    /// session closes when its last viewer lets go instead.
    pub async fn close_ephemeral(&self) {
        let sessions: Vec<Arc<BrowserSession>> = {
            let mut entries = self.entries.lock().unwrap();
            let ids: Vec<u32> = entries
                .iter_mut()
                .filter(|(_, entry)| entry.session.is_ephemeral())
                .filter_map(|(id, entry)| {
                    entry.close_deferred = entry.viewers > 0;
                    (entry.viewers == 0).then_some(*id)
                })
                .collect();
            ids.iter()
                .filter_map(|id| entries.remove(id).map(|entry| entry.session))
                .collect()
        };
        if !sessions.is_empty() {
            self.changed();
        }
        for session in sessions {
            session.close().await;
        }
    }
}
