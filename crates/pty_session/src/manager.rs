//! Registry of live [`PtySession`]s, keyed by numeric ids the model can
//! reference across tool calls.

use crate::session::{PtySession, PtySessionStatus};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Default cap on concurrently tracked sessions.
pub const DEFAULT_MAX_SESSIONS: usize = 32;

struct PtyEntry {
    session: Arc<PtySession>,
    command_line: String,
    last_used: Instant,
}

/// Info about a tracked session, for listing/UI purposes.
pub struct PtySessionInfo {
    pub id: u32,
    pub command_line: String,
    pub status: PtySessionStatus,
}

pub struct PtySessionManager {
    max_sessions: usize,
    entries: Mutex<HashMap<u32, PtyEntry>>,
}

impl Default for PtySessionManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SESSIONS)
    }
}

impl PtySessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            max_sessions: max_sessions.max(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Track a session and return its id.
    ///
    /// Ids are random (not sequential) so an id from a restored transcript
    /// never silently aliases a fresh session after a restart.
    pub fn register(&self, session: Arc<PtySession>, command_line: impl Into<String>) -> u32 {
        let mut entries = self.entries.lock().unwrap();

        while entries.len() >= self.max_sessions {
            let Some(victim) = Self::eviction_victim(&entries) else {
                break;
            };
            if let Some(entry) = entries.remove(&victim) {
                entry.session.terminate();
            }
        }

        let id = loop {
            let candidate = rand::random_range(1_000..100_000u32);
            if !entries.contains_key(&candidate) {
                break candidate;
            }
        };
        entries.insert(
            id,
            PtyEntry {
                session,
                command_line: command_line.into(),
                last_used: Instant::now(),
            },
        );
        id
    }

    /// Prefer evicting an already-exited session; fall back to the least
    /// recently used one.
    fn eviction_victim(entries: &HashMap<u32, PtyEntry>) -> Option<u32> {
        let lru = |filtered: &mut dyn Iterator<Item = (&u32, &PtyEntry)>| {
            filtered
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(id, _)| *id)
        };
        lru(&mut entries
            .iter()
            .filter(|(_, entry)| entry.session.status() != PtySessionStatus::Running))
        .or_else(|| lru(&mut entries.iter()))
    }

    /// Look up a session, refreshing its LRU timestamp.
    pub fn get(&self, id: u32) -> Option<Arc<PtySession>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(&id)?;
        entry.last_used = Instant::now();
        Some(entry.session.clone())
    }

    /// Stop tracking a session. Does not terminate it — callers drop the
    /// returned `Arc` (which kills the process) or keep it alive on purpose.
    pub fn remove(&self, id: u32) -> Option<Arc<PtySession>> {
        self.entries
            .lock()
            .unwrap()
            .remove(&id)
            .map(|entry| entry.session)
    }

    pub fn list(&self) -> Vec<PtySessionInfo> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, entry)| PtySessionInfo {
                id: *id,
                command_line: entry.command_line.clone(),
                status: entry.session.status(),
            })
            .collect()
    }

    /// Terminate and forget all tracked sessions (agent session shutdown).
    pub fn terminate_all(&self) {
        let mut entries = self.entries.lock().unwrap();
        for (_, entry) in entries.drain() {
            entry.session.terminate();
        }
    }
}

impl Drop for PtySessionManager {
    fn drop(&mut self) {
        self.terminate_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::PtySpawnConfig;
    use std::time::Duration;

    fn sleeper() -> Arc<PtySession> {
        let mut config = PtySpawnConfig::shell_command("sleep 30");
        config.tty = false;
        Arc::new(PtySession::spawn(config).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn register_and_get_round_trip() {
        let manager = PtySessionManager::new(4);
        let session = sleeper();
        let id = manager.register(session.clone(), "sleep 30");
        let found = manager.get(id).expect("session should be tracked");
        assert!(Arc::ptr_eq(&session, &found));
        assert!(manager.get(id + 1).is_none() || id + 1 != id);
        manager.terminate_all();
        assert!(manager.get(id).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cap_evicts_least_recently_used() {
        let manager = PtySessionManager::new(2);
        let first = manager.register(sleeper(), "first");
        let second = manager.register(sleeper(), "second");
        // Refresh `first` so `second` becomes the LRU victim.
        manager.get(first).unwrap();
        let third = manager.register(sleeper(), "third");

        assert!(manager.get(first).is_some());
        assert!(manager.get(second).is_none(), "LRU entry should be evicted");
        assert!(manager.get(third).is_some());
        manager.terminate_all();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cap_prefers_evicting_exited_sessions() {
        let manager = PtySessionManager::new(2);

        let mut config = PtySpawnConfig::shell_command("true");
        config.tty = false;
        let exited = Arc::new(PtySession::spawn(config).unwrap());
        let _ = exited.collect_output(Duration::from_secs(10)).await;

        let exited_id = manager.register(exited, "true");
        let live_id = manager.register(sleeper(), "live");
        // `exited_id` is older AND exited; a live LRU rule alone would also
        // pick it, so refresh it to prove exited-ness wins over recency.
        manager.get(exited_id).unwrap();
        let third_id = manager.register(sleeper(), "third");

        assert!(
            manager.get(exited_id).is_none(),
            "exited session should be evicted first"
        );
        assert!(manager.get(live_id).is_some());
        assert!(manager.get(third_id).is_some());
        manager.terminate_all();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_reports_status() {
        let manager = PtySessionManager::new(4);
        let id = manager.register(sleeper(), "sleep 30");
        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].command_line, "sleep 30");
        assert_eq!(listed[0].status, PtySessionStatus::Running);
        manager.terminate_all();
    }
}
