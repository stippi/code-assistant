use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    style::{Style, Stylize},
};
use tui_textarea::TextArea;

/// Manages the input area using tui-textarea
pub struct InputManager {
    pub textarea: TextArea<'static>,
}

impl InputManager {
    pub fn new() -> Self {
        Self {
            textarea: Self::create_text_area(),
        }
    }

    /// Handle a key event and return (should_quit, submitted_content)
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> (bool, Option<String>) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                return (true, None); // Signal to quit
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::SHIFT,
                ..
            } => {
                self.textarea.insert_newline();
                return (false, None);
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                // Submit input
                let content = self.get_content();
                if !content.is_empty() {
                    self.clear();
                    return (false, Some(content));
                }
                return (false, None);
            }
            _ => {
                // Forward the key event directly to tui-textarea
                self.textarea.input(key_event);
            }
        }
        (false, None)
    }

    /// Get the current content of the textarea
    pub fn get_content(&self) -> String {
        self.textarea.lines().join("\n")
    }

    fn create_text_area() -> TextArea<'static> {
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Type your message...");
        textarea.set_placeholder_style(Style::default().dim());
        textarea
    }

    /// Clear the textarea content
    pub fn clear(&mut self) {
        self.textarea = Self::create_text_area()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn create_key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn test_input_manager_basic_operations() {
        let mut input_manager = InputManager::new();

        // Test initial state
        assert_eq!(input_manager.get_content(), "");

        // Test character input
        let (quit, submitted) = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(!quit);
        assert!(submitted.is_none());

        let (quit, submitted) = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(!quit);
        assert!(submitted.is_none());

        // Content should contain the typed characters
        let content = input_manager.get_content();
        assert_eq!(content, "hi");

        // Test submission
        let (quit, submitted) =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!quit);
        assert_eq!(submitted, Some("hi".to_string()));

        // Content should be cleared after submission
        assert_eq!(input_manager.get_content(), "");
    }

    #[test]
    fn test_quit_signal() {
        let mut input_manager = InputManager::new();

        let (quit, submitted) = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(quit);
        assert!(submitted.is_none());
    }

    #[test]
    fn test_newline_handling() {
        let mut input_manager = InputManager::new();

        // Type some text
        input_manager.handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));

        // Shift+Enter should add newline without submitting
        let (quit, submitted) =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(!quit);
        assert!(submitted.is_none());

        // Add more text
        input_manager.handle_key_event(create_key_event(KeyCode::Char('b'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('y'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('e'), KeyModifiers::NONE));

        // Should have multiline content
        let content = input_manager.get_content();
        assert_eq!(content, "hi\nbye");

        // Regular Enter should submit
        let (quit, submitted) =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!quit);
        assert_eq!(submitted, Some("hi\nbye".to_string()));
    }
}
