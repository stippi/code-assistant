//! Session-scoped wakeups: the agent arms a timed continuation of its own
//! session ("check the build in 20 minutes"); when the deadline passes, a
//! framed message is injected and a turn is started — the session stays
//! idle-but-alive in between, exactly like between user messages.
//!
//! Wakeups are deliberately not persisted: they die with the process. Durable
//! cross-session scheduling is an application concern (see
//! `docs/session-wakeups.md`). What is persisted is the `schedule_wakeup`
//! result: which process armed the wakeup and, once it fired, when — so a
//! frontend can tell an armed wakeup from one lost with a restart.
//!
//! Wakeup ids are numbered per session (1, 2, 3, … across restarts, seeded
//! from the session's history), so the model sees short ids; internally a
//! wakeup is identified by session and id.

use crate::session::SessionService;
use crate::session::sleep_inhibitor::SleepInhibitor;
use agent_core::SerializedToolExecution;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, warn};

/// Start of the frame of an injected wakeup message, so the agent and the
/// transcript can tell it from a human message: `[scheduled wakeup #3] …`.
const WAKEUP_FRAME: &str = "[scheduled wakeup";

/// The message injected when wakeup `id` fires.
pub fn wakeup_message(id: u64, prompt: &str) -> String {
    format!("{WAKEUP_FRAME} #{id}] {prompt}")
}

/// An injected wakeup message, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWakeupMessage<'a> {
    /// `None` for messages recorded before they carried the id.
    pub id: Option<u64>,
    pub prompt: &'a str,
}

/// Parse an injected wakeup message; `None` for any other message.
pub fn parse_wakeup_message(message: &str) -> Option<ParsedWakeupMessage<'_>> {
    let rest = message.strip_prefix(WAKEUP_FRAME)?;
    let (id, prompt) = match rest.strip_prefix(" #") {
        Some(numbered) => {
            let (id, prompt) = numbered.split_once(']')?;
            (Some(id.parse().ok()?), prompt)
        }
        None => (None, rest.strip_prefix(']')?),
    };
    Some(ParsedWakeupMessage {
        id,
        prompt: prompt.trim(),
    })
}

/// Identifies this process among the ones that ran before: a wakeup whose
/// result names another instance was lost with that process.
pub fn process_instance() -> &'static str {
    static INSTANCE: LazyLock<String> = LazyLock::new(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:x}-{started:x}", std::process::id())
    });
    &INSTANCE
}

/// The highest wakeup id `executions` (a session's history) already use.
pub fn highest_wakeup_id(executions: &[SerializedToolExecution]) -> u64 {
    executions
        .iter()
        .filter(|exec| exec.tool_name == "schedule_wakeup")
        .filter_map(|exec| exec.result_json.get("wakeup_id")?.as_u64())
        .max()
        .unwrap_or(0)
}

/// Delivers a fired wakeup into a session. The production implementation is
/// [`SessionService`]; tests substitute a recorder.
#[async_trait::async_trait]
pub trait WakeupSink: Send + Sync + 'static {
    async fn fire(&self, session_id: &str, wakeup_id: u64, message: &str);
}

#[async_trait::async_trait]
impl WakeupSink for SessionService {
    async fn fire(&self, session_id: &str, wakeup_id: u64, message: &str) {
        if let Err(e) = self
            .inject_wakeup(session_id.to_string(), wakeup_id, message.to_string())
            .await
        {
            warn!("Failed to deliver wakeup to session {session_id}: {e}");
        }
    }
}

struct ArmedWakeup {
    session_id: String,
    id: u64,
    fire_at: Instant,
    prompt: String,
}

enum Command {
    Arm(ArmedWakeup),
    Cancel { session_id: String, id: u64 },
    CancelSession { session_id: String },
}

/// Cloneable handle to the wakeup scheduler task.
#[derive(Clone)]
pub struct WakeupHandle {
    tx: mpsc::UnboundedSender<Command>,
    /// The last id issued per session.
    issued: Arc<Mutex<HashMap<String, u64>>>,
}

impl WakeupHandle {
    /// Arm a wakeup for `session_id`, firing after `delay`. Returns its id
    /// (for cancellation): the next one of the session, above `used`, the
    /// highest id its history already uses.
    pub fn arm(&self, session_id: String, used: u64, delay: Duration, prompt: String) -> u64 {
        let id = {
            let mut issued = self.issued.lock().unwrap();
            let last = issued.entry(session_id.clone()).or_default();
            *last = (*last).max(used) + 1;
            *last
        };
        let _ = self.tx.send(Command::Arm(ArmedWakeup {
            session_id,
            id,
            fire_at: Instant::now() + delay,
            prompt,
        }));
        id
    }

    /// Cancel a single wakeup of a session — a session cannot cancel another
    /// session's wakeups.
    pub fn cancel(&self, session_id: String, id: u64) {
        let _ = self.tx.send(Command::Cancel { session_id, id });
    }

    /// Cancel all wakeups of a session (used when the session is deleted).
    pub fn cancel_session(&self, session_id: String) {
        let _ = self.tx.send(Command::CancelSession { session_id });
    }
}

/// A [`WakeupHandle`] bound to one session, handed to tools through
/// `ToolServices` — the session id comes from the wiring, not the model.
#[derive(Clone)]
pub struct SessionWakeups {
    pub handle: WakeupHandle,
    pub session_id: String,
    /// The highest wakeup id the session's history uses
    /// ([`highest_wakeup_id`]), so ids continue across restarts.
    pub used_ids: u64,
}

impl SessionWakeups {
    pub fn arm(&self, delay: Duration, prompt: String) -> u64 {
        self.handle
            .arm(self.session_id.clone(), self.used_ids, delay, prompt)
    }

    pub fn cancel(&self, id: u64) {
        self.handle.cancel(self.session_id.clone(), id);
    }
}

/// Spawn the scheduler task. One per process; the handle is cheap to clone.
///
/// An armed wakeup holds the sleep inhibitor (when provided): a machine that
/// sleeps through the deadline would silently swallow the wakeup.
pub fn spawn_wakeup_scheduler(
    sink: impl WakeupSink,
    sleep_inhibitor: Option<Arc<SleepInhibitor>>,
) -> WakeupHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run_scheduler(rx, sink, sleep_inhibitor));
    WakeupHandle {
        tx,
        issued: Arc::default(),
    }
}

async fn run_scheduler(
    mut rx: mpsc::UnboundedReceiver<Command>,
    sink: impl WakeupSink,
    sleep_inhibitor: Option<Arc<SleepInhibitor>>,
) {
    // Keyed by arm order, which breaks fire-time ties deterministically.
    let mut armed: BTreeMap<u64, ArmedWakeup> = BTreeMap::new();
    let next_key = AtomicU64::new(0);
    let inhibitor = InhibitorGuard::new(sleep_inhibitor);

    loop {
        let next_deadline = armed.values().map(|w| w.fire_at).min();

        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else {
                    // All handles dropped; nothing can arm or fire anymore.
                    break;
                };
                match command {
                    Command::Arm(wakeup) => {
                        debug!(
                            "Wakeup {} armed for session {} in {:?}",
                            wakeup.id,
                            wakeup.session_id,
                            wakeup.fire_at.saturating_duration_since(Instant::now())
                        );
                        armed.insert(next_key.fetch_add(1, Ordering::Relaxed), wakeup);
                    }
                    Command::Cancel { session_id, id } => {
                        armed.retain(|_, w| !(w.session_id == session_id && w.id == id));
                        debug!("Wakeup {id} of session {session_id} cancelled");
                    }
                    Command::CancelSession { session_id } => {
                        armed.retain(|_, w| w.session_id != session_id);
                    }
                }
            }
            _ = sleep_until_or_never(next_deadline) => {
                let now = Instant::now();
                let due: Vec<u64> = armed
                    .iter()
                    .filter(|(_, w)| w.fire_at <= now)
                    .map(|(key, _)| *key)
                    .collect();
                for key in due {
                    let wakeup = armed.remove(&key).expect("due key was just collected");
                    debug!("Wakeup {} fires for session {}", wakeup.id, wakeup.session_id);
                    sink.fire(
                        &wakeup.session_id,
                        wakeup.id,
                        &wakeup_message(wakeup.id, &wakeup.prompt),
                    )
                    .await;
                }
            }
        }

        inhibitor.set_armed(!armed.is_empty());
    }
}

async fn sleep_until_or_never(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Holds the sleep inhibitor exactly while at least one wakeup is armed.
struct InhibitorGuard {
    inhibitor: Option<Arc<SleepInhibitor>>,
    held: std::cell::Cell<bool>,
}

impl InhibitorGuard {
    fn new(inhibitor: Option<Arc<SleepInhibitor>>) -> Self {
        Self {
            inhibitor,
            held: std::cell::Cell::new(false),
        }
    }

    fn set_armed(&self, armed: bool) {
        let Some(inhibitor) = &self.inhibitor else {
            return;
        };
        if armed && !self.held.get() {
            inhibitor.wakeup_armed();
            self.held.set(true);
        } else if !armed && self.held.get() {
            inhibitor.wakeup_disarmed();
            self.held.set(false);
        }
    }
}

impl Drop for InhibitorGuard {
    fn drop(&mut self) {
        self.set_armed(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        fired: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl WakeupSink for Arc<RecordingSink> {
        async fn fire(&self, session_id: &str, _wakeup_id: u64, message: &str) {
            self.fired
                .lock()
                .unwrap()
                .push((session_id.to_string(), message.to_string()));
        }
    }

    #[test]
    fn wakeup_messages_parse_with_and_without_id() {
        assert_eq!(
            parse_wakeup_message(&wakeup_message(3, "check the build")),
            Some(ParsedWakeupMessage {
                id: Some(3),
                prompt: "check the build"
            })
        );
        // Recorded before messages carried the id.
        assert_eq!(
            parse_wakeup_message("[scheduled wakeup] check the build"),
            Some(ParsedWakeupMessage {
                id: None,
                prompt: "check the build"
            })
        );
        assert_eq!(parse_wakeup_message("check the build"), None);
    }

    #[test]
    fn ids_count_per_session_and_continue_the_history() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle = WakeupHandle {
            tx,
            issued: Arc::default(),
        };
        let arm = |session: &str, used| {
            handle.arm(session.into(), used, Duration::from_secs(1), "x".into())
        };
        assert_eq!(arm("s1", 0), 1);
        assert_eq!(arm("s1", 0), 2);
        assert_eq!(arm("s2", 0), 1);
        // A session whose history (from before a restart) used ids up to 7.
        assert_eq!(arm("s3", 7), 8);
        assert_eq!(arm("s3", 7), 9);
    }

    fn recording_scheduler() -> (WakeupHandle, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let handle = spawn_wakeup_scheduler(sink.clone(), None);
        (handle, sink)
    }

    /// Let the scheduler task process everything currently queued.
    async fn settle() {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn fires_after_delay_with_framed_message() {
        let (handle, sink) = recording_scheduler();
        handle.arm(
            "s1".into(),
            0,
            Duration::from_secs(300),
            "check build".into(),
        );
        settle().await;

        tokio::time::advance(Duration::from_secs(299)).await;
        settle().await;
        assert!(sink.fired.lock().unwrap().is_empty());

        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let fired = sink.fired.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![("s1".to_string(), wakeup_message(1, "check build"))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_wakeup_does_not_fire() {
        let (handle, sink) = recording_scheduler();
        let id = handle.arm("s1".into(), 0, Duration::from_secs(60), "x".into());
        settle().await;
        handle.cancel("s1".into(), id);
        settle().await;

        tokio::time::advance(Duration::from_secs(120)).await;
        settle().await;
        assert!(sink.fired.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_hits_only_that_sessions_wakeup_of_the_id() {
        let (handle, sink) = recording_scheduler();
        let id = handle.arm("s1".into(), 0, Duration::from_secs(60), "a".into());
        let same_id = handle.arm("s2".into(), 0, Duration::from_secs(60), "b".into());
        assert_eq!(id, same_id);
        settle().await;
        handle.cancel("s1".into(), id);
        settle().await;

        tokio::time::advance(Duration::from_secs(61)).await;
        settle().await;
        let fired = sink.fired.lock().unwrap().clone();
        assert_eq!(fired, vec![("s2".to_string(), wakeup_message(1, "b"))]);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_requires_matching_session() {
        let (handle, sink) = recording_scheduler();
        let id = handle.arm("s1".into(), 0, Duration::from_secs(60), "x".into());
        settle().await;
        handle.cancel("someone-else".into(), id);
        settle().await;

        tokio::time::advance(Duration::from_secs(61)).await;
        settle().await;
        assert_eq!(sink.fired.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_session_removes_all_of_that_session() {
        let (handle, sink) = recording_scheduler();
        handle.arm("s1".into(), 0, Duration::from_secs(10), "a".into());
        handle.arm("s1".into(), 0, Duration::from_secs(20), "b".into());
        handle.arm("s2".into(), 0, Duration::from_secs(30), "c".into());
        settle().await;
        handle.cancel_session("s1".into());
        settle().await;

        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        let fired = sink.fired.lock().unwrap().clone();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].0, "s2");
    }

    #[tokio::test(start_paused = true)]
    async fn multiple_wakeups_fire_in_deadline_order() {
        let (handle, sink) = recording_scheduler();
        handle.arm("s1".into(), 0, Duration::from_secs(20), "second".into());
        handle.arm("s1".into(), 0, Duration::from_secs(10), "first".into());
        settle().await;

        tokio::time::advance(Duration::from_secs(15)).await;
        settle().await;
        assert_eq!(sink.fired.lock().unwrap().len(), 1);
        assert!(sink.fired.lock().unwrap()[0].1.contains("first"));

        tokio::time::advance(Duration::from_secs(10)).await;
        settle().await;
        assert_eq!(sink.fired.lock().unwrap().len(), 2);
        assert!(sink.fired.lock().unwrap()[1].1.contains("second"));
    }

    #[tokio::test(start_paused = true)]
    async fn rearming_while_sleeping_shortens_the_deadline() {
        let (handle, sink) = recording_scheduler();
        handle.arm("s1".into(), 0, Duration::from_secs(3600), "slow".into());
        settle().await;
        handle.arm("s1".into(), 0, Duration::from_secs(5), "fast".into());
        settle().await;

        tokio::time::advance(Duration::from_secs(6)).await;
        settle().await;
        let fired = sink.fired.lock().unwrap().clone();
        assert_eq!(fired.len(), 1);
        assert!(fired[0].1.contains("fast"));
    }
}
