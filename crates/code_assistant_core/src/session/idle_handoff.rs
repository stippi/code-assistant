//! Prepared hand-offs (see `docs/hand-off.md`): when a long session goes
//! idle, a timer gives the user two minutes; if they stay away, the core has
//! the agent write a hand-off prompt while the prompt cache is still warm and
//! offers it in the composer as `/new <prompt>`.
//!
//! A timer per session, armed when a run ends and pushed back by user
//! activity. Whether the session qualifies (still idle, long enough, not
//! prepared yet) is decided when the timer fires.

use crate::session::SessionService;
use crate::utils::file_utils::atomic_write_json;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::warn;

/// How long a session must stay without user activity.
pub const IDLE_DELAY: Duration = Duration::from_secs(120);

/// Configuration persisted at `<config_dir>/hand-off.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoffConfig {
    /// Prepare a hand-off once the last request's input (input, cache write
    /// and cache read tokens) reaches this many tokens; 0 disables it.
    pub idle_threshold_tokens: u64,
}

impl Default for HandoffConfig {
    fn default() -> Self {
        Self {
            idle_threshold_tokens: 150_000,
        }
    }
}

impl HandoffConfig {
    /// Path of the config file in the resolved config directory.
    pub fn path() -> PathBuf {
        crate::config_dir::config_dir().join("hand-off.json")
    }

    /// Load the config; a missing or malformed file yields the defaults.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Failed to parse {}: {e}; using defaults", path.display());
            Self::default()
        })
    }

    /// Persist the config to the resolved config directory.
    pub fn save(&self) -> Result<()> {
        atomic_write_json(&Self::path(), self)
    }
}

/// Prepares the hand-off of a session whose timer fired. The production
/// implementation is [`SessionService`]; tests substitute a recorder.
#[async_trait::async_trait]
pub trait IdleHandoffSink: Send + Sync + 'static {
    async fn prepare(&self, session_id: &str, threshold_tokens: u64);
}

#[async_trait::async_trait]
impl IdleHandoffSink for SessionService {
    async fn prepare(&self, session_id: &str, threshold_tokens: u64) {
        if let Err(e) = self
            .prepare_handoff(session_id.to_string(), threshold_tokens)
            .await
        {
            warn!("Failed to prepare a hand-off for session {session_id}: {e:#}");
        }
    }
}

/// The threshold in effect when a timer fires; read then, so a settings
/// change applies without a restart.
pub type ThresholdSource = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Cloneable handle to the per-session timers.
#[derive(Clone)]
pub struct IdleHandoffTimers {
    inner: Arc<Inner>,
}

struct Inner {
    sink: Arc<dyn IdleHandoffSink>,
    delay: Duration,
    threshold: ThresholdSource,
    timers: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

/// Timers firing into `sink` after [`IDLE_DELAY`], with the threshold from
/// `hand-off.json`.
pub fn spawn_idle_handoff(sink: impl IdleHandoffSink) -> IdleHandoffTimers {
    IdleHandoffTimers::new(
        sink,
        IDLE_DELAY,
        Arc::new(|| HandoffConfig::load().idle_threshold_tokens),
    )
}

impl IdleHandoffTimers {
    pub fn new(sink: impl IdleHandoffSink, delay: Duration, threshold: ThresholdSource) -> Self {
        Self {
            inner: Arc::new(Inner {
                sink: Arc::new(sink),
                delay,
                threshold,
                timers: Mutex::default(),
            }),
        }
    }

    /// Start (or restart) the session's timer. Must be called on a tokio
    /// runtime.
    pub fn arm(&self, session_id: &str) {
        let inner = self.inner.clone();
        let session = session_id.to_string();
        let task = tokio::spawn(async move {
            tokio::time::sleep(inner.delay).await;
            {
                // Unless a newer timer replaced this one meanwhile.
                let mut timers = inner.timers.lock().unwrap();
                if timers
                    .get(&session)
                    .is_some_and(|timer| timer.id() == tokio::task::id())
                {
                    timers.remove(&session);
                }
            }
            let threshold = (inner.threshold)();
            if threshold > 0 {
                inner.sink.prepare(&session, threshold).await;
            }
        });
        if let Some(previous) = self
            .inner
            .timers
            .lock()
            .unwrap()
            .insert(session_id.to_string(), task.abort_handle())
        {
            previous.abort();
        }
    }

    /// The user did something in the session: restart its timer, if armed.
    pub fn touch(&self, session_id: &str) {
        if self.inner.timers.lock().unwrap().contains_key(session_id) {
            self.arm(session_id);
        }
    }

    /// Stop the session's timer, if armed.
    pub fn disarm(&self, session_id: &str) {
        if let Some(timer) = self.inner.timers.lock().unwrap().remove(session_id) {
            timer.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<(String, u64)>>>);

    #[async_trait::async_trait]
    impl IdleHandoffSink for Recorder {
        async fn prepare(&self, session_id: &str, threshold_tokens: u64) {
            self.0
                .lock()
                .unwrap()
                .push((session_id.to_string(), threshold_tokens));
        }
    }

    impl Recorder {
        fn fired(&self) -> Vec<(String, u64)> {
            self.0.lock().unwrap().clone()
        }
    }

    fn timers(recorder: &Recorder, threshold: u64) -> IdleHandoffTimers {
        IdleHandoffTimers::new(
            recorder.clone(),
            Duration::from_secs(120),
            Arc::new(move || threshold),
        )
    }

    async fn advance(seconds: u64) {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        // Let the fired timers run.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fires_after_the_delay_with_the_threshold() {
        let recorder = Recorder::default();
        let timers = timers(&recorder, 150_000);
        timers.arm("s1");

        advance(119).await;
        assert!(recorder.fired().is_empty());
        advance(2).await;
        assert_eq!(recorder.fired(), [("s1".to_string(), 150_000)]);
    }

    #[tokio::test(start_paused = true)]
    async fn activity_pushes_the_deadline_back() {
        let recorder = Recorder::default();
        let timers = timers(&recorder, 1);
        timers.arm("s1");

        advance(100).await;
        timers.touch("s1");
        advance(100).await;
        assert!(recorder.fired().is_empty());
        advance(21).await;
        assert_eq!(recorder.fired().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn activity_does_not_arm_and_disarm_stops() {
        let recorder = Recorder::default();
        let timers = timers(&recorder, 1);
        timers.touch("s1");
        timers.arm("s2");
        timers.disarm("s2");

        advance(300).await;
        assert!(recorder.fired().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_threshold_disables_preparing() {
        let recorder = Recorder::default();
        let timers = timers(&recorder, 0);
        timers.arm("s1");

        advance(300).await;
        assert!(recorder.fired().is_empty());
    }

    #[test]
    fn config_defaults_and_reads_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hand-off.json");
        assert_eq!(
            HandoffConfig::load_from(&path).idle_threshold_tokens,
            150_000
        );
        std::fs::write(&path, r#"{"idle_threshold_tokens": 80000}"#).unwrap();
        assert_eq!(
            HandoffConfig::load_from(&path).idle_threshold_tokens,
            80_000
        );
    }
}
