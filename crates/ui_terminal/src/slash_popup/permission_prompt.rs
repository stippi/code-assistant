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

const DECISIONS: &[(PermissionDecision, &str, &str)] = &[
    (
        PermissionDecision::GrantedOnce,
        "Allow once",
        "Run this tool call, ask again next time",
    ),
    (
        PermissionDecision::GrantedSession,
        "Always allow (session)",
        "Run and stop asking for this tool in this session",
    ),
    (
        PermissionDecision::Denied,
        "Deny",
        "Reject the call; the agent is told not to retry",
    ),
];

pub struct PermissionPromptPopup {
    request_id: String,
    title: String,
    rows: Vec<PopupRow>,
    selected: usize,
}

impl PermissionPromptPopup {
    pub fn for_request(request: &ToolPermissionRequestData) -> Self {
        let rows = DECISIONS
            .iter()
            .map(|(_, label, description)| PopupRow {
                label: (*label).to_string(),
                description: (*description).to_string(),
                has_submenu: false,
            })
            .collect();
        Self {
            request_id: request.request_id.clone(),
            title: format!("Permission required: {}", request.summary),
            rows,
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
        let (decision, ..) = DECISIONS[self.selected];
        PopupAction::Commit(CommandResult::RespondPermission {
            request_id: Some(self.request_id.clone()),
            decision,
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn request(id: &str) -> ToolPermissionRequestData {
        ToolPermissionRequestData {
            request_id: id.to_string(),
            tool_id: Some("tool-1".to_string()),
            tool_name: "delete_files".to_string(),
            summary: "Run tool `delete_files`".to_string(),
            metadata: serde_json::json!({}),
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
