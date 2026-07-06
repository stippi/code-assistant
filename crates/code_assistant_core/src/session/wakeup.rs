//! Session-scoped wakeups: the agent arms a timed continuation of its own
//! session ("check the build in 20 minutes"); when the deadline passes, a
//! framed message is injected and a turn is started — the session stays
//! idle-but-alive in between, exactly like between user messages.
//!
//! Wakeups are deliberately not persisted: they die with the process. Durable
//! cross-session scheduling is an application concern (see
//! `docs/session-wakeups.md`).

use crate::session::sleep_inhibitor::SleepInhibitor;
use crate::session::SessionService;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, warn};

/// Prefix framing an injected wakeup message, so the agent and the transcript
/// can tell it from a human message.
pub const WAKEUP_PREFIX: &str = "[scheduled wakeup]";

/// Delivers a fired wakeup into a session. The production implementation is
/// [`SessionService`]; tests substitute a recorder.
#[async_trait::async_trait]
pub trait WakeupSink: Send + Sync + 'static {
    async fn fire(&self, session_id: &str, message: &str);
}

#[async_trait::async_trait]
impl WakeupSink for SessionService {
    async fn fire(&self, session_id: &str, message: &str) {
        if let Err(e) = self
            .inject_wakeup(session_id.to_string(), message.to_string())
            .await
        {
            warn!("Failed to deliver wakeup to session {session_id}: {e}");
        }
    }
}

struct ArmedWakeup {
    session_id: String,
    fire_at: Instant,
    prompt: String,
}

enum Command {
    Arm { id: u64, wakeup: ArmedWakeup },
    Cancel { session_id: String, id: u64 },
    CancelSession { session_id: String },
}

/// Cloneable handle to the wakeup scheduler task.
#[derive(Clone)]
pub struct WakeupHandle {
    tx: mpsc::UnboundedSender<Command>,
    next_id: Arc<AtomicU64>,
}

impl WakeupHandle {
    /// Arm a wakeup for `session_id`, firing after `delay`. Returns the
    /// wakeup id (for cancellation).
    pub fn arm(&self, session_id: String, delay: Duration, prompt: String) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Arm {
            id,
            wakeup: ArmedWakeup {
                session_id,
                fire_at: Instant::now() + delay,
                prompt,
            },
        });
        id
    }

    /// Cancel a single wakeup. The session id must match the one the wakeup
    /// was armed for — a session cannot cancel another session's wakeups.
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
}

impl SessionWakeups {
    pub fn arm(&self, delay: Duration, prompt: String) -> u64 {
        self.handle.arm(self.session_id.clone(), delay, prompt)
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
        next_id: Arc::new(AtomicU64::new(1)),
    }
}

async fn run_scheduler(
    mut rx: mpsc::UnboundedReceiver<Command>,
    sink: impl WakeupSink,
    sleep_inhibitor: Option<Arc<SleepInhibitor>>,
) {
    // BTreeMap keyed by id: iteration order is arm order, which breaks
    // fire-time ties deterministically.
    let mut armed: BTreeMap<u64, ArmedWakeup> = BTreeMap::new();
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
                    Command::Arm { id, wakeup } => {
                        debug!(
                            "Wakeup {id} armed for session {} in {:?}",
                            wakeup.session_id,
                            wakeup.fire_at.saturating_duration_since(Instant::now())
                        );
                        armed.insert(id, wakeup);
                    }
                    Command::Cancel { session_id, id } => {
                        if armed.get(&id).is_some_and(|w| w.session_id == session_id) {
                            armed.remove(&id);
                            debug!("Wakeup {id} cancelled");
                        }
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
                    .map(|(id, _)| *id)
                    .collect();
                for id in due {
                    let wakeup = armed.remove(&id).expect("due id was just collected");
                    debug!("Wakeup {id} fires for session {}", wakeup.session_id);
                    sink.fire(
                        &wakeup.session_id,
                        &format!("{WAKEUP_PREFIX} {}", wakeup.prompt),
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
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSink {
        fired: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl WakeupSink for Arc<RecordingSink> {
        async fn fire(&self, session_id: &str, message: &str) {
            self.fired
                .lock()
                .unwrap()
                .push((session_id.to_string(), message.to_string()));
        }
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
        handle.arm("s1".into(), Duration::from_secs(300), "check build".into());
        settle().await;

        tokio::time::advance(Duration::from_secs(299)).await;
        settle().await;
        assert!(sink.fired.lock().unwrap().is_empty());

        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let fired = sink.fired.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![("s1".to_string(), format!("{WAKEUP_PREFIX} check build"))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_wakeup_does_not_fire() {
        let (handle, sink) = recording_scheduler();
        let id = handle.arm("s1".into(), Duration::from_secs(60), "x".into());
        settle().await;
        handle.cancel("s1".into(), id);
        settle().await;

        tokio::time::advance(Duration::from_secs(120)).await;
        settle().await;
        assert!(sink.fired.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_requires_matching_session() {
        let (handle, sink) = recording_scheduler();
        let id = handle.arm("s1".into(), Duration::from_secs(60), "x".into());
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
        handle.arm("s1".into(), Duration::from_secs(10), "a".into());
        handle.arm("s1".into(), Duration::from_secs(20), "b".into());
        handle.arm("s2".into(), Duration::from_secs(30), "c".into());
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
        handle.arm("s1".into(), Duration::from_secs(20), "second".into());
        handle.arm("s1".into(), Duration::from_secs(10), "first".into());
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
        handle.arm("s1".into(), Duration::from_secs(3600), "slow".into());
        settle().await;
        handle.arm("s1".into(), Duration::from_secs(5), "fast".into());
        settle().await;

        tokio::time::advance(Duration::from_secs(6)).await;
        settle().await;
        let fired = sink.fired.lock().unwrap().clone();
        assert_eq!(fired.len(), 1);
        assert!(fired[0].1.contains("fast"));
    }
}
