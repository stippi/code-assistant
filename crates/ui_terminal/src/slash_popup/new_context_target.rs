//! Modal prompt asking where the context opened by `/new` or `/handoff`
//! continues.
//!
//! Like the permission prompt it is not user-initiated: the app event layer
//! pushes it when a [`UiEvent::RequestNewContextTarget`] arrives and removes
//! it when the request resolves. Enter answers with the highlighted target;
//! Esc cancels the command.
//!
//! [`UiEvent::RequestNewContextTarget`]: code_assistant_core::ui::UiEvent::RequestNewContextTarget

use crate::commands::CommandResult;
use crate::slash_popup::{PopupAction, PopupRow, SlashPopup};
use code_assistant_core::session::new_context::{NewContextTarget, NewContextTargetRequest};

pub struct NewContextTargetPopup {
    request_id: String,
    rows: Vec<PopupRow>,
    /// Target per row, parallel to `rows`, taken from the request's options.
    targets: Vec<NewContextTarget>,
    selected: usize,
}

impl NewContextTargetPopup {
    pub fn for_request(request: &NewContextTargetRequest) -> Self {
        Self {
            request_id: request.request_id.clone(),
            rows: request
                .options
                .iter()
                .map(|option| PopupRow {
                    label: option.label.clone(),
                    description: option.description.clone(),
                    has_submenu: false,
                })
                .collect(),
            targets: request.options.iter().map(|option| option.target).collect(),
            selected: 0,
        }
    }
}

impl SlashPopup for NewContextTargetPopup {
    fn title(&self) -> &str {
        "Where should the new context continue? (Esc cancels)"
    }

    fn set_query(&mut self, _query: &str) {
        // The typed text is not a filter.
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
        PopupAction::Commit(CommandResult::RespondNewContextTarget {
            request_id: self.request_id.clone(),
            target: self.targets[self.selected],
        })
    }

    fn escape(&self) -> PopupAction {
        PopupAction::Commit(CommandResult::CancelNewContext)
    }

    fn request_id(&self) -> Option<&str> {
        Some(&self.request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash_popup::PopupStack;
    use code_assistant_core::session::new_context::NewContextTargetOption;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn request() -> NewContextTargetRequest {
        let option = |target, label: &str| NewContextTargetOption {
            target,
            label: label.to_string(),
            description: String::new(),
        };
        NewContextTargetRequest {
            request_id: "new-context-1".to_string(),
            options: vec![
                option(NewContextTarget::SameSession, "This session"),
                option(NewContextTarget::NewSession, "New session"),
            ],
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_answers_with_the_highlighted_target() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(NewContextTargetPopup::for_request(&request())));
        stack.handle_key(key(KeyCode::Down));
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            Some(CommandResult::RespondNewContextTarget {
                ref request_id,
                target: NewContextTarget::NewSession,
            }) if request_id == "new-context-1"
        ));
        assert!(!stack.is_active());
    }

    #[test]
    fn esc_cancels_the_command() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(NewContextTargetPopup::for_request(&request())));
        let result = stack.handle_key(key(KeyCode::Esc));
        assert!(matches!(result, Some(CommandResult::CancelNewContext)));
        assert!(!stack.is_active());
    }

    #[test]
    fn the_stack_removes_it_when_the_request_resolves() {
        let mut stack = PopupStack::new();
        stack.push(Box::new(NewContextTargetPopup::for_request(&request())));
        assert!(stack.has_request_popup());
        stack.remove_request_popup("new-context-1");
        assert!(!stack.has_request_popup());
    }
}
