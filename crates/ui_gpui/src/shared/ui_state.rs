//! Per-session GPUI-specific UI state that is persisted independently from the
//! main session record.
//!
//! Each session gets a small `ui_state.json` file in its session folder. This
//! avoids re-serialising the (potentially large) full session just because the
//! user toggled a plan banner or collapsed a tool block.
//!
//! The [`UiStateStore`] keeps an in-memory cache of all loaded states and a
//! dirty set, in front of an injected [`UiStatePersistence`].  Mutations are
//! cheap (HashMap write) and persistence is debounced — a single write is
//! scheduled after the last mutation within a configurable window.

use anyhow::Result;
use code_assistant_core::persistence::SessionLayout;
use gpui_kit::App;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
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
// Persistence
// ---------------------------------------------------------------------------

/// Where per-session UI states are kept across restarts.
pub trait UiStatePersistence: Send + Sync {
    /// The stored state of a session, if there is one.
    fn load(&self, session_id: &str) -> Result<Option<UiSessionState>>;

    /// Store the state of a session, replacing the previous one.
    fn save(&self, session_id: &str, state: &UiSessionState) -> Result<()>;

    /// Remove the stored state of a session. Removing a missing state is not
    /// an error.
    fn delete(&self, session_id: &str) -> Result<()>;
}

/// UI states as `ui_state.json` files in the session folders.
pub struct FileUiStatePersistence {
    layout: SessionLayout,
}

impl FileUiStatePersistence {
    pub fn new(layout: SessionLayout) -> Self {
        Self { layout }
    }

    fn file_path(&self, session_id: &str) -> Result<PathBuf> {
        self.layout.ui_state(session_id)
    }
}

impl UiStatePersistence for FileUiStatePersistence {
    fn load(&self, session_id: &str) -> Result<Option<UiSessionState>> {
        let json = match std::fs::read_to_string(self.file_path(session_id)?) {
            Ok(json) => json,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(serde_json::from_str(&json)?))
    }

    fn save(&self, session_id: &str, state: &UiSessionState) -> Result<()> {
        // A deleted session keeps no state; writing would recreate its folder.
        if !self.layout.session_dir(session_id)?.is_dir() {
            return Ok(());
        }
        let json = serde_json::to_string_pretty(state)?;
        code_assistant_core::utils::file_utils::atomic_write(
            &self.file_path(session_id)?,
            json.as_bytes(),
        )
    }

    fn delete(&self, session_id: &str) -> Result<()> {
        match std::fs::remove_file(self.file_path(session_id)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// UiStateStore — in-memory cache in front of the persistence
// ---------------------------------------------------------------------------

/// The app's per-session UI states: a cache of the loaded ones and the set of
/// changed ones not yet written. Owned by [`Gpui`](crate::Gpui); views reach
/// it through [`read`] and [`update`].
pub struct UiStateStore {
    persistence: Arc<dyn UiStatePersistence>,
    /// In-memory cache of loaded session UI states.
    states: HashMap<String, UiSessionState>,
    /// Session IDs with unsaved changes.
    dirty: HashSet<String>,
}

/// Read from the app's UI state store. `None` when there is none (a test
/// without a [`Gpui`](crate::Gpui) global).
pub fn read<R>(cx: &App, f: impl FnOnce(&mut UiStateStore) -> R) -> Option<R> {
    let store = cx.try_global::<crate::Gpui>()?.ui_state.clone();
    let mut store = store.lock().unwrap();
    Some(f(&mut store))
}

/// Change the app's UI state and, if that changed anything, schedule the
/// debounced write.
pub fn update(cx: &App, f: impl FnOnce(&mut UiStateStore)) {
    let changed = read(cx, |store| {
        f(store);
        store.has_dirty()
    });
    if changed == Some(true)
        && let Some(sender) = cx.try_global::<crate::UiEventSender>()
    {
        let _ = sender
            .0
            .try_send(code_assistant_core::ui::UiEvent::PersistUiState);
    }
}

impl UiStateStore {
    pub fn new(persistence: Arc<dyn UiStatePersistence>) -> Self {
        Self {
            persistence,
            states: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    // -- Query / Mutate --

    /// Return a clone of the state for `session_id`, loading it if necessary.
    pub fn get(&mut self, session_id: &str) -> UiSessionState {
        if !self.states.contains_key(session_id) {
            let state = self.load(session_id);
            self.states.insert(session_id.to_owned(), state);
        }
        self.states.get(session_id).cloned().unwrap_or_default()
    }

    /// Return a clone of a specific tool's collapse override, loading the
    /// session's state if necessary.
    pub fn get_tool_collapsed(&mut self, session_id: &str, tool_id: &str) -> Option<bool> {
        let state = self.get(session_id);
        state.tool_collapse_overrides.get(tool_id).copied()
    }

    /// Return the `plan_collapsed` flag for a session, loading the session's
    /// state if necessary.
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

    /// Return the diff mode override for a write_file tool block, loading the
    /// session's state if necessary.
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

    /// Return the persisted scroll position for a session, loading the
    /// session's state if necessary.
    pub fn get_scroll(&mut self, session_id: &str) -> Option<ScrollPosition> {
        self.get(session_id).scroll
    }

    /// Set the persisted scroll position for a session. A no-op update from a
    /// settling scroll animation does not dirty the session.
    pub fn set_scroll(&mut self, session_id: &str, scroll: ScrollPosition) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        if state.scroll == Some(scroll) {
            return;
        }
        state.scroll = Some(scroll);
        self.dirty.insert(session_id.to_owned());
    }

    /// Return whether the right (review) sidebar is open for a session,
    /// loading the session's state if necessary.
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

    /// Which view the right panel last showed for a session.
    #[cfg(feature = "browser-panel")]
    pub fn get_right_panel_view(&mut self, session_id: &str) -> Option<String> {
        self.get(session_id).right_panel_view
    }

    /// Persist which view the right panel shows for a session.
    #[cfg(feature = "browser-panel")]
    pub fn set_right_panel_view(&mut self, session_id: &str, view: &str) {
        let state = self.states.entry(session_id.to_owned()).or_default();
        if state.right_panel_view.as_deref() == Some(view) {
            return;
        }
        state.right_panel_view = Some(view.to_owned());
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

    /// Remove the cached and the stored state of a deleted session.
    pub fn remove_session(&mut self, session_id: &str) {
        self.states.remove(session_id);
        self.dirty.remove(session_id);
        if let Err(e) = self.persistence.delete(session_id) {
            warn!("Failed to remove UI state of session {}: {}", session_id, e);
        }
    }

    // -- Persistence --

    /// Take the changed states, to be written off the main thread with
    /// [`PendingWrites::write`]. Afterwards nothing is dirty.
    pub fn take_dirty(&mut self) -> PendingWrites {
        let states = self
            .dirty
            .drain()
            .filter_map(|id| {
                let state = self.states.get(&id)?.clone();
                Some((id, state))
            })
            .collect();
        PendingWrites {
            persistence: self.persistence.clone(),
            states,
        }
    }

    /// Check whether any sessions have unsaved changes.
    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    fn load(&self, session_id: &str) -> UiSessionState {
        match self.persistence.load(session_id) {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warn!("Failed to load UI state of session {}: {}", session_id, e);
                UiSessionState::default()
            }
        }
    }
}

/// Changed UI states taken from the store, not yet written.
pub struct PendingWrites {
    persistence: Arc<dyn UiStatePersistence>,
    states: Vec<(String, UiSessionState)>,
}

impl PendingWrites {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Write the states; meant for a background thread.
    pub fn write(self) {
        for (session_id, state) in self.states {
            match self.persistence.save(&session_id, &state) {
                Ok(()) => debug!("Saved UI state of session {}", session_id),
                Err(e) => warn!("Failed to save UI state of session {}: {}", session_id, e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockUiStatePersistence;
    use tempfile::TempDir;

    fn test_store() -> (UiStateStore, MockUiStatePersistence) {
        let persistence = MockUiStatePersistence::default();
        (
            UiStateStore::new(Arc::new(persistence.clone())),
            persistence,
        )
    }

    #[test]
    fn test_get_returns_default_for_unknown_session() {
        let (mut store, _) = test_store();
        let state = store.get("nonexistent");
        assert!(!state.plan_collapsed);
        assert!(state.tool_collapse_overrides.is_empty());
        assert!(state.tool_diff_mode_overrides.is_empty());
    }

    #[test]
    fn test_set_plan_collapsed_marks_dirty() {
        let (mut store, _) = test_store();
        store.set_plan_collapsed("session-1", true);
        assert!(store.dirty.contains("session-1"));
        assert!(store.get("session-1").plan_collapsed);
    }

    #[test]
    fn test_set_tool_collapsed_roundtrip() {
        let (mut store, _) = test_store();
        store.set_tool_collapsed("s1", "tool-abc", true);
        assert_eq!(store.get_tool_collapsed("s1", "tool-abc"), Some(true));
        assert_eq!(store.get_tool_collapsed("s1", "tool-other"), None);
    }

    #[test]
    fn test_set_tool_diff_mode_roundtrip() {
        let (mut store, _) = test_store();
        store.set_tool_diff_mode("s1", "tool-xyz", false);
        assert_eq!(store.get_tool_diff_mode("s1", "tool-xyz"), Some(false));
        assert_eq!(store.get_tool_diff_mode("s1", "tool-other"), None);
    }

    #[test]
    fn test_set_scroll_roundtrip_and_dirty() {
        let (mut store, _) = test_store();
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
        let (mut store, _) = test_store();
        let pos = ScrollPosition {
            item_ix: 3,
            offset_in_item: 0.0,
            follow_tail: true,
        };
        store.set_scroll("s1", pos);
        // Clear dirty (simulate a flush), then set the identical value again.
        store.dirty.clear();
        store.set_scroll("s1", pos);
        assert!(
            !store.dirty.contains("s1"),
            "identical scroll must not re-dirty the session"
        );
    }

    #[test]
    fn test_pending_writes_store_the_dirty_states() {
        let (mut store, persistence) = test_store();
        store.set_plan_collapsed("s1", true);
        store.set_tool_collapsed("s1", "t1", true);
        store.set_plan_collapsed("s2", false);

        let writes = store.take_dirty();
        assert!(store.dirty.is_empty());
        writes.write();

        let s1 = persistence.stored("s1").expect("s1 written");
        assert!(s1.plan_collapsed);
        assert_eq!(s1.tool_collapse_overrides.get("t1"), Some(&true));
        assert!(persistence.stored("s2").is_some());
    }

    #[test]
    fn test_set_review_compare_mode_roundtrip_and_dirty() {
        let (mut store, _) = test_store();
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
    fn test_get_loads_the_stored_state() {
        let (mut store, persistence) = test_store();
        let state = UiSessionState {
            plan_collapsed: true,
            tool_collapse_overrides: HashMap::from([("t1".to_owned(), false)]),
            ..Default::default()
        };
        persistence.save("s1", &state).unwrap();

        let loaded = store.get("s1");
        assert!(loaded.plan_collapsed);
        assert_eq!(loaded.tool_collapse_overrides.get("t1"), Some(&false));
    }

    #[test]
    fn test_get_falls_back_to_default_when_loading_fails() {
        let (mut store, persistence) = test_store();
        persistence
            .save(
                "s1",
                &UiSessionState {
                    plan_collapsed: true,
                    ..Default::default()
                },
            )
            .unwrap();
        persistence.fail_reads(Some(std::io::ErrorKind::PermissionDenied));

        assert!(!store.get("s1").plan_collapsed);
    }

    #[test]
    fn test_failed_writes_leave_the_store_usable() {
        let (mut store, persistence) = test_store();
        persistence.fail_writes(Some(std::io::ErrorKind::StorageFull));
        store.set_plan_collapsed("s1", true);
        store.take_dirty().write();

        assert!(persistence.stored("s1").is_none());
        assert!(store.get("s1").plan_collapsed, "the cache keeps the change");
    }

    #[test]
    fn test_remove_session() {
        let (mut store, persistence) = test_store();
        persistence.save("s1", &UiSessionState::default()).unwrap();
        store.set_plan_collapsed("s1", true);

        store.remove_session("s1");
        assert!(!store.states.contains_key("s1"));
        assert!(!store.dirty.contains("s1"));
        assert!(persistence.stored("s1").is_none());
    }

    #[test]
    fn test_file_persistence_round_trips_and_deletes() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("p/s1")).unwrap();
        let persistence = FileUiStatePersistence::new(SessionLayout::new(dir.path().to_owned()));
        assert!(persistence.load("p/s1").unwrap().is_none());

        let state = UiSessionState {
            scroll: Some(ScrollPosition {
                item_ix: 42,
                offset_in_item: 3.25,
                follow_tail: false,
            }),
            ..Default::default()
        };
        persistence.save("p/s1", &state).unwrap();
        assert_eq!(
            persistence.load("p/s1").unwrap().and_then(|s| s.scroll),
            state.scroll
        );
        assert!(dir.path().join("p/s1/ui_state.json").exists());

        persistence.delete("p/s1").unwrap();
        assert!(persistence.load("p/s1").unwrap().is_none());
        persistence.delete("p/s1").unwrap();
    }

    #[test]
    fn test_file_persistence_does_not_recreate_a_deleted_session() {
        let dir = TempDir::new().unwrap();
        let persistence = FileUiStatePersistence::new(SessionLayout::new(dir.path().to_owned()));
        persistence
            .save("gone", &UiSessionState::default())
            .unwrap();
        assert!(!dir.path().join("gone").exists());
    }

    #[test]
    fn test_file_persistence_reports_a_corrupt_file() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("bad")).unwrap();
        std::fs::write(dir.path().join("bad/ui_state.json"), "not valid json!!!").unwrap();
        let persistence = FileUiStatePersistence::new(SessionLayout::new(dir.path().to_owned()));
        assert!(persistence.load("bad").is_err());
    }
}
