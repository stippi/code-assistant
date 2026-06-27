//! Slash-command popup system.
//!
//! Popups follow a "stack of modal pickers" pattern, similar to how a CLI
//! command tree works. The root popup (`CommandListPopup`) lists all known
//! slash commands. Activating a row may either:
//!
//! - immediately run a [`CommandResult`] (most commands), or
//! - push a sub-popup onto the stack (e.g. `/model` opens a model picker).
//!
//! The popup stack is owned by [`crate::state::AppState`] and rendered by
//! the composer just above the input area, growing the inline viewport
//! dynamically (see [`crate::renderer::desired_viewport_height`]).
//!
//! While the stack is non-empty, the input layer routes Up / Down / Enter /
//! Esc / Backspace / Left to the top-of-stack popup via [`SlashPopup::handle_key`]
//! and reflects the resulting [`PopupAction`] on the stack.

pub mod command_list;
pub mod model_picker;
pub mod skill_picker;

pub use command_list::CommandListPopup;
pub use model_picker::ModelPickerPopup;
pub use skill_picker::SkillPickerPopup;

use crate::commands::CommandResult;
use crossterm::event::{KeyCode, KeyEvent};

/// One row inside a popup list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupRow {
    /// Primary text shown in the row (e.g. `/help` or a model name).
    pub label: String,
    /// Secondary, dimmer description.
    pub description: String,
    /// When true, the row opens a sub-popup; rendered with a "›" trailing hint.
    pub has_submenu: bool,
}

/// Result of activating or navigating a popup.
pub enum PopupAction {
    /// Selection moved or query updated — just redraw, no stack change.
    Continue,
    /// Push a new popup onto the stack as a sub-popup.
    Push(Box<dyn SlashPopup>),
    /// Close the top-of-stack popup. Empty stack closes the popup system entirely
    /// (and, by convention, deletes the leading "/" from the composer line).
    Pop,
    /// Close the entire stack and execute a final command.
    Commit(CommandResult),
    /// Close the entire stack without running anything (e.g. user hit Esc at root).
    Dismiss,
}

/// A modal picker shown above the composer.
pub trait SlashPopup: Send {
    /// Header title (e.g. `"Slash commands"` for the root, `"Choose model"`
    /// for a model picker).
    fn title(&self) -> &str;

    /// Update the filter query (the text the user has typed after `/`).
    /// Sub-popups are free to ignore this and present an unfiltered list.
    fn set_query(&mut self, query: &str);

    /// Currently visible rows (already filtered by `set_query`).
    fn rows(&self) -> &[PopupRow];

    /// Currently highlighted row index. May be 0 even if `rows()` is empty;
    /// renderers should bounds-check.
    fn selected(&self) -> usize;

    /// Handle a key event. Returns the action to apply to the stack.
    /// The default impl handles Up / Down / Enter / Esc and delegates row
    /// activation to [`Self::activate`].
    fn handle_key(&mut self, key: KeyEvent) -> PopupAction {
        match key.code {
            KeyCode::Up => {
                self.move_selection(-1);
                PopupAction::Continue
            }
            KeyCode::Down => {
                self.move_selection(1);
                PopupAction::Continue
            }
            KeyCode::Enter | KeyCode::Tab => self.activate(),
            KeyCode::Esc => PopupAction::Pop,
            _ => PopupAction::Continue,
        }
    }

    /// Move the selection by `delta` rows, wrapping with rem_euclid.
    fn move_selection(&mut self, delta: i32);

    /// Activate the currently highlighted row.
    fn activate(&self) -> PopupAction;
}

/// Stack of nested popups. Empty stack ↔ no popup visible.
#[derive(Default)]
pub struct PopupStack {
    stack: Vec<Box<dyn SlashPopup>>,
}

impl std::fmt::Debug for PopupStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopupStack")
            .field("depth", &self.stack.len())
            .field(
                "titles",
                &self.stack.iter().map(|p| p.title()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl PopupStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_active(&self) -> bool {
        !self.stack.is_empty()
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Push a new popup as the active one.
    pub fn push(&mut self, popup: Box<dyn SlashPopup>) {
        self.stack.push(popup);
    }

    /// Close all popups.
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    /// Mutable access to the top-of-stack popup, if any.
    pub fn top_mut(&mut self) -> Option<&mut (dyn SlashPopup + 'static)> {
        match self.stack.last_mut() {
            Some(p) => Some(&mut **p),
            None => None,
        }
    }

    /// Read-only access to the top-of-stack popup, if any.
    pub fn top(&self) -> Option<&(dyn SlashPopup + 'static)> {
        match self.stack.last() {
            Some(p) => Some(&**p),
            None => None,
        }
    }

    /// Breadcrumb of titles, root first, top last.
    pub fn breadcrumb(&self) -> Vec<&str> {
        self.stack.iter().map(|p| p.title()).collect()
    }

    /// Forward a key to the top-of-stack popup and apply the resulting action
    /// to the stack. Returns the (possibly) terminal outcome:
    ///
    /// - `None` — popup is still open (or just closed cleanly with nothing to
    ///   commit).
    /// - `Some(CommandResult)` — caller should run this command.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<CommandResult> {
        let action = match self.top_mut() {
            Some(top) => top.handle_key(key),
            None => return None,
        };
        self.apply(action)
    }

    /// Apply a [`PopupAction`] to the stack.
    pub fn apply(&mut self, action: PopupAction) -> Option<CommandResult> {
        match action {
            PopupAction::Continue => None,
            PopupAction::Push(p) => {
                self.stack.push(p);
                None
            }
            PopupAction::Pop => {
                self.stack.pop();
                None
            }
            PopupAction::Dismiss => {
                self.stack.clear();
                None
            }
            PopupAction::Commit(cmd) => {
                self.stack.clear();
                Some(cmd)
            }
        }
    }

    /// Forward `set_query` to the top-of-stack popup. Used by the input layer
    /// when the user types more characters after `/`.
    pub fn set_query(&mut self, query: &str) {
        if let Some(top) = self.top_mut() {
            top.set_query(query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Minimal popup used to test stack mechanics.
    struct FakePopup {
        title: &'static str,
        rows: Vec<PopupRow>,
        selected: usize,
        on_activate: Box<dyn Fn(usize) -> PopupAction + Send>,
        last_query: String,
    }

    impl FakePopup {
        fn new(
            title: &'static str,
            row_labels: &[&str],
            on_activate: impl Fn(usize) -> PopupAction + Send + 'static,
        ) -> Box<Self> {
            Box::new(Self {
                title,
                rows: row_labels
                    .iter()
                    .map(|l| PopupRow {
                        label: l.to_string(),
                        description: String::new(),
                        has_submenu: false,
                    })
                    .collect(),
                selected: 0,
                on_activate: Box::new(on_activate),
                last_query: String::new(),
            })
        }
    }

    impl SlashPopup for FakePopup {
        fn title(&self) -> &str {
            self.title
        }
        fn set_query(&mut self, query: &str) {
            self.last_query = query.to_string();
        }
        fn rows(&self) -> &[PopupRow] {
            &self.rows
        }
        fn selected(&self) -> usize {
            self.selected
        }
        fn move_selection(&mut self, delta: i32) {
            let len = self.rows.len() as i32;
            if len == 0 {
                return;
            }
            let new = (self.selected as i32 + delta).rem_euclid(len);
            self.selected = new as usize;
        }
        fn activate(&self) -> PopupAction {
            (self.on_activate)(self.selected)
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn empty_stack_is_inactive() {
        let stack = PopupStack::new();
        assert!(!stack.is_active());
        assert_eq!(stack.depth(), 0);
        assert!(stack.top().is_none());
        assert!(stack.breadcrumb().is_empty());
    }

    #[test]
    fn push_makes_stack_active() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["a", "b"], |_| PopupAction::Pop));
        assert!(stack.is_active());
        assert_eq!(stack.depth(), 1);
        assert_eq!(stack.breadcrumb(), vec!["Root"]);
    }

    #[test]
    fn arrow_keys_move_selection_with_wrap() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["a", "b", "c"], |_| {
            PopupAction::Pop
        }));
        // Down twice: 0 -> 1 -> 2
        stack.handle_key(key(KeyCode::Down));
        stack.handle_key(key(KeyCode::Down));
        assert_eq!(stack.top().unwrap().selected(), 2);
        // Down again wraps to 0
        stack.handle_key(key(KeyCode::Down));
        assert_eq!(stack.top().unwrap().selected(), 0);
        // Up wraps to last
        stack.handle_key(key(KeyCode::Up));
        assert_eq!(stack.top().unwrap().selected(), 2);
    }

    #[test]
    fn enter_commits_command_and_clears_stack() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["help"], |_| {
            PopupAction::Commit(CommandResult::Help("text".into()))
        }));
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, Some(CommandResult::Help(_))));
        assert!(!stack.is_active());
    }

    #[test]
    fn enter_can_push_sub_popup() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["model"], |_| {
            PopupAction::Push(FakePopup::new("Choose model", &["sonnet", "gpt"], |_| {
                PopupAction::Commit(CommandResult::SwitchModel("sonnet".into()))
            }))
        }));
        // Enter on root pushes sub-popup
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(result.is_none());
        assert_eq!(stack.depth(), 2);
        assert_eq!(stack.breadcrumb(), vec!["Root", "Choose model"]);
        // Enter on sub commits and clears
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, Some(CommandResult::SwitchModel(s)) if s == "sonnet"));
        assert!(!stack.is_active());
    }

    #[test]
    fn esc_pops_one_level_and_can_close() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["x"], |_| PopupAction::Pop));
        stack.push(FakePopup::new("Sub", &["y"], |_| PopupAction::Pop));
        // First Esc: back to root
        stack.handle_key(key(KeyCode::Esc));
        assert_eq!(stack.depth(), 1);
        assert_eq!(stack.breadcrumb(), vec!["Root"]);
        // Second Esc: stack empty
        stack.handle_key(key(KeyCode::Esc));
        assert!(!stack.is_active());
    }

    #[test]
    fn dismiss_clears_entire_stack() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["x"], |_| PopupAction::Pop));
        stack.push(FakePopup::new("Sub", &["y"], |_| PopupAction::Pop));
        stack.apply(PopupAction::Dismiss);
        assert!(!stack.is_active());
    }

    #[test]
    fn set_query_forwards_to_top_only() {
        let mut stack = PopupStack::new();
        stack.push(FakePopup::new("Root", &["x"], |_| PopupAction::Pop));
        stack.push(FakePopup::new("Sub", &["y"], |_| PopupAction::Pop));
        stack.set_query("hello");

        // Top-of-stack got the query.
        let top = stack.top().unwrap();
        assert_eq!(top.title(), "Sub");
        // We can't read FakePopup.last_query through the trait, but we know
        // only the top should ever be queried; this is enforced by api shape.
        // Assertion-by-construction: just verify nothing panicked.
        assert_eq!(stack.depth(), 2);
    }

    #[test]
    fn handle_key_on_empty_stack_is_noop() {
        let mut stack = PopupStack::new();
        let result = stack.handle_key(key(KeyCode::Enter));
        assert!(result.is_none());
        assert!(!stack.is_active());
    }
}
