//! One browser tab: a live page the agent drives across tool calls, with the
//! state that belongs to it (answered dialogs, the timeouts it runs under).
//!
//! [`crate::BrowserSession`] owns the browser and its tabs; every verb that
//! touches a page lives here.

use crate::ax_tree::{GetFullAxTreeRaw, RefMap, RenderOptions};
use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetContentQuadsParams, ScrollIntoViewIfNeededParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, InsertTextParams,
};
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, CookieSameSite, TimeSinceEpoch};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, DialogType, EventJavascriptDialogOpening, HandleJavaScriptDialogParams,
};
use chromiumoxide::element::Element;
use chromiumoxide::keys::{KeyDefinition, get_key_definition};
use chromiumoxide::layout::Point;
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// JS that discovers the actionable elements on the page and returns them as an
/// array of `{selector, role, label}`. Best-effort: it prefers `#id` selectors,
/// falls back to an `:nth-of-type` path, skips hidden/disabled elements, and is
/// bounded so a huge page can't blow up the observation.
///
/// It also descends into open shadow roots and puts elements that live inside a
/// modal/dialog (or a fixed high-z-index overlay) first, so a dialog's buttons
/// are never dropped by the cap when the page behind it is long.
const DISCOVER_ELEMENTS_JS: &str = r#"
(() => {
  const MAX = 200;
  const SEL = 'a,button,input,textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=menuitem],[role=menuitemcheckbox],[role=menuitemradio],[role=switch],[role=option],[contenteditable=true],[onclick],[tabindex]';
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
      if (parent && parent.children) {
        const sameTag = Array.from(parent.children).filter(c => c.tagName === node.tagName);
        if (sameTag.length > 1) {
          sel += ':nth-of-type(' + (sameTag.indexOf(node) + 1) + ')';
        }
      }
      parts.unshift(sel);
      node = node.parentNode && node.parentNode.host ? node.parentNode.host : node.parentNode;
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

  // Rank: elements inside a dialog / high-z fixed overlay first, so the
  // topmost interactive surface is never truncated away.
  const inDialog = (el) => {
    let node = el;
    while (node && node.nodeType === 1) {
      const role = node.getAttribute && node.getAttribute('role');
      if (node.tagName === 'DIALOG' || role === 'dialog' || role === 'alertdialog' || node.getAttribute && node.getAttribute('aria-modal') === 'true') {
        return true;
      }
      if (node.parentNode && node.parentNode.host) { node = node.parentNode.host; continue; }
      node = node.parentNode;
    }
    return false;
  };

  // Gather across the main document and any open shadow roots.
  const collect = (root, acc) => {
    let nodes = [];
    try { nodes = Array.from(root.querySelectorAll(SEL)); } catch (e) {}
    for (const el of nodes) acc.push(el);
    let all = [];
    try { all = Array.from(root.querySelectorAll('*')); } catch (e) {}
    for (const el of all) {
      if (el.shadowRoot) collect(el.shadowRoot, acc);
    }
  };

  const candidates = [];
  collect(document, candidates);

  // Stable sort: dialog elements first, keeping document order otherwise.
  const ranked = candidates
    .map((el, i) => ({ el, i, dlg: inDialog(el) ? 0 : 1 }))
    .sort((a, b) => (a.dlg - b.dlg) || (a.i - b.i));

  for (const { el } of ranked) {
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

/// A JavaScript dialog (`alert` / `confirm` / `prompt` / `beforeunload`) the
/// session answered on its own, reported so the model knows it happened.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HandledDialog {
    pub kind: String,
    pub message: String,
    /// Whether the dialog was accepted (OK) or dismissed (Cancel).
    pub accepted: bool,
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
    /// Dialogs answered since the previous observation.
    #[serde(default)]
    pub dialogs: Vec<HandledDialog>,
}

/// How long a [`BrowserSession`] waits for the page before giving up.
#[derive(Debug, Clone, Copy)]
pub struct BrowserTimeouts {
    /// One interaction: a click, a read, a screenshot, a script.
    pub command: Duration,
    /// Loading a page until its `load` event.
    pub navigation: Duration,
}

impl Default for BrowserTimeouts {
    fn default() -> Self {
        Self {
            command: Duration::from_secs(15),
            navigation: Duration::from_secs(30),
        }
    }
}

/// The page did not answer within its limit: it is busy, hung, or (for a
/// navigation) still loading something. Callers can downcast an
/// `anyhow::Error` to this to tell a stuck page from a failed action.
#[derive(Debug)]
pub struct BrowserTimeout {
    pub what: &'static str,
    pub after: Duration,
}

impl std::fmt::Display for BrowserTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} timed out after {}s",
            self.what,
            self.after.as_secs_f64()
        )
    }
}

impl std::error::Error for BrowserTimeout {}

/// One tab of a [`crate::BrowserSession`].
pub struct Tab {
    id: String,
    /// The page every interaction targets. `Page` is internally reference
    /// counted and its methods take `&self`, so all verbs below are `&self`.
    page: Page,
    /// Dialogs answered since the last observation (drained by `observe`).
    dialogs: Arc<Mutex<Vec<HandledDialog>>>,
    /// Whether `confirm`/`prompt` dialogs are accepted rather than dismissed.
    accept_dialogs: Arc<AtomicBool>,
    /// Answers dialogs as they open (aborted on drop).
    dialog_task: JoinHandle<()>,
    /// Shared with the session, so changing its limits reaches every tab.
    timeouts: Arc<Mutex<BrowserTimeouts>>,
    /// `ref_N` handles handed out by `read_page`/`find`.
    refs: Mutex<RefMap>,
}

impl Tab {
    /// Take over `page` as the tab `id`, answering its dialogs from now on.
    pub(crate) async fn new(
        id: String,
        page: Page,
        timeouts: Arc<Mutex<BrowserTimeouts>>,
    ) -> Result<Self> {
        let dialogs = Arc::new(Mutex::new(Vec::new()));
        let accept_dialogs = Arc::new(AtomicBool::new(false));
        let dialog_task =
            spawn_dialog_handler(&page, dialogs.clone(), accept_dialogs.clone()).await?;
        Ok(Self {
            id,
            page,
            dialogs,
            accept_dialogs,
            dialog_task,
            timeouts,
            refs: Mutex::new(RefMap::default()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn page(&self) -> &Page {
        &self.page
    }

    /// The tab's URL and title, best effort (empty while it is unresponsive).
    pub async fn location(&self) -> (String, String) {
        let read = async {
            let url = self.page.url().await.ok().flatten().unwrap_or_default();
            let title = self
                .page
                .get_title()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            (url, title)
        };
        tokio::time::timeout(self.timeouts().command, read)
            .await
            .unwrap_or_default()
    }

    fn timeouts(&self) -> BrowserTimeouts {
        *self.timeouts.lock().unwrap()
    }

    /// Run one interaction with the page, giving up after `limit`.
    ///
    /// chromiumoxide's own per-command timeout does not hold when the
    /// renderer is stuck: a click on a hung page was measured to block for
    /// minutes. Bounding each verb here keeps a busy or broken page from
    /// stalling the agent.
    async fn bounded<T>(
        &self,
        what: &'static str,
        limit: Duration,
        interaction: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        match tokio::time::timeout(limit, interaction).await {
            Ok(result) => result,
            Err(_) => Err(BrowserTimeout { what, after: limit }.into()),
        }
    }

    /// The page's accessibility tree as YAML-style lines, `- role "name"
    /// [ref_N] attrs`. `interactive_only` gives a flat list of actionable
    /// elements; `root_ref` limits it to that element's subtree.
    pub async fn read_page(
        &self,
        interactive_only: bool,
        root_ref: Option<&str>,
        max_depth: usize,
    ) -> Result<Vec<String>> {
        self.bounded("reading the page", self.timeouts().command, async {
            let tree = self.page.execute(GetFullAxTreeRaw {}).await?.result;
            let opts = RenderOptions {
                interactive_only,
                root_ref,
                max_depth,
            };
            crate::ax_tree::render(&tree.nodes, &mut self.refs.lock().unwrap(), &opts)
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    /// Lines of the accessibility tree (role, name, ref, attributes) that
    /// contain `query`, case-insensitively — at most `limit`.
    pub async fn find(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        let needle = query.to_lowercase();
        let lines = self.read_page(false, None, usize::MAX).await?;
        Ok(lines
            .into_iter()
            .map(|l| l.trim_start().to_string())
            .filter(|l| l.to_lowercase().contains(&needle))
            .take(limit)
            .collect())
    }

    fn backend_node(&self, r: &str) -> Result<BackendNodeId> {
        self.refs
            .lock()
            .unwrap()
            .backend_id(r)
            .map(BackendNodeId::new)
            .ok_or_else(|| anyhow::anyhow!("unknown ref '{r}' (read the page to get current refs)"))
    }

    /// Scroll the element `r` into view if it is not.
    pub async fn scroll_to_ref(&self, r: &str) -> Result<()> {
        let node = self.backend_node(r)?;
        self.bounded("scrolling into view", self.timeouts().command, async {
            self.page
                .execute(
                    ScrollIntoViewIfNeededParams::builder()
                        .backend_node_id(node)
                        .build(),
                )
                .await
                .map_err(|_| stale_ref(r))?;
            Ok(())
        })
        .await
    }

    /// The viewport point (CSS px) at the center of element `r`, scrolled into
    /// view first — where a click on it lands.
    pub async fn ref_point(&self, r: &str) -> Result<Point> {
        self.scroll_to_ref(r).await?;
        let node = self.backend_node(r)?;
        self.bounded("locating an element", self.timeouts().command, async {
            let quads = self
                .page
                .execute(
                    GetContentQuadsParams::builder()
                        .backend_node_id(node)
                        .build(),
                )
                .await
                .map_err(|_| anyhow::anyhow!("{r} is not visible on the page"))?
                .result
                .quads;
            let quad = quads
                .first()
                .map(|q| q.inner().clone())
                .filter(|q| q.len() == 8)
                .ok_or_else(|| anyhow::anyhow!("{r} is not visible on the page"))?;
            Ok(Point {
                x: (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0,
                y: (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0,
            })
        })
        .await
    }

    /// Typing presses a key per character, so long text gets more time.
    fn typing_limit(&self, text: &str) -> Duration {
        self.timeouts().command + Duration::from_millis(20) * text.chars().count() as u32
    }

    /// Accept (`true`) or dismiss (`false`, the default) `confirm` and `prompt`
    /// dialogs from now on. `alert` and `beforeunload` are always accepted: an
    /// alert has nothing to decide, and a `beforeunload` prompt only appears
    /// when leaving the page was already requested.
    pub fn set_accept_dialogs(&self, accept: bool) {
        self.accept_dialogs.store(accept, Ordering::Relaxed);
    }

    /// Navigate to a URL and wait for its `load` event. (`goto` already waits;
    /// a further `wait_for_navigation` has no timeout and could hang on a page
    /// that starts a script redirect right after loading.)
    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.bounded("navigation", self.timeouts().navigation, async {
            self.page.goto(url).await?;
            Ok(())
        })
        .await
    }

    /// Capture a PNG screenshot — the model's eyes. `full_page` captures the
    /// entire scrollable page instead of just the current viewport.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        self.bounded("screenshot", self.timeouts().command, async {
            let params = ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(full_page)
                .build();
            Ok(self.page.screenshot(params).await?)
        })
        .await
    }

    /// Scroll the page or an element. With a `selector` and no delta, scroll
    /// that element into view. With a `selector` **and** a non-zero `(dx, dy)`,
    /// scroll *inside* that element — the fix for modal/dialog content that
    /// lives in its own scroll container (a plain `window.scrollBy` moves the
    /// page behind it, not the dialog). With no selector, scroll the page by
    /// `(dx, dy)` (positive `dy` scrolls down). The selector is JSON-encoded
    /// into the script, so it cannot break out of the string.
    pub async fn scroll(&self, selector: Option<&str>, dx: f64, dy: f64) -> Result<()> {
        self.bounded("scroll", self.timeouts().command, async {
        match selector {
            Some(sel) => {
                let sel_json = serde_json::to_string(sel)?;
                let js = if dx == 0.0 && dy == 0.0 {
                    format!(
                        "(() => {{ const e = document.querySelector({sel_json}); \
                         if (!e) return false; \
                         e.scrollIntoView({{block: 'center', inline: 'center'}}); \
                         return true; }})()"
                    )
                } else {
                    // Scroll within the element's own scroll container (or the
                    // nearest scrollable ancestor if the element itself does not
                    // scroll), so a dialog's inner content moves.
                    format!(
                        "(() => {{ let e = document.querySelector({sel_json}); \
                         if (!e) return false; \
                         const scrollable = (n) => {{ \
                           while (n && n !== document.body) {{ \
                             const s = getComputedStyle(n); \
                             if (/(auto|scroll)/.test(s.overflowY + s.overflow) && n.scrollHeight > n.clientHeight) return n; \
                             n = n.parentElement; \
                           }} \
                           return e; \
                         }}; \
                         const target = (e.scrollHeight > e.clientHeight) ? e : scrollable(e); \
                         target.scrollBy({dx}, {dy}); \
                         return true; }})()"
                    )
                };
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
        })
        .await
    }

    /// Read the current location, title, visible text, and the actionable
    /// elements on the page.
    pub async fn observe(&self) -> Result<PageObservation> {
        self.observe_with(true).await
    }

    /// Like [`observe`](Self::observe), but `include_text` can suppress the
    /// (often large and redundant) `innerText` dump — the model keeps the
    /// screenshot plus the interactive-element list, and avoids re-reading a
    /// long form's text on every step.
    pub async fn observe_with(&self, include_text: bool) -> Result<PageObservation> {
        self.bounded("reading the page", self.timeouts().command, async {
            let url = self.page.url().await?.unwrap_or_default();
            let title = self.page.get_title().await?.unwrap_or_default();
            let text = if include_text {
                self.page
                    .evaluate("document.body ? document.body.innerText : ''")
                    .await?
                    .into_value::<String>()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            // Element discovery is best-effort: a failure (e.g. mid-navigation)
            // just yields an empty list rather than failing the observation.
            let elements = match self.page.evaluate(DISCOVER_ELEMENTS_JS).await {
                Ok(v) => v
                    .into_value::<Vec<InteractiveElement>>()
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            let (viewport_width, viewport_height) =
                self.viewport_size().await.unwrap_or((0.0, 0.0));
            let dialogs = std::mem::take(&mut *self.dialogs.lock().unwrap());
            Ok(PageObservation {
                url,
                title,
                text,
                elements,
                viewport_width,
                viewport_height,
                dialogs,
            })
        })
        .await
    }

    /// The viewport size in CSS pixels — the reference frame for coordinate
    /// clicks (CDP mouse events use CSS pixels, so a coordinate resolved against
    /// this lands where intended regardless of screenshot scaling or DPR).
    pub async fn viewport_size(&self) -> Result<(f64, f64)> {
        self.bounded(
            "reading the viewport size",
            self.timeouts().command,
            async {
                let dims = self
                    .page
                    .evaluate("[window.innerWidth, window.innerHeight]")
                    .await?
                    .into_value::<(f64, f64)>()
                    .unwrap_or((0.0, 0.0));
                Ok(dims)
            },
        )
        .await
    }

    /// Find an element by a selector, turning chromiumoxide's opaque CDP miss
    /// ("Could not find node with given id") into a message that names the
    /// selector.
    ///
    /// Besides plain CSS, this understands robust prefixes that survive a site
    /// re-rendering with fresh hashed ids (a real problem on portals like
    /// ELSTER):
    /// - `text=Foo` — the first visible element whose trimmed text /
    ///   aria-label / value contains `Foo` (case-insensitive).
    /// - `role=button` — the first element with that ARIA role (or, for a bare
    ///   tag, that tag). `role=button[name=Save]` also matches on text.
    /// - `aria=Save` — the first element whose aria-label matches.
    ///
    /// These are resolved to a concrete node in the page, so a fragile hashed
    /// `#id` is never needed.
    async fn find_element(&self, selector: &str) -> Result<Element> {
        if let Some(css) = self.resolve_semantic_selector(selector).await? {
            return self
                .page
                .find_element(&css)
                .await
                .map_err(|_| anyhow::anyhow!("no element matches selector '{selector}'"));
        }
        self.page
            .find_element(selector)
            .await
            .map_err(|_| anyhow::anyhow!("no element matches selector '{selector}'"))
    }

    /// If `selector` uses a semantic prefix (`text=`, `role=`, `aria=`), locate
    /// the matching element in the page and stamp it with a unique data
    /// attribute, returning a concrete CSS selector for it. Returns `Ok(None)`
    /// for a plain CSS selector (handled directly by the caller).
    async fn resolve_semantic_selector(&self, selector: &str) -> Result<Option<String>> {
        let sel = selector.trim();
        let (kind, query) = if let Some(q) = sel.strip_prefix("text=") {
            ("text", q)
        } else if let Some(q) = sel.strip_prefix("role=") {
            ("role", q)
        } else if let Some(q) = sel.strip_prefix("aria=") {
            ("aria", q)
        } else {
            return Ok(None);
        };
        let kind_json = serde_json::to_string(kind)?;
        let query_json = serde_json::to_string(query.trim())?;
        // Tag the match with a unique attribute so we can hand back a stable CSS
        // selector even on a page that mints fresh ids every render.
        let js = format!(
            r#"(() => {{
  const kind = {kind_json};
  const q = {query_json};
  const SEL = 'a,button,input,textarea,select,summary,[role],[onclick],[tabindex],label';
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const visible = (el) => {{
    const r = el.getClientRects();
    if (!r.length) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  }};
  const labelText = (el) => norm(el.getAttribute('aria-label')) || norm(el.textContent) || norm(el.value) || norm(el.getAttribute('placeholder')) || norm(el.title);
  const roleOf = (el) => (el.getAttribute('role') || el.tagName.toLowerCase());
  // role=button[name=Save] → role + optional name filter.
  let wantRole = q, wantName = null;
  const m = q.match(/^([^\[]+)\[name=(.+)\]$/);
  if (kind === 'role' && m) {{ wantRole = m[1].trim(); wantName = norm(m[2]); }}
  const needle = norm(q);
  const collect = (root, acc) => {{
    let nodes = [];
    try {{ nodes = Array.from(root.querySelectorAll(SEL)); }} catch (e) {{}}
    for (const el of nodes) acc.push(el);
    let all = [];
    try {{ all = Array.from(root.querySelectorAll('*')); }} catch (e) {{}}
    for (const el of all) if (el.shadowRoot) collect(el.shadowRoot, acc);
  }};
  const cands = [];
  collect(document, cands);
  const match = cands.find((el) => {{
    if (!visible(el)) return false;
    if (kind === 'text') return labelText(el).includes(needle);
    if (kind === 'aria') return norm(el.getAttribute('aria-label')).includes(needle);
    if (kind === 'role') {{
      if (norm(roleOf(el)) !== norm(wantRole)) return false;
      return wantName ? labelText(el).includes(wantName) : true;
    }}
    return false;
  }});
  if (!match) return null;
  const token = 'ca-sel-' + Math.random().toString(36).slice(2);
  match.setAttribute('data-ca-sel', token);
  return '[data-ca-sel="' + token + '"]';
}})()"#
        );
        let resolved = self
            .page
            .evaluate(js)
            .await?
            .into_value::<Option<String>>()
            .unwrap_or(None);
        match resolved {
            Some(css) => Ok(Some(css)),
            None => Err(anyhow::anyhow!("no element matches selector '{selector}'")),
        }
    }

    /// Click the first element matching a selector. The element is scrolled
    /// into view first (via `scrollIntoView`, which handles nested scroll
    /// containers), so an off-screen or collapsed target no longer fails with
    /// "Node is either not visible or not an HTMLElement".
    pub async fn click(&self, selector: &str) -> Result<()> {
        self.bounded("click", self.timeouts().command, async {
            let element = self.find_element(selector).await?;
            let _ = element.scroll_into_view().await;
            element.click().await?;
            Ok(())
        })
        .await
    }

    /// Click at viewport coordinates `(x, y)`. For canvas/WebGL surfaces and
    /// anything without a stable selector (games, maps, drag targets).
    pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        self.bounded("click", self.timeouts().command, async {
            self.page.click(Point { x, y }).await?;
            Ok(())
        })
        .await
    }

    /// Move the mouse to viewport coordinates `(x, y)` without clicking — drives
    /// hover states and canvas pointer-move handlers.
    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
        self.bounded("mouse move", self.timeouts().command, async {
            self.page.move_mouse(Point { x, y }).await?;
            Ok(())
        })
        .await
    }

    /// Focus a field and type text into it, appending to any existing value.
    /// The element is scrolled into view first. Never used for credentials —
    /// those go through the human-in-the-loop login handoff. Prefer
    /// [`fill`](Self::fill) to replace a prefilled field.
    pub async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        self.bounded("typing", self.typing_limit(text), async {
            let element = self.find_element(selector).await?;
            let _ = element.scroll_into_view().await;
            element.focus().await?;
            self.type_chars(text).await
        })
        .await
    }

    /// Clear a field, then type `text` — the replace semantics editing a
    /// prefilled input needs (the old workflow required End + repeated
    /// Backspace). Works for `<input>`/`<textarea>` (value) and
    /// contenteditable elements.
    pub async fn fill(&self, selector: &str, text: &str) -> Result<()> {
        self.bounded("typing", self.typing_limit(text), async {
            let element = self.find_element(selector).await?;
            let _ = element.scroll_into_view().await;
            element.focus().await?;
            self.clear_focused(&element).await?;
            self.type_chars(text).await
        })
        .await
    }

    /// Type `text` into the focused element. Characters on the US keyboard
    /// layout are pressed as real keys, so key handlers see them; anything
    /// else (`ü`, `ß`, `€`, emoji) is inserted as text, since chromiumoxide's
    /// key table only knows the US layout.
    async fn type_chars(&self, text: &str) -> Result<()> {
        let mut buf = [0u8; 4];
        for c in text.chars() {
            let c: &str = c.encode_utf8(&mut buf);
            match get_key_definition(c) {
                Some(def) => self.press(def, 0).await?,
                None => {
                    self.page.execute(InsertTextParams::new(c)).await?;
                }
            }
        }
        Ok(())
    }

    /// Empty a field's current content.
    pub async fn clear(&self, selector: &str) -> Result<()> {
        self.bounded("clearing a field", self.timeouts().command, async {
            let element = self.find_element(selector).await?;
            let _ = element.scroll_into_view().await;
            element.focus().await?;
            self.clear_focused(&element).await?;
            Ok(())
        })
        .await
    }

    /// Clear an already-focused element's value/text via JS and fire the
    /// `input`/`change` events frameworks listen for.
    async fn clear_focused(&self, element: &Element) -> Result<()> {
        element
            .call_js_fn(
                "function() { \
                 if ('value' in this) { this.value = ''; } \
                 else if (this.isContentEditable) { this.textContent = ''; } \
                 this.dispatchEvent(new Event('input', {bubbles: true})); \
                 this.dispatchEvent(new Event('change', {bubbles: true})); }",
                true,
            )
            .await?;
        Ok(())
    }

    /// Focus a field and press a key or key chord (e.g. `"Enter"`,
    /// `"Meta+A"`, `"Control+a"`, `"Shift+Tab"`) on it. The element is scrolled
    /// into view and focused first.
    pub async fn press_key(&self, selector: &str, key: &str) -> Result<()> {
        self.bounded("key press", self.timeouts().command, async {
            let element = self.find_element(selector).await?;
            let _ = element.scroll_into_view().await;
            element.focus().await?;
            self.dispatch_key(key).await
        })
        .await
    }

    /// Press a key or chord without targeting a selector — it goes to whatever
    /// element currently has focus (e.g. arrow keys for a focused game canvas).
    pub async fn press_key_global(&self, key: &str) -> Result<()> {
        self.bounded("key press", self.timeouts().command, async {
            self.dispatch_key(key).await
        })
        .await
    }

    /// Dispatch a key or a modifier chord (`Ctrl+`, `Control+`, `Meta+`,
    /// `Cmd+`, `Command+`, `Alt+`, `Option+`, `Shift+`, joined by `+`) to the
    /// focused element via raw CDP `DispatchKeyEvent`, setting the modifier
    /// bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8) so combinations like select-all
    /// work instead of erroring "Key not found". `Page` has no `press_key`, so
    /// we drive it directly — this also lets plain and chord keys share one
    /// code path.
    async fn dispatch_key(&self, key: &str) -> Result<()> {
        let (modifiers, main_key) = parse_chord(key);
        let def = get_key_definition(main_key)
            .ok_or_else(|| anyhow::anyhow!("unknown key '{main_key}'"))?;
        self.press(def, modifiers).await
    }

    /// Press and release one key with the given modifier bitmask.
    async fn press(&self, def: &KeyDefinition, modifiers: i64) -> Result<()> {
        // Shift makes a letter uppercase in the emitted key/text.
        let shift = modifiers & 8 != 0;
        let key_str = if def.key.len() == 1 && shift {
            def.key.to_uppercase()
        } else {
            def.key.to_string()
        };

        // Only insert text for a printable key with no command modifier held:
        // Ctrl/Meta/Alt combinations are commands, not text.
        let command_modifier = modifiers & (1 | 2 | 4) != 0;
        let text: Option<String> = if command_modifier {
            None
        } else if let Some(t) = def.text {
            Some(t.to_string())
        } else if key_str.len() == 1 {
            Some(key_str.clone())
        } else {
            None
        };

        let down_type = if text.is_some() {
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::RawKeyDown
        };

        let mut down = DispatchKeyEventParams::builder()
            .r#type(down_type)
            .key(key_str.clone())
            .code(def.code)
            .windows_virtual_key_code(def.key_code)
            .native_virtual_key_code(def.key_code);
        if modifiers != 0 {
            down = down.modifiers(modifiers);
        }
        if let Some(t) = &text {
            down = down.text(t.clone());
        }
        let down = down
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build key event: {e}"))?;

        let mut up = DispatchKeyEventParams::builder()
            .r#type(DispatchKeyEventType::KeyUp)
            .key(key_str)
            .code(def.code)
            .windows_virtual_key_code(def.key_code)
            .native_virtual_key_code(def.key_code);
        if modifiers != 0 {
            up = up.modifiers(modifiers);
        }
        let up = up
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build key event: {e}"))?;

        self.page.execute(down).await?;
        self.page.execute(up).await?;
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
        // The deadline is checked between polls; the outer limit also covers
        // a poll that never returns because the page is hung.
        let poll = async {
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
        };
        let _ = tokio::time::timeout(deadline + Duration::from_millis(200), poll).await;
    }

    /// Poll until an element matching `selector` exists or `timeout` elapses.
    /// Returns whether it appeared. Bounded as a whole, so a hung page cannot
    /// hold a single poll past the deadline.
    pub async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<bool> {
        let poll = async {
            loop {
                if self.page.find_element(selector).await.is_ok() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        Ok(tokio::time::timeout(timeout, poll).await.unwrap_or(false))
    }

    /// Evaluate a JavaScript expression in the page and return its JSON value.
    pub async fn eval(&self, js: &str) -> Result<serde_json::Value> {
        self.bounded("script evaluation", self.timeouts().command, async {
            let result = self.page.evaluate(js).await?;
            Ok(result.into_value().unwrap_or(serde_json::Value::Null))
        })
        .await
    }

    /// Export the whole cookie jar (all domains), including in-memory **session
    /// cookies** that a graceful close does *not* flush to disk. Used to carry a
    /// login across a headful→headless relaunch on the same profile without the
    /// user having to authenticate again.
    pub async fn export_cookies(&self) -> Result<Vec<CookieParam>> {
        self.bounded("reading cookies", self.timeouts().command, async {
            let resp = self.page.execute(GetAllCookiesRaw {}).await?;
            Ok(resp
                .result
                .cookies
                .iter()
                .filter_map(cookie_to_param)
                .collect())
        })
        .await
    }

    /// Re-inject cookies captured by [`export_cookies`](Self::export_cookies).
    /// The page must already be on an `http(s)` URL (CDP rejects setting cookies
    /// from `about:blank`/`data:`); each cookie also carries its own url/domain,
    /// so cross-domain (SSO) cookies restore correctly. A reload afterwards makes
    /// them take effect. No-op for an empty jar.
    pub async fn import_cookies(&self, cookies: Vec<CookieParam>) -> Result<()> {
        self.bounded("setting cookies", self.timeouts().command, async {
            if cookies.is_empty() {
                return Ok(());
            }
            self.page.set_cookies(cookies).await?;
            Ok(())
        })
        .await
    }
}

fn stale_ref(r: &str) -> anyhow::Error {
    anyhow::anyhow!("{r} is no longer in the page (read the page to get current refs)")
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.dialog_task.abort();
    }
}

/// Answer every JavaScript dialog on `page` as it opens. Chrome stalls the
/// renderer while a dialog is open, so without this the click that raised it —
/// and every CDP command after it — hangs until the per-command timeout, often
/// for minutes. Each answered dialog is appended to `log`.
async fn spawn_dialog_handler(
    page: &Page,
    log: Arc<Mutex<Vec<HandledDialog>>>,
    accept_dialogs: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    let mut events = page
        .event_listener::<EventJavascriptDialogOpening>()
        .await?;
    let page = page.clone();
    Ok(tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let (kind, accept) = match event.r#type {
                DialogType::Alert => ("alert", true),
                DialogType::Beforeunload => ("beforeunload", true),
                DialogType::Confirm => ("confirm", accept_dialogs.load(Ordering::Relaxed)),
                DialogType::Prompt => ("prompt", accept_dialogs.load(Ordering::Relaxed)),
            };
            let mut params = HandleJavaScriptDialogParams::new(accept);
            if accept {
                params.prompt_text = event.default_prompt.clone();
            }
            // Record before answering: the answer unblocks the action that
            // raised the dialog, and its follow-up observation must see it.
            log.lock().unwrap().push(HandledDialog {
                kind: kind.to_string(),
                message: event.message.clone(),
                accepted: accept,
            });
            let _ = page.execute(params).await;
        }
    }))
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

/// Split a key spec like `"Meta+A"` / `"Control+shift+Tab"` into the CDP
/// modifier bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8) and the final key name.
/// Segments are case-insensitive for the modifier names; the final key keeps
/// its case (chromiumoxide's key table is case-sensitive, e.g. `Enter`, `a`).
/// A lone `"+"` (the plus key) is handled by treating only non-final segments
/// as potential modifiers.
fn parse_chord(spec: &str) -> (i64, &str) {
    let spec = spec.trim();
    // Split on '+', but keep a trailing empty piece so "Ctrl++" (the plus key)
    // still yields "+" as the final key.
    let parts: Vec<&str> = spec.split('+').collect();
    if parts.len() < 2 {
        return (0, spec);
    }
    let mut modifiers = 0i64;
    // Everything before the last non-empty segment is a modifier candidate.
    // The final key is the last segment (or "+" if the spec ended in "+").
    let (main, mods) = if parts.last() == Some(&"") {
        // Spec ended with '+', so the key is literally '+'.
        ("+", &parts[..parts.len() - 1])
    } else {
        (parts[parts.len() - 1], &parts[..parts.len() - 1])
    };
    for m in mods {
        match m.trim().to_ascii_lowercase().as_str() {
            "alt" | "option" | "opt" => modifiers |= 1,
            "ctrl" | "control" => modifiers |= 2,
            "meta" | "cmd" | "command" | "super" | "win" => modifiers |= 4,
            "shift" => modifiers |= 8,
            "" => {}
            // An unknown "modifier" means this wasn't a chord after all; treat
            // the whole thing as a literal key.
            _ => return (0, spec),
        }
    }
    if modifiers == 0 {
        (0, spec)
    } else {
        (modifiers, main)
    }
}

#[cfg(test)]
mod chord_tests {
    use super::parse_chord;

    #[test]
    fn parses_modifier_chords() {
        // Meta=4, Ctrl=2, Shift=8, Alt=1.
        assert_eq!(parse_chord("Meta+A"), (4, "A"));
        assert_eq!(parse_chord("Control+a"), (2, "a"));
        assert_eq!(parse_chord("Cmd+a"), (4, "a"));
        assert_eq!(parse_chord("Shift+Tab"), (8, "Tab"));
        assert_eq!(parse_chord("Alt+F4"), (1, "F4"));
        // Case-insensitive modifier names, combined bitmask.
        assert_eq!(parse_chord("ctrl+shift+k"), (2 | 8, "k"));
        assert_eq!(parse_chord("Meta+Shift+z"), (4 | 8, "z"));
    }

    #[test]
    fn plain_keys_and_literal_plus_are_not_chords() {
        assert_eq!(parse_chord("Enter"), (0, "Enter"));
        assert_eq!(parse_chord("a"), (0, "a"));
        assert_eq!(parse_chord("+"), (0, "+"));
        // Not a chord: an unknown leading segment ⇒ treated literally.
        assert_eq!(parse_chord("a+b"), (0, "a+b"));
    }

    #[test]
    fn control_plus_the_plus_key() {
        assert_eq!(parse_chord("Ctrl++"), (2, "+"));
    }
}
