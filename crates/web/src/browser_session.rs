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
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, CookieSameSite, TimeSinceEpoch};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::element::Element;
use chromiumoxide::layout::Point;
use chromiumoxide::page::{Page, ScreenshotParams};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

/// JS that discovers the actionable elements on the page and returns them as an
/// array of `{selector, role, label}`. Best-effort: it prefers `#id` selectors,
/// falls back to an `:nth-of-type` path, skips hidden/disabled elements, and is
/// bounded so a huge page can't blow up the observation.
const DISCOVER_ELEMENTS_JS: &str = r#"
(() => {
  const MAX = 40;
  const SEL = 'a,button,input,textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=menuitem],[onclick],[tabindex]';
  const seen = new Set();
  const out = [];

  const visible = (el) => {
    if (el.disabled) return false;
    const rects = el.getClientRects();
    if (!rects.length) return false;
    const r = rects[0];
    if (r.width < 1 || r.height < 1) return false;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none') return false;
    return true;
  };

  const cssPath = (el) => {
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1 && node.tagName !== 'HTML') {
      let sel = node.tagName.toLowerCase();
      if (node.id) { parts.unshift('#' + CSS.escape(node.id)); break; }
      const parent = node.parentNode;
      if (parent) {
        const sameTag = Array.from(parent.children).filter(c => c.tagName === node.tagName);
        if (sameTag.length > 1) {
          sel += ':nth-of-type(' + (sameTag.indexOf(node) + 1) + ')';
        }
      }
      parts.unshift(sel);
      node = node.parentNode;
    }
    return parts.join(' > ');
  };

  const roleOf = (el) => {
    const r = el.getAttribute('role');
    if (r) return r;
    const tag = el.tagName.toLowerCase();
    if (tag === 'input') return (el.getAttribute('type') || 'text');
    return tag;
  };

  const labelOf = (el) => {
    const pick = (s) => (s || '').replace(/\s+/g, ' ').trim();
    let l = pick(el.getAttribute('aria-label'));
    if (!l) l = pick(el.textContent);
    if (!l) l = pick(el.value);
    if (!l) l = pick(el.getAttribute('placeholder'));
    if (!l) l = pick(el.getAttribute('name'));
    if (!l) l = pick(el.getAttribute('alt'));
    if (!l) l = pick(el.getAttribute('title'));
    return l.slice(0, 80);
  };

  for (const el of document.querySelectorAll(SEL)) {
    if (out.length >= MAX) break;
    if (!visible(el)) continue;
    const selector = cssPath(el);
    if (!selector || seen.has(selector)) continue;
    seen.add(selector);
    out.push({ selector, role: roleOf(el), label: labelOf(el) });
  }
  return out;
})()
"#;

/// One actionable element discovered on the page, so the model can target it by
/// selector instead of guessing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InteractiveElement {
    /// A CSS selector that resolves to this element (`#id` when available, else
    /// an `:nth-of-type` path).
    pub selector: String,
    /// The element's ARIA role or tag name (button, a, input, checkbox, …).
    pub role: String,
    /// A short human label: visible text, aria-label, placeholder, name, …
    pub label: String,
}

/// What the model sees after acting: where it is and what's on the page.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PageObservation {
    pub url: String,
    pub title: String,
    /// Visible text (`document.body.innerText`), the cheap textual companion to
    /// a screenshot.
    pub text: String,
    /// Actionable elements (bounded), so the model targets real selectors
    /// instead of guessing from the screenshot.
    #[serde(default)]
    pub elements: Vec<InteractiveElement>,
    /// Viewport size in CSS pixels (`window.innerWidth`/`innerHeight`). Disclosed
    /// so the model can express coordinate clicks in `px` — it cannot read the
    /// true size off a screenshot the API has already resized.
    #[serde(default)]
    pub viewport_width: f64,
    #[serde(default)]
    pub viewport_height: f64,
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
    /// Whether this is an ephemeral throwaway browser (no persistent profile).
    /// Ephemeral sessions are dropped at the end of an agent turn (see
    /// [`BrowserSessionManager::close_ephemeral`]) so a forgotten
    /// `browser_navigate` on the default profile can't leak a Chrome process;
    /// persistent named profiles survive across turns on purpose.
    ephemeral: bool,
}

impl BrowserSession {
    /// Launch a browser for `config` and open a blank page to drive.
    pub async fn open(
        config: crate::browser::BrowserLaunchConfig,
        label: impl Into<String>,
    ) -> Result<Self> {
        let ephemeral = matches!(config.profile, crate::browser::BrowserProfile::Ephemeral);
        let launched = LaunchedBrowser::launch(config).await?;
        let page = launched.browser.new_page("about:blank").await?;
        Ok(Self {
            launched: AsyncMutex::new(launched),
            page,
            label: label.into(),
            ephemeral,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether this is an ephemeral throwaway browser (no persistent profile).
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral
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

    /// Read the current location, title, visible text, and the actionable
    /// elements on the page.
    pub async fn observe(&self) -> Result<PageObservation> {
        let url = self.page.url().await?.unwrap_or_default();
        let title = self.page.get_title().await?.unwrap_or_default();
        let text = self
            .page
            .evaluate("document.body ? document.body.innerText : ''")
            .await?
            .into_value::<String>()
            .unwrap_or_default();
        // Element discovery is best-effort: a failure (e.g. mid-navigation)
        // just yields an empty list rather than failing the observation.
        let elements = match self.page.evaluate(DISCOVER_ELEMENTS_JS).await {
            Ok(v) => v
                .into_value::<Vec<InteractiveElement>>()
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let (viewport_width, viewport_height) = self.viewport_size().await.unwrap_or((0.0, 0.0));
        Ok(PageObservation {
            url,
            title,
            text,
            elements,
            viewport_width,
            viewport_height,
        })
    }

    /// The viewport size in CSS pixels — the reference frame for coordinate
    /// clicks (CDP mouse events use CSS pixels, so a coordinate resolved against
    /// this lands where intended regardless of screenshot scaling or DPR).
    pub async fn viewport_size(&self) -> Result<(f64, f64)> {
        let dims = self
            .page
            .evaluate("[window.innerWidth, window.innerHeight]")
            .await?
            .into_value::<(f64, f64)>()
            .unwrap_or((0.0, 0.0));
        Ok(dims)
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

    /// Click at viewport coordinates `(x, y)`. For canvas/WebGL surfaces and
    /// anything without a stable selector (games, maps, drag targets).
    pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        self.page.click(Point { x, y }).await?;
        Ok(())
    }

    /// Move the mouse to viewport coordinates `(x, y)` without clicking — drives
    /// hover states and canvas pointer-move handlers.
    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
        self.page.move_mouse(Point { x, y }).await?;
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

    /// Press a key (e.g. `"Enter"`) on the element matching a selector. The
    /// element is focused first so the key event lands on it — CDP dispatches
    /// key events to whatever currently has focus, not to a named node.
    pub async fn press_key(&self, selector: &str, key: &str) -> Result<()> {
        let element = self.find(selector).await?;
        element.focus().await?;
        element.press_key(key).await?;
        Ok(())
    }

    /// Press a key without targeting a selector — it goes to whatever element
    /// currently has focus (e.g. arrow keys for a focused game canvas). Routed
    /// through `<body>` only because CDP needs a node to dispatch from; the key
    /// still lands on the focused element.
    pub async fn press_key_global(&self, key: &str) -> Result<()> {
        self.find("body").await?.press_key(key).await?;
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

    /// Export the whole cookie jar (all domains), including in-memory **session
    /// cookies** that a graceful close does *not* flush to disk. Used to carry a
    /// login across a headful→headless relaunch on the same profile without the
    /// user having to authenticate again.
    pub async fn export_cookies(&self) -> Result<Vec<CookieParam>> {
        let resp = self.page.execute(GetAllCookiesRaw {}).await?;
        Ok(resp
            .result
            .cookies
            .iter()
            .filter_map(cookie_to_param)
            .collect())
    }

    /// Re-inject cookies captured by [`export_cookies`](Self::export_cookies).
    /// The page must already be on an `http(s)` URL (CDP rejects setting cookies
    /// from `about:blank`/`data:`); each cookie also carries its own url/domain,
    /// so cross-domain (SSO) cookies restore correctly. A reload afterwards makes
    /// them take effect. No-op for an empty jar.
    pub async fn import_cookies(&self, cookies: Vec<CookieParam>) -> Result<()> {
        if cookies.is_empty() {
            return Ok(());
        }
        self.page.set_cookies(cookies).await?;
        Ok(())
    }

    /// Close the browser gracefully so a persistent profile flushes its cookies
    /// to disk. After this the session is dead. Dropping without calling this
    /// still kills the process (via `kill_on_drop`) but skips the flush.
    pub async fn close(&self) {
        self.launched.lock().await.close().await;
    }
}

/// Raw `Network.getAllCookies` command. We bypass chromiumoxide's typed `Cookie`
/// because its 0.5.2 CDP bindings require a `sameParty` field that current Chrome
/// no longer sends, which fails deserialization. A lenient struct (everything
/// `#[serde(default)]`) tolerates that protocol drift.
#[derive(serde::Serialize)]
struct GetAllCookiesRaw {}

impl chromiumoxide::Method for GetAllCookiesRaw {
    fn identifier(&self) -> chromiumoxide::types::MethodId {
        "Network.getAllCookies".into()
    }
}

impl chromiumoxide::Command for GetAllCookiesRaw {
    type Response = RawCookies;
}

#[derive(Debug, serde::Deserialize)]
struct RawCookies {
    #[serde(default)]
    cookies: Vec<RawCookie>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCookie {
    name: String,
    value: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    expires: f64,
    #[serde(default)]
    http_only: bool,
    #[serde(default)]
    secure: bool,
    #[serde(default)]
    session: bool,
    #[serde(default)]
    same_site: Option<String>,
}

/// Map a read-back cookie to a settable [`CookieParam`], preserving the fields
/// that matter for re-injection. A leading-dot domain is kept as-is for the
/// `domain` field but stripped for the `url` host. Session cookies (no expiry)
/// are re-injected without an `expires`, so they stay session cookies.
fn cookie_to_param(c: &RawCookie) -> Option<CookieParam> {
    let host = c.domain.trim_start_matches('.');
    if host.is_empty() {
        return None;
    }
    let url = format!("http{}://{}", if c.secure { "s" } else { "" }, host);
    let mut builder = CookieParam::builder()
        .name(c.name.clone())
        .value(c.value.clone())
        .url(url)
        .domain(c.domain.clone())
        .path(c.path.clone())
        .secure(c.secure)
        .http_only(c.http_only);
    if let Some(same_site) = c.same_site.as_deref().and_then(parse_same_site) {
        builder = builder.same_site(same_site);
    }
    if !c.session && c.expires > 0.0 {
        builder = builder.expires(TimeSinceEpoch::new(c.expires));
    }
    builder.build().ok()
}

fn parse_same_site(s: &str) -> Option<CookieSameSite> {
    match s {
        "Strict" => Some(CookieSameSite::Strict),
        "Lax" => Some(CookieSameSite::Lax),
        "None" => Some(CookieSameSite::None),
        _ => None,
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
