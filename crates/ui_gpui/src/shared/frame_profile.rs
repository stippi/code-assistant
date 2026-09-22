//! Opt-in frame profiling for the messages view.
//!
//! Off unless [`ENV_VAR`] is set, in which case every wrapped element records
//! how long its `request_layout`, `prepaint` and `paint` take, and the code
//! that builds it can record its build time with a [`scope`]. A reporter
//! prints per-label averages per drawn frame alongside gpui's own
//! `Window::draw` timings.
//!
//! When off, [`timed`] hands the element back untouched and [`scope`]
//! returns `None`, so the normal build carries a single atomic load per call
//! and no wrapper elements.
//!
//! ```text
//! CODE_ASSISTANT_FRAME_PROFILE=1       report only (every REPORT_INTERVAL)
//! CODE_ASSISTANT_FRAME_PROFILE=scroll  report and sweep the message list up and down
//!                                      by moving its scroll offset
//! CODE_ASSISTANT_FRAME_PROFILE=wheel   report and sweep with scroll-wheel events
//!                                      dispatched through the window
//! ```

use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Window,
};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Environment variable that switches profiling on.
pub const ENV_VAR: &str = "CODE_ASSISTANT_FRAME_PROFILE";

/// Label of the whole virtualized message list.
pub const LIST: &str = "messages.list";
/// Label of one list row (a message with all its blocks).
pub const ROW: &str = "row";
/// Label prefix of block elements (`block.text`, `block.tool:edit`, ...).
pub const BLOCK_PREFIX: &str = "block.";

/// How often the reporter prints.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// Collect and report.
    Report,
    /// Collect, report, and sweep the list by moving its scroll offset.
    Scroll,
    /// Collect, report, and sweep the list with dispatched scroll-wheel
    /// events, so hit testing and the scroll handlers run too.
    Wheel,
}

impl Mode {
    pub fn parse(value: Option<&str>) -> Mode {
        match value.map(str::trim) {
            None | Some("" | "0" | "off" | "false") => Mode::Off,
            Some("scroll") => Mode::Scroll,
            Some("wheel") => Mode::Wheel,
            Some(_) => Mode::Report,
        }
    }
}

static MODE: OnceLock<Mode> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static SCROLLING: AtomicBool = AtomicBool::new(false);

/// Reads [`ENV_VAR`] once and switches collection on when it asks for it.
pub fn init_from_env() -> Mode {
    let mode = *MODE.get_or_init(|| Mode::parse(std::env::var(ENV_VAR).ok().as_deref()));
    if mode != Mode::Off {
        enable();
    }
    mode
}

/// Switches collection on, including gpui's frame timings.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
    gpui_kit::set_trace_enabled(true);
}

#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Marks reports as taken while a scroll sweep runs.
pub fn set_scrolling(scrolling: bool) {
    SCROLLING.store(scrolling, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Building the element tree (a [`scope`] around a `render` body).
    Build,
    RequestLayout,
    Prepaint,
    Paint,
}

/// Accumulated time of one label since the last [`take`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelStats {
    /// Elements that went through `request_layout`.
    pub elements: u64,
    /// Build scopes that ended.
    pub builds: u64,
    pub build: Duration,
    pub request_layout: Duration,
    pub prepaint: Duration,
    pub paint: Duration,
}

impl LabelStats {
    fn add(&mut self, phase: Phase, elapsed: Duration) {
        match phase {
            Phase::Build => {
                self.builds += 1;
                self.build += elapsed;
            }
            Phase::RequestLayout => {
                self.elements += 1;
                self.request_layout += elapsed;
            }
            Phase::Prepaint => self.prepaint += elapsed,
            Phase::Paint => self.paint += elapsed,
        }
    }

    pub fn total(&self) -> Duration {
        self.build + self.request_layout + self.prepaint + self.paint
    }

    /// How many of this label a frame held (elements, or scopes for
    /// labels that only have those).
    pub fn count(&self) -> u64 {
        self.elements.max(self.builds)
    }
}

static STATS: LazyLock<Mutex<HashMap<SharedString, LabelStats>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn record(label: &SharedString, phase: Phase, elapsed: Duration) {
    let mut stats = STATS.lock().unwrap();
    match stats.get_mut(label) {
        Some(entry) => entry.add(phase, elapsed),
        None => stats.entry(label.clone()).or_default().add(phase, elapsed),
    }
}

/// Takes everything recorded since the previous call.
pub fn take() -> HashMap<SharedString, LabelStats> {
    std::mem::take(&mut *STATS.lock().unwrap())
}

/// Records the time until it is dropped as [`Phase::Build`] of its label.
pub struct Scope {
    label: SharedString,
    start: Instant,
}

impl Drop for Scope {
    fn drop(&mut self) {
        record(&self.label, Phase::Build, self.start.elapsed());
    }
}

/// A build scope for `label`; `None` while profiling is off.
pub fn scope(label: impl Into<SharedString>) -> Option<Scope> {
    enabled().then(|| Scope {
        label: label.into(),
        start: Instant::now(),
    })
}

/// Wraps `element` so its layout, prepaint and paint times land under
/// `label`; hands it back untouched while profiling is off.
pub fn timed(label: impl Into<SharedString>, element: AnyElement) -> AnyElement {
    if !enabled() {
        return element;
    }
    Timed {
        label: label.into(),
        child: element,
    }
    .into_any_element()
}

/// Transparent wrapper: reuses the child's layout node, so it adds no
/// layout of its own.
pub struct Timed {
    label: SharedString,
    child: AnyElement,
}

impl IntoElement for Timed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Timed {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let start = Instant::now();
        let layout_id = self.child.request_layout(window, cx);
        record(&self.label, Phase::RequestLayout, start.elapsed());
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let start = Instant::now();
        self.child.prepaint(window, cx);
        record(&self.label, Phase::Prepaint, start.elapsed());
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let start = Instant::now();
        self.child.paint(window, cx);
        record(&self.label, Phase::Paint, start.elapsed());
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// Distribution of `Window::draw` durations over a report interval.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameSummary {
    pub frames: usize,
    pub mean: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub max: Duration,
}

impl FrameSummary {
    pub fn from_draws(mut draws: Vec<Duration>) -> Self {
        if draws.is_empty() {
            return Self::default();
        }
        draws.sort_unstable();
        let frames = draws.len();
        let percentile = |p: f64| draws[((frames - 1) as f64 * p).round() as usize];
        Self {
            frames,
            mean: draws.iter().sum::<Duration>() / frames as u32,
            p50: percentile(0.5),
            p95: percentile(0.95),
            max: draws[frames - 1],
        }
    }
}

/// One report interval: gpui's frame timings plus the per-label times,
/// both over the same drawn frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub elapsed: Duration,
    pub scrolling: bool,
    pub frames: FrameSummary,
    /// Sorted by total time, largest first.
    pub labels: Vec<(SharedString, LabelStats)>,
}

impl Report {
    pub fn new(
        elapsed: Duration,
        scrolling: bool,
        draws: Vec<Duration>,
        stats: HashMap<SharedString, LabelStats>,
    ) -> Self {
        let mut labels: Vec<_> = stats.into_iter().collect();
        labels.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));
        Self {
            elapsed,
            scrolling,
            frames: FrameSummary::from_draws(draws),
            labels,
        }
    }

    fn stats(&self, label: &str) -> Option<&LabelStats> {
        self.labels
            .iter()
            .find(|(name, _)| name.as_ref() == label)
            .map(|(_, stats)| stats)
    }

    /// Time the list spent laying rows out (taffy plus text measuring),
    /// which no row wrapper sees: the list's prepaint minus what the rows
    /// recorded inside it. `None` without a list or rows.
    pub fn row_layout_compute(&self) -> Option<Duration> {
        let list = self.stats(LIST)?;
        let row = self.stats(ROW)?;
        Some(
            list.prepaint
                .saturating_sub(row.build)
                .saturating_sub(row.request_layout)
                .saturating_sub(row.prepaint),
        )
    }
}

fn per_frame_ms(total: Duration, frames: usize) -> f64 {
    if frames == 0 {
        return 0.0;
    }
    total.as_secs_f64() * 1000.0 / frames as f64
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let frames = self.frames.frames;
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        writeln!(
            f,
            "[frame-profile] t={:.1}s scroll={} frames={} draw/frame mean={:.2}ms p50={:.2}ms p95={:.2}ms max={:.2}ms",
            self.elapsed.as_secs_f64(),
            if self.scrolling { "yes" } else { "no" },
            frames,
            ms(self.frames.mean),
            ms(self.frames.p50),
            ms(self.frames.p95),
            ms(self.frames.max),
        )?;
        if frames == 0 {
            return Ok(());
        }
        let draw_ms = ms(self.frames.mean);
        let share = |per_frame: f64| {
            if draw_ms > 0.0 {
                per_frame / draw_ms * 100.0
            } else {
                0.0
            }
        };
        writeln!(
            f,
            "  {:<34} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
            "label (ms per frame)",
            "n/frame",
            "build",
            "layout",
            "prepaint",
            "paint",
            "total",
            "%draw"
        )?;
        for (label, stats) in &self.labels {
            let total = per_frame_ms(stats.total(), frames);
            writeln!(
                f,
                "  {:<34} {:>8.1} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>5.0}%",
                label.as_ref(),
                stats.count() as f64 / frames as f64,
                per_frame_ms(stats.build, frames),
                per_frame_ms(stats.request_layout, frames),
                per_frame_ms(stats.prepaint, frames),
                per_frame_ms(stats.paint, frames),
                total,
                share(total),
            )?;
        }
        if let Some(compute) = self.row_layout_compute() {
            let total = per_frame_ms(compute, frames);
            writeln!(
                f,
                "  {:<34} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8.2} {:>5.0}%",
                "row layout compute (derived)",
                "",
                "",
                "",
                "",
                "",
                total,
                share(total),
            )?;
        }
        Ok(())
    }
}

static FRAMES: LazyLock<Mutex<gpui_kit::FrameTimingCollector>> =
    LazyLock::new(|| Mutex::new(gpui_kit::FrameTimingCollector::new()));

/// Takes the frames drawn and the labels recorded since the previous report.
pub fn report(elapsed: Duration) -> Report {
    let draws = FRAMES
        .lock()
        .unwrap()
        .collect_unseen()
        .iter()
        .filter_map(|event| match event {
            gpui_kit::FrameEvent::Draw(frame) => Some(frame.draw_duration()),
            gpui_kit::FrameEvent::Present(_) => None,
        })
        .collect();
    Report::new(elapsed, SCROLLING.load(Ordering::Relaxed), draws, take())
}

/// Prints a report every [`REPORT_INTERVAL`] to stderr for the rest of the
/// app's life.
pub fn spawn_reporter(cx: &mut App) {
    // Discard whatever was recorded before the first interval starts.
    drop(report(Duration::ZERO));
    let started = Instant::now();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(REPORT_INTERVAL).await;
            eprint!("{}", report(started.elapsed()));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{
        Context, Render, TestAppContext, VisualTestContext, div, point, prelude::*, px, size,
    };

    #[test]
    fn mode_parses_the_env_value() {
        assert_eq!(Mode::parse(None), Mode::Off);
        assert_eq!(Mode::parse(Some("")), Mode::Off);
        assert_eq!(Mode::parse(Some("0")), Mode::Off);
        assert_eq!(Mode::parse(Some("off")), Mode::Off);
        assert_eq!(Mode::parse(Some("1")), Mode::Report);
        assert_eq!(Mode::parse(Some("true")), Mode::Report);
        assert_eq!(Mode::parse(Some(" scroll ")), Mode::Scroll);
        assert_eq!(Mode::parse(Some("wheel")), Mode::Wheel);
    }

    #[test]
    fn label_stats_sum_phases_and_count_elements() {
        let mut stats = LabelStats::default();
        stats.add(Phase::Build, Duration::from_millis(1));
        stats.add(Phase::RequestLayout, Duration::from_millis(2));
        stats.add(Phase::RequestLayout, Duration::from_millis(2));
        stats.add(Phase::Prepaint, Duration::from_millis(3));
        stats.add(Phase::Paint, Duration::from_millis(4));
        assert_eq!(stats.elements, 2);
        assert_eq!(stats.builds, 1);
        assert_eq!(stats.count(), 2);
        assert_eq!(stats.total(), Duration::from_millis(12));
    }

    #[test]
    fn frame_summary_reports_percentiles() {
        let draws: Vec<_> = (1..=20).map(Duration::from_millis).collect();
        let summary = FrameSummary::from_draws(draws);
        assert_eq!(summary.frames, 20);
        assert_eq!(summary.mean, Duration::from_micros(10_500));
        assert_eq!(summary.p50, Duration::from_millis(11));
        assert_eq!(summary.p95, Duration::from_millis(19));
        assert_eq!(summary.max, Duration::from_millis(20));
        assert_eq!(
            FrameSummary::from_draws(Vec::new()),
            FrameSummary::default()
        );
    }

    fn stats(build: u64, layout: u64, prepaint: u64, paint: u64) -> LabelStats {
        LabelStats {
            elements: 1,
            builds: 1,
            build: Duration::from_millis(build),
            request_layout: Duration::from_millis(layout),
            prepaint: Duration::from_millis(prepaint),
            paint: Duration::from_millis(paint),
        }
    }

    #[test]
    fn report_sorts_labels_by_total_and_derives_row_layout_compute() {
        let mut recorded = HashMap::new();
        recorded.insert(SharedString::from(LIST), stats(0, 1, 40, 5));
        recorded.insert(SharedString::from(ROW), stats(2, 10, 8, 4));
        recorded.insert(SharedString::from("block.text"), stats(1, 6, 5, 2));
        let report = Report::new(
            Duration::from_secs(4),
            true,
            vec![Duration::from_millis(10); 2],
            recorded,
        );

        let order: Vec<_> = report.labels.iter().map(|(l, _)| l.as_ref()).collect();
        assert_eq!(order, vec![LIST, ROW, "block.text"]);
        // 40 - (2 + 10 + 8)
        assert_eq!(report.row_layout_compute(), Some(Duration::from_millis(20)));

        let text = report.to_string();
        assert!(text.contains("scroll=yes frames=2"), "{text}");
        // ROW total 24ms over 2 frames = 12.00ms per frame, 120% of a 10ms draw.
        assert!(text.contains("12.00"), "{text}");
        assert!(text.contains("row layout compute (derived)"), "{text}");
    }

    #[test]
    fn report_without_list_has_no_derived_line() {
        let mut recorded = HashMap::new();
        recorded.insert(SharedString::from(ROW), stats(1, 1, 1, 1));
        let report = Report::new(
            Duration::ZERO,
            false,
            vec![Duration::from_millis(1)],
            recorded,
        );
        assert_eq!(report.row_layout_compute(), None);
        assert!(!report.to_string().contains("derived"));
    }

    struct Root;

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    fn timed_element_records_every_phase(cx: &mut TestAppContext) {
        enable();
        let label = SharedString::from("frame_profile.test.timed");
        let (_, cx) = cx.add_window_view(|_, _| Root);
        let cx: &mut VisualTestContext = cx;
        cx.draw(point(px(0.), px(0.)), size(px(200.), px(200.)), {
            let label = label.clone();
            move |_, _| {
                let _build = scope(label.clone());
                timed(label, div().child("hello").into_any_element())
            }
        });

        let stats = take().remove(&label).expect("label recorded");
        assert_eq!(stats.elements, 1);
        assert_eq!(stats.builds, 1);
        assert!(stats.request_layout > Duration::ZERO);
        assert!(stats.prepaint > Duration::ZERO);
        assert!(stats.paint > Duration::ZERO);
    }
}
