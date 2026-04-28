//! Filesystem watcher for cross-instance session awareness.
//!
//! Monitors the sessions directory for changes made by other code-assistant
//! processes and emits UI events so the current instance stays up to date.
//!
//! # Watched events
//!
//! | File pattern | Trigger | UI effect |
//! |---|---|---|
//! | `metadata.json` | Create / Modify / Remove | Refresh sidebar session list |
//! | `<session_id>.json` | Modify | Reload session if currently viewed |
//! | `<session_id>.agent.lock` | Create / Remove | Update activity state (agent running elsewhere) |
//!
//! # Cross-platform notes
//!
//! The `notify` crate uses the platform-native backend:
//! - **macOS**: FSEvents (via the `macos_fsevent` feature)
//! - **Linux**: inotify
//! - **Windows**: ReadDirectoryChangesW
//!
//! Different backends produce different event sequences for the same logical
//! operation.  For example, an atomic write (tmp + rename) may generate
//! `Create` + `Modify` on some platforms, a single `Modify` on others, or
//! even multiple `Modify` events in quick succession.
//!
//! To handle this, the watcher collects raw events into a set of *dirty
//! files* and flushes them on a debounce timer.  This guarantees at most one
//! UI reaction per logical change, regardless of platform.

use crate::persistence::FileSessionPersistence;
use crate::session::instance::SessionActivityState;
use crate::ui::ui_events::UiEvent;
use crate::utils::file_utils;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, trace, warn};

/// How long to wait after the last filesystem event before flushing.
/// Short enough to feel responsive, long enough to coalesce bursts.
const DEBOUNCE_DURATION: Duration = Duration::from_millis(300);

/// A handle to the background filesystem watcher.
///
/// Dropping this stops the watcher.
pub struct SessionWatcher {
    _watcher: RecommendedWatcher,
}

/// Categorised dirty-file set, accumulated between debounce flushes.
#[derive(Default)]
struct DirtySet {
    /// `metadata.json` was touched → refresh sidebar.
    metadata_dirty: bool,
    /// Session JSON files that changed on disk.
    /// We only reload the currently-viewed one, but we track all of them
    /// in case the user switches sessions between accumulation and flush.
    changed_session_ids: HashSet<String>,
    /// Agent lock files that appeared or disappeared.
    changed_agent_locks: HashSet<String>,
}

impl SessionWatcher {
    /// Start watching the sessions directory.
    ///
    /// `event_tx` is used to push UI events (e.g. `RefreshChatList`,
    /// `UpdateSessionActivityState`) into the existing event loop.
    ///
    /// `current_session_id` is read at flush time to decide whether a
    /// session-file change requires reloading the currently viewed session.
    pub fn start(
        persistence: &FileSessionPersistence,
        event_tx: async_channel::Sender<UiEvent>,
        current_session_id: Arc<Mutex<Option<String>>>,
    ) -> anyhow::Result<Self> {
        let sessions_dir = persistence.sessions_dir()?;
        debug!("Starting filesystem watcher on {}", sessions_dir.display());

        let dirty = Arc::new(Mutex::new(DirtySet::default()));

        // --- notify callback (sync, runs on notify's background thread) ---
        let dirty_for_callback = dirty.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => accumulate_event(&dirty_for_callback, &event),
                Err(e) => warn!("Filesystem watcher error: {e}"),
            })?;

        watcher.watch(&sessions_dir, RecursiveMode::NonRecursive)?;

        // --- debounce flush task (async, runs on the tokio runtime) ---
        let rt = tokio::runtime::Handle::current();
        rt.spawn(flush_loop(
            dirty,
            sessions_dir,
            event_tx,
            current_session_id,
        ));

        Ok(Self { _watcher: watcher })
    }
}

// ---------------------------------------------------------------------------
// Accumulate (sync, notify thread)
// ---------------------------------------------------------------------------

/// Classify a raw filesystem event and add it to the dirty set.
fn accumulate_event(dirty: &Mutex<DirtySet>, event: &Event) {
    for path in &event.paths {
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let mut set = dirty.lock().unwrap();

        if file_name == "metadata.json" {
            trace!("Watcher: metadata.json changed ({:?})", event.kind);
            set.metadata_dirty = true;
            continue;
        }

        if let Some(session_id) = file_name.strip_suffix(".agent.lock") {
            trace!(
                "Watcher: agent lock changed for {session_id} ({:?})",
                event.kind
            );
            set.changed_agent_locks.insert(session_id.to_string());
            continue;
        }

        if let Some(session_id) = file_name.strip_suffix(".json") {
            // Skip non-session JSON files
            if session_id.ends_with(".ui_state") || session_id == "metadata" {
                continue;
            }
            trace!(
                "Watcher: session file changed for {session_id} ({:?})",
                event.kind
            );
            set.changed_session_ids.insert(session_id.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Flush (async, tokio runtime)
// ---------------------------------------------------------------------------

/// Periodically drain the dirty set and emit UI events.
async fn flush_loop(
    dirty: Arc<Mutex<DirtySet>>,
    sessions_dir: PathBuf,
    event_tx: async_channel::Sender<UiEvent>,
    current_session_id: Arc<Mutex<Option<String>>>,
) {
    loop {
        tokio::time::sleep(DEBOUNCE_DURATION).await;

        // Take the dirty set (swap with a fresh default).
        let snapshot = {
            let mut set = dirty.lock().unwrap();
            std::mem::take(&mut *set)
        };

        // Nothing to do?
        if !snapshot.metadata_dirty
            && snapshot.changed_session_ids.is_empty()
            && snapshot.changed_agent_locks.is_empty()
        {
            continue;
        }

        // 1) Sidebar refresh
        if snapshot.metadata_dirty {
            debug!("Watcher flush: refreshing chat list");
            let _ = event_tx.try_send(UiEvent::RefreshChatList);
        }

        // 2) Agent lock changes → activity state updates
        for session_id in &snapshot.changed_agent_locks {
            let is_locked = file_utils::is_agent_locked(&sessions_dir, session_id);

            let activity_state = if is_locked {
                SessionActivityState::AgentRunning
            } else {
                SessionActivityState::Idle
            };

            debug!("Watcher flush: agent lock for {session_id} → {activity_state:?}");
            let _ = event_tx.try_send(UiEvent::UpdateSessionActivityState {
                session_id: session_id.clone(),
                activity_state,
            });
        }

        // 3) Session file changes → reload currently viewed session
        if !snapshot.changed_session_ids.is_empty() {
            let current = current_session_id.lock().unwrap().clone();
            if let Some(current_id) = current {
                if snapshot.changed_session_ids.contains(&current_id) {
                    debug!("Watcher flush: reloading current session {current_id}");
                    let _ = event_tx.try_send(UiEvent::RefreshCurrentSession {
                        session_id: current_id,
                    });
                }
            }
        }
    }
}
