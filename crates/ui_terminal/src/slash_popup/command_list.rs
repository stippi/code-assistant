//! Root popup that lists all known slash commands.

use crate::commands::{all_commands, CommandResult};
use crate::slash_popup::session_picker::SessionPickerPopup;
use crate::slash_popup::skill_picker::SkillPickerPopup;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::service::SkillCatalogEntry;

pub struct CommandListPopup {
    /// All rows the popup knows about, before filtering.
    all_rows: Vec<PopupRow>,
    /// All command names (parallel to `all_rows`); used to dispatch on activate.
    all_names: Vec<&'static str>,
    /// Currently visible rows after filtering.
    visible_rows: Vec<PopupRow>,
    /// Indices into `all_rows`/`all_names` for the currently visible rows.
    visible_indices: Vec<usize>,
    /// Highlighted row inside `visible_rows`.
    selected: usize,
    /// Cached skill catalog used to build the `/skill` sub-popup.
    skills: Vec<SkillCatalogEntry>,
    /// Cached session list used to build the `/sessions` sub-popup.
    sessions: Vec<ChatMetadata>,
}

impl Default for CommandListPopup {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandListPopup {
    pub fn new() -> Self {
        Self::with_context(Vec::new(), Vec::new())
    }

    /// Construct the command list with a cached skill catalog so activating
    /// `/skill` can open a populated picker without a backend round-trip.
    pub fn with_skills(skills: Vec<SkillCatalogEntry>) -> Self {
        Self::with_context(skills, Vec::new())
    }

    /// Construct the command list with cached skill and session catalogs so
    /// activating `/skill` or `/sessions` can open a populated picker without a
    /// backend round-trip.
    pub fn with_context(skills: Vec<SkillCatalogEntry>, sessions: Vec<ChatMetadata>) -> Self {
        let mut all_rows = Vec::new();
        let mut all_names = Vec::new();
        for cmd in all_commands() {
            all_rows.push(PopupRow {
                label: format!("/{}", cmd.name),
                description: cmd.description.to_string(),
                has_submenu: command_has_submenu(cmd.name),
            });
            all_names.push(cmd.name);
        }
        let visible_indices = (0..all_rows.len()).collect::<Vec<_>>();
        let visible_rows = all_rows.clone();
        Self {
            all_rows,
            all_names,
            visible_rows,
            visible_indices,
            selected: 0,
            skills,
            sessions,
        }
    }

    /// Return the command name for the currently selected visible row.
    fn selected_name(&self) -> Option<&'static str> {
        let idx = *self.visible_indices.get(self.selected)?;
        self.all_names.get(idx).copied()
    }
}

/// Returns true if the command should open a sub-popup when activated without
/// arguments (instead of running immediately).
fn command_has_submenu(name: &str) -> bool {
    matches!(name, "model" | "skill" | "sessions")
}

/// Build the [`PopupAction`] for activating a slash command by name.
/// Pure function so we can unit-test the dispatch table without instantiating
/// the popup.
pub(crate) fn dispatch_command(name: &str) -> PopupAction {
    match name {
        "model" => PopupAction::Push(Box::new(super::model_picker::ModelPickerPopup::new())),
        "help" => PopupAction::Commit(CommandResult::Help(String::new())),
        "provider" => PopupAction::Commit(CommandResult::ListProviders),
        "current" => PopupAction::Commit(CommandResult::ShowCurrentModel),
        "plan" => PopupAction::Commit(CommandResult::TogglePlan),
        "clear" => PopupAction::Commit(CommandResult::ClearContext),
        "compact" => PopupAction::Commit(CommandResult::CompactContext),
        "goal" => PopupAction::Commit(CommandResult::InsertInputTemplate("/goal ".into())),
        other => PopupAction::Commit(CommandResult::InvalidCommand(format!(
            "Unknown command: /{other}"
        ))),
    }
}

impl SlashPopup for CommandListPopup {
    fn title(&self) -> &str {
        "Slash commands"
    }

    fn set_query(&mut self, query: &str) {
        // Filter rows by prefix-match on the command name (case-insensitive).
        // `query` is the text after the leading "/", so for "/cl" we receive "cl".
        let q = query.to_lowercase();
        self.visible_rows.clear();
        self.visible_indices.clear();
        for (i, name) in self.all_names.iter().enumerate() {
            if name.to_lowercase().starts_with(&q) {
                self.visible_rows.push(self.all_rows[i].clone());
                self.visible_indices.push(i);
            }
        }
        // Clamp selection.
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
        match self.selected_name() {
            // The skill picker needs the session-scoped catalog, which the
            // static `dispatch_command` table can't provide; build it here from
            // the cached entries instead.
            Some("skill") => PopupAction::Push(Box::new(SkillPickerPopup::from_entries(
                self.skills.clone(),
            ))),
            // The session picker needs the cached session list, which the
            // static `dispatch_command` table can't provide; build it here.
            Some("sessions") => PopupAction::Push(Box::new(SessionPickerPopup::from_sessions(
                self.sessions.clone(),
            ))),
            Some(name) => dispatch_command(name),
            None => PopupAction::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash_popup::PopupStack;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn lists_all_commands_initially() {
        let popup = CommandListPopup::new();
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"/help"));
        assert!(labels.contains(&"/model"));
        assert!(labels.contains(&"/clear"));
        assert!(labels.contains(&"/compact"));
        assert_eq!(popup.rows().len(), all_commands().len());
    }

    #[test]
    fn filter_by_prefix() {
        let mut popup = CommandListPopup::new();
        popup.set_query("c");
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        // "current", "clear", "compact" all start with c
        assert_eq!(labels, vec!["/current", "/clear", "/compact"]);
    }

    #[test]
    fn filter_narrowing_keeps_selection_in_range() {
        let mut popup = CommandListPopup::new();
        popup.set_query("c"); // 3 rows
        popup.move_selection(2); // selected = 2 (last)
        popup.set_query("cl"); // 1 row
        assert_eq!(popup.selected(), 0);
        assert_eq!(popup.rows().len(), 1);
        assert_eq!(popup.rows()[0].label, "/clear");
    }

    #[test]
    fn empty_filter_result_does_not_panic() {
        let mut popup = CommandListPopup::new();
        popup.set_query("zzzzz");
        assert!(popup.rows().is_empty());
        // Activating an empty filter is a no-op (Continue), not a panic.
        let action = popup.activate();
        assert!(matches!(action, PopupAction::Continue));
    }

    #[test]
    fn model_command_opens_submenu_with_marker() {
        let popup = CommandListPopup::new();
        let model_row = popup.rows().iter().find(|r| r.label == "/model").unwrap();
        assert!(
            model_row.has_submenu,
            "model row should be marked as having a sub-menu"
        );
    }

    #[test]
    fn non_submenu_commands_are_not_marked() {
        let popup = CommandListPopup::new();
        for row in popup.rows() {
            if row.label != "/model" && row.label != "/skill" && row.label != "/sessions" {
                assert!(
                    !row.has_submenu,
                    "{} should not be marked as having a sub-menu",
                    row.label
                );
            }
        }
    }

    #[test]
    fn skill_command_opens_a_submenu() {
        let popup = CommandListPopup::new();
        let skill_row = popup.rows().iter().find(|r| r.label == "/skill").unwrap();
        assert!(skill_row.has_submenu);
    }

    #[test]
    fn sessions_command_opens_a_submenu() {
        let popup = CommandListPopup::new();
        let sessions_row = popup
            .rows()
            .iter()
            .find(|r| r.label == "/sessions")
            .unwrap();
        assert!(sessions_row.has_submenu);
    }

    #[test]
    fn enter_on_clear_commits_clear_context() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(CommandListPopup::new()));
        stack.set_query("cl"); // narrow to /clear
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, Some(CommandResult::ClearContext)));
        assert!(!stack.is_active());
    }

    #[test]
    fn enter_on_goal_inserts_the_required_template() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(CommandListPopup::new()));
        stack.set_query("go");
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::InsertInputTemplate(ref template)) if template == "/goal "
        ));
        assert!(!stack.is_active());
    }

    #[test]
    fn enter_on_model_pushes_submenu() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(CommandListPopup::new()));
        stack.set_query("mo"); // /model
        let result = stack.handle_key(key(KeyCode::Enter));
        // No final command — sub-popup is now on top.
        assert!(result.is_none());
        assert_eq!(stack.depth(), 2);
        assert_eq!(stack.breadcrumb()[0], "Slash commands");
    }
}
