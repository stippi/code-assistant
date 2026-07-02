//! Sub-popup that lets the user pick a skill to activate.
//!
//! Pushed by [`super::command_list::CommandListPopup`] when `/skill` is
//! activated. Unlike the model picker (which reads global config), the skills
//! are session-scoped and supplied from [`crate::state::AppState::skills`],
//! which is populated from a `SessionService::list_skills`
//! request.

use crate::commands::CommandResult;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use code_assistant_core::session::service::SkillCatalogEntry;

pub struct SkillPickerPopup {
    /// All skill entries (parallel to `all_rows`), used to dispatch on activate.
    all_entries: Vec<SkillCatalogEntry>,
    /// All rows, before filtering.
    all_rows: Vec<PopupRow>,
    /// Currently visible rows.
    visible_rows: Vec<PopupRow>,
    /// Indices into `all_entries`/`all_rows` for the visible rows.
    visible_indices: Vec<usize>,
    /// Highlighted row in `visible_rows`.
    selected: usize,
}

impl SkillPickerPopup {
    pub fn from_entries(entries: Vec<SkillCatalogEntry>) -> Self {
        let all_rows: Vec<PopupRow> = entries
            .iter()
            .map(|e| PopupRow {
                label: e.name.clone(),
                description: format!("({}) {}", e.scope_label, e.description),
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

    /// The entry for the currently highlighted visible row.
    fn selected_entry(&self) -> Option<&SkillCatalogEntry> {
        let idx = *self.visible_indices.get(self.selected)?;
        self.all_entries.get(idx)
    }
}

impl SlashPopup for SkillPickerPopup {
    fn title(&self) -> &str {
        "Activate skill"
    }

    fn set_query(&mut self, query: &str) {
        // Substring match (case-insensitive) on name + description so partial
        // typing surfaces results.
        self.visible_rows.clear();
        self.visible_indices.clear();
        let q = query.to_lowercase();
        for (i, entry) in self.all_entries.iter().enumerate() {
            if q.is_empty()
                || entry.name.to_lowercase().contains(&q)
                || entry.description.to_lowercase().contains(&q)
            {
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
            Some(entry) => PopupAction::Commit(CommandResult::InvokeSkill {
                scope: Some(entry.scope_token.clone()),
                name: entry.name.clone(),
            }),
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

    fn entry(name: &str, scope_token: &str, scope_label: &str, desc: &str) -> SkillCatalogEntry {
        SkillCatalogEntry {
            name: name.to_string(),
            description: desc.to_string(),
            scope_token: scope_token.to_string(),
            scope_label: scope_label.to_string(),
        }
    }

    #[test]
    fn lists_entries_with_scope_in_description() {
        let popup = SkillPickerPopup::from_entries(vec![
            entry("pdf", "proj", "project", "Extract PDFs."),
            entry("review", ":config:", "user", "Audit auth."),
        ]);
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["pdf", "review"]);
        assert!(popup.rows()[0].description.contains("(project)"));
        assert!(popup.rows()[1].description.contains("(user)"));
    }

    #[test]
    fn enter_commits_invoke_skill_with_scope_token() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(SkillPickerPopup::from_entries(vec![
            entry("pdf", "proj", "project", "Extract PDFs."),
            entry("review", ":config:", "user", "Audit auth."),
        ])));
        stack.handle_key(key(KeyCode::Down)); // move to "review"
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::InvokeSkill { ref scope, ref name })
                if scope.as_deref() == Some(":config:") && name == "review"
        ));
        assert!(!stack.is_active());
    }

    #[test]
    fn filter_matches_name_and_description() {
        let mut popup = SkillPickerPopup::from_entries(vec![
            entry(
                "pdf-extraction",
                "proj",
                "project",
                "Extract text from PDFs.",
            ),
            entry(
                "security-review",
                ":config:",
                "user",
                "Audit auth and crypto.",
            ),
        ]);
        popup.set_query("auth");
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["security-review"]);
    }

    #[test]
    fn empty_catalog_activate_is_noop() {
        let popup = SkillPickerPopup::from_entries(vec![]);
        assert!(matches!(popup.activate(), PopupAction::Continue));
    }
}
