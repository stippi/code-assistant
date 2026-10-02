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
use chromiumoxide::cdp::browser_protocol::target::GetTargetsParams;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

/// A tab as listed for the model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
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

/// One launched browser and its tabs, driven across many tool calls.
pub struct BrowserSession {
    /// Kept alive so the browser process outlives individual tool calls; behind
    /// an async mutex only because a graceful [`close`](Self::close) needs `&mut`.
    launched: AsyncMutex<LaunchedBrowser>,
    tabs: Mutex<Tabs>,
    label: String,
    /// Whether this is an ephemeral throwaway browser (no persistent profile).
    /// Ephemeral sessions are dropped at the end of an agent turn (see
    /// [`BrowserSessionManager::close_ephemeral`]) so a forgotten
    /// `browser_navigate` on the default profile can't leak a Chrome process;
    /// persistent named profiles survive across turns on purpose.
    ephemeral: bool,
    timeouts: Arc<Mutex<BrowserTimeouts>>,
}

impl BrowserSession {
    /// Launch a browser for `config` with one blank tab.
    pub async fn open(
        config: crate::browser::BrowserLaunchConfig,
        label: impl Into<String>,
    ) -> Result<Self> {
        let ephemeral = matches!(config.profile, crate::browser::BrowserProfile::Ephemeral);
        let launched = LaunchedBrowser::launch(config).await?;
        let session = Self {
            launched: AsyncMutex::new(launched),
            tabs: Mutex::new(Tabs::default()),
            label: label.into(),
            ephemeral,
            timeouts: Arc::new(Mutex::new(BrowserTimeouts::default())),
        };
        session.create_tab(true).await?;
        Ok(session)
    }

    /// Use other limits than [`BrowserTimeouts::default`], for every tab.
    pub fn with_timeouts(self, timeouts: BrowserTimeouts) -> Self {
        *self.timeouts.lock().unwrap() = timeouts;
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether this is an ephemeral throwaway browser (no persistent profile).
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    /// Open a new blank tab. `foreground` makes it the tab that calls without
    /// a tab id target.
    pub async fn create_tab(&self, foreground: bool) -> Result<Arc<Tab>> {
        let page = self
            .launched
            .lock()
            .await
            .browser
            .new_page("about:blank")
            .await?;
        let id = self.tabs.lock().unwrap().mint_id();
        let tab = Arc::new(Tab::new(id.clone(), page, self.timeouts.clone()).await?);
        let mut tabs = self.tabs.lock().unwrap();
        tabs.list.push(tab.clone());
        if foreground || tabs.active.is_none() {
            tabs.active = Some(id);
        }
        Ok(tab)
    }

    /// Adopt tabs the page opened itself (`target=_blank`, `window.open`) and
    /// forget tabs that were closed. Returns the ids of newly adopted tabs.
    ///
    /// Only pages with an opener are adopted: Chrome's own initial blank tab
    /// has none and stays out of the list.
    pub async fn sync_tabs(&self) -> Result<Vec<String>> {
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
            let tab = Arc::new(Tab::new(id.clone(), page.clone(), self.timeouts.clone()).await?);
            self.tabs.lock().unwrap().list.push(tab);
            adopted.push(id);
        }
        let open: Vec<_> = pages.iter().map(|p| p.target_id().clone()).collect();
        let mut tabs = self.tabs.lock().unwrap();
        tabs.list.retain(|t| open.contains(t.page().target_id()));
        let active_gone = tabs
            .active
            .as_deref()
            .is_some_and(|id| tabs.get(id).is_none());
        if active_gone {
            tabs.active = tabs.list.last().map(|t| t.id().to_string());
        }
        Ok(adopted)
    }

    /// The tab `id`, or the active tab when `id` is `None`.
    pub fn tab(&self, id: Option<&str>) -> Result<Arc<Tab>> {
        let tabs = self.tabs.lock().unwrap();
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
        self.tabs.lock().unwrap().active = Some(tab.id().to_string());
        Ok(())
    }

    /// Close tab `id`. Closing the active tab activates the most recent other.
    pub async fn close_tab(&self, id: &str) -> Result<()> {
        let tab = self.tab(Some(id))?;
        {
            let mut tabs = self.tabs.lock().unwrap();
            tabs.list.retain(|t| t.id() != id);
            if tabs.active.as_deref() == Some(id) {
                tabs.active = tabs.list.last().map(|t| t.id().to_string());
            }
        }
        tab.page().clone().close().await?;
        Ok(())
    }

    /// The open tabs with their location.
    pub async fn tabs(&self) -> Vec<TabInfo> {
        let (list, active) = {
            let tabs = self.tabs.lock().unwrap();
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
        self.launched.lock().await.close().await;
    }

    // Transitional: the verbs below act on the active tab, for callers that
    // predate tabs. They go away with the selector-based tools.

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.active_tab()?.navigate(url).await
    }
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        self.active_tab()?.screenshot(full_page).await
    }
    pub async fn scroll(&self, selector: Option<&str>, dx: f64, dy: f64) -> Result<()> {
        self.active_tab()?.scroll(selector, dx, dy).await
    }
    pub async fn observe(&self) -> Result<crate::PageObservation> {
        self.active_tab()?.observe().await
    }
    pub async fn observe_with(&self, include_text: bool) -> Result<crate::PageObservation> {
        self.active_tab()?.observe_with(include_text).await
    }
    pub async fn viewport_size(&self) -> Result<(f64, f64)> {
        self.active_tab()?.viewport_size().await
    }
    pub async fn click(&self, selector: &str) -> Result<()> {
        self.active_tab()?.click(selector).await
    }
    pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        self.active_tab()?.click_at(x, y).await
    }
    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
        self.active_tab()?.move_mouse(x, y).await
    }
    pub async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        self.active_tab()?.type_text(selector, text).await
    }
    pub async fn fill(&self, selector: &str, text: &str) -> Result<()> {
        self.active_tab()?.fill(selector, text).await
    }
    pub async fn clear(&self, selector: &str) -> Result<()> {
        self.active_tab()?.clear(selector).await
    }
    pub async fn press_key(&self, selector: &str, key: &str) -> Result<()> {
        self.active_tab()?.press_key(selector, key).await
    }
    pub async fn press_key_global(&self, key: &str) -> Result<()> {
        self.active_tab()?.press_key_global(key).await
    }
    pub async fn settle(&self) {
        if let Ok(tab) = self.active_tab() {
            tab.settle().await;
        }
    }
    pub async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<bool> {
        self.active_tab()?.wait_for(selector, timeout).await
    }
    pub async fn eval(&self, js: &str) -> Result<serde_json::Value> {
        self.active_tab()?.eval(js).await
    }
    pub async fn import_cookies(&self, cookies: Vec<CookieParam>) -> Result<()> {
        self.active_tab()?.import_cookies(cookies).await
    }
    pub fn set_accept_dialogs(&self, accept: bool) {
        if let Ok(tab) = self.active_tab() {
            tab.set_accept_dialogs(accept);
        }
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
}

/// Id-keyed registry of live [`BrowserSession`]s, one per agent session.
pub struct BrowserSessionManager {
    max_sessions: usize,
    entries: Mutex<HashMap<u32, Entry>>,
}

impl Default for BrowserSessionManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SESSIONS)
    }
}

impl BrowserSessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            max_sessions: max_sessions.max(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Track a session and return its id. Ids are random (not sequential) so an
    /// id from a restored transcript never silently aliases a fresh session.
    /// Evicting a session at the cap drops its `Arc`; if nothing else holds it,
    /// the browser process is killed via `kill_on_drop`.
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
            },
        );
        id
    }

    fn lru_victim(entries: &HashMap<u32, Entry>) -> Option<u32> {
        entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(id, _)| *id)
    }

    /// Look up a session, refreshing its LRU timestamp.
    pub fn get(&self, id: u32) -> Option<Arc<BrowserSession>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(&id)?;
        entry.last_used = Instant::now();
        Some(entry.session.clone())
    }

    /// Look up a session by its label, refreshing its LRU timestamp. Tools key
    /// one live browser per profile name, so this is the primary lookup for
    /// them.
    pub fn get_by_label(&self, label: &str) -> Option<Arc<BrowserSession>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.values_mut().find(|entry| entry.label == label)?;
        entry.last_used = Instant::now();
        Some(entry.session.clone())
    }

    /// Stop tracking the session with the given label and return it.
    pub fn remove_by_label(&self, label: &str) -> Option<Arc<BrowserSession>> {
        let mut entries = self.entries.lock().unwrap();
        let id = *entries
            .iter()
            .find(|(_, entry)| entry.label == label)
            .map(|(id, _)| id)?;
        entries.remove(&id).map(|entry| entry.session)
    }

    /// Stop tracking a session and return it, so the caller can close it
    /// gracefully before dropping.
    pub fn remove(&self, id: u32) -> Option<Arc<BrowserSession>> {
        self.entries
            .lock()
            .unwrap()
            .remove(&id)
            .map(|entry| entry.session)
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

    /// Gracefully close and forget every tracked session (flushing profiles).
    pub async fn close_all(&self) {
        let sessions: Vec<Arc<BrowserSession>> = {
            let mut entries = self.entries.lock().unwrap();
            entries.drain().map(|(_, entry)| entry.session).collect()
        };
        for session in sessions {
            session.close().await;
        }
    }

    /// Gracefully close and forget every *ephemeral* (throwaway) session,
    /// leaving persistent named profiles open. Called at the end of an agent
    /// turn so a forgotten `browser_navigate` on the default profile can't
    /// leak a Chrome process or spam CDP errors between turns.
    pub async fn close_ephemeral(&self) {
        let sessions: Vec<Arc<BrowserSession>> = {
            let mut entries = self.entries.lock().unwrap();
            let ids: Vec<u32> = entries
                .iter()
                .filter(|(_, entry)| entry.session.is_ephemeral())
                .map(|(id, _)| *id)
                .collect();
            ids.iter()
                .filter_map(|id| entries.remove(id).map(|entry| entry.session))
                .collect()
        };
        for session in sessions {
            session.close().await;
        }
    }
}
