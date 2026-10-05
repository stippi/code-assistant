//! A tab's screencast, shared by everyone who wants its frames.
//!
//! Chrome runs one screencast per page session, so a recording and a live
//! view cannot each start their own. The [`Screencast`] pump owns it instead:
//! it runs while at least one consumer is subscribed, sized for the largest
//! one, acknowledges every frame and fans it out:
//!
//! - a live view ([`LiveFrames`]) only ever sees the newest frame,
//! - a collector (a recording) gets every frame.
//!
//! A screencast shows the browser window, not the emulated viewport, so the
//! pump fits the window to the viewport whenever it (re)starts.

use crate::tab::{JPEG_QUALITY, capture, fit_window_to, scroll_and_viewport};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat,
    StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

/// One painted frame of a tab.
#[derive(Debug)]
pub struct ScreencastFrame {
    pub jpeg: Vec<u8>,
    pub metadata: FrameMetadata,
    /// When the frame arrived here.
    pub received: Instant,
}

/// Where a frame sits on the page: what maps a point on the frame back to
/// the CSS pixels input events take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetadata {
    /// Size of the area the frame shows, in DIP (device independent pixels).
    pub device_width: f64,
    pub device_height: f64,
    /// Top offset of the page within the frame, in DIP.
    pub offset_top: f64,
    /// Pinch zoom: CSS pixels times this are DIP.
    pub page_scale_factor: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    /// When Chrome painted the frame (seconds since the epoch), if it said.
    pub painted_at: Option<f64>,
}

/// How long one CDP call of the pump may take before it gives up.
const CALL_LIMIT: Duration = Duration::from_secs(10);
/// How long the pump waits before trying a failed start again.
const RETRY_AFTER: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Consumers {
    next_id: u64,
    /// Live views by id, with the largest frame each wants.
    live: HashMap<u64, (u32, u32)>,
    /// Collectors by id: their size and where their frames go.
    collectors: HashMap<u64, ((u32, u32), FrameSender)>,
}

type FrameSender = mpsc::UnboundedSender<Arc<ScreencastFrame>>;

impl Consumers {
    /// The frame size the screencast should run at: the largest any
    /// consumer wants, or `None` when nobody watches.
    fn wanted(&self) -> Option<(u32, u32)> {
        self.live
            .values()
            .chain(self.collectors.values().map(|(size, _)| size))
            .copied()
            .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)))
    }
}

struct Shared {
    consumers: Mutex<Consumers>,
    latest: watch::Sender<Option<Arc<ScreencastFrame>>>,
    /// Rings when consumers change or the viewport moved.
    wake: Notify,
    /// The size the screencast runs at, `None` while stopped.
    running: Mutex<Option<(u32, u32)>>,
    /// Restart even if the size did not change (the viewport did).
    refit: AtomicBool,
    /// Frames received, for the debug log.
    delivered: AtomicUsize,
}

/// The screencast pump of one tab. Aborted on drop.
pub(crate) struct Screencast {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Screencast {
    /// Start the (idle) pump for `page`. Needs a tokio runtime.
    pub(crate) async fn spawn(page: Page) -> anyhow::Result<Self> {
        let events = page.event_listener::<EventScreencastFrame>().await?;
        let shared = Arc::new(Shared {
            consumers: Mutex::default(),
            latest: watch::channel(None).0,
            wake: Notify::new(),
            running: Mutex::new(None),
            refit: AtomicBool::new(false),
            delivered: AtomicUsize::new(0),
        });
        let task = tokio::spawn(pump(page, shared.clone(), events));
        Ok(Self { shared, task })
    }

    /// Watch the newest frame, at most `max_size` large.
    pub(crate) fn live(&self, max_size: (u32, u32)) -> LiveFrames {
        let id = {
            let mut consumers = self.shared.consumers.lock().unwrap();
            if consumers.wanted().is_none() {
                // A frame from an earlier viewing would be stale.
                self.shared.latest.send_replace(None);
            }
            consumers.next_id += 1;
            let id = consumers.next_id;
            consumers.live.insert(id, max_size);
            id
        };
        self.shared.wake.notify_one();
        let mut frames = self.shared.latest.subscribe();
        frames.mark_changed();
        LiveFrames {
            frames,
            _guard: ConsumerGuard {
                shared: self.shared.clone(),
                id,
            },
        }
    }

    /// Collect every frame from now on, each at most `max_size` large.
    pub(crate) fn collect(&self, max_size: (u32, u32)) -> FrameCollector {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = {
            let mut consumers = self.shared.consumers.lock().unwrap();
            consumers.next_id += 1;
            let id = consumers.next_id;
            consumers.collectors.insert(id, (max_size, tx));
            id
        };
        self.shared.wake.notify_one();
        FrameCollector {
            frames: rx,
            _guard: ConsumerGuard {
                shared: self.shared.clone(),
                id,
            },
        }
    }

    /// The viewport changed: fit the window again and restart.
    pub(crate) fn refit(&self) {
        if self.is_running() {
            self.shared.refit.store(true, Ordering::Relaxed);
            self.shared.wake.notify_one();
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.shared.running.lock().unwrap().is_some()
    }
}

impl Drop for Screencast {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A live view of a tab: the newest frame, older ones skipped. Dropping it
/// unsubscribes; the screencast stops when nobody is left.
pub struct LiveFrames {
    frames: watch::Receiver<Option<Arc<ScreencastFrame>>>,
    _guard: ConsumerGuard,
}

impl LiveFrames {
    /// Wait for a frame newer than the last one returned. `None` once the
    /// tab is gone.
    pub async fn next(&mut self) -> Option<Arc<ScreencastFrame>> {
        loop {
            self.frames.changed().await.ok()?;
            if let Some(frame) = self.frames.borrow_and_update().clone() {
                return Some(frame);
            }
        }
    }
}

/// Every frame from subscription on (see [`Screencast::collect`]).
pub(crate) struct FrameCollector {
    pub(crate) frames: mpsc::UnboundedReceiver<Arc<ScreencastFrame>>,
    _guard: ConsumerGuard,
}

struct ConsumerGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        let mut consumers = self.shared.consumers.lock().unwrap();
        consumers.live.remove(&self.id);
        consumers.collectors.remove(&self.id);
        drop(consumers);
        self.shared.wake.notify_one();
    }
}

async fn pump(
    page: Page,
    shared: Arc<Shared>,
    mut events: impl futures::Stream<Item = Arc<EventScreencastFrame>> + Unpin,
) {
    loop {
        tokio::select! {
            _ = shared.wake.notified() => reconcile(&page, &shared).await,
            event = events.next() => match event {
                Some(event) => deliver(&page, &shared, &event).await,
                None => break,
            },
        }
    }
}

/// Bring the screencast in line with its consumers: start, resize, restart
/// after a viewport change, or stop. A start that fails (the page navigated
/// away under it, say) is tried again shortly.
async fn reconcile(page: &Page, shared: &Arc<Shared>) {
    let wanted = shared.consumers.lock().unwrap().wanted();
    let running = *shared.running.lock().unwrap();
    let refit = shared.refit.swap(false, Ordering::Relaxed);
    if wanted == running && !refit {
        return;
    }
    let Some((width, height)) = wanted else {
        tracing::debug!("screencast: stopping");
        let _ =
            tokio::time::timeout(CALL_LIMIT, page.execute(StopScreencastParams::default())).await;
        *shared.running.lock().unwrap() = None;
        return;
    };
    tracing::debug!("screencast: starting at {width}×{height} (refit: {refit})");
    let started = async {
        let (sx, sy, vw, vh) = scroll_and_viewport(page).await?;
        if let Err(e) = fit_window_to(page, vw, vh).await {
            tracing::debug!("screencast: cannot fit the window to the viewport: {e}");
        }
        page.execute(
            StartScreencastParams::builder()
                .format(StartScreencastFormat::Jpeg)
                .quality(JPEG_QUALITY)
                .max_width(width as i64)
                .max_height(height as i64)
                .build(),
        )
        .await?;
        *shared.running.lock().unwrap() = Some((width, height));
        // A still page paints nothing: show live views the page as it is.
        if !shared.consumers.lock().unwrap().live.is_empty() {
            let scale = (width as f64 / vw).min(height as f64 / vh).min(1.0);
            let jpeg = capture(page, sx, sy, vw, vh, scale, CaptureScreenshotFormat::Jpeg).await?;
            let metadata = FrameMetadata {
                device_width: vw,
                device_height: vh,
                offset_top: 0.0,
                page_scale_factor: 1.0,
                scroll_x: sx,
                scroll_y: sy,
                painted_at: None,
            };
            publish_live(shared, jpeg, metadata);
        }
        anyhow::Ok(())
    };
    let failure = match tokio::time::timeout(CALL_LIMIT, started).await {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e.to_string(),
        Err(_) => "timed out".to_string(),
    };
    tracing::debug!("screencast: failed to start, trying again: {failure}");
    // Also when only the first picture failed after the screencast started.
    shared.refit.store(true, Ordering::Relaxed);
    let shared = shared.clone();
    tokio::spawn(async move {
        tokio::time::sleep(RETRY_AFTER).await;
        shared.wake.notify_one();
    });
}

fn publish_live(shared: &Shared, jpeg: Vec<u8>, metadata: FrameMetadata) {
    // No frame can arrive meanwhile: the pump delivers them in the same task.
    shared.latest.send_replace(Some(Arc::new(ScreencastFrame {
        jpeg,
        metadata,
        received: Instant::now(),
    })));
}

/// Acknowledge a frame (Chrome sends no more until it is) and hand it out.
async fn deliver(page: &Page, shared: &Shared, event: &EventScreencastFrame) {
    use base64::Engine;
    let _ = tokio::time::timeout(
        CALL_LIMIT,
        page.execute(ScreencastFrameAckParams::new(event.session_id)),
    )
    .await;
    let Ok(jpeg) =
        base64::engine::general_purpose::STANDARD.decode(AsRef::<str>::as_ref(&event.data))
    else {
        return;
    };
    let meta = &event.metadata;
    let frame = Arc::new(ScreencastFrame {
        jpeg,
        metadata: FrameMetadata {
            device_width: meta.device_width,
            device_height: meta.device_height,
            offset_top: meta.offset_top,
            page_scale_factor: meta.page_scale_factor,
            scroll_x: meta.scroll_offset_x,
            scroll_y: meta.scroll_offset_y,
            painted_at: meta.timestamp.as_ref().map(|t| *t.inner()),
        },
        received: Instant::now(),
    });
    let delivered = shared.delivered.fetch_add(1, Ordering::Relaxed) + 1;
    if delivered.is_multiple_of(60) {
        tracing::debug!("screencast: {delivered} frames delivered");
    }
    let consumers = shared.consumers.lock().unwrap();
    for (_, tx) in consumers.collectors.values() {
        let _ = tx.send(frame.clone());
    }
    if !consumers.live.is_empty() {
        shared.latest.send_replace(Some(frame));
    }
}
