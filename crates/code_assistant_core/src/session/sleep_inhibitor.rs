use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tracing::{debug, warn};

/// Prevents the system from going to idle sleep while any agent is running.
///
/// This uses a reference-counted approach: the first agent to start acquires
/// the system wake lock, and it is released only when the last running agent
/// finishes. The struct is cheaply cloneable (all state behind `Arc` in the
/// caller) and safe to share between the `SessionManager` and spawned agent
/// tasks.
#[derive(Default)]
pub struct SleepInhibitor {
    /// Number of currently running agents.
    running_count: AtomicUsize,
    /// The active wake lock, held as long as `running_count > 0`.
    wake_lock: Mutex<Option<keepawake::KeepAwake>>,
}

impl SleepInhibitor {
    /// Called when an agent starts running. Acquires the system wake lock
    /// if this is the first active agent.
    pub fn agent_started(&self) {
        let prev = self.running_count.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            self.acquire_lock();
        }
        debug!(
            "Agent started (running agents: {})",
            self.running_count.load(Ordering::SeqCst)
        );
    }

    /// Called when an agent stops running (success, error, or abort).
    /// Releases the system wake lock when the last agent finishes.
    pub fn agent_stopped(&self) {
        let prev = self.running_count.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(
            prev > 0,
            "agent_stopped called more times than agent_started"
        );
        if prev == 1 {
            self.release_lock();
        }
        debug!(
            "Agent stopped (running agents: {})",
            self.running_count.load(Ordering::SeqCst)
        );
    }

    /// Called while at least one session wakeup is armed — a machine that
    /// sleeps through the deadline would silently swallow the wakeup. Shares
    /// the refcount with running agents.
    pub fn wakeup_armed(&self) {
        let prev = self.running_count.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            self.acquire_lock();
        }
        debug!("Wakeup armed, holding wake lock");
    }

    /// Counterpart of [`Self::wakeup_armed`]: the last armed wakeup fired or
    /// was cancelled.
    pub fn wakeup_disarmed(&self) {
        let prev = self.running_count.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "wakeup_disarmed without wakeup_armed");
        if prev == 1 {
            self.release_lock();
        }
        debug!("Wakeups disarmed, wake lock released");
    }

    fn acquire_lock(&self) {
        let result = keepawake::Builder::default()
            .idle(true)
            .reason("AI agent is running")
            .app_name("code-assistant")
            .app_reverse_domain("com.code-assistant")
            .create();

        match result {
            Ok(lock) => {
                debug!("System sleep inhibited while agent is running");
                if let Ok(mut guard) = self.wake_lock.lock() {
                    *guard = Some(lock);
                }
            }
            Err(e) => {
                warn!("Failed to inhibit system sleep: {}", e);
            }
        }
    }

    fn release_lock(&self) {
        if let Ok(mut guard) = self.wake_lock.lock() {
            if guard.take().is_some() {
                debug!("System sleep inhibition released (no agents running)");
            }
        }
    }
}
