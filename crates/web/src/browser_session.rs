//! Interactive browser sessions for agent tools.
//!
//! [`crate::WebClient`] covers the one-shot case: fetch a page, extract it,
//! discard it. Browser *agency* needs the opposite — a page the agent drives
//! over many tool calls: navigate, look (screenshot / read), click, type, wait.
//!
//! This mirrors the `pty_session` crate one-to-one:
//! - [`BrowserSession`] — one live page on a launched browser, with the
//!   interaction verbs, kept across tool calls.
//! - [`BrowserSessionManager`] — an id-keyed registry with an LRU cap, one per
//!   agent session, so browser sessions survive across tool calls but die with
//!   their agent session.

use crate::browser::LaunchedBrowser;
use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::element::Element;
use chromiumoxide::page::{Page, ScreenshotParams};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

/// What the model sees after acting: where it is and what's on the page.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PageObservation {
    pub url: String,
    pub title: String,
    /// Visible text (`document.body.innerText`), the cheap textual companion to
    /// a screenshot.
    pub text: String,
}

/// One live page on a launched browser, driven across many tool calls.
pub struct BrowserSession {
    /// Kept alive so the browser process outlives individual tool calls; behind
    /// an async mutex only because a graceful [`close`](Self::close) needs `&mut`.
    launched: AsyncMutex<LaunchedBrowser>,
    /// The page every interaction targets. `Page` is internally reference
    /// counted and its methods take `&self`, so all verbs below are `&self`.
    page: Page,
    label: String,
}

impl BrowserSession {
    /// Launch a browser for `config` and open a blank page to drive.
    pub async fn open(
        config: crate::browser::BrowserLaunchConfig,
        label: impl Into<String>,
    ) -> Result<Self> {
        let launched = LaunchedBrowser::launch(config).await?;
        let page = launched.browser.new_page("about:blank").await?;
        Ok(Self {
            launched: AsyncMutex::new(launched),
            page,
            label: label.into(),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Navigate to a URL and wait for the load to settle.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.page.goto(url).await?;
        self.page.wait_for_navigation().await?;
        Ok(())
    }

    /// Capture a PNG screenshot — the model's eyes. `full_page` captures the
    /// entire scrollable page instead of just the current viewport.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(full_page)
            .build();
        Ok(self.page.screenshot(params).await?)
    }

    /// Scroll the page. With a `selector`, scroll that element into view;
    /// otherwise scroll by `(dx, dy)` pixels relative to the current position
    /// (positive `dy` scrolls down). The selector is JSON-encoded into the
    /// script, so it cannot break out of the string.
    pub async fn scroll(&self, selector: Option<&str>, dx: f64, dy: f64) -> Result<()> {
        match selector {
            Some(sel) => {
                let sel_json = serde_json::to_string(sel)?;
                let js = format!(
                    "(() => {{ const e = document.querySelector({sel_json}); \
                     if (!e) return false; \
                     e.scrollIntoView({{block: 'center', inline: 'center'}}); \
                     return true; }})()"
                );
                let found = self
                    .page
                    .evaluate(js)
                    .await?
                    .into_value::<bool>()
                    .unwrap_or(false);
                if !found {
                    anyhow::bail!("no element matches selector '{sel}'");
                }
            }
            None => {
                self.page
                    .evaluate(format!("window.scrollBy({dx}, {dy})"))
                    .await?;
            }
        }
        Ok(())
    }

    /// Read the current location, title, and visible text.
    pub async fn observe(&self) -> Result<PageObservation> {
        let url = self.page.url().await?.unwrap_or_default();
        let title = self.page.get_title().await?.unwrap_or_default();
        let text = self
            .page
            .evaluate("document.body ? document.body.innerText : ''")
            .await?
            .into_value::<String>()
            .unwrap_or_default();
        Ok(PageObservation { url, title, text })
    }

    /// Find an element, turning chromiumoxide's opaque CDP miss ("Could not
    /// find node with given id") into a message that names the selector.
    async fn find(&self, selector: &str) -> Result<Element> {
        self.page
            .find_element(selector)
            .await
            .map_err(|_| anyhow::anyhow!("no element matches selector '{selector}'"))
    }

    /// Click the first element matching a CSS selector.
    pub async fn click(&self, selector: &str) -> Result<()> {
        self.find(selector).await?.click().await?;
        Ok(())
    }

    /// Focus a field and type text into it. Never used for credentials — those
    /// go through the human-in-the-loop login handoff.
    pub async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        let element = self.find(selector).await?;
        element.focus().await?;
        element.type_str(text).await?;
        Ok(())
    }

    /// Press a key (e.g. `"Enter"`) on the element matching a selector.
    pub async fn press_key(&self, selector: &str, key: &str) -> Result<()> {
        self.find(selector).await?.press_key(key).await?;
        Ok(())
    }

    /// Wait (bounded) for the page to reach a stable state after a navigation
    /// or an action that may have triggered one.
    ///
    /// Fixes the race where [`observe`](Self::observe) reads an empty body
    /// while a new document is still loading: a short head start lets a
    /// click-triggered navigation actually begin, then we poll until
    /// `document.readyState` is `complete`. An eval failure (the execution
    /// context is torn down mid-navigation) counts as "not ready yet"; the
    /// deadline bounds the wait so a perpetually-loading page can't hang us.
    pub async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let start = Instant::now();
        let deadline = Duration::from_millis(3000);
        loop {
            let complete = self
                .page
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|r| r.into_value::<String>().ok())
                .as_deref()
                == Some("complete");
            if complete || start.elapsed() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    /// Poll until an element matching `selector` exists or `timeout` elapses.
    /// Returns whether it appeared. A poll loop (not a CDP wait) so it can never
    /// hang past the deadline.
    pub async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<bool> {
        let start = Instant::now();
        loop {
            if self.page.find_element(selector).await.is_ok() {
                return Ok(true);
            }
            if start.elapsed() >= timeout {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Evaluate a JavaScript expression in the page and return its JSON value.
    pub async fn eval(&self, js: &str) -> Result<serde_json::Value> {
        let result = self.page.evaluate(js).await?;
        Ok(result.into_value().unwrap_or(serde_json::Value::Null))
    }

    /// Close the browser gracefully so a persistent profile flushes its cookies
    /// to disk. After this the session is dead. Dropping without calling this
    /// still kills the process (via `kill_on_drop`) but skips the flush.
    pub async fn close(&self) {
        self.launched.lock().await.close().await;
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
}
