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

/// A persisted scroll position for a session's message list.
///
/// The anchor is the list's *logical* scroll position (an item index plus a
/// pixel offset within that item), which survives remeasuring far better than a
/// raw pixel offset. `follow_tail` records whether the view was pinned to the
/// bottom.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct ScrollPosition {
    /// Index of the item at the top of the viewport.
    pub item_ix: usize,
    /// Pixel offset within that item.
    pub offset_in_item: f32,
    /// Whether the view was following the tail (pinned to the bottom).
    pub follow_tail: bool,
}

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

    /// write_file diff mode overrides set by the user.
    /// Key: tool_id, Value: `true` means show diff view, `false` means show
    /// plain new-file view. Only stored when the user explicitly toggles away
    /// from the default (diff mode = true).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_diff_mode_overrides: HashMap<String, bool>,

    /// Last known scroll position of the message list, so the session is shown
    /// exactly as it was left across app restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll: Option<ScrollPosition>,

    /// Whether the right (review) sidebar is open for this session.
    #[serde(default)]
    pub right_panel_open: bool,

    /// Which view the right panel last showed (e.g. "review").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_panel_view: Option<String>,

    /// Last review compare mode ("working_tree" or "branch_vs_base").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_compare_mode: Option<String>,
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

    /// Return the diff mode override for a write_file tool block, loading from
    /// disk if the session hasn't been loaded yet.
    pub fn get_tool_diff_mode(&mut self, session_id: &str, tool_id: &str) -> Option<bool> {
        let state = self.get(session_id);
        state.tool_diff_mode_overrides.get(tool_id).copied()
    }

    /// Set a write_file diff mode override.
    pub fn set_tool_diff_mode(&mut self, session_id: &str, tool_id: &str, diff_mode: bool) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        state
            .tool_diff_mode_overrides
            .insert(tool_id.to_owned(), diff_mode);
        self.dirty.insert(session_id.to_owned());
    }

    /// Return the persisted scroll position for a session, loading from disk if
    /// the session hasn't been loaded yet.
    pub fn get_scroll(&mut self, session_id: &str) -> Option<ScrollPosition> {
        self.get(session_id).scroll
    }

    /// Set the persisted scroll position for a session. Marks the session dirty
    /// so the next debounced flush writes it to disk. Returns `true` if the
    /// value actually changed (a no-op update from a settling scroll animation
    /// neither dirties the session nor returns `true`).
    pub fn set_scroll(&mut self, session_id: &str, scroll: ScrollPosition) -> bool {
        let state = self.states.entry(session_id.to_owned()).or_default();
        if state.scroll == Some(scroll) {
            return false;
        }
        state.scroll = Some(scroll);
        self.dirty.insert(session_id.to_owned());
        true
    }

    /// Return whether the right (review) sidebar is open for a session.
    /// Loads from disk if the session hasn't been loaded yet.
    pub fn get_right_panel_open(&mut self, session_id: &str) -> bool {
        self.get(session_id).right_panel_open
    }

    /// Set whether the right (review) sidebar is open for a session.
    pub fn set_right_panel_open(&mut self, session_id: &str, open: bool) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        if state.right_panel_open == open {
            return;
        }
        state.right_panel_open = open;
        self.dirty.insert(session_id.to_owned());
    }

    /// Return the persisted review compare mode for a session.
    pub fn get_review_compare_mode(&mut self, session_id: &str) -> Option<String> {
        self.get(session_id).review_compare_mode
    }

    /// Persist the review compare mode for a session.
    pub fn set_review_compare_mode(&mut self, session_id: &str, mode: String) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        if state.review_compare_mode.as_deref() == Some(mode.as_str()) {
            return;
        }
        state.review_compare_mode = Some(mode);
        self.dirty.insert(session_id.to_owned());
    }

    /// Remove the in-memory state and on-disk file for a deleted session.
    pub fn remove_session(&mut self, session_id: &str) {
        self.states.remove(session_id);
        self.dirty.remove(session_id);
        let path = self.file_path(session_id);
        if path.exists()
            && let Err(e) = std::fs::remove_file(&path)
        {
            warn!("Failed to remove UI state file {}: {}", path.display(), e);
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
        if let Err(e) = code_assistant_core::utils::file_utils::atomic_write(&path, json.as_bytes())
        {
            warn!("Failed to write UI state file {}: {}", path.display(), e);
        } else {
            debug!("Saved UI state to {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper to create a store backed by a temporary directory.
    fn test_store() -> (UiStateStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = UiStateStore {
            sessions_dir: dir.path().to_owned(),
            states: HashMap::new(),
            dirty: HashSet::new(),
        };
        (store, dir)
    }

    #[test]
    fn test_get_returns_default_for_unknown_session() {
        let (mut store, _dir) = test_store();
        let state = store.get("nonexistent");
        assert!(!state.plan_collapsed);
        assert!(state.tool_collapse_overrides.is_empty());
        assert!(state.tool_diff_mode_overrides.is_empty());
    }

    #[test]
    fn test_set_plan_collapsed_marks_dirty() {
        let (mut store, _dir) = test_store();
        store.set_plan_collapsed("session-1", true);
        assert!(store.dirty.contains("session-1"));
        assert!(store.get("session-1").plan_collapsed);
    }

    #[test]
    fn test_set_tool_collapsed_roundtrip() {
        let (mut store, _dir) = test_store();
        store.set_tool_collapsed("s1", "tool-abc", true);
        assert_eq!(store.get_tool_collapsed("s1", "tool-abc"), Some(true));
        assert_eq!(store.get_tool_collapsed("s1", "tool-other"), None);
    }

    #[test]
    fn test_set_tool_diff_mode_roundtrip() {
        let (mut store, _dir) = test_store();
        store.set_tool_diff_mode("s1", "tool-xyz", false);
        assert_eq!(store.get_tool_diff_mode("s1", "tool-xyz"), Some(false));
        assert_eq!(store.get_tool_diff_mode("s1", "tool-other"), None);
    }

    #[test]
    fn test_set_scroll_roundtrip_and_dirty() {
        let (mut store, _dir) = test_store();
        assert_eq!(store.get_scroll("s1"), None);

        let pos = ScrollPosition {
            item_ix: 7,
            offset_in_item: 12.5,
            follow_tail: false,
        };
        store.set_scroll("s1", pos);
        assert!(store.dirty.contains("s1"));
        assert_eq!(store.get_scroll("s1"), Some(pos));
    }

    #[test]
    fn test_set_scroll_no_op_when_unchanged() {
        let (mut store, _dir) = test_store();
        let pos = ScrollPosition {
            item_ix: 3,
            offset_in_item: 0.0,
            follow_tail: true,
        };
        store.set_scroll("s1", pos);
        // Clear dirty (simulate a flush), then set the identical value again.
        store.dirty.clear();
        let changed = store.set_scroll("s1", pos);
        assert!(!changed, "identical scroll must report no change");
        assert!(
            !store.dirty.contains("s1"),
            "identical scroll must not re-dirty the session"
        );
    }

    #[test]
    fn test_scroll_survives_serialization() {
        let (mut store, _dir) = test_store();
        store.set_scroll(
            "s1",
            ScrollPosition {
                item_ix: 42,
                offset_in_item: 3.25,
                follow_tail: false,
            },
        );
        let files = store.take_dirty();
        assert_eq!(files.len(), 1);
        let (_path, json) = &files[0];
        let parsed: UiSessionState = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.scroll,
            Some(ScrollPosition {
                item_ix: 42,
                offset_in_item: 3.25,
                follow_tail: false,
            })
        );
    }

    #[test]
    fn test_take_dirty_clears_dirty_set() {
        let (mut store, _dir) = test_store();
        store.set_plan_collapsed("s1", true);
        store.set_plan_collapsed("s2", false);

        let files = store.take_dirty();
        assert_eq!(files.len(), 2);
        assert!(store.dirty.is_empty());
    }

    #[test]
    fn test_take_dirty_produces_valid_json() {
        let (mut store, _dir) = test_store();
        store.set_plan_collapsed("s1", true);
        store.set_tool_collapsed("s1", "t1", true);

        let files = store.take_dirty();
        assert_eq!(files.len(), 1);
        let (_path, json) = &files[0];
        let parsed: UiSessionState = serde_json::from_str(json).unwrap();
        assert!(parsed.plan_collapsed);
        assert_eq!(parsed.tool_collapse_overrides.get("t1"), Some(&true));
    }

    #[test]
    fn test_set_review_compare_mode_roundtrip_and_dirty() {
        let (mut store, _dir) = test_store();
        assert_eq!(store.get_review_compare_mode("s1"), None);

        store.set_review_compare_mode("s1", "branch_vs_base".to_owned());
        assert!(store.dirty.contains("s1"));
        assert_eq!(
            store.get_review_compare_mode("s1").as_deref(),
            Some("branch_vs_base")
        );

        // Setting the identical value must not re-dirty the session.
        store.dirty.clear();
        store.set_review_compare_mode("s1", "branch_vs_base".to_owned());
        assert!(!store.dirty.contains("s1"));
    }

    #[test]
    fn test_load_from_disk() {
        let (mut store, dir) = test_store();
        // Write a state file manually
        let state = UiSessionState {
            plan_collapsed: true,
            tool_collapse_overrides: HashMap::from([("t1".to_owned(), false)]),
            tool_diff_mode_overrides: HashMap::new(),
            scroll: None,
            ..Default::default()
        };
        let path = dir.path().join("s1.ui_state.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(serde_json::to_string(&state).unwrap().as_bytes())
            .unwrap();

        // Load it via get
        let loaded = store.get("s1");
        assert!(loaded.plan_collapsed);
        assert_eq!(loaded.tool_collapse_overrides.get("t1"), Some(&false));
    }

    #[test]
    fn test_load_from_disk_handles_missing_file() {
        let (mut store, _dir) = test_store();
        let state = store.get("no-such-session");
        assert!(!state.plan_collapsed);
    }

    #[test]
    fn test_load_from_disk_handles_corrupt_file() {
        let (mut store, dir) = test_store();
        let path = dir.path().join("bad.ui_state.json");
        std::fs::write(&path, "not valid json!!!").unwrap();

        let state = store.get("bad");
        assert!(!state.plan_collapsed); // falls back to default
    }

    #[test]
    fn test_remove_session() {
        let (mut store, dir) = test_store();
        store.set_plan_collapsed("s1", true);

        // Simulate a file on disk
        let path = dir.path().join("s1.ui_state.json");
        std::fs::write(&path, "{}").unwrap();
        assert!(path.exists());

        store.remove_session("s1");
        assert!(!store.states.contains_key("s1"));
        assert!(!store.dirty.contains("s1"));
        assert!(!path.exists());
    }
}
