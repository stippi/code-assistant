//! Sub-popup that lets the user pick a model by name.
//!
//! Pushed by [`super::command_list::CommandListPopup`] when `/model` is
//! activated without an inline argument. Models are read from the
//! shared [`ConfigurationSystem`] at popup-construction time.

use crate::commands::CommandResult;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use llm::provider_config::ConfigurationSystem;

pub struct ModelPickerPopup {
    /// All model rows, before any future filtering.
    all_rows: Vec<PopupRow>,
    /// Currently visible rows.
    visible_rows: Vec<PopupRow>,
    /// Highlighted row in `visible_rows`.
    selected: usize,
}

impl ModelPickerPopup {
    pub fn new() -> Self {
        let rows = build_rows_from_config();
        Self::from_rows(rows)
    }

    /// Construction helper used by tests.
    fn from_rows(rows: Vec<PopupRow>) -> Self {
        Self {
            all_rows: rows.clone(),
            visible_rows: rows,
            selected: 0,
        }
    }
}

impl Default for ModelPickerPopup {
    fn default() -> Self {
        Self::new()
    }
}

fn build_rows_from_config() -> Vec<PopupRow> {
    let Ok(config) = ConfigurationSystem::load() else {
        return Vec::new();
    };
    let mut names: Vec<String> = config.models.keys().cloned().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let description = config
                .get_model_with_provider(&name)
                .map(|(_, prov)| prov.label.clone())
                .unwrap_or_default();
            PopupRow {
                label: name,
                description,
                has_submenu: false,
            }
        })
        .collect()
}

impl SlashPopup for ModelPickerPopup {
    fn title(&self) -> &str {
        "Choose model"
    }

    fn set_query(&mut self, query: &str) {
        // While the popup is open the composer text becomes the popup query.
        // Substring match (case-insensitive) so partial mid-string typos still
        // surface results (e.g. "sonnet" finds "Claude Sonnet 4.5").
        if query.is_empty() {
            self.visible_rows = self.all_rows.clone();
        } else {
            let q = query.to_lowercase();
            self.visible_rows = self
                .all_rows
                .iter()
                .filter(|r| r.label.to_lowercase().contains(&q))
                .cloned()
                .collect();
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
        match self.visible_rows.get(self.selected) {
            Some(row) => PopupAction::Commit(CommandResult::SwitchModel(row.label.clone())),
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

    fn fake_popup_with_models(models: &[&str]) -> ModelPickerPopup {
        let rows = models
            .iter()
            .map(|m| PopupRow {
                label: (*m).to_string(),
                description: "fake provider".to_string(),
                has_submenu: false,
            })
            .collect();
        ModelPickerPopup::from_rows(rows)
    }

    #[test]
    fn enter_commits_switch_model_with_selected_label() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(fake_popup_with_models(&[
            "Claude Sonnet 4.5",
            "GPT-5",
        ])));
        stack.handle_key(key(KeyCode::Down)); // move to GPT-5
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::SwitchModel(ref s)) if s == "GPT-5"
        ));
        assert!(!stack.is_active());
    }

    #[test]
    fn esc_pops_one_level_back_to_root() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(super::super::command_list::CommandListPopup::new()));
        stack.push(Box::new(fake_popup_with_models(&["Claude Sonnet 4.5"])));
        assert_eq!(stack.depth(), 2);
        stack.handle_key(key(KeyCode::Esc));
        assert_eq!(stack.depth(), 1);
        assert_eq!(stack.top().unwrap().title(), "Slash commands");
    }

    #[test]
    fn empty_model_list_does_not_panic_on_activate() {
        let popup = fake_popup_with_models(&[]);
        let action = popup.activate();
        assert!(matches!(action, PopupAction::Continue));
    }

    #[test]
    fn substring_filter_narrows_visible_rows() {
        let mut popup = fake_popup_with_models(&["Claude Sonnet 4.5", "GPT-5", "Claude Haiku"]);
        popup.set_query("claude");
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["Claude Sonnet 4.5", "Claude Haiku"]);
    }

    #[test]
    fn substring_filter_is_case_insensitive() {
        let mut popup = fake_popup_with_models(&["Claude Sonnet 4.5", "GPT-5"]);
        popup.set_query("CLA");
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["Claude Sonnet 4.5"]);
    }

    #[test]
    fn substring_filter_matches_mid_string() {
        // "sonnet" appears mid-string in "Claude Sonnet 4.5" — should still match.
        let mut popup = fake_popup_with_models(&["Claude Sonnet 4.5", "GPT-5"]);
        popup.set_query("sonnet");
        let labels: Vec<&str> = popup.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["Claude Sonnet 4.5"]);
    }
}
