use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    style::{Style, Stylize},
};
use tui_textarea::TextArea;

use super::commands::{CommandProcessor, CommandResult};

/// Result of handling a key event
#[derive(Debug, PartialEq)]
pub enum KeyEventResult {
    /// Continue processing normally
    Continue,
    /// Quit the application
    Quit,
    /// Submit a message
    SendMessage(String),
    /// Escape key was pressed - main loop decides what to do
    Escape,
    /// Display information message
    ShowInfo(String),
    /// Switch to a different model
    SwitchModel(String),
    /// Show current model information
    ShowCurrentModel,
}

/// Manages the input area using tui-textarea
pub struct InputManager {
    pub textarea: TextArea<'static>,
    command_processor: Option<CommandProcessor>,
}

impl InputManager {
    pub fn new() -> Self {
        let command_processor = CommandProcessor::new().ok();
        Self {
            textarea: Self::create_text_area(),
            command_processor,
        }
    }

    /// Handle a key event and return the appropriate result
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> KeyEventResult {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => KeyEventResult::Quit,
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            } => KeyEventResult::Escape,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::SHIFT,
                ..
            } => {
                self.textarea.insert_newline();
                KeyEventResult::Continue
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

                    // Check if this is a slash command
                    if let Some(ref processor) = self.command_processor {
                        match processor.process_command(&content) {
                            CommandResult::Continue => KeyEventResult::SendMessage(content),
                            CommandResult::Help(help_text) => KeyEventResult::ShowInfo(help_text),
                            CommandResult::ListModels => {
                                KeyEventResult::ShowInfo(processor.get_models_list())
                            }
                            CommandResult::ListProviders => {
                                KeyEventResult::ShowInfo(processor.get_providers_list())
                            }
                            CommandResult::SwitchModel(model_name) => {
                                KeyEventResult::SwitchModel(model_name)
                            }
                            CommandResult::ShowCurrentModel => KeyEventResult::ShowCurrentModel,
                            CommandResult::InvalidCommand(error) => {
                                KeyEventResult::ShowInfo(format!("Error: {error}"))
                            }
                        }
                    } else {
                        // Command processor not available, treat as regular message
                        KeyEventResult::SendMessage(content)
                    }
                } else {
                    KeyEventResult::Continue
                }
            }
            _ => {
                // Forward the key event directly to tui-textarea
                self.textarea.input(key_event);
                KeyEventResult::Continue
            }
        }
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
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(result, KeyEventResult::Continue);

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(result, KeyEventResult::Continue);

        // Content should contain the typed characters
        let content = input_manager.get_content();
        assert_eq!(content, "hi");

        // Test submission
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(result, KeyEventResult::SendMessage("hi".to_string()));

        // Content should be cleared after submission
        assert_eq!(input_manager.get_content(), "");
    }

    #[test]
    fn test_quit_signal() {
        let mut input_manager = InputManager::new();

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(result, KeyEventResult::Quit);
    }

    #[test]
    fn test_escape_key() {
        let mut input_manager = InputManager::new();

        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(result, KeyEventResult::Escape);
    }

    #[test]
    fn test_newline_handling() {
        let mut input_manager = InputManager::new();

        // Type some text
        input_manager.handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));

        // Shift+Enter should add newline without submitting
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(result, KeyEventResult::Continue);

        // Add more text
        input_manager.handle_key_event(create_key_event(KeyCode::Char('b'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('y'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('e'), KeyModifiers::NONE));

        // Should have multiline content
        let content = input_manager.get_content();
        assert_eq!(content, "hi\nbye");

        // Regular Enter should submit
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(result, KeyEventResult::SendMessage("hi\nbye".to_string()));
    }
}
