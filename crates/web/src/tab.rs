//! One browser tab: a live page the agent drives across tool calls, with the
//! state that belongs to it (answered dialogs, the timeouts it runs under).
//!
//! [`crate::BrowserSession`] owns the browser and its tabs; every verb that
//! touches a page lives here.

use crate::ax_tree::{GetFullAxTreeRaw, RefMap, RenderOptions};
use crate::page_log::PageLog;
use crate::recording::{self, Recording};
use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::browser::{
    Bounds, GetWindowForTargetParams, SetWindowBoundsParams, WindowId,
};
use chromiumoxide::cdp::browser_protocol::dom::ResolveNodeParams;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetContentQuadsParams, ScrollIntoViewIfNeededParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::{
    MediaFeature, SetDeviceMetricsOverrideParams, SetEmulatedMediaParams,
    SetTouchEmulationEnabledParams, SetUserAgentOverrideParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::network::GetResponseBodyParams;
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, CookieSameSite, TimeSinceEpoch};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, DialogType, EventJavascriptDialogOpening,
    EventScreencastFrame, HandleJavaScriptDialogParams, ScreencastFrameAckParams,
    StartScreencastFormat, StartScreencastParams, StopScreencastParams, Viewport,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EvaluateParams, RemoteObject,
};
use chromiumoxide::keys::{KeyDefinition, get_key_definition};
use chromiumoxide::layout::Point;
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// A JavaScript dialog (`alert` / `confirm` / `prompt` / `beforeunload`) the
/// session answered on its own, reported so the model knows it happened.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HandledDialog {
    pub kind: String,
    pub message: String,
    /// Whether the dialog was accepted (OK) or dismissed (Cancel).
    pub accepted: bool,
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
    /// Dialogs answered since the last [`take_dialogs`](Self::take_dialogs).
    dialogs: Arc<Mutex<Vec<HandledDialog>>>,
    /// Whether `confirm`/`prompt` dialogs are accepted rather than dismissed.
    accept_dialogs: Arc<AtomicBool>,
    /// Answers dialogs as they open (aborted on drop).
    dialog_task: JoinHandle<()>,
    /// Shared with the session, so changing its limits reaches every tab.
    timeouts: Arc<Mutex<BrowserTimeouts>>,
    /// `ref_N` handles handed out by `read_page`/`find`.
    refs: Mutex<RefMap>,
    /// Screenshot pixels per CSS pixel in the latest screenshot: coordinates
    /// the model reads off it are divided by this.
    frame_scale: Mutex<f64>,
    /// Console messages and network requests, collected in the background.
    log: Arc<PageLog>,
    log_tasks: Vec<JoinHandle<()>>,
    /// The browser's own user agent, to restore after emulating a phone.
    original_user_agent: Mutex<Option<String>>,
    /// Where the mouse is and which buttons are held (CDP `buttons` mask), so
    /// a move while a button is down is a drag and a release lands in place.
    mouse_at: Mutex<Point>,
    held_buttons: Mutex<i64>,
    /// Keys pressed with `key_down` and not released yet: (code, name).
    held_keys: Mutex<Vec<(&'static str, String)>>,
}

/// A captured screenshot and the size of its coordinate frame.
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A mouse button for [`Tab::click_point`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
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
        let log = Arc::new(PageLog::default());
        let log_tasks = crate::page_log::spawn_listeners(&page, log.clone()).await?;
        Ok(Self {
            id,
            page,
            dialogs,
            accept_dialogs,
            dialog_task,
            timeouts,
            refs: Mutex::new(RefMap::default()),
            frame_scale: Mutex::new(1.0),
            log,
            log_tasks,
            original_user_agent: Mutex::new(None),
            mouse_at: Mutex::new(Point { x: 0.0, y: 0.0 }),
            held_buttons: Mutex::new(0),
            held_keys: Mutex::new(Vec::new()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Dialogs answered since the last call.
    pub fn take_dialogs(&self) -> Vec<HandledDialog> {
        std::mem::take(&mut *self.dialogs.lock().unwrap())
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

    /// Capture the viewport. `scale` sizes the image relative to CSS pixels
    /// (default 1); it is reduced further so neither edge exceeds
    /// [`MAX_SCREENSHOT_EDGE`], which keeps the image from being resized again
    /// downstream. The result's size is the coordinate frame for
    /// [`frame_point`](Self::frame_point) until the next screenshot.
    pub async fn screenshot_frame(&self, scale: Option<f64>) -> Result<Screenshot> {
        self.bounded("screenshot", self.timeouts().command, async {
            let (sx, sy, vw, vh) = self.scroll_and_viewport().await?;
            let scale = fit_scale(scale.unwrap_or(1.0), vw, vh);
            let png = self
                .capture(sx, sy, vw, vh, scale, CaptureScreenshotFormat::Png)
                .await?;
            *self.frame_scale.lock().unwrap() = scale;
            Ok(Screenshot {
                png,
                width: (vw * scale).round() as u32,
                height: (vh * scale).round() as u32,
            })
        })
        .await
    }

    /// Capture the region `(x0, y0, x1, y1)` of the latest screenshot's frame,
    /// enlarged (up to 4×, within [`MAX_SCREENSHOT_EDGE`]) for a closer look.
    /// Does not change the coordinate frame. `scale` shrinks the result.
    pub async fn zoom(&self, region: [f64; 4], scale: Option<f64>) -> Result<Screenshot> {
        let frame = *self.frame_scale.lock().unwrap();
        let [x0, y0, x1, y1] = region.map(|v| v / frame);
        if x1 <= x0 || y1 <= y0 {
            anyhow::bail!("region must be (x0, y0, x1, y1) with x1 > x0 and y1 > y0");
        }
        self.bounded("zoom", self.timeouts().command, async {
            let (sx, sy, _, _) = self.scroll_and_viewport().await?;
            let (w, h) = (x1 - x0, y1 - y0);
            let enlarge = (MAX_SCREENSHOT_EDGE as f64 / w.max(h)).min(4.0) * scale.unwrap_or(1.0);
            let png = self
                .capture(
                    sx + x0,
                    sy + y0,
                    w,
                    h,
                    enlarge,
                    CaptureScreenshotFormat::Png,
                )
                .await?;
            Ok(Screenshot {
                png,
                width: (w * enlarge).round() as u32,
                height: (h * enlarge).round() as u32,
            })
        })
        .await
    }

    /// Record the page for `duration` of real time and lay `frames` moments,
    /// evenly spread from start to end, out as one contact sheet. Each cell
    /// is a frame the page painted, its size `scale` × CSS pixels (default:
    /// as large as fits [`MAX_SCREENSHOT_EDGE`]). The browser only sends a
    /// frame when the page repaints; a cell that looks the same as the one
    /// before is marked so. Does not change the coordinate frame.
    pub async fn record(
        &self,
        duration: Duration,
        frames: usize,
        scale: Option<f64>,
    ) -> Result<Recording> {
        anyhow::ensure!(frames > 0, "a recording needs at least one frame");
        let limit = self.timeouts().command * 2 + duration;
        self.bounded("recording", limit, async {
            let (sx, sy, vw, vh) = self.scroll_and_viewport().await?;
            let (columns, rows) = recording::grid(frames as u32);
            let (cw, ch) = recording::cell_size(vw, vh, columns, rows, scale);
            let mut events = self.page.event_listener::<EventScreencastFrame>().await?;
            // A frame shows the whole window, which can be larger than the
            // viewport and is cut to it afterwards: frames sized for the
            // window at the cell scale leave the viewport at least cell size.
            let scale = cw as f64 / vw;
            let mut screencast = StartScreencastParams::builder()
                .format(StartScreencastFormat::Jpeg)
                .quality(JPEG_QUALITY);
            if let Some((_, w, h)) = self.window_bounds().await {
                screencast = screencast
                    .max_width((w as f64 * scale).ceil() as i64)
                    .max_height((h as f64 * scale).ceil() as i64);
            }
            self.page.execute(screencast.build()).await?;
            let captured = async {
                // The page as the recording starts: the screencast only sends
                // what is painted from now on, and a still page paints nothing.
                // A JPEG like the screencast's, so the same picture compares
                // as the same.
                let first = self
                    .capture(sx, sy, vw, vh, scale, CaptureScreenshotFormat::Jpeg)
                    .await?;
                let first = recording::Frame {
                    at: 0.0,
                    encoded: first,
                    viewport: (1.0, 1.0),
                };
                let painted = self.collect_frames(&mut events, duration, (vw, vh)).await?;
                Ok::<_, anyhow::Error>(std::iter::once(first).chain(painted).collect())
            }
            .await;
            // Stop even when capturing failed, so frames do not keep coming.
            let stopped = self.page.execute(StopScreencastParams::default()).await;
            let captured: Vec<recording::Frame> = captured?;
            stopped?;
            let times = recording::cell_times(frames, duration.as_secs_f64());
            recording::contact_sheet(&captured, &times, columns, rows, (cw, ch))
        })
        .await
    }

    /// Screencast frames painted from now until `duration` from now,
    /// acknowledged as they come (the browser sends no more until it is),
    /// each with its time in seconds from now. Frames painted earlier are
    /// dropped.
    ///
    /// A frame shows the browser window, which can be smaller than the
    /// `viewport` (CSS px) when it is emulated. Then the window grows by what
    /// is missing, and frames that do not show the whole viewport are
    /// dropped.
    async fn collect_frames(
        &self,
        events: &mut (impl futures::Stream<Item = Arc<EventScreencastFrame>> + Unpin),
        duration: Duration,
        viewport: (f64, f64),
    ) -> Result<Vec<recording::Frame>> {
        use base64::Engine;
        let (vw, vh) = viewport;
        let started = Instant::now();
        let start_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let deadline = tokio::time::Instant::from_std(started + duration);
        let mut frames = Vec::new();
        let mut grown_for = None;
        loop {
            let event = match tokio::time::timeout_at(deadline, events.next()).await {
                Ok(Some(event)) => event,
                Ok(None) | Err(_) => break,
            };
            self.page
                .execute(ScreencastFrameAckParams::new(event.session_id))
                .await?;
            let meta = &event.metadata;
            // When the frame was painted; its arrival if the browser omits that.
            let at = meta
                .timestamp
                .as_ref()
                .map(|t| *t.inner() - start_epoch)
                .unwrap_or_else(|| started.elapsed().as_secs_f64());
            if at < 0.0 {
                continue;
            }
            let (dw, dh) = (meta.device_width, meta.device_height);
            if dw + 0.5 < vw || dh + 0.5 < vh {
                // Once per size: growing takes a moment to show in frames.
                if grown_for != Some((dw, dh)) {
                    grown_for = Some((dw, dh));
                    self.grow_window_by((vw - dw).max(0.0), (vh - dh).max(0.0))
                        .await;
                }
                continue;
            }
            frames.push(recording::Frame {
                at,
                encoded: base64::engine::general_purpose::STANDARD
                    .decode(AsRef::<str>::as_ref(&event.data))?,
                viewport: (vw / dw, vh / dh),
            });
        }
        Ok(frames)
    }

    /// Scroll offset and viewport size, in CSS pixels.
    async fn scroll_and_viewport(&self) -> Result<(f64, f64, f64, f64)> {
        Ok(self
            .page
            .evaluate("[window.scrollX, window.scrollY, window.innerWidth, window.innerHeight]")
            .await?
            .into_value::<(f64, f64, f64, f64)>()?)
    }

    /// Capture the document rectangle `(x, y, w, h)` (CSS px) at `scale`, a
    /// JPEG at the quality recordings use or a PNG.
    async fn capture(
        &self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        scale: f64,
        format: CaptureScreenshotFormat,
    ) -> Result<Vec<u8>> {
        use base64::Engine;
        let mut params = CaptureScreenshotParams::builder()
            .clip(Viewport {
                x,
                y,
                width: w,
                height: h,
                scale,
            })
            .build();
        if format == CaptureScreenshotFormat::Jpeg {
            params.quality = Some(JPEG_QUALITY);
        }
        params.format = Some(format);
        let data = self.page.execute(params).await?.result.data;
        Ok(base64::engine::general_purpose::STANDARD.decode(AsRef::<str>::as_ref(&data))?)
    }

    /// Convert a point in the latest screenshot's frame to CSS pixels.
    pub fn frame_point(&self, x: f64, y: f64) -> Point {
        let scale = *self.frame_scale.lock().unwrap();
        Point {
            x: x / scale,
            y: y / scale,
        }
    }

    /// Click at a viewport point (CSS px): `count` 2 is a double click, 3 a
    /// triple click. `modifiers` is the CDP bitmask (Alt=1, Ctrl=2, Meta=4,
    /// Shift=8).
    pub async fn click_point(
        &self,
        at: Point,
        button: Button,
        count: u32,
        modifiers: i64,
    ) -> Result<()> {
        self.bounded("click", self.timeouts().command, async {
            self.mouse(DispatchMouseEventType::MouseMoved, at, None, 0, modifiers)
                .await?;
            for n in 1..=count.max(1) {
                self.mouse(
                    DispatchMouseEventType::MousePressed,
                    at,
                    Some(button),
                    n,
                    modifiers,
                )
                .await?;
                self.mouse(
                    DispatchMouseEventType::MouseReleased,
                    at,
                    Some(button),
                    n,
                    modifiers,
                )
                .await?;
            }
            Ok(())
        })
        .await
    }

    /// Move the mouse to a viewport point (CSS px) without clicking.
    pub async fn hover_point(&self, at: Point) -> Result<()> {
        self.bounded("mouse move", self.timeouts().command, async {
            self.mouse(DispatchMouseEventType::MouseMoved, at, None, 0, 0)
                .await
        })
        .await
    }

    /// Press the left button at `from`, move to `to` in steps, release.
    pub async fn drag(&self, from: Point, to: Point) -> Result<()> {
        self.bounded("drag", self.timeouts().command, async {
            use DispatchMouseEventType::*;
            self.mouse(MouseMoved, from, None, 0, 0).await?;
            self.mouse(MousePressed, from, Some(Button::Left), 1, 0)
                .await?;
            const STEPS: u32 = 8;
            for i in 1..=STEPS {
                let t = i as f64 / STEPS as f64;
                let at = Point {
                    x: from.x + (to.x - from.x) * t,
                    y: from.y + (to.y - from.y) * t,
                };
                self.mouse(MouseMoved, at, Some(Button::Left), 0, 0).await?;
            }
            self.mouse(MouseReleased, to, Some(Button::Left), 1, 0)
                .await
        })
        .await
    }

    /// Turn the mouse wheel at a viewport point (CSS px). Positive `dy`
    /// scrolls down. The event reaches whatever is under the point, so an
    /// inner scroll container scrolls, not just the page.
    pub async fn wheel(&self, at: Point, dx: f64, dy: f64) -> Result<()> {
        self.bounded("scroll", self.timeouts().command, async {
            let event = DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MouseWheel)
                .x(at.x)
                .y(at.y)
                .delta_x(dx)
                .delta_y(dy)
                .build()
                .map_err(anyhow::Error::msg)?;
            self.page.execute(event).await?;
            Ok(())
        })
        .await
    }

    async fn mouse(
        &self,
        kind: DispatchMouseEventType,
        at: Point,
        button: Option<Button>,
        click_count: u32,
        modifiers: i64,
    ) -> Result<()> {
        let mask = |b: Button| match b {
            Button::Left => 1,
            Button::Right => 2,
            Button::Middle => 4,
        };
        let held = *self.held_buttons.lock().unwrap();
        // The buttons down after this event, and the one it is about.
        let (buttons, button) = match (&kind, button) {
            (DispatchMouseEventType::MousePressed, Some(b)) => (held | mask(b), Some(b)),
            (DispatchMouseEventType::MouseReleased, Some(b)) => (held & !mask(b), Some(b)),
            _ => {
                let dragging = [Button::Left, Button::Right, Button::Middle]
                    .into_iter()
                    .find(|b| held & mask(*b) != 0);
                (held, button.or(dragging))
            }
        };
        let mut event = DispatchMouseEventParams::builder()
            .r#type(kind)
            .x(at.x)
            .y(at.y)
            .modifiers(modifiers)
            .buttons(buttons);
        if let Some(button) = button {
            event = event.button(match button {
                Button::Left => MouseButton::Left,
                Button::Right => MouseButton::Right,
                Button::Middle => MouseButton::Middle,
            });
        }
        if click_count > 0 {
            event = event.click_count(click_count as i64);
        }
        self.page
            .execute(event.build().map_err(anyhow::Error::msg)?)
            .await?;
        *self.mouse_at.lock().unwrap() = at;
        *self.held_buttons.lock().unwrap() = buttons;
        Ok(())
    }

    /// Type `text` into whatever has focus.
    pub async fn type_into_focused(&self, text: &str) -> Result<()> {
        self.bounded("typing", self.typing_limit(text), self.type_chars(text))
            .await
    }

    /// Press keys: space-separated keys or chords (`"Enter"`, `"ctrl+a"`,
    /// `"cmd+shift+z"`, `"Backspace Backspace"`), the sequence `repeat` times.
    /// Common aliases (`Return`, `Esc`, `Space`, `Up`, `PageDown`, …) and any
    /// case are accepted.
    pub async fn press_keys(&self, keys: &str, repeat: u32) -> Result<()> {
        let presses = keys.split_whitespace().count() as u32 * repeat.max(1);
        let limit = self.timeouts().command + Duration::from_millis(20) * presses;
        let chords = parse_keys(keys)?;
        self.bounded("key press", limit, async {
            for _ in 0..repeat.max(1) {
                for (def, modifiers) in &chords {
                    self.press(def, *modifiers).await?;
                }
            }
            Ok(())
        })
        .await
    }

    /// Console messages, oldest first, at most the last `limit`: only errors
    /// (and exceptions) if asked, only those containing `pattern` if given.
    pub fn console_messages(
        &self,
        only_errors: bool,
        pattern: Option<&str>,
        limit: usize,
    ) -> Vec<String> {
        let pattern = pattern.map(str::to_lowercase);
        let console = self.log.console.lock().unwrap();
        let matching: Vec<String> = console
            .iter()
            .filter(|m| !only_errors || m.is_error())
            .map(|m| m.line())
            .filter(|l| {
                pattern
                    .as_ref()
                    .is_none_or(|p| l.to_lowercase().contains(p))
            })
            .collect();
        matching[matching.len().saturating_sub(limit)..].to_vec()
    }

    /// Network requests, oldest first, at most the last `limit`, only those
    /// whose URL contains `url_pattern` if given. Each line starts with the
    /// request id to fetch its body with [`response_body`](Self::response_body).
    pub fn network_requests(&self, url_pattern: Option<&str>, limit: usize) -> Vec<String> {
        let network = self.log.network.lock().unwrap();
        let matching: Vec<String> = network
            .iter()
            .filter(|r| url_pattern.is_none_or(|p| r.url.contains(p)))
            .map(|r| r.line())
            .collect();
        matching[matching.len().saturating_sub(limit)..].to_vec()
    }

    /// The body of a finished response, cut to `max_chars`. Binary bodies are
    /// described, not returned.
    pub async fn response_body(&self, request_id: &str, max_chars: usize) -> Result<String> {
        self.bounded("reading a response body", self.timeouts().command, async {
            let body = self
                .page
                .execute(GetResponseBodyParams::new(request_id.to_string()))
                .await
                .map_err(|e| anyhow::anyhow!("no body for request {request_id}: {e}"))?
                .result;
            if body.base64_encoded {
                return Ok(format!(
                    "(binary body, about {} bytes)",
                    body.body.len() * 3 / 4
                ));
            }
            Ok(truncate_chars(&body.body, max_chars))
        })
        .await
    }

    /// Run JavaScript in the page with REPL semantics: top-level `await`
    /// works and the value of the last expression is returned, as JSON when it
    /// serializes, else as its description. A thrown error is an `Err`.
    pub async fn javascript(&self, code: &str) -> Result<String> {
        self.bounded("script evaluation", self.timeouts().command, async {
            let params = EvaluateParams::builder()
                .expression(code)
                .repl_mode(true)
                .await_promise(true)
                .return_by_value(true)
                .user_gesture(true)
                .build()
                .map_err(anyhow::Error::msg)?;
            let result = self.page.execute(params).await?.result;
            if let Some(details) = result.exception_details {
                let message = details
                    .exception
                    .as_ref()
                    .and_then(|e| e.description.clone())
                    .unwrap_or(details.text);
                anyhow::bail!("{message}");
            }
            Ok(describe_value(&result.result))
        })
        .await
    }

    /// Set a form control by ref: a `<select>` by option value or visible
    /// text (an array for a multi-select), a checkbox/radio/switch by
    /// `true`/`false`, anything else (inputs, textareas, contenteditable) by
    /// text. Fires the `input`/`change` events frameworks listen for. Returns
    /// what was set.
    pub async fn form_input(&self, r: &str, value: &serde_json::Value) -> Result<String> {
        let node = self.backend_node(r)?;
        self.bounded("setting a form field", self.timeouts().command, async {
            let object = self
                .page
                .execute(ResolveNodeParams::builder().backend_node_id(node).build())
                .await
                .map_err(|_| stale_ref(r))?
                .result
                .object
                .object_id
                .ok_or_else(|| stale_ref(r))?;
            let call = CallFunctionOnParams::builder()
                .function_declaration(FORM_INPUT_JS)
                .object_id(object)
                .argument(CallArgument::builder().value(value.clone()).build())
                .return_by_value(true)
                .await_promise(true)
                .build()
                .map_err(anyhow::Error::msg)?;
            let result = self.page.execute(call).await?.result;
            if let Some(details) = result.exception_details {
                let message = details
                    .exception
                    .as_ref()
                    .and_then(|e| e.description.clone())
                    .unwrap_or(details.text);
                anyhow::bail!("{}", message.trim_start_matches("Error: "));
            }
            Ok(describe_value(&result.result))
        })
        .await
    }

    /// Emulate a viewport of `width`×`height` CSS pixels. `mobile` also
    /// emulates a phone: mobile layout, touch (5 points) and an Android Chrome
    /// user agent; reload for the page to pick that up.
    pub async fn set_viewport(&self, width: u32, height: u32, mobile: bool) -> Result<()> {
        self.bounded("resizing the viewport", self.timeouts().command, async {
            let metrics = SetDeviceMetricsOverrideParams::builder()
                .width(width as i64)
                .height(height as i64)
                .device_scale_factor(1.0)
                .mobile(mobile)
                .build()
                .map_err(anyhow::Error::msg)?;
            self.page.execute(metrics).await?;
            let touch = SetTouchEmulationEnabledParams::builder()
                .enabled(mobile)
                .max_touch_points(5)
                .build()
                .map_err(anyhow::Error::msg)?;
            self.page.execute(touch).await?;
            self.grow_window(width, height).await;

            let original = {
                let known = self.original_user_agent.lock().unwrap().clone();
                match known {
                    Some(ua) => ua,
                    None => {
                        let ua: String = self
                            .page
                            .evaluate("navigator.userAgent")
                            .await?
                            .into_value()?;
                        *self.original_user_agent.lock().unwrap() = Some(ua.clone());
                        ua
                    }
                }
            };
            let user_agent = if mobile {
                MOBILE_USER_AGENT.to_string()
            } else {
                original
            };
            self.page
                .execute(SetUserAgentOverrideParams::new(user_agent))
                .await?;
            Ok(())
        })
        .await
    }

    /// Make the browser window large enough for a `width`×`height` viewport.
    /// Headless Chrome only shows a screencast what is inside its window, and
    /// its window has room for browser UI (143 px of its height on macOS), so
    /// the window gets [`WINDOW_UI_ALLOWANCE`] on top; a recording grows it
    /// further if that is not enough. Best effort: a window that cannot be
    /// resized stays as is.
    async fn grow_window(&self, width: u32, height: u32) {
        let Some((id, w, h)) = self.window_bounds().await else {
            return;
        };
        let (width, height) = (width as i64, (height + WINDOW_UI_ALLOWANCE) as i64);
        if w < width || h < height {
            self.set_window_size(id, w.max(width), h.max(height)).await;
        }
    }

    /// Make the browser window `dw` × `dh` pixels larger.
    async fn grow_window_by(&self, dw: f64, dh: f64) {
        if let Some((id, w, h)) = self.window_bounds().await {
            let (w, h) = (w + dw.ceil() as i64, h + dh.ceil() as i64);
            self.set_window_size(id, w, h).await;
        }
    }

    async fn window_bounds(&self) -> Option<(WindowId, i64, i64)> {
        let window = self
            .page
            .execute(GetWindowForTargetParams::default())
            .await
            .ok()?
            .result;
        let bounds = &window.bounds;
        Some((
            window.window_id,
            bounds.width.unwrap_or(0),
            bounds.height.unwrap_or(0),
        ))
    }

    async fn set_window_size(&self, id: WindowId, width: i64, height: i64) {
        let bounds = Bounds::builder().width(width).height(height).build();
        let _ = self
            .page
            .execute(SetWindowBoundsParams::new(id, bounds))
            .await;
    }

    /// Emulate `prefers-color-scheme` (`"light"` / `"dark"`); `None` follows
    /// the browser again.
    pub async fn set_color_scheme(&self, scheme: Option<&str>) -> Result<()> {
        self.bounded("setting the color scheme", self.timeouts().command, async {
            let params = SetEmulatedMediaParams::builder()
                .feature(MediaFeature::new(
                    "prefers-color-scheme",
                    scheme.unwrap_or(""),
                ))
                .build();
            self.page.execute(params).await?;
            Ok(())
        })
        .await
    }

    /// The page's visible text — its `<main>`/`<article>` if it has one, else
    /// the whole body — cut to `max_chars`.
    pub async fn page_text(&self, max_chars: usize) -> Result<String> {
        self.bounded("reading the page text", self.timeouts().command, async {
            let text: String = self
                .page
                .evaluate(
                    "(() => { const main = document.querySelector('main, [role=main], article'); \
                     const el = main && main.innerText.trim() ? main : document.body; \
                     return el ? el.innerText : ''; })()",
                )
                .await?
                .into_value()?;
            Ok(truncate_chars(&text, max_chars))
        })
        .await
    }

    /// Go back (`-1`) or forward (`1`) in the tab's history and wait for the
    /// page to settle.
    pub async fn history(&self, delta: i32) -> Result<()> {
        self.bounded("history navigation", self.timeouts().navigation, async {
            self.page.evaluate(format!("history.go({delta})")).await?;
            Ok(())
        })
        .await?;
        self.settle().await;
        Ok(())
    }

    /// Press and keep holding keys: space-separated keys or chords, in order.
    /// They stay down — across tool calls — until [`key_up`](Self::key_up).
    pub async fn key_down(&self, keys: &str) -> Result<()> {
        let chords = parse_keys(keys)?;
        self.bounded("key press", self.timeouts().command, async {
            for ((def, modifiers), name) in chords.iter().zip(keys.split_whitespace()) {
                self.key_event(def, *modifiers, true).await?;
                let mut held = self.held_keys.lock().unwrap();
                if !held.iter().any(|(code, _)| *code == def.code) {
                    held.push((def.code, name.to_string()));
                }
            }
            Ok(())
        })
        .await
    }

    /// Release keys held with [`key_down`](Self::key_down), last first.
    pub async fn key_up(&self, keys: &str) -> Result<()> {
        let chords = parse_keys(keys)?;
        self.bounded("key release", self.timeouts().command, async {
            for (def, modifiers) in chords.iter().rev() {
                self.key_event(def, *modifiers, false).await?;
                self.held_keys
                    .lock()
                    .unwrap()
                    .retain(|(code, _)| *code != def.code);
            }
            Ok(())
        })
        .await
    }

    /// What is still held down: keys from `key_down` and mouse buttons from
    /// `mouse_down`, by name (`"w"`, `"left mouse button"`).
    pub fn held_inputs(&self) -> Vec<String> {
        let mut held: Vec<String> = self
            .held_keys
            .lock()
            .unwrap()
            .iter()
            .map(|(_, name)| name.clone())
            .collect();
        let buttons = *self.held_buttons.lock().unwrap();
        for (mask, name) in [(1, "left"), (2, "right"), (4, "middle")] {
            if buttons & mask != 0 {
                held.push(format!("{name} mouse button"));
            }
        }
        held
    }

    /// Hold keys down for `duration`, then release them: the input a game
    /// reads as "keep walking for half a second".
    pub async fn hold_keys(&self, keys: &str, duration: Duration) -> Result<()> {
        let chords = parse_keys(keys)?;
        let limit = self.timeouts().command + duration;
        self.bounded("holding keys", limit, async {
            for (def, modifiers) in &chords {
                self.key_event(def, *modifiers, true).await?;
            }
            tokio::time::sleep(duration).await;
            for (def, modifiers) in chords.iter().rev() {
                self.key_event(def, *modifiers, false).await?;
            }
            Ok(())
        })
        .await
    }

    /// Press a mouse button and keep it down, at `at` (CSS px) or where the
    /// mouse is. Moves while it is down drag; release with
    /// [`mouse_up`](Self::mouse_up).
    pub async fn mouse_down(&self, at: Option<Point>, button: Button) -> Result<()> {
        self.bounded("mouse press", self.timeouts().command, async {
            let at = self.move_to(at).await?;
            self.mouse(DispatchMouseEventType::MousePressed, at, Some(button), 1, 0)
                .await
        })
        .await
    }

    /// Release a mouse button, at `at` (CSS px) or where the mouse is.
    pub async fn mouse_up(&self, at: Option<Point>, button: Button) -> Result<()> {
        self.bounded("mouse release", self.timeouts().command, async {
            let at = self.move_to(at).await?;
            self.mouse(
                DispatchMouseEventType::MouseReleased,
                at,
                Some(button),
                1,
                0,
            )
            .await
        })
        .await
    }

    /// Move to `at` if given; return where the mouse is.
    async fn move_to(&self, at: Option<Point>) -> Result<Point> {
        match at {
            Some(at) => {
                self.mouse(DispatchMouseEventType::MouseMoved, at, None, 0, 0)
                    .await?;
                Ok(at)
            }
            None => Ok(*self.mouse_at.lock().unwrap()),
        }
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

    /// Press and release one key with the given modifier bitmask.
    async fn press(&self, def: &KeyDefinition, modifiers: i64) -> Result<()> {
        self.key_event(def, modifiers, true).await?;
        self.key_event(def, modifiers, false).await
    }

    /// Send one key-down (`down`) or key-up event.
    async fn key_event(&self, def: &KeyDefinition, modifiers: i64, down: bool) -> Result<()> {
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

        let kind = match (down, text.is_some()) {
            (true, true) => DispatchKeyEventType::KeyDown,
            (true, false) => DispatchKeyEventType::RawKeyDown,
            (false, _) => DispatchKeyEventType::KeyUp,
        };
        // No `nativeVirtualKeyCode`: the table's codes are Windows virtual key
        // codes, which on macOS name other keys (W's 87 is Keypad 5), and a
        // held key then made Chrome auto-repeat a stream of Numpad5 keydowns.
        let mut event = DispatchKeyEventParams::builder()
            .r#type(kind)
            .key(key_str)
            .code(def.code)
            .windows_virtual_key_code(def.key_code);
        if modifiers != 0 {
            event = event.modifiers(modifiers);
        }
        if down && let Some(t) = text {
            event = event.text(t);
        }
        let event = event
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build key event: {e}"))?;
        self.page.execute(event).await?;
        Ok(())
    }

    /// Wait (bounded) for the page to reach a stable state after a navigation
    /// or an action that may have triggered one.
    ///
    /// Avoids reading an empty body while a new document is still loading: a
    /// short head start lets a
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

/// Sets a form control (`this`) to `v`; see [`Tab::form_input`].
const FORM_INPUT_JS: &str = r#"function (v) {
  const el = this;
  const fire = () => {
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const on = v === true || v === 'true' || v === 'on' || v === 1 || v === 'checked';
  if (el.tagName === 'SELECT') {
    const want = (Array.isArray(v) ? v : [v]).map(String);
    let matched = 0;
    for (const o of el.options) {
      const hit = want.includes(o.value) || want.includes(o.text.trim());
      if (el.multiple) o.selected = hit;
      else if (hit && !matched) o.selected = true;
      if (hit) matched++;
    }
    if (!matched) {
      throw new Error('no option matches ' + JSON.stringify(v) + '; options: '
        + Array.from(el.options).map((o) => o.text.trim()).join(', '));
    }
    fire();
    return 'selected ' + Array.from(el.selectedOptions).map((o) => o.text.trim()).join(', ');
  }
  if (el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) {
    // A click toggles like a user would, firing the events frameworks expect.
    if (el.checked !== on) el.click();
    return el.checked ? 'checked' : 'unchecked';
  }
  const role = el.getAttribute('role');
  if (role === 'checkbox' || role === 'switch' || role === 'radio') {
    if ((el.getAttribute('aria-checked') === 'true') !== on) el.click();
    return el.getAttribute('aria-checked') === 'true' ? 'checked' : 'unchecked';
  }
  if (el.isContentEditable) {
    el.focus();
    el.textContent = String(v);
    fire();
    return 'set text';
  }
  if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
    el.focus();
    // The prototype's setter, so frameworks that track the value (React)
    // see the change.
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, String(v));
    fire();
    return 'set value';
  }
  throw new Error('not a form field (' + el.tagName.toLowerCase() + ')');
}"#;

/// User agent while emulating a phone.
const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36";

/// A JS result as text: JSON when it has a value, else its description
/// (`undefined`, a function, a DOM node, …).
fn describe_value(object: &RemoteObject) -> String {
    match &object.value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(value) => value.to_string(),
        None => object
            .description
            .clone()
            .or_else(|| {
                object
                    .unserializable_value
                    .as_ref()
                    .map(|v| v.inner().clone())
            })
            .unwrap_or_else(|| object.r#type.as_ref().to_string()),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}\n… (truncated at {max_chars} characters)")
}

/// The longest edge a screenshot may have: the image limit of the model
/// API, past which images are downscaled again (and coordinates would drift).
pub const MAX_SCREENSHOT_EDGE: u32 = 1568;

/// Room a headless browser window gets beyond its viewport for the browser
/// UI it reserves (143 px of its height on macOS).
pub(crate) const WINDOW_UI_ALLOWANCE: u32 = 200;

/// JPEG quality of recorded frames.
const JPEG_QUALITY: i64 = 90;

/// `requested`, reduced as needed so a `vw`×`vh` capture fits the edge limit.
fn fit_scale(requested: f64, vw: f64, vh: f64) -> f64 {
    let requested = if requested > 0.0 { requested } else { 1.0 };
    let longest = vw.max(vh) * requested;
    if longest > MAX_SCREENSHOT_EDGE as f64 {
        requested * MAX_SCREENSHOT_EDGE as f64 / longest
    } else {
        requested
    }
}

/// Parse space-separated keys or chords (`"w"`, `"ctrl+a Enter"`) into key
/// definitions with their modifier masks; an unknown key is an error.
fn parse_keys(keys: &str) -> Result<Vec<(&'static KeyDefinition, i64)>> {
    let chords: Vec<_> = keys
        .split_whitespace()
        .map(|chord| {
            let (modifiers, key) = parse_chord(chord);
            key_definition(key)
                .map(|def| (def, modifiers))
                .ok_or_else(|| anyhow::anyhow!("unknown key '{key}'"))
        })
        .collect::<Result<_>>()?;
    if chords.is_empty() {
        anyhow::bail!("no keys given");
    }
    Ok(chords)
}

/// Look a key up by name, tolerating case and common aliases, since key names
/// come from a model (`"return"`, `"Esc"`, `"pagedown"`, `"f5"`).
fn key_definition(name: &str) -> Option<&'static KeyDefinition> {
    if let Some(def) = get_key_definition(name) {
        return Some(def);
    }
    let alias = match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => "Enter",
        "esc" | "escape" => "Escape",
        "space" | "spacebar" => " ",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "pageup" | "page_up" => "PageUp",
        "pagedown" | "page_down" => "PageDown",
        "home" => "Home",
        "end" => "End",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" => "Insert",
        other => {
            // f1..f12 and other names whose canonical form is capitalized.
            let mut chars = other.chars();
            let capitalized: String = chars.next()?.to_uppercase().chain(chars).collect();
            return get_key_definition(&capitalized);
        }
    };
    get_key_definition(alias)
}

fn stale_ref(r: &str) -> anyhow::Error {
    anyhow::anyhow!("{r} is no longer in the page (read the page to get current refs)")
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.dialog_task.abort();
        for task in &self.log_tasks {
            task.abort();
        }
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
