//! Per-session GPUI-specific UI state that is persisted independently from the
//! main session JSON file.
//!
//! Each session gets a small `<session_id>.ui_state.json` file in the sessions
//! directory.  This avoids re-serialising the (potentially large) full session
//! just because the user toggled a plan banner or collapsed a tool block.
//!
//! The [`UiStateStore`] is a global singleton that keeps an in-memory cache of
//! all loaded states and a dirty set.  Mutations are cheap (HashMap write) and
//! persistence is debounced — a single write is scheduled after the last
//! mutation within a configurable window.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tracing::{debug, warn};

/// Duration to wait after the last mutation before flushing to disk.
const DEBOUNCE_MS: u64 = 500;

/// Returns the debounce duration.  Separated out so tests can refer to it.
pub fn debounce_duration() -> std::time::Duration {
    std::time::Duration::from_millis(DEBOUNCE_MS)
}

// ---------------------------------------------------------------------------
// UiSessionState — the data model
// ---------------------------------------------------------------------------

/// Per-session UI state that is persisted to a separate file.
///
/// New fields can be added freely with `#[serde(default)]` for backward
/// compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiSessionState {
    /// Whether the plan banner is collapsed for this session.
    #[serde(default)]
    pub plan_collapsed: bool,

    /// Tool-block collapse/expand overrides set by the user.
    /// Key: tool_id, Value: `true` means collapsed.
    /// Only tool blocks that the user has *explicitly* toggled are stored here;
    /// blocks at their renderer-default state are omitted.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_collapse_overrides: HashMap<String, bool>,
}

// ---------------------------------------------------------------------------
// UiStateStore — global singleton
// ---------------------------------------------------------------------------

static STORE: OnceLock<Mutex<UiStateStore>> = OnceLock::new();

pub struct UiStateStore {
    /// Root directory for session files (e.g. `~/.local/share/code-assistant/sessions`).
    sessions_dir: PathBuf,
    /// In-memory cache of loaded session UI states.
    states: HashMap<String, UiSessionState>,
    /// Session IDs with unsaved changes.
    dirty: HashSet<String>,
}

impl UiStateStore {
    // -- Global singleton access --

    /// Initialise the global store.  Must be called once at startup (e.g. in
    /// `Gpui::new`) before any other access.
    pub fn init_global(sessions_dir: PathBuf) {
        let store = Self {
            sessions_dir,
            states: HashMap::new(),
            dirty: HashSet::new(),
        };
        let _ = STORE.set(Mutex::new(store));
    }

    /// Access the global store.  Returns `None` if [`init_global`] was not
    /// called (e.g. in tests or non-GPUI mode).
    pub fn global() -> &'static Mutex<UiStateStore> {
        STORE
            .get()
            .expect("UiStateStore not initialised — call UiStateStore::init_global first")
    }

    /// Try to access the global store, returning `None` if it hasn't been
    /// initialised yet.
    pub fn try_global() -> Option<&'static Mutex<UiStateStore>> {
        STORE.get()
    }

    // -- Query / Mutate --

    /// Return a clone of the state for `session_id`, loading from disk if
    /// necessary.
    pub fn get(&mut self, session_id: &str) -> UiSessionState {
        if !self.states.contains_key(session_id) {
            let state = self.load_from_disk(session_id);
            self.states.insert(session_id.to_owned(), state);
        }
        self.states.get(session_id).cloned().unwrap_or_default()
    }

    /// Return a clone of a specific tool's collapse override, loading from disk
    /// if the session hasn't been loaded yet.
    pub fn get_tool_collapsed(&mut self, session_id: &str, tool_id: &str) -> Option<bool> {
        let state = self.get(session_id);
        state.tool_collapse_overrides.get(tool_id).copied()
    }

    /// Return the `plan_collapsed` flag for a session, loading from disk if
    /// necessary.
    pub fn get_plan_collapsed(&mut self, session_id: &str) -> bool {
        self.get(session_id).plan_collapsed
    }

    /// Set the `plan_collapsed` flag for a session.
    pub fn set_plan_collapsed(&mut self, session_id: &str, collapsed: bool) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        state.plan_collapsed = collapsed;
        self.dirty.insert(session_id.to_owned());
    }

    /// Set a tool-block collapse override.
    pub fn set_tool_collapsed(&mut self, session_id: &str, tool_id: &str, collapsed: bool) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        state
            .tool_collapse_overrides
            .insert(tool_id.to_owned(), collapsed);
        self.dirty.insert(session_id.to_owned());
    }

    /// Remove the in-memory state and on-disk file for a deleted session.
    pub fn remove_session(&mut self, session_id: &str) {
        self.states.remove(session_id);
        self.dirty.remove(session_id);
        let path = self.file_path(session_id);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Failed to remove UI state file {}: {}", path.display(), e);
            }
        }
    }

    // -- Persistence --

    /// Take the set of dirty session IDs and return their serialised states so
    /// that the caller can write them on a background thread.
    ///
    /// After calling this the dirty set is empty.
    pub fn take_dirty(&mut self) -> Vec<(PathBuf, String)> {
        let ids: Vec<String> = self.dirty.drain().collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(state) = self.states.get(&id) {
                let path = self.file_path(&id);
                match serde_json::to_string_pretty(state) {
                    Ok(json) => out.push((path, json)),
                    Err(e) => warn!("Failed to serialise UI state for session {}: {}", id, e),
                }
            }
        }
        out
    }

    /// Check whether any sessions have unsaved changes.
    #[allow(dead_code)]
    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    // -- Internal --

    fn file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{session_id}.ui_state.json"))
    }

    fn load_from_disk(&self, session_id: &str) -> UiSessionState {
        let path = self.file_path(session_id);
        if !path.exists() {
            return UiSessionState::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(state) => {
                    debug!(
                        "Loaded UI state for session {} from {}",
                        session_id,
                        path.display()
                    );
                    state
                }
                Err(e) => {
                    warn!("Failed to parse UI state file {}: {}", path.display(), e);
                    UiSessionState::default()
                }
            },
            Err(e) => {
                warn!("Failed to read UI state file {}: {}", path.display(), e);
                UiSessionState::default()
            }
        }
    }
}

/// Write a list of `(path, json_content)` pairs to disk.
///
/// Designed to be called from a background thread via `cx.background_spawn`.
pub fn write_ui_state_files(files: Vec<(PathBuf, String)>) {
    for (path, json) in files {
        if let Err(e) = crate::utils::file_utils::atomic_write(&path, json.as_bytes()) {
            warn!("Failed to write UI state file {}: {}", path.display(), e);
        } else {
            debug!("Saved UI state to {}", path.display());
        }
    }
}
