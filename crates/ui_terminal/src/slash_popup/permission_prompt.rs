//! Modal prompt for a tool permission request.
//!
//! Unlike the other popups this one is not user-initiated: the app event
//! layer pushes it when a [`UiEvent::RequestToolPermission`] arrives (one at
//! a time, oldest first) and removes it when the request resolves. Enter
//! commits the highlighted decision; Esc closes the popup without answering —
//! the request stays pending and can still be answered with `/allow`,
//! `/always` or `/deny`.
//!
//! [`UiEvent::RequestToolPermission`]: code_assistant_core::ui::UiEvent::RequestToolPermission

use crate::commands::CommandResult;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use code_assistant_core::session::permissions::ToolPermissionRequestData;
use tools_core::PermissionDecision;

pub struct PermissionPromptPopup {
    request_id: String,
    title: String,
    rows: Vec<PopupRow>,
    /// Decision per row, parallel to `rows` — taken straight from the
    /// request's options so this popup never decides the choices itself.
    decisions: Vec<PermissionDecision>,
    selected: usize,
}

impl PermissionPromptPopup {
    pub fn for_request(request: &ToolPermissionRequestData) -> Self {
        let rows = request
            .options
            .iter()
            .map(|option| PopupRow {
                label: option.label.clone(),
                description: option.description.clone(),
                has_submenu: false,
            })
            .collect();
        let decisions = request
            .options
            .iter()
            .map(|option| option.decision)
            .collect();
        Self {
            request_id: request.request_id.clone(),
            title: format!("Permission required: {}", request.summary),
            rows,
            decisions,
            selected: 0,
        }
    }
}

impl SlashPopup for PermissionPromptPopup {
    fn title(&self) -> &str {
        &self.title
    }

    fn set_query(&mut self, _query: &str) {
        // The user may keep composing a message while the prompt is open;
        // the typed text is not a filter.
    }

    fn rows(&self) -> &[PopupRow] {
        &self.rows
    }

    fn selected(&self) -> usize {
        self.selected
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.rows.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
    }

    fn activate(&self) -> PopupAction {
        PopupAction::Commit(CommandResult::RespondPermission {
            request_id: Some(self.request_id.clone()),
            decision: self.decisions[self.selected],
        })
    }

    fn permission_request_id(&self) -> Option<&str> {
        Some(&self.request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash_popup::PopupStack;
    use code_assistant_core::session::permissions::PermissionOption;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn option(decision: PermissionDecision, label: &str) -> PermissionOption {
        PermissionOption {
            decision,
            label: label.to_string(),
            description: String::new(),
        }
    }

    fn request(id: &str) -> ToolPermissionRequestData {
        ToolPermissionRequestData {
            request_id: id.to_string(),
            tool_id: Some("tool-1".to_string()),
            tool_name: "delete_files".to_string(),
            summary: "Run tool `delete_files`".to_string(),
            metadata: serde_json::json!({}),
            options: vec![
                option(PermissionDecision::GrantedOnce, "Allow once"),
                option(PermissionDecision::GrantedSession, "Always (session)"),
                option(PermissionDecision::Denied, "Deny"),
            ],
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_commits_decision_with_request_id() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(PermissionPromptPopup::for_request(&request("r1"))));
        stack.handle_key(key(KeyCode::Down)); // "Always allow (session)"
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::RespondPermission {
                ref request_id,
                decision: PermissionDecision::GrantedSession,
            }) if request_id.as_deref() == Some("r1")
        ));
        assert!(!stack.is_active());
    }

    #[test]
    fn esc_closes_without_answering() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(PermissionPromptPopup::for_request(&request("r1"))));
        let result = stack.handle_key(key(KeyCode::Esc));
        assert!(result.is_none());
        assert!(!stack.is_active());
    }

    #[test]
    fn typed_query_does_not_change_rows() {
        let mut popup = PermissionPromptPopup::for_request(&request("r1"));
        popup.set_query("some draft text");
        assert_eq!(popup.rows().len(), 3);
    }

    #[test]
    fn stack_finds_and_removes_permission_popup_by_id() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(PermissionPromptPopup::for_request(&request("r1"))));
        assert!(stack.has_permission_popup());
        stack.remove_permission_popup("other");
        assert!(stack.has_permission_popup());
        stack.remove_permission_popup("r1");
        assert!(!stack.has_permission_popup());
        assert!(!stack.is_active());
    }
}
