//! Sub-popup that lets the user switch to another session.
//!
//! Pushed by [`super::command_list::CommandListPopup`] when `/sessions` (or
//! `/resume`) is activated. The session list is supplied from
//! [`crate::state::AppState::sessions`], which is populated by a
//! `SessionService::list_sessions` request (see `Actions::refresh_chat_list`).

use crate::commands::CommandResult;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use code_assistant_core::persistence::ChatMetadata;

pub struct SessionPickerPopup {
    /// All session entries (parallel to `all_rows`), used to dispatch on activate.
    all_entries: Vec<ChatMetadata>,
    /// All rows, before filtering.
    all_rows: Vec<PopupRow>,
    /// Currently visible rows.
    visible_rows: Vec<PopupRow>,
    /// Indices into `all_entries`/`all_rows` for the visible rows.
    visible_indices: Vec<usize>,
    /// Highlighted row in `visible_rows`.
    selected: usize,
}

/// Human-readable label for a session row: the session name, or a shortened id
/// when the session was never named.
fn session_label(meta: &ChatMetadata) -> String {
    let name = meta.name.trim();
    if !name.is_empty() {
        name.to_string()
    } else {
        let short: String = meta.id.chars().take(8).collect();
        format!("session {short}")
    }
}

/// Secondary description: message count, project, and a resumable hint.
fn session_description(meta: &ChatMetadata) -> String {
    let messages = format!(
        "{} message{}",
        meta.message_count,
        if meta.message_count == 1 { "" } else { "s" }
    );
    let mut parts = vec![messages];
    let project = meta.initial_project.trim();
    if !project.is_empty() && project != "unknown" {
        parts.push(project.to_string());
    }
    if meta.is_resumable {
        parts.push("resumable".to_string());
    }
    parts.join(" · ")
}

impl SessionPickerPopup {
    pub fn from_sessions(mut entries: Vec<ChatMetadata>) -> Self {
        // Newest first, so the most recently touched sessions are at the top.
        entries.sort_by_key(|meta| std::cmp::Reverse(meta.updated_at));
        let all_rows: Vec<PopupRow> = entries
            .iter()
            .map(|meta| PopupRow {
                label: session_label(meta),
                description: session_description(meta),
                has_submenu: false,
            })
            .collect();
        let visible_indices = (0..all_rows.len()).collect::<Vec<_>>();
        let visible_rows = all_rows.clone();
        Self {
            all_entries: entries,
            all_rows,
            visible_rows,
            visible_indices,
            selected: 0,
        }
    }

    /// The metadata for the currently highlighted visible row.
    fn selected_entry(&self) -> Option<&ChatMetadata> {
        let idx = *self.visible_indices.get(self.selected)?;
        self.all_entries.get(idx)
    }
}

impl SlashPopup for SessionPickerPopup {
    fn title(&self) -> &str {
        "Switch session"
    }

    fn set_query(&mut self, query: &str) {
        // Substring match (case-insensitive) on label, project and id so partial
        // typing surfaces results.
        self.visible_rows.clear();
        self.visible_indices.clear();
        let q = query.to_lowercase();
        for (i, entry) in self.all_entries.iter().enumerate() {
            let haystack = format!(
                "{} {} {}",
                self.all_rows[i].label, entry.initial_project, entry.id
            )
            .to_lowercase();
            if q.is_empty() || haystack.contains(&q) {
                self.visible_rows.push(self.all_rows[i].clone());
                self.visible_indices.push(i);
            }
        }
        if self.visible_rows.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.visible_rows.len() {
            self.selected = self.visible_rows.len() - 1;
        }
    }

    fn rows(&self) -> &[PopupRow] {
        &self.visible_rows
    }

    fn selected(&self) -> usize {
        self.selected
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.visible_rows.len() as i32;
        if len == 0 {
            return;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
    }

    fn activate(&self) -> PopupAction {
        match self.selected_entry() {
            Some(entry) => PopupAction::Commit(CommandResult::SwitchSession(entry.id.clone())),
            None => PopupAction::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash_popup::PopupStack;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, SystemTime};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn meta(id: &str, name: &str, messages: usize, updated_offset_secs: u64) -> ChatMetadata {
        ChatMetadata {
            id: id.to_string(),
            name: name.to_string(),
            created_at: SystemTime::UNIX_EPOCH,
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(updated_offset_secs),
            message_count: messages,
            total_usage: Default::default(),
            last_usage: Default::default(),
            tokens_limit: None,
            tool_syntax: code_assistant_core::types::ToolSyntax::Native,
            initial_project: "my-proj".to_string(),
            plan_collapsed: false,
            is_resumable: false,
        }
    }

    #[test]
    fn lists_sessions_newest_first() {
        let popup = SessionPickerPopup::from_sessions(vec![
            meta("aaa", "Older", 3, 100),
            meta("bbb", "Newer", 5, 200),
        ]);
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["Newer", "Older"]);
    }

    #[test]
    fn unnamed_session_falls_back_to_short_id() {
        let popup = SessionPickerPopup::from_sessions(vec![meta("abcdef123456", "", 0, 1)]);
        assert_eq!(popup.rows()[0].label, "session abcdef12");
    }

    #[test]
    fn description_has_message_count_and_project() {
        let popup = SessionPickerPopup::from_sessions(vec![meta("x", "S", 1, 1)]);
        let desc = &popup.rows()[0].description;
        assert!(desc.contains("1 message"));
        assert!(desc.contains("my-proj"));
    }

    #[test]
    fn enter_commits_switch_session_with_id() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(SessionPickerPopup::from_sessions(vec![
            meta("aaa", "Older", 3, 100),
            meta("bbb", "Newer", 5, 200),
        ])));
        // Top row is "Newer" (bbb); move down to "Older" (aaa).
        stack.handle_key(key(KeyCode::Down));
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::SwitchSession(ref id)) if id == "aaa"
        ));
        assert!(!stack.is_active());
    }

    #[test]
    fn filter_matches_name_and_id() {
        let mut popup = SessionPickerPopup::from_sessions(vec![
            meta("deadbeef", "Refactor", 3, 100),
            meta("cafe", "Docs", 5, 200),
        ]);
        popup.set_query("refac");
        assert_eq!(popup.rows().len(), 1);
        assert_eq!(popup.rows()[0].label, "Refactor");
        popup.set_query("cafe");
        assert_eq!(popup.rows().len(), 1);
        assert_eq!(popup.rows()[0].label, "Docs");
    }

    #[test]
    fn empty_list_activate_is_noop() {
        let popup = SessionPickerPopup::from_sessions(vec![]);
        assert!(matches!(popup.activate(), PopupAction::Continue));
    }
}
