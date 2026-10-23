//! Background notifications into a session: the agent starts something,
//! goes on with other work (or ends its turn), and is told when there is
//! something to look at — instead of polling.
//!
//! Two producers share one delivery path (a framed message, injected like a
//! wakeup: a turn starts when the session is idle, the message is queued when
//! a turn runs):
//!
//! - **Monitors** (`monitor` tool): a command whose output is a stream of
//!   events. Every line it prints reaches the agent as
//!   `[monitor #3] <description>` + the lines (batched briefly); when the
//!   command ends — exit, timeout, flood guard — a final
//!   `[monitor #3 ended] …` follows. Stopping a monitor (`stop_monitor`)
//!   delivers nothing.
//! - **Background jobs** (`execute_command` session mode): when a process
//!   that kept running in the background exits, the agent gets
//!   `[background job <id> exited] <command>` and decides whether to read
//!   the rest of its output with `write_stdin`. A job the agent saw exit
//!   itself (a `write_stdin` call stops tracking it) is not reported.
//!
//! Like wakeups, monitors are not persisted: they die with the process (and
//! with their session). Monitor ids are numbered per session, continuing the
//! session's history. See `docs/background-monitors.md`.

use crate::session::SessionService;
use crate::session::sleep_inhibitor::SleepInhibitor;
use agent_core::SerializedToolExecution;
use pty_session::{PtySession, PtySessionManager, TerminalOutputSink};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, warn};

/// Lines arriving within this window after the first one are delivered
/// together, so a burst of output becomes one message.
const BATCH_WINDOW: Duration = Duration::from_millis(500);
/// At most this many lines per message; more are counted, not delivered.
const MAX_EVENT_LINES: usize = 40;
/// Longer lines are cut.
const MAX_LINE_CHARS: usize = 400;
/// Flood guard: a monitor delivering more messages than this within
/// [`FLOOD_WINDOW`] is stopped — every message may start a model turn.
const MAX_EVENTS_PER_WINDOW: usize = 20;
const FLOOD_WINDOW: Duration = Duration::from_secs(60);
/// After the command exited, wait this long for output still in flight.
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(300);
/// A background job's exit is reported this long after it happened, so a
/// tool call waiting on the job right now sees the exit first (and the
/// notification is dropped).
const JOB_EXIT_GRACE: Duration = Duration::from_secs(1);
/// Command lines in a job notification are cut to this many characters.
const MAX_TITLE_CHARS: usize = 120;

const MONITOR_FRAME: &str = "[monitor #";
const JOB_FRAME: &str = "[background job ";

/// The message delivering lines a monitor printed.
pub fn monitor_event_message(
    id: u64,
    description: &str,
    lines: &[String],
    omitted: usize,
) -> String {
    let mut message = format!("{MONITOR_FRAME}{id}] {}", single_line(description));
    push_lines(&mut message, lines, omitted);
    message
}

/// The message delivered when a monitor's command ended on its own (or was
/// stopped by the timeout or the flood guard), with its last lines.
pub fn monitor_ended_message(
    id: u64,
    description: &str,
    end: &MonitorEnd,
    lines: &[String],
    omitted: usize,
) -> String {
    let mut message = format!(
        "{MONITOR_FRAME}{id} ended] {}\n{}",
        single_line(description),
        end.describe()
    );
    push_lines(&mut message, lines, omitted);
    message
}

/// The message delivered when background job `job_id` exited.
pub fn job_exited_message(job_id: u32, command_line: &str, exit_code: Option<i32>) -> String {
    let status = match exit_code {
        Some(code) => format!("It exited with code {code}."),
        None => "It exited without an exit code (terminated by a signal).".to_string(),
    };
    format!(
        "{JOB_FRAME}{job_id} exited] {}\n{status} Read its remaining output with write_stdin \
         (session_id {job_id}) if you need it.",
        title(command_line)
    )
}

fn push_lines(message: &mut String, lines: &[String], omitted: usize) {
    for line in lines {
        message.push('\n');
        message.push_str(line);
    }
    if omitted > 0 {
        message.push_str(&format!("\n… ({omitted} more lines omitted)"));
    }
}

/// `text` on one line.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first line of a command line, cut for a message header.
fn title(command_line: &str) -> String {
    let first = command_line.trim().lines().next().unwrap_or_default();
    let mut title: String = first.chars().take(MAX_TITLE_CHARS).collect();
    if title.len() < first.len() || command_line.trim().lines().nth(1).is_some() {
        title.push('…');
    }
    title
}

/// What an injected notification reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// Lines monitor `id` printed.
    MonitorEvent { id: u64 },
    /// Monitor `id` ended.
    MonitorEnded { id: u64 },
    /// Background job `id` (its `write_stdin` session id) exited.
    JobExited { id: u32 },
}

/// An injected notification message, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNotification<'a> {
    pub kind: NotificationKind,
    /// The monitor's description, or the job's command line.
    pub title: &'a str,
    /// Everything after the header line; may be empty.
    pub body: &'a str,
}

/// Parse an injected monitor or background-job message; `None` for any
/// other message.
pub fn parse_notification_message(message: &str) -> Option<ParsedNotification<'_>> {
    let (header, body) = message.split_once('\n').unwrap_or((message, ""));
    let (kind, title) = if let Some(rest) = header.strip_prefix(MONITOR_FRAME) {
        let (tag, title) = rest.split_once("] ")?;
        let kind = match tag.strip_suffix(" ended") {
            Some(id) => NotificationKind::MonitorEnded {
                id: id.parse().ok()?,
            },
            None => NotificationKind::MonitorEvent {
                id: tag.parse().ok()?,
            },
        };
        (kind, title)
    } else {
        let (tag, title) = header.strip_prefix(JOB_FRAME)?.split_once("] ")?;
        let id = tag.strip_suffix(" exited")?.parse().ok()?;
        (NotificationKind::JobExited { id }, title)
    };
    Some(ParsedNotification {
        kind,
        title: title.trim(),
        body,
    })
}

/// Parse a message made of nothing but injected notifications — several
/// arrive in one message when they were queued while a turn ran. `None` when
/// any of it is something else.
pub fn parse_notification_messages(message: &str) -> Option<Vec<ParsedNotification<'_>>> {
    let is_header = |line: &str| line.starts_with(MONITOR_FRAME) || line.starts_with(JOB_FRAME);
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in message.split_inclusive('\n') {
        if is_header(line) {
            starts.push(offset);
        } else if starts.is_empty() && !line.trim().is_empty() {
            return None;
        }
        offset += line.len();
    }
    if starts.is_empty() {
        return None;
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(message.len());
            parse_notification_message(message[start..end].trim_end())
        })
        .collect()
}

/// The highest monitor id `executions` (a session's history) already use.
pub fn highest_monitor_id(executions: &[SerializedToolExecution]) -> u64 {
    executions
        .iter()
        .filter(|exec| exec.tool_name == "monitor")
        .filter_map(|exec| exec.result_json.get("monitor_id")?.as_u64())
        .max()
        .unwrap_or(0)
}

/// Why a monitor ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorEnd {
    /// The command exited.
    Exited(Option<i32>),
    /// It ran into its timeout and was stopped.
    TimedOut(Duration),
    /// It printed too often and was stopped.
    TooManyEvents,
}

impl MonitorEnd {
    fn describe(&self) -> String {
        match self {
            Self::Exited(Some(code)) => format!("The command exited with code {code}."),
            Self::Exited(None) => {
                "The command exited without an exit code (terminated by a signal).".to_string()
            }
            Self::TimedOut(timeout) => format!(
                "The monitor reached its timeout of {}s and was stopped.",
                timeout.as_secs()
            ),
            Self::TooManyEvents => format!(
                "The monitor was stopped: it reported more than {MAX_EVENTS_PER_WINDOW} events \
                 within {}s. Make the command print only when something changes.",
                FLOOD_WINDOW.as_secs()
            ),
        }
    }
}

/// A running monitor, as frontends show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMonitorInfo {
    pub id: u64,
    pub description: String,
    pub command_line: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the timeout stops it.
    pub ends_at: chrono::DateTime<chrono::Utc>,
}

/// Delivers notifications into a session. The production implementation is
/// [`SessionService`]; tests substitute a recorder.
#[async_trait::async_trait]
pub trait MonitorSink: Send + Sync + 'static {
    async fn deliver(&self, session_id: &str, message: &str);

    /// The monitors running in `session_id` changed (oldest first).
    fn active_changed(&self, _session_id: &str, _active: Vec<ActiveMonitorInfo>) {}
}

#[async_trait::async_trait]
impl MonitorSink for SessionService {
    async fn deliver(&self, session_id: &str, message: &str) {
        if let Err(e) = self
            .inject_notification(session_id.to_string(), message.to_string())
            .await
        {
            warn!("Failed to deliver a notification to session {session_id}: {e}");
        }
    }

    fn active_changed(&self, session_id: &str, active: Vec<ActiveMonitorInfo>) {
        self.publish_active_monitors(session_id, active);
    }
}

/// Receives a monitor's raw output; created with [`monitor_output`].
pub struct MonitorOutput(mpsc::UnboundedReceiver<Vec<u8>>);

/// A [`TerminalOutputSink`] to bind to the monitor's process before it is
/// spawned, and the receiving end [`MonitorHandle::start`] reads from.
pub fn monitor_output() -> (Arc<dyn TerminalOutputSink>, MonitorOutput) {
    struct ChannelSink(mpsc::UnboundedSender<Vec<u8>>);
    impl TerminalOutputSink for ChannelSink {
        fn emit(&self, bytes: &[u8]) {
            let _ = self.0.send(bytes.to_vec());
        }
    }
    let (tx, rx) = mpsc::unbounded_channel();
    (Arc::new(ChannelSink(tx)), MonitorOutput(rx))
}

/// What a new monitor runs.
pub struct MonitorStart {
    pub description: String,
    pub command_line: String,
    pub timeout: Duration,
    /// The spawned command, its output bound to `output`.
    pub process: Arc<PtySession>,
    pub output: MonitorOutput,
}

struct ActiveMonitor {
    info: ActiveMonitorInfo,
    /// Only held: dropping it stops the monitor without a notification.
    _stop: oneshot::Sender<()>,
}

#[derive(Default)]
struct State {
    /// The last id issued per session.
    issued: HashMap<String, u64>,
    /// Running monitors per session, by id.
    active: HashMap<String, Vec<ActiveMonitor>>,
}

impl State {
    fn infos(&self, session_id: &str) -> Vec<ActiveMonitorInfo> {
        self.active
            .get(session_id)
            .map(|monitors| monitors.iter().map(|m| m.info.clone()).collect())
            .unwrap_or_default()
    }

    fn count(&self) -> usize {
        self.active.values().map(Vec::len).sum()
    }
}

/// Cloneable handle to the process' monitors and background notifications.
#[derive(Clone)]
pub struct MonitorHandle {
    sink: Arc<dyn MonitorSink>,
    state: Arc<Mutex<State>>,
    sleep_inhibitor: Option<Arc<SleepInhibitor>>,
}

impl MonitorHandle {
    /// A running monitor holds the sleep inhibitor (when provided): a
    /// sleeping machine watches nothing.
    pub fn new(sink: impl MonitorSink, sleep_inhibitor: Option<Arc<SleepInhibitor>>) -> Self {
        Self {
            sink: Arc::new(sink),
            state: Arc::default(),
            sleep_inhibitor,
        }
    }

    /// Start watching `start.process` for `session_id`. Returns the monitor's
    /// id: the next one of the session, above `used`, the highest id its
    /// history already uses. Must be called within a tokio runtime.
    pub fn start(&self, session_id: String, used: u64, start: MonitorStart) -> u64 {
        let (stop_tx, stop_rx) = oneshot::channel();
        let now = chrono::Utc::now();
        let (id, active) = {
            let mut state = self.state.lock().unwrap();
            let last = state.issued.entry(session_id.clone()).or_default();
            *last = (*last).max(used) + 1;
            let id = *last;
            let was_idle = state.count() == 0;
            state
                .active
                .entry(session_id.clone())
                .or_default()
                .push(ActiveMonitor {
                    info: ActiveMonitorInfo {
                        id,
                        description: start.description.clone(),
                        command_line: start.command_line.clone(),
                        started_at: now,
                        ends_at: now
                            + chrono::TimeDelta::from_std(start.timeout)
                                .unwrap_or(chrono::TimeDelta::MAX),
                    },
                    _stop: stop_tx,
                });
            if was_idle && let Some(inhibitor) = &self.sleep_inhibitor {
                inhibitor.monitors_started();
            }
            (id, state.infos(&session_id))
        };
        debug!("Monitor {id} started for session {session_id}");
        self.sink.active_changed(&session_id, active);
        tokio::spawn(run_monitor(self.clone(), session_id, id, start, stop_rx));
        id
    }

    /// Stop a monitor of a session without a notification. Returns whether
    /// it was still running.
    pub fn stop(&self, session_id: &str, id: u64) -> bool {
        self.remove(session_id, |monitor| monitor.info.id == id)
    }

    /// Stop all monitors of a session (used when the session is deleted).
    pub fn stop_session(&self, session_id: &str) {
        self.remove(session_id, |_| true);
    }

    /// The monitors running in `session_id`, oldest first.
    pub fn active(&self, session_id: &str) -> Vec<ActiveMonitorInfo> {
        self.state.lock().unwrap().infos(session_id)
    }

    /// Forget the monitors of `session_id` matching `which` (dropping their
    /// stop senders stops their tasks); `true` if any was found.
    fn remove(&self, session_id: &str, which: impl Fn(&ActiveMonitor) -> bool) -> bool {
        let (removed, active) = {
            let mut state = self.state.lock().unwrap();
            let Some(monitors) = state.active.get_mut(session_id) else {
                return false;
            };
            let before = monitors.len();
            monitors.retain(|monitor| !which(monitor));
            let removed = monitors.len() < before;
            if monitors.is_empty() {
                state.active.remove(session_id);
            }
            if removed
                && state.count() == 0
                && let Some(inhibitor) = &self.sleep_inhibitor
            {
                inhibitor.monitors_stopped();
            }
            (removed, state.infos(session_id))
        };
        if removed {
            self.sink.active_changed(session_id, active);
        }
        removed
    }
}

/// A [`MonitorHandle`] bound to one session, handed to tools through
/// `ToolServices` — the session id comes from the wiring, not the model.
#[derive(Clone)]
pub struct SessionMonitors {
    pub handle: MonitorHandle,
    pub session_id: String,
    /// The highest monitor id the session's history uses
    /// ([`highest_monitor_id`]), so ids continue across restarts.
    pub used_ids: u64,
}

impl SessionMonitors {
    pub fn start(&self, start: MonitorStart) -> u64 {
        self.handle
            .start(self.session_id.clone(), self.used_ids, start)
    }

    pub fn stop(&self, id: u64) -> bool {
        self.handle.stop(&self.session_id, id)
    }

    /// Tell the session when background job `job_id` (tracked in `jobs`)
    /// exits — unless the agent saw the exit itself by then, which stops
    /// tracking the job. A job evicted or terminated with its session is not
    /// reported either.
    pub fn watch_job_exit(
        &self,
        jobs: &Arc<PtySessionManager>,
        job_id: u32,
        process: &PtySession,
        command_line: &str,
    ) {
        let exited = process.exited();
        let jobs = Arc::downgrade(jobs);
        let handle = self.handle.clone();
        let session_id = self.session_id.clone();
        let command_line = command_line.to_string();
        tokio::spawn(async move {
            let exit_code = exited.await;
            tokio::time::sleep(JOB_EXIT_GRACE).await;
            if jobs.upgrade().is_some_and(|jobs| jobs.is_tracked(job_id)) {
                debug!("Background job {job_id} of session {session_id} exited");
                handle
                    .sink
                    .deliver(
                        &session_id,
                        &job_exited_message(job_id, &command_line, exit_code),
                    )
                    .await;
            }
        });
    }
}

/// Splits raw output into sanitized, non-empty lines.
#[derive(Default)]
struct LineSplitter {
    partial: Vec<u8>,
}

impl LineSplitter {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.partial.extend_from_slice(bytes);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        split_lines(&complete)
    }

    /// The last line, when the output did not end with a newline.
    fn finish(&mut self) -> Vec<String> {
        split_lines(&std::mem::take(&mut self.partial))
    }
}

fn split_lines(bytes: &[u8]) -> Vec<String> {
    pty_session::sanitize_terminal_output(bytes)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if line.chars().count() > MAX_LINE_CHARS {
                let mut cut: String = line.chars().take(MAX_LINE_CHARS).collect();
                cut.push('…');
                cut
            } else {
                line.to_string()
            }
        })
        .collect()
}

/// Lines waiting to be delivered.
#[derive(Default)]
struct Batch {
    lines: Vec<String>,
    omitted: usize,
}

impl Batch {
    fn extend(&mut self, lines: Vec<String>) {
        for line in lines {
            if self.lines.len() < MAX_EVENT_LINES {
                self.lines.push(line);
            } else {
                self.omitted += 1;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.omitted == 0
    }

    fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

async fn run_monitor(
    handle: MonitorHandle,
    session_id: String,
    id: u64,
    start: MonitorStart,
    mut stop: oneshot::Receiver<()>,
) {
    let MonitorStart {
        description,
        timeout,
        process,
        output: MonitorOutput(mut output),
        ..
    } = start;
    let deadline = Instant::now() + timeout;
    let exited = process.exited();
    tokio::pin!(exited);
    let mut lines = LineSplitter::default();
    let mut batch = Batch::default();
    let mut flush_at: Option<Instant> = None;
    let mut delivered: VecDeque<Instant> = VecDeque::new();

    let end = loop {
        tokio::select! {
            // Stopped (or its session deleted): no notification.
            _ = &mut stop => {
                debug!("Monitor {id} of session {session_id} stopped");
                process.terminate();
                return;
            }
            _ = tokio::time::sleep_until(deadline) => break MonitorEnd::TimedOut(timeout),
            exit_code = &mut exited => break MonitorEnd::Exited(exit_code),
            _ = sleep_until_or_never(flush_at) => {
                flush_at = None;
                let now = Instant::now();
                while delivered.front().is_some_and(|at| now.duration_since(*at) > FLOOD_WINDOW) {
                    delivered.pop_front();
                }
                if delivered.len() >= MAX_EVENTS_PER_WINDOW {
                    break MonitorEnd::TooManyEvents;
                }
                delivered.push_back(now);
                let Batch { lines, omitted } = batch.take();
                handle
                    .sink
                    .deliver(&session_id, &monitor_event_message(id, &description, &lines, omitted))
                    .await;
            }
            chunk = output.recv() => match chunk {
                Some(bytes) => {
                    batch.extend(lines.push(&bytes));
                    if !batch.is_empty() && flush_at.is_none() {
                        flush_at = Some(Instant::now() + BATCH_WINDOW);
                    }
                }
                // All output read: the process is about to exit.
                None => break MonitorEnd::Exited((&mut exited).await),
            },
        }
    };

    if let MonitorEnd::Exited(_) = end {
        // Output still in flight when the exit was noticed.
        while let Ok(Some(bytes)) = tokio::time::timeout(EXIT_DRAIN_GRACE, output.recv()).await {
            batch.extend(lines.push(&bytes));
        }
    } else {
        process.terminate();
    }
    batch.extend(lines.finish());

    // Gone from the active list before the agent hears about it. A monitor
    // stopped meanwhile (its stop sender dropped) stays silent.
    if !handle.stop(&session_id, id) {
        return;
    }
    debug!("Monitor {id} of session {session_id} ended: {end:?}");
    let Batch { lines, omitted } = batch;
    handle
        .sink
        .deliver(
            &session_id,
            &monitor_ended_message(id, &description, &end, &lines, omitted),
        )
        .await;
}

async fn sleep_until_or_never(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pty_session::PtySpawnConfig;

    #[derive(Default)]
    struct RecordingSink {
        delivered: Mutex<Vec<(String, String)>>,
        active: Mutex<Vec<(String, Vec<u64>)>>,
    }

    #[async_trait::async_trait]
    impl MonitorSink for Arc<RecordingSink> {
        async fn deliver(&self, session_id: &str, message: &str) {
            self.delivered
                .lock()
                .unwrap()
                .push((session_id.to_string(), message.to_string()));
        }

        fn active_changed(&self, session_id: &str, active: Vec<ActiveMonitorInfo>) {
            self.active.lock().unwrap().push((
                session_id.to_string(),
                active.iter().map(|m| m.id).collect(),
            ));
        }
    }

    impl RecordingSink {
        fn delivered(&self) -> Vec<String> {
            self.delivered
                .lock()
                .unwrap()
                .iter()
                .map(|(_, message)| message.clone())
                .collect()
        }
    }

    fn recording() -> (MonitorHandle, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        (MonitorHandle::new(sink.clone(), None), sink)
    }

    fn start(command_line: &str, timeout: Duration) -> MonitorStart {
        let (sink, output) = monitor_output();
        let mut config = PtySpawnConfig::shell_command(command_line);
        config.tty = false;
        config.output_sink = Some(sink);
        MonitorStart {
            description: "test monitor".into(),
            command_line: command_line.into(),
            timeout,
            process: Arc::new(PtySession::spawn(config).unwrap()),
            output,
        }
    }

    async fn wait_for(what: &str, condition: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    #[test]
    fn notifications_parse() {
        let event = monitor_event_message(3, "PR #42\nchecks", &["passed".into()], 0);
        assert_eq!(event, "[monitor #3] PR #42 checks\npassed");
        assert_eq!(
            parse_notification_message(&event),
            Some(ParsedNotification {
                kind: NotificationKind::MonitorEvent { id: 3 },
                title: "PR #42 checks",
                body: "passed",
            })
        );

        let ended = monitor_ended_message(3, "PR checks", &MonitorEnd::Exited(Some(0)), &[], 2);
        let parsed = parse_notification_message(&ended).unwrap();
        assert_eq!(parsed.kind, NotificationKind::MonitorEnded { id: 3 });
        assert_eq!(parsed.title, "PR checks");
        assert!(parsed.body.starts_with("The command exited with code 0."));
        assert!(parsed.body.ends_with("(2 more lines omitted)"));

        let job = job_exited_message(12345, "cargo build\n--release", Some(1));
        let parsed = parse_notification_message(&job).unwrap();
        assert_eq!(parsed.kind, NotificationKind::JobExited { id: 12345 });
        assert_eq!(parsed.title, "cargo build…");
        assert!(parsed.body.contains("code 1"));
        assert!(parsed.body.contains("session_id 12345"));

        let joined = format!("{event}\n\n{job}");
        let parsed = parse_notification_messages(&joined).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].body, "passed");
        assert_eq!(parsed[1].kind, NotificationKind::JobExited { id: 12345 });
        assert_eq!(parse_notification_messages(&format!("hi\n{event}")), None);

        assert_eq!(parse_notification_message("[monitor #x] nope"), None);
        assert_eq!(parse_notification_message("hello"), None);
    }

    #[test]
    fn line_splitter_keeps_partial_lines_and_drops_blank_ones() {
        let mut splitter = LineSplitter::default();
        assert_eq!(splitter.push(b"one\ntw"), vec!["one"]);
        assert_eq!(
            splitter.push(b"o\r\n\n  \n\x1b[32mthree\x1b[0m\n"),
            vec!["two", "three"]
        );
        assert!(splitter.push(b"four").is_empty());
        assert_eq!(splitter.finish(), vec!["four"]);
    }

    #[test]
    fn batch_caps_lines() {
        let mut batch = Batch::default();
        batch.extend((0..MAX_EVENT_LINES + 5).map(|i| i.to_string()).collect());
        assert_eq!(batch.lines.len(), MAX_EVENT_LINES);
        assert_eq!(batch.omitted, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lines_arrive_batched_and_the_end_is_reported() {
        let (handle, sink) = recording();
        let id = handle.start(
            "s1".into(),
            0,
            start(
                "echo first; echo second; sleep 1; echo third; exit 4",
                Duration::from_secs(30),
            ),
        );
        assert_eq!(id, 1);
        assert_eq!(handle.active("s1").len(), 1);

        wait_for("the end", || sink.delivered().len() >= 2).await;
        let delivered = sink.delivered();
        assert_eq!(delivered[0], "[monitor #1] test monitor\nfirst\nsecond");
        assert!(
            delivered[1]
                .starts_with("[monitor #1 ended] test monitor\nThe command exited with code 4."),
            "{}",
            delivered[1]
        );
        assert!(delivered[1].ends_with("\nthird"), "{}", delivered[1]);
        assert!(handle.active("s1").is_empty());
        let active = sink.active.lock().unwrap().clone();
        assert_eq!(active, vec![("s1".into(), vec![1]), ("s1".into(), vec![])]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_monitor_stays_silent_and_kills_its_command() {
        let (handle, sink) = recording();
        let monitor = start("sleep 30", Duration::from_secs(60));
        let process = monitor.process.clone();
        let id = handle.start("s1".into(), 0, monitor);

        assert!(!handle.stop("other-session", id));
        assert!(handle.stop("s1", id));
        tokio::time::timeout(Duration::from_secs(10), process.exited())
            .await
            .expect("the command must be killed");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(sink.delivered().is_empty());
        assert!(!handle.stop("s1", id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_timeout_stops_the_monitor_with_a_notification() {
        let (handle, sink) = recording();
        handle.start(
            "s1".into(),
            0,
            start("sleep 30", Duration::from_millis(300)),
        );
        wait_for("the timeout", || !sink.delivered().is_empty()).await;
        assert!(sink.delivered()[0].contains("reached its timeout"));
        assert!(handle.active("s1").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ids_continue_the_history_per_session() {
        let (handle, _sink) = recording();
        assert_eq!(
            handle.start("s1".into(), 7, start("true", Duration::from_secs(5))),
            8
        );
        assert_eq!(
            handle.start("s1".into(), 7, start("true", Duration::from_secs(5))),
            9
        );
        assert_eq!(
            handle.start("s2".into(), 0, start("true", Duration::from_secs(5))),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_session_stops_all_of_its_monitors() {
        let (handle, sink) = recording();
        handle.start("s1".into(), 0, start("sleep 30", Duration::from_secs(60)));
        handle.start("s1".into(), 0, start("sleep 30", Duration::from_secs(60)));
        handle.start("s2".into(), 0, start("sleep 30", Duration::from_secs(60)));
        handle.stop_session("s1");
        assert!(handle.active("s1").is_empty());
        assert_eq!(handle.active("s2").len(), 1);
        handle.stop_session("s2");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(sink.delivered().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_background_job_exit_is_reported_unless_seen() {
        let (handle, sink) = recording();
        let monitors = SessionMonitors {
            handle,
            session_id: "s1".into(),
            used_ids: 0,
        };
        let jobs = Arc::new(PtySessionManager::default());
        let spawn = |command_line: &str| {
            let mut config = PtySpawnConfig::shell_command(command_line);
            config.tty = false;
            Arc::new(PtySession::spawn(config).unwrap())
        };

        // Nobody looks: reported.
        let reported = spawn("sleep 0.2; exit 2");
        let reported_id = jobs.register(reported.clone(), "build");
        monitors.watch_job_exit(&jobs, reported_id, &reported, "build");

        // The agent sees the exit itself and stops tracking the job.
        let seen = spawn("sleep 0.2");
        let seen_id = jobs.register(seen.clone(), "seen");
        monitors.watch_job_exit(&jobs, seen_id, &seen, "seen");
        seen.collect_output(Duration::from_secs(10)).await;
        jobs.remove(seen_id);

        wait_for("the report", || !sink.delivered().is_empty()).await;
        tokio::time::sleep(JOB_EXIT_GRACE + Duration::from_millis(300)).await;
        assert_eq!(
            sink.delivered(),
            vec![job_exited_message(reported_id, "build", Some(2))]
        );
    }
}
