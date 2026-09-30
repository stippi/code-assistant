//! Root popup that lists all known slash commands, followed by the session's
//! skills (`/<skill-name> <request>`).

use crate::commands::{CommandResult, all_commands};
use crate::slash_popup::session_picker::SessionPickerPopup;
use crate::slash_popup::skill_picker::SkillPickerPopup;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::service::SkillCatalogEntry;

/// What a root row stands for.
enum Entry {
    Command(&'static str),
    Skill(String),
}

impl Entry {
    fn name(&self) -> &str {
        match self {
            Entry::Command(name) => name,
            Entry::Skill(name) => name,
        }
    }
}

pub struct CommandListPopup {
    /// All rows the popup knows about, before filtering.
    all_rows: Vec<PopupRow>,
    /// What each row stands for (parallel to `all_rows`); used to dispatch on
    /// activate.
    all_entries: Vec<Entry>,
    /// Currently visible rows after filtering.
    visible_rows: Vec<PopupRow>,
    /// Indices into `all_rows`/`all_entries` for the currently visible rows.
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
        let mut all_entries = Vec::new();
        for cmd in all_commands() {
            all_rows.push(PopupRow {
                label: format!("/{}", cmd.name),
                description: cmd.description.to_string(),
                has_submenu: command_has_submenu(cmd.name),
            });
            all_entries.push(Entry::Command(cmd.name));
        }
        // A skill named like a command is shadowed by it.
        for skill in skills
            .iter()
            .filter(|skill| !all_commands().iter().any(|cmd| cmd.name == skill.name))
        {
            all_rows.push(PopupRow {
                label: format!("/{}", skill.name),
                description: format!("({}) {}", skill.scope_label, skill.description),
                has_submenu: false,
            });
            all_entries.push(Entry::Skill(skill.name.clone()));
        }
        let visible_indices = (0..all_rows.len()).collect::<Vec<_>>();
        let visible_rows = all_rows.clone();
        Self {
            all_rows,
            all_entries,
            visible_rows,
            visible_indices,
            selected: 0,
            skills,
            sessions,
        }
    }

    /// Return the entry for the currently selected visible row.
    fn selected_entry(&self) -> Option<&Entry> {
        let idx = *self.visible_indices.get(self.selected)?;
        self.all_entries.get(idx)
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
        // Waiting for the argument that goes with them.
        "goal" | "new" | "hand-off" | "compact" => {
            PopupAction::Commit(CommandResult::InsertInputTemplate(format!("/{name} ")))
        }
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
        for (i, entry) in self.all_entries.iter().enumerate() {
            if entry.name().to_lowercase().starts_with(&q) {
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
        match self.selected_entry() {
            // The skill picker needs the session-scoped catalog, which the
            // static `dispatch_command` table can't provide; build it here from
            // the cached entries instead.
            Some(Entry::Command("skill")) => PopupAction::Push(Box::new(
                SkillPickerPopup::from_entries(self.skills.clone()),
            )),
            // The session picker needs the cached session list, which the
            // static `dispatch_command` table can't provide; build it here.
            Some(Entry::Command("sessions")) => PopupAction::Push(Box::new(
                SessionPickerPopup::from_sessions(self.sessions.clone()),
            )),
            Some(Entry::Command(name)) => dispatch_command(name),
            // The trigger goes into the composer, ready for the request.
            Some(Entry::Skill(name)) => {
                PopupAction::Commit(CommandResult::InsertInputTemplate(format!("/{name} ")))
            }
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

    fn skill(name: &str) -> SkillCatalogEntry {
        SkillCatalogEntry {
            name: name.to_string(),
            description: "Audit auth.".to_string(),
            scope_label: "user".to_string(),
        }
    }

    #[test]
    fn skills_are_listed_after_the_commands() {
        let mut popup = CommandListPopup::with_skills(vec![skill("review")]);
        assert_eq!(popup.rows().len(), all_commands().len() + 1);
        let last = popup.rows().last().unwrap();
        assert_eq!(last.label, "/review");
        assert_eq!(last.description, "(user) Audit auth.");
        popup.set_query("rev");
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["/review"]);
    }

    #[test]
    fn enter_on_a_skill_inserts_its_trigger() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(CommandListPopup::with_skills(vec![skill(
            "review",
        )])));
        stack.set_query("review");
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::InsertInputTemplate(ref template)) if template == "/review "
        ));
    }

    #[test]
    fn enter_on_a_new_context_command_inserts_its_template() {
        for (query, template) in [
            ("new", "/new "),
            ("hand", "/hand-off "),
            ("comp", "/compact "),
        ] {
            let mut stack = PopupStack::new();
            stack.push(Box::new(CommandListPopup::new()));
            stack.set_query(query);
            let result = stack.handle_key(key(KeyCode::Enter));
            assert!(
                matches!(result, Some(CommandResult::InsertInputTemplate(ref t)) if t == template),
                "{query}: {result:?}"
            );
        }
    }

    #[test]
    fn a_skill_named_like_a_command_is_shadowed() {
        let popup = CommandListPopup::with_skills(vec![skill("hand-off"), skill("review")]);
        assert_eq!(popup.rows().len(), all_commands().len() + 1);
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
